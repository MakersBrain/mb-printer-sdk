// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reusable asynchronous native transport and discovery boundaries.
use crate::{
    NotificationSupport, Transport, TransportError, TransportErrorKind, TransportFuture,
    WaitOutcome, WriteKind,
};
#[cfg(all(feature = "bluetooth-rfcomm", target_os = "linux"))]
use std::fs::File;
use std::{net::SocketAddr, path::Path, time::Duration};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn io_error(operation: &'static str, _error: impl std::fmt::Display) -> TransportError {
    TransportError::new(TransportErrorKind::Io, format!("{operation} failed"))
}

#[cfg(any(feature = "serial", feature = "usb", feature = "bluetooth-rfcomm"))]
trait BlockingIo: Send + 'static {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn read(&mut self, timeout: Duration) -> Result<WaitOutcome, String>;
    fn disconnect(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(any(feature = "serial", feature = "usb", feature = "bluetooth-rfcomm"))]
enum DeviceCommand {
    Write(Vec<u8>, tokio::sync::oneshot::Sender<Result<(), String>>),
    Read(
        Duration,
        tokio::sync::oneshot::Sender<Result<WaitOutcome, String>>,
    ),
    Disconnect(tokio::sync::oneshot::Sender<Result<(), String>>),
}

/// One bounded queue and one persistent owner thread per blocking device.
#[cfg(any(feature = "serial", feature = "usb", feature = "bluetooth-rfcomm"))]
struct PersistentDevice {
    commands: tokio::sync::mpsc::Sender<DeviceCommand>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(any(feature = "serial", feature = "usb", feature = "bluetooth-rfcomm"))]
impl PersistentDevice {
    fn spawn<B: BlockingIo>(mut backend: B) -> Self {
        let (commands, mut receiver) = tokio::sync::mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            while let Some(command) = receiver.blocking_recv() {
                match command {
                    DeviceCommand::Write(bytes, reply) => {
                        let _ = reply.send(backend.write(&bytes));
                    }
                    DeviceCommand::Read(timeout, reply) => {
                        let _ = reply.send(backend.read(timeout));
                    }
                    DeviceCommand::Disconnect(reply) => {
                        let result = backend.disconnect();
                        let _ = reply.send(result);
                        break;
                    }
                }
            }
        });
        Self {
            commands,
            worker: Some(worker),
        }
    }

    async fn write(&self, bytes: &[u8]) -> Result<(), TransportError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.commands
            .send(DeviceCommand::Write(bytes.to_vec(), reply))
            .await
            .map_err(|_| {
                TransportError::new(TransportErrorKind::Disconnected, "device worker stopped")
            })?;
        result
            .await
            .map_err(|_| {
                TransportError::new(TransportErrorKind::Disconnected, "device worker stopped")
            })?
            .map_err(|error| io_error("device write", error))
    }

    async fn read(&self, timeout: Duration) -> Result<WaitOutcome, TransportError> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.commands
            .send(DeviceCommand::Read(timeout, reply))
            .await
            .map_err(|_| {
                TransportError::new(TransportErrorKind::Disconnected, "device worker stopped")
            })?;
        result
            .await
            .map_err(|_| {
                TransportError::new(TransportErrorKind::Disconnected, "device worker stopped")
            })?
            .map_err(|error| io_error("device read", error))
    }

    async fn disconnect(&mut self) -> Result<(), TransportError> {
        if self.worker.is_none() {
            return Ok(());
        }
        let (reply, result) = tokio::sync::oneshot::channel();
        self.commands
            .send(DeviceCommand::Disconnect(reply))
            .await
            .map_err(|_| {
                TransportError::new(TransportErrorKind::Disconnected, "device worker stopped")
            })?;
        let backend_result = result
            .await
            .map_err(|_| {
                TransportError::new(TransportErrorKind::Disconnected, "device worker stopped")
            })?
            .map_err(|error| io_error("device disconnect", error));
        if let Some(worker) = self.worker.take() {
            tokio::task::spawn_blocking(move || worker.join())
                .await
                .map_err(|_| {
                    TransportError::new(TransportErrorKind::Io, "device worker join failed")
                })?
                .map_err(|_| {
                    TransportError::new(TransportErrorKind::Io, "device worker panicked")
                })?;
        }
        backend_result
    }
}

#[cfg(any(feature = "serial", feature = "usb", feature = "bluetooth-rfcomm"))]
impl Drop for PersistentDevice {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let (reply, _) = tokio::sync::oneshot::channel();
            let _ = self.commands.try_send(DeviceCommand::Disconnect(reply));
            // Never block Drop. Dropping the handle detaches a tardy worker.
            self.worker.take();
        }
    }
}

#[cfg(feature = "snmp")]
pub mod snmp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPrinter {
    pub transport: &'static str,
    pub id: String,
    pub name: Option<String>,
    pub endpoint: String,
}
pub trait DiscoveryBackend {
    fn discover(&self) -> Result<Vec<DiscoveredPrinter>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterStatus {
    pub online: bool,
    pub state: String,
    pub raw: Vec<u8>,
}
pub trait StatusBackend {
    fn query_status(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<PrinterStatus, TransportError>>;
}
pub struct CommandStatusBackend<T, F> {
    pub transport: T,
    pub query: Vec<u8>,
    pub decode: F,
}
impl<T: Transport, F: Fn(&[u8]) -> Result<PrinterStatus, String> + Send> StatusBackend
    for CommandStatusBackend<T, F>
{
    fn query_status(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<PrinterStatus, TransportError>> {
        Box::pin(async move {
            self.transport
                .write(&self.query, WriteKind::Command)
                .await?;
            match self.transport.wait_response(timeout).await? {
                WaitOutcome::Response(bytes) => (self.decode)(&bytes).map_err(|error| {
                    TransportError::new(TransportErrorKind::InvalidConfiguration, error)
                }),
                WaitOutcome::Timeout => Err(TransportError::new(
                    TransportErrorKind::Timeout,
                    "status response timed out",
                )),
                WaitOutcome::Unavailable => Err(TransportError::new(
                    TransportErrorKind::Unsupported,
                    "status response unavailable",
                )),
            }
        })
    }
}

pub struct FileTransport {
    file: tokio::fs::File,
    payload_limit: usize,
}
impl FileTransport {
    pub async fn open(
        path: impl AsRef<Path>,
        payload_limit: usize,
    ) -> Result<Self, TransportError> {
        if payload_limit == 0 {
            return Err(TransportError::new(
                TransportErrorKind::InvalidConfiguration,
                "file payload limit must be positive",
            ));
        }
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .await
            .map_err(|error| io_error("file open", error))?;
        Ok(Self {
            file,
            payload_limit,
        })
    }
}
impl Transport for FileTransport {
    fn payload_limit(&self) -> usize {
        self.payload_limit
    }
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        Box::pin(async { Ok(NotificationSupport::Unavailable) })
    }
    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        _: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            self.file
                .write_all(bytes)
                .await
                .map_err(|error| io_error("file write", error))?;
            self.file
                .flush()
                .await
                .map_err(|error| io_error("file flush", error))
        })
    }
    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
    fn wait_response(
        &mut self,
        _: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        Box::pin(async { Ok(WaitOutcome::Unavailable) })
    }
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            self.file
                .flush()
                .await
                .map_err(|error| io_error("file flush", error))
        })
    }
}

pub struct TcpTransport {
    stream: tokio::net::TcpStream,
    payload_limit: usize,
    response_limit: usize,
}
impl TcpTransport {
    pub async fn connect(
        address: SocketAddr,
        payload_limit: usize,
        response_limit: usize,
    ) -> Result<Self, TransportError> {
        if payload_limit == 0 || response_limit == 0 {
            return Err(TransportError::new(
                TransportErrorKind::InvalidConfiguration,
                "TCP limits must be positive",
            ));
        }
        let stream = tokio::net::TcpStream::connect(address)
            .await
            .map_err(|error| io_error("TCP connect", error))?;
        stream
            .set_nodelay(true)
            .map_err(|error| io_error("TCP configuration", error))?;
        Ok(Self {
            stream,
            payload_limit,
            response_limit,
        })
    }
}
impl Transport for TcpTransport {
    fn payload_limit(&self) -> usize {
        self.payload_limit
    }
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        Box::pin(async { Ok(NotificationSupport::Unavailable) })
    }
    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        _: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            self.stream
                .write_all(bytes)
                .await
                .map_err(|error| io_error("TCP write", error))
        })
    }
    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        Box::pin(async move {
            let mut bytes = vec![0; self.response_limit];
            match tokio::time::timeout(timeout, self.stream.read(&mut bytes)).await {
                Err(_) => Ok(WaitOutcome::Timeout),
                Ok(Ok(0)) => Ok(WaitOutcome::Unavailable),
                Ok(Ok(length)) => {
                    bytes.truncate(length);
                    Ok(WaitOutcome::Response(bytes))
                }
                Ok(Err(error)) => Err(io_error("TCP read", error)),
            }
        })
    }
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            self.stream
                .shutdown()
                .await
                .map_err(|error| io_error("TCP shutdown", error))
        })
    }
}

#[cfg(feature = "serial")]
pub mod serial {
    use super::*;
    use std::path::PathBuf;
    #[derive(Debug, Clone, Copy)]
    pub struct SerialConfig {
        pub baud_rate: u32,
        pub timeout_ms: u64,
        pub payload_limit: usize,
        pub response_limit: usize,
    }
    impl Default for SerialConfig {
        fn default() -> Self {
            Self {
                baud_rate: 115_200,
                timeout_ms: 500,
                payload_limit: 512,
                response_limit: 64,
            }
        }
    }
    pub struct SerialTransport {
        device: PersistentDevice,
        config: SerialConfig,
    }
    struct SerialBackend {
        port: Box<dyn serialport::SerialPort>,
        response_limit: usize,
    }
    impl BlockingIo for SerialBackend {
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            std::io::Write::write_all(&mut self.port, bytes)
                .and_then(|()| std::io::Write::flush(&mut self.port))
                .map_err(|error| error.to_string())
        }
        fn read(&mut self, timeout: Duration) -> Result<WaitOutcome, String> {
            self.port
                .set_timeout(timeout)
                .map_err(|error| error.to_string())?;
            let mut bytes = vec![0; self.response_limit];
            match std::io::Read::read(&mut self.port, &mut bytes) {
                Ok(0) => Ok(WaitOutcome::Unavailable),
                Ok(length) => {
                    bytes.truncate(length);
                    Ok(WaitOutcome::Response(bytes))
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    Ok(WaitOutcome::Timeout)
                }
                Err(error) => Err(error.to_string()),
            }
        }
    }
    impl SerialTransport {
        /// Opens and configures a serial device with the default 115200 8-N-1 profile.
        pub fn open(path: impl AsRef<Path>, payload_limit: usize) -> Result<Self, TransportError> {
            Self::open_configured(
                path,
                SerialConfig {
                    payload_limit,
                    ..Default::default()
                },
            )
        }
        pub fn open_configured(
            path: impl AsRef<Path>,
            config: SerialConfig,
        ) -> Result<Self, TransportError> {
            if config.payload_limit == 0 || config.response_limit == 0 || config.timeout_ms == 0 {
                return Err(TransportError::new(
                    TransportErrorKind::InvalidConfiguration,
                    "serial limits must be positive",
                ));
            }
            let port = serialport::new(path.as_ref().to_string_lossy(), config.baud_rate)
                .timeout(Duration::from_millis(config.timeout_ms))
                .data_bits(serialport::DataBits::Eight)
                .parity(serialport::Parity::None)
                .stop_bits(serialport::StopBits::One)
                .flow_control(serialport::FlowControl::None)
                .open()
                .map_err(|error| io_error("serial open", error))?;
            Ok(Self {
                device: PersistentDevice::spawn(SerialBackend {
                    port,
                    response_limit: config.response_limit,
                }),
                config,
            })
        }
    }
    impl Transport for SerialTransport {
        fn payload_limit(&self) -> usize {
            self.config.payload_limit
        }
        fn subscribe_notifications(
            &mut self,
        ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
            Box::pin(async { Ok(NotificationSupport::Unavailable) })
        }
        fn write<'a>(
            &'a mut self,
            bytes: &'a [u8],
            _: WriteKind,
        ) -> TransportFuture<'a, Result<(), TransportError>> {
            Box::pin(async move { self.device.write(bytes).await })
        }
        fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
            Box::pin(tokio::time::sleep(duration))
        }
        fn wait_response(
            &mut self,
            timeout: Duration,
        ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
            Box::pin(async move { self.device.read(timeout).await })
        }
        fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
            Box::pin(async move { self.device.disconnect().await })
        }
    }
    #[derive(Debug, Clone)]
    pub struct SerialDiscovery {
        pub directories: Vec<PathBuf>,
    }
    impl Default for SerialDiscovery {
        fn default() -> Self {
            Self {
                directories: vec![PathBuf::from("/dev")],
            }
        }
    }
    impl DiscoveryBackend for SerialDiscovery {
        fn discover(&self) -> Result<Vec<DiscoveredPrinter>, String> {
            if self.directories == [PathBuf::from("/dev")] {
                let mut ports = serialport::available_ports()
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .filter(|port| {
                        #[cfg(unix)]
                        {
                            Path::new(&port.port_name).exists()
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = port;
                            true
                        }
                    })
                    .map(|port| {
                        let name = match port.port_type {
                            serialport::SerialPortType::UsbPort(info) => info.product,
                            serialport::SerialPortType::BluetoothPort => {
                                Some("Bluetooth serial".into())
                            }
                            _ => None,
                        };
                        DiscoveredPrinter {
                            transport: "serial",
                            id: port.port_name.clone(),
                            name,
                            endpoint: port.port_name,
                        }
                    })
                    .collect::<Vec<_>>();
                ports.sort_by(|left, right| left.endpoint.cmp(&right.endpoint));
                return Ok(ports);
            }
            let mut found = Vec::new();
            for directory in &self.directories {
                let entries = match std::fs::read_dir(directory) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                for entry in entries {
                    let path = entry.map_err(|error| error.to_string())?.path();
                    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    if ["ttyUSB", "ttyACM", "ttyS", "cu.", "rfcomm"]
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
                    {
                        found.push(DiscoveredPrinter {
                            transport: "serial",
                            id: name.into(),
                            name: None,
                            endpoint: path.to_string_lossy().into_owned(),
                        });
                    }
                }
            }
            found.sort_by(|left, right| left.endpoint.cmp(&right.endpoint));
            Ok(found)
        }
    }
}

#[cfg(feature = "usb")]
pub mod usb {
    use super::*;
    use rusb::UsbContext as _;
    pub const MAX_IEEE1284_DEVICE_ID_BYTES: usize =
        mb_printer_core::protocol::ieee1284::MAX_DEVICE_ID_BYTES;
    pub use mb_printer_core::protocol::ieee1284::DeviceId as Ieee1284DeviceId;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UsbPortStatus {
        pub selected: bool,
        pub paper_empty: bool,
        pub error: bool,
    }

    pub fn parse_ieee1284_device_id(data: &[u8]) -> Result<Ieee1284DeviceId, String> {
        mb_printer_core::protocol::ieee1284::parse_device_id(data)
            .map_err(|error| error.to_string())
    }

    pub const fn parse_port_status(value: u8) -> UsbPortStatus {
        UsbPortStatus {
            selected: value & 0x10 != 0,
            paper_empty: value & 0x20 != 0,
            error: value & 0x08 == 0,
        }
    }

    pub trait UsbBulkBackend {
        fn write_bulk(&mut self, bytes: &[u8]) -> Result<(), String>;
        fn read_bulk(&mut self, timeout_ms: u64, maximum: usize)
        -> Result<Option<Vec<u8>>, String>;
    }
    pub trait UsbPrinterClassBackend {
        fn get_device_id_raw(
            &mut self,
            timeout_ms: u64,
            maximum: usize,
        ) -> Result<Option<Vec<u8>>, String>;
        fn get_port_status_raw(&mut self, timeout_ms: u64) -> Result<Option<u8>, String>;
    }
    pub trait UsbDiscoveryBackend {
        fn discover_usb(&self) -> Result<Vec<DiscoveredPrinter>, String>;
    }
    pub struct UsbTransport<B> {
        backend: std::sync::Arc<std::sync::Mutex<B>>,
        device: PersistentDevice,
        payload_limit: usize,
        command_limit: usize,
    }
    struct UsbIo<B> {
        backend: std::sync::Arc<std::sync::Mutex<B>>,
        response_limit: usize,
    }
    impl<B: UsbBulkBackend + Send + 'static> BlockingIo for UsbIo<B> {
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.backend
                .lock()
                .map_err(|_| "USB backend lock failed".to_owned())?
                .write_bulk(bytes)
        }
        fn read(&mut self, timeout: Duration) -> Result<WaitOutcome, String> {
            let timeout_ms = u64::try_from(timeout.as_millis())
                .unwrap_or(u64::MAX)
                .max(1);
            self.backend
                .lock()
                .map_err(|_| "USB backend lock failed".to_owned())?
                .read_bulk(timeout_ms, self.response_limit)
                .map(|value| value.map_or(WaitOutcome::Timeout, WaitOutcome::Response))
        }
    }
    impl<B: UsbBulkBackend + Send + 'static> UsbTransport<B> {
        pub fn new(backend: B, payload_limit: usize, response_limit: usize) -> Self {
            Self::new_with_limits(backend, payload_limit, payload_limit, response_limit)
        }
        pub fn new_with_limits(
            backend: B,
            payload_limit: usize,
            command_limit: usize,
            response_limit: usize,
        ) -> Self {
            let backend = std::sync::Arc::new(std::sync::Mutex::new(backend));
            let device = PersistentDevice::spawn(UsbIo {
                backend: backend.clone(),
                response_limit,
            });
            Self {
                backend,
                device,
                payload_limit,
                command_limit,
            }
        }
        pub fn backend(&self) -> std::sync::MutexGuard<'_, B> {
            self.backend.lock().expect("USB backend lock poisoned")
        }
    }
    impl<B: UsbBulkBackend + UsbPrinterClassBackend + Send + 'static> UsbTransport<B> {
        pub async fn get_device_id(
            &mut self,
            timeout_ms: u64,
        ) -> Result<Option<Ieee1284DeviceId>, TransportError> {
            if timeout_ms == 0 {
                return Err(TransportError::new(
                    TransportErrorKind::InvalidConfiguration,
                    "USB Printer Class timeout must be positive",
                ));
            }
            let backend = self.backend.clone();
            tokio::task::spawn_blocking(move || {
                backend
                    .lock()
                    .map_err(|_| "USB backend lock failed".to_owned())?
                    .get_device_id_raw(timeout_ms, MAX_IEEE1284_DEVICE_ID_BYTES)
            })
            .await
            .map_err(|_| TransportError::new(TransportErrorKind::Io, "USB worker failed"))?
            .map_err(|error| io_error("USB device ID", error))?
            .map(|data| parse_ieee1284_device_id(&data))
            .transpose()
            .map_err(|error| TransportError::new(TransportErrorKind::InvalidConfiguration, error))
        }

        pub async fn get_port_status(
            &mut self,
            timeout_ms: u64,
        ) -> Result<Option<UsbPortStatus>, TransportError> {
            if timeout_ms == 0 {
                return Err(TransportError::new(
                    TransportErrorKind::InvalidConfiguration,
                    "USB Printer Class timeout must be positive",
                ));
            }
            let backend = self.backend.clone();
            tokio::task::spawn_blocking(move || {
                backend
                    .lock()
                    .map_err(|_| "USB backend lock failed".to_owned())?
                    .get_port_status_raw(timeout_ms)
            })
            .await
            .map_err(|_| TransportError::new(TransportErrorKind::Io, "USB worker failed"))?
            .map(|value| value.map(parse_port_status))
            .map_err(|error| io_error("USB port status", error))
        }
    }
    impl<B: UsbBulkBackend + Send + 'static> Transport for UsbTransport<B> {
        fn payload_limit(&self) -> usize {
            self.payload_limit
        }
        fn command_limit(&self) -> usize {
            self.command_limit
        }
        fn subscribe_notifications(
            &mut self,
        ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
            Box::pin(async { Ok(NotificationSupport::Unavailable) })
        }
        fn write<'a>(
            &'a mut self,
            bytes: &'a [u8],
            _: WriteKind,
        ) -> TransportFuture<'a, Result<(), TransportError>> {
            Box::pin(async move { self.device.write(bytes).await })
        }
        fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
            Box::pin(tokio::time::sleep(duration))
        }
        fn wait_response(
            &mut self,
            timeout: Duration,
        ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
            Box::pin(async move { self.device.read(timeout).await })
        }
        fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
            Box::pin(async move { self.device.disconnect().await })
        }
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UsbIdentity {
        pub vendor_id: u16,
        pub product_id: u16,
        pub bus: u8,
        pub address: u8,
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct UsbBulkCandidate {
        pub identity: UsbIdentity,
        pub interface: u8,
        pub alternate_setting: u8,
        pub out_endpoint: u8,
        pub in_endpoint: Option<u8>,
        pub max_packet_size: u16,
        pub interface_class: u8,
        pub manufacturer: Option<String>,
        pub product: Option<String>,
        pub serial_number: Option<String>,
    }
    /// Select one deterministic bulk endpoint for a previously resolved device.
    /// Printer-class interfaces win, followed by interface/alternate/endpoint order.
    pub fn select_bulk_candidate(
        candidates: &[UsbBulkCandidate],
        identity: UsbIdentity,
    ) -> Option<&UsbBulkCandidate> {
        candidates
            .iter()
            .filter(|candidate| candidate.identity == identity)
            .min_by_key(|candidate| {
                (
                    candidate.interface_class != 7,
                    candidate.interface,
                    candidate.alternate_setting,
                    candidate.out_endpoint,
                )
            })
    }
    pub fn discover_rusb() -> Result<Vec<UsbIdentity>, String> {
        let context = rusb::Context::new()
            .map_err(|error| format!("USB context initialization failed: {error}"))?;
        let devices = context.devices().map_err(|error| error.to_string())?;
        let mut found = Vec::new();
        for device in devices.iter() {
            let descriptor = device
                .device_descriptor()
                .map_err(|error| error.to_string())?;
            found.push(UsbIdentity {
                vendor_id: descriptor.vendor_id(),
                product_id: descriptor.product_id(),
                bus: device.bus_number(),
                address: device.address(),
            });
        }
        found.sort_by_key(|item| (item.bus, item.address));
        Ok(found)
    }
    /// Discover bulk-write interfaces without relying on rusb's panicking global context.
    /// Printer-class interfaces are ordered before vendor-specific alternatives.
    pub fn discover_rusb_bulk() -> Result<Vec<UsbBulkCandidate>, String> {
        let context = rusb::Context::new()
            .map_err(|error| format!("USB context initialization failed: {error}"))?;
        let devices = context.devices().map_err(|error| error.to_string())?;
        let mut found = Vec::new();
        for device in devices.iter() {
            let descriptor = device
                .device_descriptor()
                .map_err(|error| error.to_string())?;
            let identity = UsbIdentity {
                vendor_id: descriptor.vendor_id(),
                product_id: descriptor.product_id(),
                bus: device.bus_number(),
                address: device.address(),
            };
            let strings = device.open().ok().map(|handle| {
                (
                    handle.read_manufacturer_string_ascii(&descriptor).ok(),
                    handle.read_product_string_ascii(&descriptor).ok(),
                    handle.read_serial_number_string_ascii(&descriptor).ok(),
                )
            });
            for index in 0..descriptor.num_configurations() {
                let configuration = device
                    .config_descriptor(index)
                    .map_err(|error| error.to_string())?;
                for interface in configuration.interfaces() {
                    for interface_descriptor in interface.descriptors() {
                        let mut out = None;
                        let mut input = None;
                        for endpoint in interface_descriptor.endpoint_descriptors() {
                            if endpoint.transfer_type() != rusb::TransferType::Bulk {
                                continue;
                            }
                            match endpoint.direction() {
                                rusb::Direction::Out if out.is_none() => {
                                    out = Some((endpoint.address(), endpoint.max_packet_size()))
                                }
                                rusb::Direction::In if input.is_none() => {
                                    input = Some(endpoint.address())
                                }
                                _ => {}
                            }
                        }
                        if let Some((out_endpoint, max_packet_size)) = out {
                            found.push(UsbBulkCandidate {
                                identity,
                                interface: interface_descriptor.interface_number(),
                                alternate_setting: interface_descriptor.setting_number(),
                                out_endpoint,
                                in_endpoint: input,
                                max_packet_size,
                                interface_class: interface_descriptor.class_code(),
                                manufacturer: strings.as_ref().and_then(|value| value.0.clone()),
                                product: strings.as_ref().and_then(|value| value.1.clone()),
                                serial_number: strings.as_ref().and_then(|value| value.2.clone()),
                            });
                        }
                    }
                }
            }
        }
        found.sort_by_key(|candidate| {
            (
                candidate.interface_class != 7,
                candidate.identity.bus,
                candidate.identity.address,
                candidate.interface,
                candidate.alternate_setting,
            )
        });
        Ok(found)
    }
    pub struct RusbBulkBackend {
        handle: rusb::DeviceHandle<rusb::Context>,
        interface: u8,
        alternate_setting: u8,
        out_endpoint: u8,
        in_endpoint: Option<u8>,
        timeout: Duration,
    }
    impl RusbBulkBackend {
        pub fn open(
            identity: UsbIdentity,
            interface: u8,
            out_endpoint: u8,
            in_endpoint: Option<u8>,
            timeout_ms: u64,
        ) -> Result<Self, String> {
            Self::open_with_setting(
                identity,
                interface,
                0,
                out_endpoint,
                in_endpoint,
                timeout_ms,
            )
        }
        pub fn open_with_setting(
            identity: UsbIdentity,
            interface: u8,
            alternate_setting: u8,
            out_endpoint: u8,
            in_endpoint: Option<u8>,
            timeout_ms: u64,
        ) -> Result<Self, String> {
            Self::open_with_setting_and_serial(
                identity,
                interface,
                alternate_setting,
                out_endpoint,
                in_endpoint,
                timeout_ms,
                None,
            )
        }

        pub fn open_with_setting_and_serial(
            identity: UsbIdentity,
            interface: u8,
            alternate_setting: u8,
            out_endpoint: u8,
            in_endpoint: Option<u8>,
            timeout_ms: u64,
            expected_serial: Option<&str>,
        ) -> Result<Self, String> {
            let context = rusb::Context::new()
                .map_err(|error| format!("USB context initialization failed: {error}"))?;
            let devices = context.devices().map_err(|error| error.to_string())?;
            let device = devices
                .iter()
                .find(|device| {
                    device.bus_number() == identity.bus && device.address() == identity.address
                })
                .ok_or_else(|| "USB device disappeared".to_owned())?;
            let descriptor = device
                .device_descriptor()
                .map_err(|error| error.to_string())?;
            if descriptor.vendor_id() != identity.vendor_id
                || descriptor.product_id() != identity.product_id
            {
                return Err("USB identity changed before open".into());
            }
            let handle = device.open().map_err(|error| error.to_string())?;
            if let Some(expected_serial) = expected_serial {
                if expected_serial.is_empty() {
                    return Err("expected USB serial must not be empty".into());
                }
                let actual_serial = handle
                    .read_serial_number_string_ascii(&descriptor)
                    .map_err(|error| format!("USB serial revalidation failed: {error}"))?;
                verify_expected_serial(expected_serial, &actual_serial)?;
            }
            let _ = handle.set_auto_detach_kernel_driver(true);
            handle
                .claim_interface(interface)
                .map_err(|error| error.to_string())?;
            if alternate_setting != 0 {
                handle
                    .set_alternate_setting(interface, alternate_setting)
                    .map_err(|error| error.to_string())?;
            }
            Ok(Self {
                handle,
                interface,
                alternate_setting,
                out_endpoint,
                in_endpoint,
                timeout: Duration::from_millis(timeout_ms),
            })
        }
    }
    impl UsbBulkBackend for RusbBulkBackend {
        fn write_bulk(&mut self, bytes: &[u8]) -> Result<(), String> {
            let written = self
                .handle
                .write_bulk(self.out_endpoint, bytes, self.timeout)
                .map_err(|error| error.to_string())?;
            if written == bytes.len() {
                Ok(())
            } else {
                Err(format!("short USB bulk write: {written}/{}", bytes.len()))
            }
        }
        fn read_bulk(
            &mut self,
            timeout_ms: u64,
            maximum: usize,
        ) -> Result<Option<Vec<u8>>, String> {
            let Some(endpoint) = self.in_endpoint else {
                return Ok(None);
            };
            let mut bytes = vec![0; maximum.max(1)];
            match self
                .handle
                .read_bulk(endpoint, &mut bytes, Duration::from_millis(timeout_ms))
            {
                Ok(length) => {
                    bytes.truncate(length);
                    Ok(Some(bytes))
                }
                Err(rusb::Error::Timeout) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }
    }
    impl UsbPrinterClassBackend for RusbBulkBackend {
        fn get_device_id_raw(
            &mut self,
            timeout_ms: u64,
            maximum: usize,
        ) -> Result<Option<Vec<u8>>, String> {
            let mut bytes = vec![0; maximum.clamp(2, MAX_IEEE1284_DEVICE_ID_BYTES)];
            let index = (u16::from(self.interface) << 8) | u16::from(self.alternate_setting);
            match self.handle.read_control(
                0xa1,
                0,
                0,
                index,
                &mut bytes,
                Duration::from_millis(timeout_ms),
            ) {
                Ok(0) => Ok(None),
                Ok(length) => {
                    bytes.truncate(length);
                    Ok(Some(bytes))
                }
                Err(rusb::Error::Timeout) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }

        fn get_port_status_raw(&mut self, timeout_ms: u64) -> Result<Option<u8>, String> {
            let mut value = [0];
            match self.handle.read_control(
                0xa1,
                1,
                0,
                u16::from(self.interface),
                &mut value,
                Duration::from_millis(timeout_ms),
            ) {
                Ok(0) => Ok(None),
                Ok(_) => Ok(Some(value[0])),
                Err(rusb::Error::Timeout) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }
    }

    pub fn verify_expected_serial(expected: &str, actual: &str) -> Result<(), String> {
        if expected == actual {
            Ok(())
        } else {
            Err("USB serial changed before open".into())
        }
    }
    pub type RusbTransport = UsbTransport<RusbBulkBackend>;
    pub fn open_rusb(
        identity: UsbIdentity,
        interface: u8,
        out_endpoint: u8,
        in_endpoint: Option<u8>,
        payload_limit: usize,
        response_limit: usize,
        timeout_ms: u64,
    ) -> Result<RusbTransport, String> {
        Ok(UsbTransport::new(
            RusbBulkBackend::open(identity, interface, out_endpoint, in_endpoint, timeout_ms)?,
            payload_limit,
            response_limit,
        ))
    }
    pub fn open_rusb_with_limits(
        candidate: &UsbBulkCandidate,
        raster_limit: usize,
        command_limit: usize,
        response_limit: usize,
        timeout_ms: u64,
    ) -> Result<RusbTransport, String> {
        Ok(UsbTransport::new_with_limits(
            RusbBulkBackend::open_with_setting(
                candidate.identity,
                candidate.interface,
                candidate.alternate_setting,
                candidate.out_endpoint,
                candidate.in_endpoint,
                timeout_ms,
            )?,
            raster_limit
                .min(usize::from(candidate.max_packet_size))
                .max(1),
            command_limit,
            response_limit,
        ))
    }
    /// Open a previously selected device for a mutable operation and re-read
    /// its serial number before claiming the interface.
    pub fn open_rusb_with_limits_verified(
        candidate: &UsbBulkCandidate,
        expected_serial: &str,
        raster_limit: usize,
        command_limit: usize,
        response_limit: usize,
        timeout_ms: u64,
    ) -> Result<RusbTransport, String> {
        if expected_serial.is_empty() {
            return Err("expected USB serial must not be empty".into());
        }
        if candidate.serial_number.as_deref() != Some(expected_serial) {
            return Err("selected USB candidate does not match expected serial".into());
        }
        Ok(UsbTransport::new_with_limits(
            RusbBulkBackend::open_with_setting_and_serial(
                candidate.identity,
                candidate.interface,
                candidate.alternate_setting,
                candidate.out_endpoint,
                candidate.in_endpoint,
                timeout_ms,
                Some(expected_serial),
            )?,
            raster_limit
                .min(usize::from(candidate.max_packet_size))
                .max(1),
            command_limit,
            response_limit,
        ))
    }
    pub fn open_rusb_auto(
        identity: UsbIdentity,
        command_limit: usize,
        response_limit: usize,
        timeout_ms: u64,
    ) -> Result<RusbTransport, String> {
        let candidates = discover_rusb_bulk()?;
        let candidate = select_bulk_candidate(&candidates, identity)
            .ok_or_else(|| "USB device has no bulk OUT interface".to_owned())?;
        open_rusb_with_limits(
            candidate,
            usize::from(candidate.max_packet_size),
            command_limit,
            response_limit,
            timeout_ms,
        )
    }
}

#[cfg(feature = "dns-sd")]
pub mod dns_sd;

#[cfg(feature = "ipp")]
pub mod ipp;

#[cfg(feature = "ble")]
pub mod ble {
    use super::*;
    use btleplug::api::{
        Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter,
        WriteType,
    };
    use futures_util::StreamExt as _;
    use mb_printer_core::capabilities::{
        BleFlowControl, BleGattCapabilities, BleWriteType, NotificationRequirement,
    };
    use std::{collections::VecDeque, num::NonZeroUsize};
    use tracing::Instrument as _;

    const NOTIFICATION_QUEUE_CAPACITY: usize = 32;

    #[derive(Debug, Default)]
    struct CreditFlowState {
        credits: usize,
        maximum_payload: Option<usize>,
        pending_responses: VecDeque<Vec<u8>>,
    }

    impl CreditFlowState {
        fn observe(&mut self, bytes: &[u8]) -> bool {
            match bytes {
                [0x01, credits] => {
                    self.credits = self.credits.saturating_add(usize::from(*credits));
                    true
                }
                [0x02, low, high] => {
                    let limit = usize::from(u16::from_le_bytes([*low, *high]));
                    if limit != 0 {
                        self.maximum_payload = Some(limit);
                    }
                    true
                }
                _ => false,
            }
        }
    }

    async fn receive_for_credit(
        flow: &mut CreditFlowState,
        notifications: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    ) -> Result<(), TransportError> {
        while flow.credits == 0 {
            let bytes = notifications.recv().await.ok_or_else(|| {
                ble_error(
                    TransportErrorKind::Disconnected,
                    "BLE notification stream ended",
                )
            })?;
            if !flow.observe(&bytes) {
                if flow.pending_responses.len() == NOTIFICATION_QUEUE_CAPACITY {
                    return Err(ble_error(
                        TransportErrorKind::Io,
                        "BLE response queue is full while waiting for write credit",
                    ));
                }
                flow.pending_responses.push_back(bytes);
            }
        }
        flow.credits -= 1;
        Ok(())
    }

    async fn receive_flow_response(
        flow: &mut CreditFlowState,
        notifications: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
        timeout: Duration,
    ) -> Result<WaitOutcome, TransportError> {
        if let Some(bytes) = flow.pending_responses.pop_front() {
            return Ok(WaitOutcome::Response(bytes));
        }
        let response = async {
            loop {
                let bytes = notifications.recv().await.ok_or_else(|| {
                    ble_error(
                        TransportErrorKind::Disconnected,
                        "BLE notification stream ended",
                    )
                })?;
                if !flow.observe(&bytes) {
                    return Ok(WaitOutcome::Response(bytes));
                }
            }
        };
        match tokio::time::timeout(timeout, response).await {
            Ok(outcome) => outcome,
            Err(_) => Ok(WaitOutcome::Timeout),
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct BtleplugConnectOptions {
        pub scan_timeout: Duration,
        pub payload_limit: NonZeroUsize,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum NotificationState {
        Unsupported,
        Available,
        Subscribed,
        Disconnected,
    }

    fn ble_error(kind: TransportErrorKind, message: &'static str) -> TransportError {
        TransportError::new(kind, message)
    }

    fn validate_write_state(
        state: NotificationState,
        length: usize,
        limit: NonZeroUsize,
    ) -> Result<(), TransportError> {
        if state == NotificationState::Disconnected {
            return Err(ble_error(
                TransportErrorKind::Disconnected,
                "BLE transport is disconnected",
            ));
        }
        if length > limit.get() {
            return Err(ble_error(
                TransportErrorKind::InvalidConfiguration,
                "BLE write exceeds the declared payload limit",
            ));
        }
        Ok(())
    }

    async fn wait_notification_state(
        state: &mut NotificationState,
        notifications: &mut Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
        timeout: Duration,
    ) -> Result<WaitOutcome, TransportError> {
        match *state {
            NotificationState::Unsupported | NotificationState::Available => {
                return Ok(WaitOutcome::Unavailable);
            }
            NotificationState::Disconnected => {
                return Err(ble_error(
                    TransportErrorKind::Disconnected,
                    "BLE transport is disconnected",
                ));
            }
            NotificationState::Subscribed => {}
        }
        let receiver = notifications
            .as_mut()
            .expect("subscribed notification state has a receiver");
        match tokio::time::timeout(timeout, receiver.recv()).await {
            Err(_) => Ok(WaitOutcome::Timeout),
            Ok(Some(bytes)) => Ok(WaitOutcome::Response(bytes)),
            Ok(None) => {
                *state = NotificationState::Disconnected;
                Err(ble_error(
                    TransportErrorKind::Disconnected,
                    "BLE notification stream ended",
                ))
            }
        }
    }

    fn select_characteristics(
        characteristics: &[Characteristic],
        capabilities: &BleGattCapabilities,
    ) -> Result<(Characteristic, Option<Characteristic>), TransportError> {
        if capabilities.write_type != BleWriteType::WithoutResponse {
            return Err(ble_error(
                TransportErrorKind::InvalidConfiguration,
                "BLE profile has an unsupported write type",
            ));
        }

        let write = characteristics
            .iter()
            .find(|item| {
                item.uuid == capabilities.write_characteristic
                    && item
                        .properties
                        .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
            })
            .cloned();
        let write = match write {
            Some(write) => write,
            None if characteristics
                .iter()
                .any(|item| item.uuid == capabilities.write_characteristic) =>
            {
                return Err(ble_error(
                    TransportErrorKind::InvalidConfiguration,
                    "BLE write characteristic does not support write without response",
                ));
            }
            None => {
                return Err(ble_error(
                    TransportErrorKind::Connection,
                    "BLE write characteristic was not found",
                ));
            }
        };

        let notification = match &capabilities.notification {
            None => None,
            Some(profile) => match characteristics
                .iter()
                .find(|item| item.uuid == profile.characteristic)
                .cloned()
            {
                Some(characteristic)
                    if characteristic.properties.contains(CharPropFlags::NOTIFY) =>
                {
                    Some(characteristic)
                }
                Some(_) => {
                    return Err(ble_error(
                        TransportErrorKind::InvalidConfiguration,
                        "BLE notification characteristic does not support notifications",
                    ));
                }
                None if profile.requirement == NotificationRequirement::Optional => None,
                None => {
                    return Err(ble_error(
                        TransportErrorKind::Connection,
                        "required BLE notification characteristic was not found",
                    ));
                }
            },
        };

        if capabilities.flow_control.is_some() && notification.is_none() {
            return Err(ble_error(
                TransportErrorKind::Connection,
                "BLE flow control requires a notification characteristic",
            ));
        }

        Ok((write, notification))
    }

    /// Async BLE discovery for applications that own the Tokio runtime.
    pub async fn discover_btleplug_async(
        scan_timeout: Duration,
    ) -> Result<Vec<DiscoveredPrinter>, TransportError> {
        let manager = btleplug::platform::Manager::new().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not initialize the BLE manager",
            )
        })?;
        let adapters = manager.adapters().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not enumerate BLE adapters",
            )
        })?;
        let mut found = Vec::new();
        for adapter in adapters {
            adapter
                .start_scan(ScanFilter::default())
                .await
                .map_err(|_| {
                    ble_error(TransportErrorKind::Connection, "BLE scan could not start")
                })?;
            tokio::time::sleep(scan_timeout).await;
            let peripherals = adapter.peripherals().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not enumerate BLE peripherals",
                )
            })?;
            for peripheral in peripherals {
                let properties = peripheral.properties().await.map_err(|_| {
                    ble_error(
                        TransportErrorKind::Connection,
                        "could not read BLE peripheral properties",
                    )
                })?;
                let address = peripheral.address().to_string();
                found.push(DiscoveredPrinter {
                    transport: "ble",
                    id: address.clone(),
                    name: properties.and_then(|value| value.local_name),
                    endpoint: address,
                });
            }
        }
        found.sort_by(|left, right| left.endpoint.cmp(&right.endpoint));
        found.dedup_by(|left, right| left.endpoint == right.endpoint);
        Ok(found)
    }

    /// A Tokio-native GATT transport configured exclusively from model capabilities.
    pub struct BtleplugTransport {
        peripheral: btleplug::platform::Peripheral,
        write: Characteristic,
        notify: Option<Characteristic>,
        notification_state: NotificationState,
        notifications: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
        forwarding_task: Option<tokio::task::JoinHandle<()>>,
        payload_limit: NonZeroUsize,
        credit_flow: Option<CreditFlowState>,
    }

    impl BtleplugTransport {
        pub async fn connect(
            address: &str,
            capabilities: &BleGattCapabilities,
            options: BtleplugConnectOptions,
        ) -> Result<Self, TransportError> {
            let manager = btleplug::platform::Manager::new().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not initialize the BLE manager",
                )
            })?;
            let adapters = manager.adapters().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not enumerate BLE adapters",
                )
            })?;
            let mut selected = None;
            for adapter in adapters {
                adapter
                    .start_scan(ScanFilter::default())
                    .await
                    .map_err(|_| {
                        ble_error(TransportErrorKind::Connection, "BLE scan could not start")
                    })?;
                tokio::time::sleep(options.scan_timeout).await;
                let peripherals = adapter.peripherals().await.map_err(|_| {
                    ble_error(
                        TransportErrorKind::Connection,
                        "could not enumerate BLE peripherals",
                    )
                })?;
                for peripheral in peripherals {
                    if peripheral
                        .address()
                        .to_string()
                        .eq_ignore_ascii_case(address)
                    {
                        selected = Some(peripheral);
                        break;
                    }
                }
                if selected.is_some() {
                    break;
                }
            }

            let peripheral = selected.ok_or_else(|| {
                ble_error(
                    TransportErrorKind::Connection,
                    "requested BLE peripheral was not found",
                )
            })?;
            let connected = peripheral.is_connected().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not inspect BLE connection state",
                )
            })?;
            if !connected {
                peripheral.connect().await.map_err(|_| {
                    ble_error(
                        TransportErrorKind::Connection,
                        "could not connect to BLE peripheral",
                    )
                })?;
            }
            peripheral.discover_services().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not discover BLE services",
                )
            })?;

            let characteristics: Vec<_> = peripheral.characteristics().into_iter().collect();
            let (write, notify) = match select_characteristics(&characteristics, capabilities) {
                Ok(selected) => selected,
                Err(error) => {
                    let _ = peripheral.disconnect().await;
                    return Err(error);
                }
            };
            let notification_state = if notify.is_some() {
                NotificationState::Available
            } else {
                NotificationState::Unsupported
            };

            Ok(Self {
                peripheral,
                write,
                notify,
                notification_state,
                notifications: None,
                forwarding_task: None,
                payload_limit: options.payload_limit,
                credit_flow: (capabilities.flow_control == Some(BleFlowControl::PhomemoCredit))
                    .then(CreditFlowState::default),
            })
        }

        async fn subscribe_inner(&mut self) -> Result<NotificationSupport, TransportError> {
            match self.notification_state {
                NotificationState::Unsupported => return Ok(NotificationSupport::Unavailable),
                NotificationState::Subscribed => return Ok(NotificationSupport::Subscribed),
                NotificationState::Disconnected => {
                    return Err(ble_error(
                        TransportErrorKind::Disconnected,
                        "BLE transport is disconnected",
                    ));
                }
                NotificationState::Available => {}
            }

            let characteristic = self
                .notify
                .as_ref()
                .expect("available notification state has a characteristic")
                .clone();
            let mut stream = self.peripheral.notifications().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not open BLE notification stream",
                )
            })?;
            self.peripheral
                .subscribe(&characteristic)
                .await
                .map_err(|_| {
                    ble_error(
                        TransportErrorKind::Connection,
                        "could not subscribe to BLE notifications",
                    )
                })?;

            let expected_uuid = characteristic.uuid;
            let (sender, receiver) = tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
            let task = tokio::spawn(
                async move {
                    while let Some(notification) = stream.next().await {
                        if notification.uuid == expected_uuid
                            && sender.send(notification.value).await.is_err()
                        {
                            break;
                        }
                    }
                }
                .instrument(tracing::Span::current()),
            );
            self.notifications = Some(receiver);
            self.forwarding_task = Some(task);
            self.notification_state = NotificationState::Subscribed;
            Ok(NotificationSupport::Subscribed)
        }

        async fn write_inner(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
            validate_write_state(self.notification_state, bytes.len(), self.payload_limit)?;
            if let Some(flow) = &mut self.credit_flow {
                if self.notification_state != NotificationState::Subscribed {
                    return Err(ble_error(
                        TransportErrorKind::InvalidConfiguration,
                        "BLE credit flow requires notification subscription before writing",
                    ));
                }
                let notifications = self
                    .notifications
                    .as_mut()
                    .expect("subscribed credit-controlled notification state has a receiver");
                if let Err(error) = receive_for_credit(flow, notifications).await {
                    self.notification_state = NotificationState::Disconnected;
                    return Err(error);
                }
                if flow
                    .maximum_payload
                    .is_some_and(|maximum| bytes.len() > maximum)
                {
                    return Err(ble_error(
                        TransportErrorKind::InvalidConfiguration,
                        "BLE write exceeds the printer-advertised flow limit",
                    ));
                }
            }
            self.peripheral
                .write(&self.write, bytes, WriteType::WithoutResponse)
                .await
                .map_err(|_| ble_error(TransportErrorKind::Io, "BLE write failed"))
        }

        async fn wait_inner(&mut self, timeout: Duration) -> Result<WaitOutcome, TransportError> {
            if let Some(flow) = &mut self.credit_flow {
                if self.notification_state != NotificationState::Subscribed {
                    return Ok(WaitOutcome::Unavailable);
                }
                let notifications = self
                    .notifications
                    .as_mut()
                    .expect("subscribed credit-controlled notification state has a receiver");
                let outcome = receive_flow_response(flow, notifications, timeout).await;
                if outcome
                    .as_ref()
                    .is_err_and(|error| error.kind == TransportErrorKind::Disconnected)
                {
                    self.notification_state = NotificationState::Disconnected;
                }
                return outcome;
            }
            wait_notification_state(
                &mut self.notification_state,
                &mut self.notifications,
                timeout,
            )
            .await
        }

        async fn disconnect_inner(&mut self) -> Result<(), TransportError> {
            if self.notification_state == NotificationState::Disconnected {
                return Ok(());
            }
            self.notification_state = NotificationState::Disconnected;
            if let Some(task) = self.forwarding_task.take() {
                task.abort();
            }
            self.notifications.take();
            self.peripheral.disconnect().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not disconnect BLE peripheral",
                )
            })
        }
    }

    impl Transport for BtleplugTransport {
        fn payload_limit(&self) -> usize {
            self.credit_flow
                .as_ref()
                .and_then(|flow| flow.maximum_payload)
                .map_or(self.payload_limit.get(), |maximum| {
                    maximum.min(self.payload_limit.get())
                })
        }

        fn subscribe_notifications(
            &mut self,
        ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
            Box::pin(self.subscribe_inner())
        }

        fn write<'a>(
            &'a mut self,
            bytes: &'a [u8],
            _kind: WriteKind,
        ) -> TransportFuture<'a, Result<(), TransportError>> {
            Box::pin(self.write_inner(bytes))
        }

        fn wait_response(
            &mut self,
            timeout: Duration,
        ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
            Box::pin(self.wait_inner(timeout))
        }

        fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
            Box::pin(tokio::time::sleep(duration))
        }

        fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
            Box::pin(self.disconnect_inner())
        }
    }

    impl Drop for BtleplugTransport {
        fn drop(&mut self) {
            self.notification_state = NotificationState::Disconnected;
            if let Some(task) = self.forwarding_task.take() {
                task.abort();
            }
            self.notifications.take();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use mb_printer_core::capabilities::{BleNotification, BleWriteType};
        use std::collections::BTreeSet;
        use uuid::Uuid;

        const FF02: &str = "0000ff02-0000-1000-8000-00805f9b34fb";
        const FF03: &str = "0000ff03-0000-1000-8000-00805f9b34fb";

        fn characteristic(uuid: &str, properties: CharPropFlags) -> Characteristic {
            Characteristic {
                uuid: Uuid::parse_str(uuid).unwrap(),
                service_uuid: Uuid::nil(),
                properties,
                descriptors: BTreeSet::new(),
            }
        }

        fn profile(requirement: NotificationRequirement) -> BleGattCapabilities {
            BleGattCapabilities {
                write_characteristic: Uuid::parse_str(FF02).unwrap(),
                write_type: BleWriteType::WithoutResponse,
                notification: Some(BleNotification {
                    characteristic: Uuid::parse_str(FF03).unwrap(),
                    requirement,
                }),
                flow_control: None,
            }
        }

        #[tokio::test]
        async fn credit_flow_separates_control_frames_and_gates_each_write() {
            let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
            let mut flow = CreditFlowState::default();
            sender.send(vec![0x02, 20, 0]).await.unwrap();
            sender.send(vec![0x1a, 0x08, 0xa2]).await.unwrap();
            sender.send(vec![0x01, 1]).await.unwrap();

            receive_for_credit(&mut flow, &mut receiver).await.unwrap();
            assert_eq!(flow.maximum_payload, Some(20));
            assert_eq!(flow.credits, 0);
            assert_eq!(
                receive_flow_response(&mut flow, &mut receiver, Duration::ZERO)
                    .await
                    .unwrap(),
                WaitOutcome::Response(vec![0x1a, 0x08, 0xa2])
            );

            sender.send(vec![0x01, 2]).await.unwrap();
            receive_for_credit(&mut flow, &mut receiver).await.unwrap();
            receive_for_credit(&mut flow, &mut receiver).await.unwrap();
            assert_eq!(flow.credits, 0);
        }

        #[test]
        fn characteristic_selection_requires_write_without_response() {
            let only_write = characteristic(FF02, CharPropFlags::WRITE);
            let error =
                select_characteristics(&[only_write], &profile(NotificationRequirement::Optional))
                    .unwrap_err();
            assert_eq!(error.kind, TransportErrorKind::InvalidConfiguration);

            let correct = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
            let (write, notify) =
                select_characteristics(&[correct], &profile(NotificationRequirement::Optional))
                    .unwrap();
            assert_eq!(write.uuid, Uuid::parse_str(FF02).unwrap());
            assert!(notify.is_none());
        }

        #[test]
        fn optional_missing_notification_is_unavailable_but_required_is_an_error() {
            let write = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
            let (_, notify) = select_characteristics(
                std::slice::from_ref(&write),
                &profile(NotificationRequirement::Optional),
            )
            .unwrap();
            assert!(notify.is_none());

            let error =
                select_characteristics(&[write], &profile(NotificationRequirement::Required))
                    .unwrap_err();
            assert_eq!(error.kind, TransportErrorKind::Connection);

            let mut credit_profile = profile(NotificationRequirement::Optional);
            credit_profile.flow_control = Some(BleFlowControl::PhomemoCredit);
            let write = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
            let error = select_characteristics(&[write], &credit_profile).unwrap_err();
            assert_eq!(error.kind, TransportErrorKind::Connection);
        }

        #[tokio::test]
        async fn notification_state_distinguishes_unavailable_timeout_response_and_disconnect() {
            let mut unsupported = NotificationState::Unsupported;
            assert_eq!(
                wait_notification_state(&mut unsupported, &mut None, Duration::ZERO)
                    .await
                    .unwrap(),
                WaitOutcome::Unavailable
            );

            let (sender, receiver) = tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
            let mut notifications = Some(receiver);
            let mut subscribed = NotificationState::Subscribed;
            sender.send(vec![9]).await.unwrap();
            assert_eq!(
                wait_notification_state(
                    &mut subscribed,
                    &mut notifications,
                    Duration::from_secs(1),
                )
                .await
                .unwrap(),
                WaitOutcome::Response(vec![9])
            );
            assert_eq!(
                wait_notification_state(&mut subscribed, &mut notifications, Duration::ZERO)
                    .await
                    .unwrap(),
                WaitOutcome::Timeout
            );
            drop(sender);
            assert!(
                wait_notification_state(
                    &mut subscribed,
                    &mut notifications,
                    Duration::from_secs(1),
                )
                .await
                .is_err()
            );
            assert_eq!(subscribed, NotificationState::Disconnected);
        }

        #[test]
        fn write_validation_rejects_oversize_and_disconnected_before_platform_io() {
            let limit = NonZeroUsize::new(20).unwrap();
            assert!(validate_write_state(NotificationState::Available, 20, limit).is_ok());
            assert_eq!(
                validate_write_state(NotificationState::Available, 21, limit)
                    .unwrap_err()
                    .kind,
                TransportErrorKind::InvalidConfiguration
            );
            assert_eq!(
                validate_write_state(NotificationState::Disconnected, 1, limit)
                    .unwrap_err()
                    .kind,
                TransportErrorKind::Disconnected
            );
        }

        #[test]
        fn configured_notification_must_have_notify_property() {
            let write = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
            let wrong_notify = characteristic(FF03, CharPropFlags::READ);
            let error = select_characteristics(
                &[write, wrong_notify],
                &profile(NotificationRequirement::Optional),
            )
            .unwrap_err();
            assert_eq!(error.kind, TransportErrorKind::InvalidConfiguration);
        }

        #[test]
        fn exact_ff02_ff03_profile_selects_both_characteristics() {
            let unrelated =
                characteristic("00002a00-0000-1000-8000-00805f9b34fb", CharPropFlags::WRITE);
            let write = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
            let notify = characteristic(FF03, CharPropFlags::NOTIFY);
            let (selected_write, selected_notify) = select_characteristics(
                &[unrelated, write.clone(), notify.clone()],
                &profile(NotificationRequirement::Optional),
            )
            .unwrap();
            assert_eq!(selected_write, write);
            assert_eq!(selected_notify, Some(notify));
        }
    }
}
#[cfg(feature = "wifi")]
pub mod wifi {
    use crate::{Transport, TransportFuture, WriteKind};
    use mb_printer_core::ipp::{self as ipp_codec, Limits as IppLimits, ValueData as IppValueData};
    use mb_printer_core::protocol::brother::wifi as core_wifi;
    pub use mb_printer_core::protocol::brother::wifi::{
        AccessPoint, PJL_FOOTER, PJL_HEADER, REBOOT_COMMAND, WirelessAuthentication,
        WirelessEncryption, WirelessField, WirelessSettings, encode_ssid, ip_address_command,
        parse_access_points, parse_authentication, parse_boolean_field, parse_encryption,
        parse_ip_address, parse_oid_value, parse_wifi_status, wifi_scan_result_command,
        wifi_scan_start_command, wifi_status_command, wireless_scan_plan, wireless_status_plan,
        xor_password,
    };
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    use thiserror::Error;
    pub struct WifiCredentials {
        pub ssid: String,
        pub password: String,
    }
    impl std::fmt::Debug for WifiCredentials {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("WifiCredentials")
                .field("ssid", &self.ssid)
                .field("password", &"[REDACTED]")
                .finish()
        }
    }
    pub trait WifiProvisioner {
        fn provision<'a>(
            &'a mut self,
            credentials: &'a WifiCredentials,
        ) -> TransportFuture<'a, Result<(), String>>;
    }
    pub use super::ipp::{IppEndpoint, IppScheme};
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct IppPrinterStatus {
        pub printer_state: Option<u32>,
        pub reasons: Vec<String>,
        pub media_ready: Vec<String>,
    }
    #[derive(Debug, Error, Clone, PartialEq, Eq)]
    pub enum IppProbeError {
        #[error("invalid IPP endpoint")]
        InvalidEndpoint,
        #[error("IPPS endpoint requires a secure transport that is not available")]
        SecureTransportUnavailable,
        #[error("IPP transport failed: {0}")]
        Transport(String),
        #[error("invalid IPP response: {0}")]
        InvalidResponse(String),
    }
    pub trait IppStatusBackend {
        fn query_ipp_status(
            &self,
            endpoint: &IppEndpoint,
            timeout_ms: u64,
        ) -> Result<IppPrinterStatus, IppProbeError>;
    }
    #[derive(Debug, Default, Clone, Copy)]
    pub struct TcpIppBackend;
    impl IppStatusBackend for TcpIppBackend {
        fn query_ipp_status(
            &self,
            endpoint: &IppEndpoint,
            timeout_ms: u64,
        ) -> Result<IppPrinterStatus, IppProbeError> {
            query_ipp_status(endpoint, timeout_ms)
        }
    }
    pub fn discover_ipp(
        endpoints: &[IppEndpoint],
        timeout_ms: u64,
    ) -> Vec<(IppEndpoint, IppPrinterStatus)> {
        endpoints
            .iter()
            .filter_map(|endpoint| {
                query_ipp_status(endpoint, timeout_ms)
                    .ok()
                    .map(|status| (endpoint.clone(), status))
            })
            .collect()
    }
    pub fn probe_ipp_endpoints(
        endpoints: &[IppEndpoint],
        timeout_ms: u64,
    ) -> Vec<(IppEndpoint, Result<IppPrinterStatus, IppProbeError>)> {
        endpoints
            .iter()
            .map(|endpoint| (endpoint.clone(), query_ipp_status(endpoint, timeout_ms)))
            .collect()
    }
    pub fn query_ipp_status(
        endpoint: &IppEndpoint,
        timeout_ms: u64,
    ) -> Result<IppPrinterStatus, IppProbeError> {
        if endpoint.host.is_empty() || !endpoint.resource.starts_with('/') || timeout_ms == 0 {
            return Err(IppProbeError::InvalidEndpoint);
        }
        if endpoint.scheme == IppScheme::Ipps {
            return Err(IppProbeError::SecureTransportUnavailable);
        }
        let address = (endpoint.host.as_str(), endpoint.port)
            .to_socket_addrs()
            .map_err(|error| IppProbeError::Transport(error.to_string()))?
            .next()
            .ok_or_else(|| IppProbeError::Transport("IPP endpoint did not resolve".into()))?;
        let timeout = Duration::from_millis(timeout_ms);
        let mut stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|error| IppProbeError::Transport(error.to_string()))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| IppProbeError::Transport(error.to_string()))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| IppProbeError::Transport(error.to_string()))?;
        let body = ipp_status_request(endpoint)?;
        let header = format!(
            "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            endpoint.resource,
            endpoint.host,
            endpoint.port,
            body.len()
        );
        stream
            .write_all(header.as_bytes())
            .map_err(|error| IppProbeError::Transport(error.to_string()))?;
        stream
            .write_all(&body)
            .map_err(|error| IppProbeError::Transport(error.to_string()))?;
        let mut response = Vec::new();
        stream
            .take(4 * 1024 * 1024)
            .read_to_end(&mut response)
            .map_err(|error| IppProbeError::Transport(error.to_string()))?;
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| IppProbeError::InvalidResponse("missing HTTP headers".into()))?;
        let headers = std::str::from_utf8(&response[..split])
            .map_err(|_| IppProbeError::InvalidResponse("HTTP headers are not UTF-8".into()))?;
        if !headers
            .lines()
            .next()
            .is_some_and(|line| line.contains(" 200 "))
        {
            return Err(IppProbeError::InvalidResponse("HTTP request failed".into()));
        }
        parse_ipp_status(&response[split + 4..]).map_err(IppProbeError::InvalidResponse)
    }
    fn ipp_status_request(endpoint: &IppEndpoint) -> Result<Vec<u8>, IppProbeError> {
        let uri = format!(
            "ipp://{}:{}{}",
            endpoint.host, endpoint.port, endpoint.resource
        );
        ipp_codec::get_printer_attributes_request(
            &uri,
            ["printer-state", "printer-state-reasons", "media-ready"],
            None,
            1,
        )
        .encode(IppLimits::default())
        .map_err(|error| IppProbeError::InvalidResponse(error.to_string()))
    }
    pub fn parse_ipp_status(body: &[u8]) -> Result<IppPrinterStatus, String> {
        let response =
            ipp_codec::decode(body, IppLimits::default()).map_err(|error| error.to_string())?;
        if response.code >= 0x0100 {
            return Err("IPP operation failed".into());
        }
        let mut status = IppPrinterStatus {
            printer_state: None,
            reasons: Vec::new(),
            media_ready: Vec::new(),
        };
        for group in response.groups {
            for attribute in group.attributes {
                for value in attribute.values {
                    match (attribute.name.as_slice(), value.data) {
                        (b"printer-state", IppValueData::Enum(state)) if state >= 0 => {
                            status.printer_state = u32::try_from(state).ok();
                        }
                        (b"printer-state-reasons", IppValueData::Bytes(value)) => status
                            .reasons
                            .push(String::from_utf8_lossy(&value).into_owned()),
                        (b"media-ready", IppValueData::Bytes(value)) => status
                            .media_ready
                            .push(String::from_utf8_lossy(&value).into_owned()),
                        _ => {}
                    }
                }
            }
        }
        Ok(status)
    }
    pub fn inquire_command(oid: &str) -> Result<Vec<u8>, String> {
        core_wifi::inquire_command(oid).map_err(|error| error.to_string())
    }
    pub struct BrotherWifiProvisioner<T> {
        pub transport: T,
        pub reboot: bool,
    }
    impl<T: Transport> WifiProvisioner for BrotherWifiProvisioner<T> {
        fn provision<'a>(
            &'a mut self,
            credentials: &'a WifiCredentials,
        ) -> TransportFuture<'a, Result<(), String>> {
            Box::pin(async move {
                let command = WirelessSettings {
                    ssid: credentials.ssid.clone(),
                    password: credentials.password.clone(),
                    encryption: WirelessEncryption::TkipAes,
                    authentication: WirelessAuthentication::WpaPsk,
                    infrastructure: true,
                    wireless_direct: false,
                    reboot: self.reboot,
                }
                .command()
                .map_err(|error| error.to_string())?;
                self.transport
                    .write(&command, WriteKind::Command)
                    .await
                    .map_err(|error| error.to_string())
            })
        }
    }
}

#[cfg(feature = "native-input")]
pub mod input {
    use std::path::{Path, PathBuf};
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NativeInput {
        pub bytes: Vec<u8>,
        pub media_type: String,
        pub source: PathBuf,
    }
    pub trait NativeInputBackend {
        fn read(&self, path: &Path) -> Result<NativeInput, String>;
    }
    pub struct FileInputBackend {
        pub maximum_bytes: u64,
    }
    impl NativeInputBackend for FileInputBackend {
        fn read(&self, path: &Path) -> Result<NativeInput, String> {
            let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
            if metadata.len() > self.maximum_bytes {
                return Err("native input exceeds configured limit".into());
            }
            let media_type = match path
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("pdf") => "application/pdf",
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("svg") => "image/svg+xml",
                _ => return Err("unsupported native input type".into()),
            };
            Ok(NativeInput {
                bytes: std::fs::read(path).map_err(|error| error.to_string())?,
                media_type: media_type.into(),
                source: path.to_owned(),
            })
        }
    }
}

#[cfg(all(feature = "bluetooth-rfcomm", target_os = "linux"))]
pub mod rfcomm {
    use super::*;
    use std::{
        io::Read,
        os::fd::{FromRawFd, OwnedFd},
        process::Command,
    };
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PairedDevice {
        pub address: String,
        pub name: String,
    }
    pub fn discover_paired() -> Result<Vec<PairedDevice>, String> {
        let output = Command::new("bluetoothctl")
            .args(["devices", "Paired"])
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        String::from_utf8(output.stdout)
            .map_err(|error| error.to_string())
            .map(|text| {
                text.lines()
                    .filter_map(|line| {
                        let rest = line.strip_prefix("Device ")?;
                        let (address, name) = rest.split_once(' ')?;
                        Some(PairedDevice {
                            address: address.into(),
                            name: name.into(),
                        })
                    })
                    .collect()
            })
    }
    pub struct RfcommTransport {
        device: PersistentDevice,
        payload_limit: usize,
        pub address: String,
        pub channel: u8,
    }
    struct RfcommBackend {
        file: File,
        response_limit: usize,
    }
    impl BlockingIo for RfcommBackend {
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            std::io::Write::write_all(&mut self.file, bytes).map_err(|error| error.to_string())
        }
        fn read(&mut self, timeout: Duration) -> Result<WaitOutcome, String> {
            let mut descriptor = libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&self.file),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
            // SAFETY: descriptor points to one initialized pollfd for the call.
            let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
            if ready < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if ready == 0 {
                return Ok(WaitOutcome::Timeout);
            }
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err("RFCOMM socket is not valid".into());
            }
            if descriptor.revents & (libc::POLLERR | libc::POLLHUP) != 0
                && descriptor.revents & libc::POLLIN == 0
            {
                return Ok(WaitOutcome::Unavailable);
            }
            let mut bytes = vec![0; self.response_limit];
            match self.file.read(&mut bytes) {
                Ok(0) => Ok(WaitOutcome::Unavailable),
                Ok(length) => {
                    bytes.truncate(length);
                    Ok(WaitOutcome::Response(bytes))
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    Ok(WaitOutcome::Timeout)
                }
                Err(error) => Err(error.to_string()),
            }
        }
    }
    impl RfcommTransport {
        /// Open a Linux RFCOMM stream directly without creating a privileged
        /// `/dev/rfcomm*` TTY. `index` is retained for API compatibility.
        pub fn bind(
            _index: u8,
            address: &str,
            channel: u8,
            payload_limit: usize,
        ) -> Result<Self, String> {
            if channel == 0 || channel > 30 {
                return Err("RFCOMM channel must be 1..30".into());
            }
            let mut bytes = address
                .split(':')
                .map(|part| u8::from_str_radix(part, 16).map_err(|_| "invalid Bluetooth address"))
                .collect::<Result<Vec<_>, _>>()?;
            if bytes.len() != 6 {
                return Err("invalid Bluetooth address".into());
            }
            // Linux bdaddr_t stores the human-readable octets least-significant first.
            bytes.reverse();
            #[repr(C)]
            struct SockAddrRc {
                family: libc::sa_family_t,
                address: [u8; 6],
                channel: u8,
            }
            // SAFETY: AF_BLUETOOTH/SOCK_STREAM/BTPROTO_RFCOMM are the Linux
            // socket ABI constants, and no borrowed pointer crosses this call.
            let descriptor = unsafe {
                libc::socket(
                    libc::AF_BLUETOOTH,
                    libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                    3, // BTPROTO_RFCOMM
                )
            };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            // SAFETY: the preceding socket call returned a fresh non-negative
            // descriptor, transferred exactly once into OwnedFd.
            let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
            let socket_address = SockAddrRc {
                family: libc::AF_BLUETOOTH as libc::sa_family_t,
                address: bytes.try_into().expect("six checked octets"),
                channel,
            };
            // SAFETY: SockAddrRc is repr(C), fully initialized, and the pointer
            // remains valid for the exact structure length during connect.
            let connected = unsafe {
                libc::connect(
                    std::os::fd::AsRawFd::as_raw_fd(&descriptor),
                    (&raw const socket_address).cast::<libc::sockaddr>(),
                    std::mem::size_of::<SockAddrRc>() as libc::socklen_t,
                )
            };
            if connected != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let file = File::from(descriptor);
            if payload_limit == 0 {
                return Err("RFCOMM payload limit must be positive".into());
            }
            Ok(Self {
                device: PersistentDevice::spawn(RfcommBackend {
                    file,
                    response_limit: 64,
                }),
                payload_limit,
                address: address.to_owned(),
                channel,
            })
        }
    }
    impl Transport for RfcommTransport {
        fn payload_limit(&self) -> usize {
            self.payload_limit
        }
        fn subscribe_notifications(
            &mut self,
        ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
            Box::pin(async { Ok(NotificationSupport::Unavailable) })
        }
        fn write<'a>(
            &'a mut self,
            bytes: &'a [u8],
            _: WriteKind,
        ) -> TransportFuture<'a, Result<(), TransportError>> {
            Box::pin(async move { self.device.write(bytes).await })
        }
        fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
            Box::pin(tokio::time::sleep(duration))
        }
        fn wait_response(
            &mut self,
            timeout: Duration,
        ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
            Box::pin(async move { self.device.read(timeout).await })
        }
        fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
            Box::pin(async move { self.device.disconnect().await })
        }
    }
}
