// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reusable blocking native transport and discovery boundaries.
use crate::{Transport, WaitOutcome};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    time::Duration,
};

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
    fn query_status(&mut self, timeout_ms: u64) -> Result<PrinterStatus, String>;
}
pub struct CommandStatusBackend<T, F> {
    pub transport: T,
    pub query: Vec<u8>,
    pub decode: F,
}
impl<T: Transport, F: Fn(&[u8]) -> Result<PrinterStatus, String>> StatusBackend
    for CommandStatusBackend<T, F>
{
    fn query_status(&mut self, timeout_ms: u64) -> Result<PrinterStatus, String> {
        self.transport.write(&self.query)?;
        match self.transport.wait_response(timeout_ms)? {
            WaitOutcome::Response(bytes) => (self.decode)(&bytes),
            WaitOutcome::Timeout => Err("status response timed out".into()),
            WaitOutcome::Unavailable => Err("status response unavailable".into()),
        }
    }
}

pub struct FileTransport {
    file: File,
    payload_limit: usize,
}
impl FileTransport {
    pub fn open(path: impl AsRef<Path>, payload_limit: usize) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| error.to_string())?;
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
    fn subscribe_notifications(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.file
            .write_all(bytes)
            .and_then(|_| self.file.flush())
            .map_err(|error| error.to_string())
    }
    fn delay_monotonic(&mut self, milliseconds: u64) {
        std::thread::sleep(Duration::from_millis(milliseconds))
    }
    fn wait_response(&mut self, _: u64) -> Result<WaitOutcome, String> {
        Ok(WaitOutcome::Unavailable)
    }
}

pub struct TcpTransport {
    stream: TcpStream,
    payload_limit: usize,
    response_limit: usize,
}
impl TcpTransport {
    pub fn connect(
        address: SocketAddr,
        payload_limit: usize,
        response_limit: usize,
    ) -> Result<Self, String> {
        let stream = TcpStream::connect(address).map_err(|error| error.to_string())?;
        stream
            .set_nodelay(true)
            .map_err(|error| error.to_string())?;
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
    fn subscribe_notifications(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(bytes)
            .map_err(|error| error.to_string())
    }
    fn delay_monotonic(&mut self, milliseconds: u64) {
        std::thread::sleep(Duration::from_millis(milliseconds))
    }
    fn wait_response(&mut self, timeout_ms: u64) -> Result<WaitOutcome, String> {
        self.stream
            .set_read_timeout(Some(Duration::from_millis(timeout_ms)))
            .map_err(|error| error.to_string())?;
        let mut bytes = vec![0; self.response_limit.max(1)];
        match self.stream.read(&mut bytes) {
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
        port: Box<dyn serialport::SerialPort>,
        config: SerialConfig,
    }
    impl SerialTransport {
        /// Opens and configures a serial device with the default 115200 8-N-1 profile.
        pub fn open(path: impl AsRef<Path>, payload_limit: usize) -> Result<Self, String> {
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
        ) -> Result<Self, String> {
            let port = serialport::new(path.as_ref().to_string_lossy(), config.baud_rate)
                .timeout(Duration::from_millis(config.timeout_ms))
                .data_bits(serialport::DataBits::Eight)
                .parity(serialport::Parity::None)
                .stop_bits(serialport::StopBits::One)
                .flow_control(serialport::FlowControl::None)
                .open()
                .map_err(|error| error.to_string())?;
            Ok(Self { port, config })
        }
    }
    impl Transport for SerialTransport {
        fn payload_limit(&self) -> usize {
            self.config.payload_limit
        }
        fn subscribe_notifications(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.port
                .write_all(bytes)
                .and_then(|_| self.port.flush())
                .map_err(|error| error.to_string())
        }
        fn delay_monotonic(&mut self, milliseconds: u64) {
            std::thread::sleep(Duration::from_millis(milliseconds))
        }
        fn wait_response(&mut self, timeout_ms: u64) -> Result<WaitOutcome, String> {
            self.port
                .set_timeout(Duration::from_millis(timeout_ms))
                .map_err(|error| error.to_string())?;
            let mut bytes = vec![0; self.config.response_limit.max(1)];
            match self.port.read(&mut bytes) {
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
    pub trait UsbBulkBackend {
        fn write_bulk(&mut self, bytes: &[u8]) -> Result<(), String>;
        fn read_bulk(&mut self, timeout_ms: u64, maximum: usize)
        -> Result<Option<Vec<u8>>, String>;
    }
    pub trait UsbDiscoveryBackend {
        fn discover_usb(&self) -> Result<Vec<DiscoveredPrinter>, String>;
    }
    pub struct UsbTransport<B> {
        backend: B,
        payload_limit: usize,
        response_limit: usize,
    }
    impl<B> UsbTransport<B> {
        pub fn new(backend: B, payload_limit: usize, response_limit: usize) -> Self {
            Self {
                backend,
                payload_limit,
                response_limit,
            }
        }
        pub fn backend(&self) -> &B {
            &self.backend
        }
    }
    impl<B: UsbBulkBackend> Transport for UsbTransport<B> {
        fn payload_limit(&self) -> usize {
            self.payload_limit
        }
        fn subscribe_notifications(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.backend.write_bulk(bytes)
        }
        fn delay_monotonic(&mut self, milliseconds: u64) {
            std::thread::sleep(Duration::from_millis(milliseconds))
        }
        fn wait_response(&mut self, timeout_ms: u64) -> Result<WaitOutcome, String> {
            match self.backend.read_bulk(timeout_ms, self.response_limit)? {
                Some(bytes) => Ok(WaitOutcome::Response(bytes)),
                None => Ok(WaitOutcome::Timeout),
            }
        }
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UsbIdentity {
        pub vendor_id: u16,
        pub product_id: u16,
        pub bus: u8,
        pub address: u8,
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
    pub struct RusbBulkBackend {
        handle: rusb::DeviceHandle<rusb::Context>,
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
            let _ = handle.set_auto_detach_kernel_driver(true);
            handle
                .claim_interface(interface)
                .map_err(|error| error.to_string())?;
            Ok(Self {
                handle,
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
}

#[cfg(feature = "ble")]
pub mod ble {
    use super::*;
    use btleplug::api::{
        Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType,
    };
    use futures_util::StreamExt;
    use std::sync::mpsc::{self, Receiver};
    pub trait BleGattBackend {
        fn subscribe(&mut self) -> Result<bool, String>;
        fn write_without_response(&mut self, bytes: &[u8]) -> Result<(), String>;
        fn wait_notification(&mut self, timeout_ms: u64) -> Result<Option<Vec<u8>>, String>;
    }
    pub trait BleDiscoveryBackend {
        fn discover_ble(&self, timeout_ms: u64) -> Result<Vec<DiscoveredPrinter>, String>;
    }
    pub struct BleTransport<B> {
        backend: B,
        payload_limit: usize,
        notifications: bool,
    }
    impl<B> BleTransport<B> {
        pub fn new(backend: B, payload_limit: usize) -> Self {
            Self {
                backend,
                payload_limit,
                notifications: false,
            }
        }
        pub fn backend(&self) -> &B {
            &self.backend
        }
    }
    impl<B: BleGattBackend> Transport for BleTransport<B> {
        fn payload_limit(&self) -> usize {
            self.payload_limit
        }
        fn subscribe_notifications(&mut self) -> Result<(), String> {
            self.notifications = self.backend.subscribe()?;
            Ok(())
        }
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.backend.write_without_response(bytes)
        }
        fn delay_monotonic(&mut self, milliseconds: u64) {
            std::thread::sleep(Duration::from_millis(milliseconds))
        }
        fn wait_response(&mut self, timeout_ms: u64) -> Result<WaitOutcome, String> {
            if !self.notifications {
                return Ok(WaitOutcome::Unavailable);
            };
            Ok(match self.backend.wait_notification(timeout_ms)? {
                Some(bytes) => WaitOutcome::Response(bytes),
                None => WaitOutcome::Timeout,
            })
        }
    }
    pub fn discover_btleplug(timeout_ms: u64) -> Result<Vec<DiscoveredPrinter>, String> {
        let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
        runtime.block_on(async move {
            let manager = btleplug::platform::Manager::new()
                .await
                .map_err(|error| error.to_string())?;
            let adapters = manager
                .adapters()
                .await
                .map_err(|error| error.to_string())?;
            let mut found = Vec::new();
            for adapter in adapters {
                adapter
                    .start_scan(ScanFilter::default())
                    .await
                    .map_err(|error| error.to_string())?;
                tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                for peripheral in adapter
                    .peripherals()
                    .await
                    .map_err(|error| error.to_string())?
                {
                    let properties = peripheral
                        .properties()
                        .await
                        .map_err(|error| error.to_string())?;
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
        })
    }
    pub struct BtleplugDiscovery;
    impl BleDiscoveryBackend for BtleplugDiscovery {
        fn discover_ble(&self, timeout_ms: u64) -> Result<Vec<DiscoveredPrinter>, String> {
            discover_btleplug(timeout_ms)
        }
    }
    pub struct BtleplugBackend {
        runtime: tokio::runtime::Runtime,
        peripheral: btleplug::platform::Peripheral,
        write: btleplug::api::Characteristic,
        notify: Option<btleplug::api::Characteristic>,
        notifications: Receiver<Vec<u8>>,
    }
    impl BtleplugBackend {
        pub fn connect(
            address: &str,
            write_uuid: Option<uuid::Uuid>,
            notify_uuid: Option<uuid::Uuid>,
            scan_timeout_ms: u64,
        ) -> Result<Self, String> {
            let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
            let (peripheral, write, notify, mut stream) = runtime.block_on(async {
                let manager = btleplug::platform::Manager::new()
                    .await
                    .map_err(|error| error.to_string())?;
                let adapters = manager
                    .adapters()
                    .await
                    .map_err(|error| error.to_string())?;
                let mut selected = None;
                for adapter in adapters {
                    adapter
                        .start_scan(ScanFilter::default())
                        .await
                        .map_err(|error| error.to_string())?;
                    tokio::time::sleep(Duration::from_millis(scan_timeout_ms)).await;
                    for peripheral in adapter
                        .peripherals()
                        .await
                        .map_err(|error| error.to_string())?
                    {
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
                let peripheral =
                    selected.ok_or_else(|| format!("BLE peripheral not found: {address}"))?;
                if !peripheral
                    .is_connected()
                    .await
                    .map_err(|error| error.to_string())?
                {
                    peripheral
                        .connect()
                        .await
                        .map_err(|error| error.to_string())?
                }
                peripheral
                    .discover_services()
                    .await
                    .map_err(|error| error.to_string())?;
                let characteristics = peripheral.characteristics();
                let write = characteristics
                    .iter()
                    .find(|item| {
                        write_uuid.map_or(
                            item.properties
                                .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
                                | item.properties.contains(CharPropFlags::WRITE),
                            |uuid| item.uuid == uuid,
                        )
                    })
                    .cloned()
                    .ok_or_else(|| "BLE write characteristic not found".to_owned())?;
                let notify = characteristics
                    .iter()
                    .find(|item| {
                        notify_uuid
                            .map_or(item.properties.contains(CharPropFlags::NOTIFY), |uuid| {
                                item.uuid == uuid
                            })
                    })
                    .cloned();
                let stream = peripheral
                    .notifications()
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>((peripheral, write, notify, stream))
            })?;
            let (tx, rx) = mpsc::channel();
            runtime.spawn(async move {
                while let Some(notification) = stream.next().await {
                    if tx.send(notification.value).is_err() {
                        break;
                    }
                }
            });
            Ok(Self {
                runtime,
                peripheral,
                write,
                notify,
                notifications: rx,
            })
        }
    }
    impl BleGattBackend for BtleplugBackend {
        fn subscribe(&mut self) -> Result<bool, String> {
            let Some(characteristic) = &self.notify else {
                return Ok(false);
            };
            self.runtime
                .block_on(self.peripheral.subscribe(characteristic))
                .map_err(|error| error.to_string())?;
            Ok(true)
        }
        fn write_without_response(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.runtime
                .block_on(
                    self.peripheral
                        .write(&self.write, bytes, WriteType::WithoutResponse),
                )
                .map_err(|error| error.to_string())
        }
        fn wait_notification(&mut self, timeout_ms: u64) -> Result<Option<Vec<u8>>, String> {
            match self
                .notifications
                .recv_timeout(Duration::from_millis(timeout_ms))
            {
                Ok(bytes) => Ok(Some(bytes)),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }
    }
    pub type BtleplugTransport = BleTransport<BtleplugBackend>;
    pub fn connect_btleplug(
        address: &str,
        write_uuid: Option<uuid::Uuid>,
        notify_uuid: Option<uuid::Uuid>,
        payload_limit: usize,
        scan_timeout_ms: u64,
    ) -> Result<BtleplugTransport, String> {
        Ok(BleTransport::new(
            BtleplugBackend::connect(address, write_uuid, notify_uuid, scan_timeout_ms)?,
            payload_limit,
        ))
    }
}

#[cfg(feature = "wifi")]
pub mod wifi {
    use crate::Transport;
    pub const PJL_HEADER: &[u8] = b"\x1b%-12345X@PJL\r\n";
    pub const PJL_FOOTER: &[u8] = b"\x1b%-12345X";
    pub const REBOOT_COMMAND: &[u8] = &[
        0x1b, 0x69, 0x58, 0x2a, 0x31, 0x03, 0, 0x01, 0x2e, 0, 0, 0, 0x2c, 0,
    ];
    const PASSWORD_KEY: [u8; 16] = [
        0x0d, 0xae, 0xe4, 0xa1, 0x8b, 0x7f, 0x26, 0x5e, 0x72, 0x5b, 0x17, 0x7a, 0x71, 0xcd, 0xec,
        0x4d,
    ];
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
        fn provision(&mut self, credentials: &WifiCredentials) -> Result<(), String>;
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AccessPoint {
        pub ssid: String,
        pub channel: u8,
        pub power: i16,
        pub enterprise: bool,
        pub encrypted: bool,
    }
    #[derive(Clone)]
    pub struct WirelessSettings {
        pub ssid: String,
        pub password: String,
        pub encryption: String,
        pub authentication: String,
        pub infrastructure: bool,
        pub wireless_direct: bool,
        pub reboot: bool,
    }
    impl std::fmt::Debug for WirelessSettings {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("WirelessSettings")
                .field("ssid", &self.ssid)
                .field("password", &"[REDACTED]")
                .field("encryption", &self.encryption)
                .field("authentication", &self.authentication)
                .finish_non_exhaustive()
        }
    }
    impl WirelessSettings {
        pub fn command(&self) -> Result<Vec<u8>, String> {
            if self.ssid.is_empty() {
                return Err("SSID must not be empty".into());
            }
            let encryption = match self.encryption.as_str() {
                "none" => 1,
                "wep" => 2,
                "tkip" => 3,
                "aes" => 4,
                "ckip" => 5,
                "cmic" => 6,
                "ckip-cmic" => 7,
                "tkip-aes" => 8,
                _ => return Err("unknown encryption".into()),
            };
            let authentication = match self.authentication.as_str() {
                "open" => 1,
                "shared-key" => 2,
                "wpa-psk" => 3,
                "leap" => 7,
                "eap-fast" => 13,
                "peap" => 15,
                "eap-ttls" => 16,
                "eap-tls" => 17,
                "wpa-only" => 18,
                "wpa2-only" => 19,
                _ => return Err("unknown authentication".into()),
            };
            if authentication != 1 && self.password.is_empty() {
                return Err("authentication needs a password".into());
            }
            if authentication == 1 && encryption != 1 {
                return Err("open authentication requires no encryption".into());
            }
            let mut parameters = vec![
                ("458867", b"0".to_vec()),
                ("458878", b"1".to_vec()),
                ("458877", encode_ssid(&self.ssid)),
            ];
            if [3, 18, 19].contains(&authentication) {
                parameters.push(("99458890", xor_password(self.password.as_bytes())))
            } else if encryption == 2 {
                parameters.push(("99458889.1", xor_password(self.password.as_bytes())))
            }
            parameters.extend([
                ("458880", encryption.to_string().into_bytes()),
                ("458881", authentication.to_string().into_bytes()),
                (
                    "459138.2",
                    u8::from(self.infrastructure).to_string().into_bytes(),
                ),
                (
                    "459138.3",
                    u8::from(self.wireless_direct).to_string().into_bytes(),
                ),
                ("458865", b"1".to_vec()),
            ]);
            let mut output = PJL_HEADER.to_vec();
            for (oid, value) in parameters {
                output.extend(b"@PJL DEFAULT OBJBRNET=\"");
                output.extend(oid.as_bytes());
                output.push(b':');
                output.extend(value);
                output.extend(b"\"\r\n")
            }
            output.extend(PJL_FOOTER);
            if self.reboot {
                output.extend(REBOOT_COMMAND)
            }
            Ok(output)
        }
    }
    pub fn xor_password(value: &[u8]) -> Vec<u8> {
        value
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ PASSWORD_KEY[index % PASSWORD_KEY.len()])
            .collect()
    }
    pub fn encode_ssid(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .iter()
            .flat_map(|byte| format!("-{byte:x}").into_bytes())
            .collect()
    }
    pub fn inquire_command(oid: &str) -> Result<Vec<u8>, String> {
        if oid.is_empty()
            || !oid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
        {
            return Err("invalid OBJBRNET OID".into());
        }
        let mut out = PJL_HEADER.to_vec();
        out.extend(b"@PJL DEFAULT OBJBRNET=\"");
        out.extend(oid.as_bytes());
        out.extend(b"\"\r\n@PJL INQUIRE OBJBRNET\r\n");
        out.extend(PJL_FOOTER);
        Ok(out)
    }
    pub fn wifi_status_command() -> Vec<u8> {
        inquire_command("458867").expect("constant OID")
    }
    pub fn ip_address_command() -> Vec<u8> {
        inquire_command("458967.2").expect("constant OID")
    }
    pub fn parse_wifi_status(data: &[u8]) -> Option<bool> {
        let text = String::from_utf8_lossy(data);
        let offset = text.find("458867")?;
        let value = text[offset + 6..]
            .trim_start_matches(|character: char| {
                character.is_whitespace() || character == ':' || character == '\"'
            })
            .chars()
            .next()?;
        match value {
            '0' => Some(false),
            '1' => Some(true),
            _ => None,
        }
    }
    pub fn parse_ip_address(data: &[u8]) -> Option<String> {
        let text = String::from_utf8_lossy(data);
        let offset = text.find("458967.2")?;
        let value = text[offset + 8..]
            .trim_start_matches(|character: char| {
                character.is_whitespace()
                    || character == ':'
                    || character == '\"'
                    || character == '-'
            })
            .split(['\"', '\r', '\n'])
            .next()?;
        let octets = value
            .split('-')
            .map(|part| u8::from_str_radix(part, 16).ok())
            .collect::<Option<Vec<_>>>()?;
        (octets.len() == 4).then(|| {
            octets
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(".")
        })
    }
    pub fn parse_access_points(data: &[u8]) -> Vec<AccessPoint> {
        String::from_utf8_lossy(data)
            .replace('\0', "")
            .lines()
            .filter_map(|line| {
                let fields = line
                    .split(',')
                    .map(|field| field.trim().trim_matches('"'))
                    .collect::<Vec<_>>();
                if fields.len() < 8 || fields[0] != "VAP" {
                    return None;
                }
                Some(AccessPoint {
                    ssid: decode_ssid(fields[1]),
                    channel: fields[4].parse().ok()?,
                    power: fields[5].parse().ok()?,
                    enterprise: fields[6] == "3",
                    encrypted: fields[7] == "2",
                })
            })
            .collect()
    }
    fn decode_ssid(value: &str) -> String {
        let parts = value.trim_matches('-').split('-').collect::<Vec<_>>();
        if parts.len() < 2 {
            return value.into();
        }
        let Some(bytes) = parts
            .iter()
            .map(|part| u8::from_str_radix(part, 16).ok())
            .collect::<Option<Vec<_>>>()
        else {
            return value.into();
        };
        String::from_utf8(bytes).unwrap_or_else(|_| value.into())
    }
    pub struct BrotherWifiProvisioner<T> {
        pub transport: T,
        pub reboot: bool,
    }
    impl<T: Transport> WifiProvisioner for BrotherWifiProvisioner<T> {
        fn provision(&mut self, credentials: &WifiCredentials) -> Result<(), String> {
            let command = WirelessSettings {
                ssid: credentials.ssid.clone(),
                password: credentials.password.clone(),
                encryption: "tkip-aes".into(),
                authentication: "wpa-psk".into(),
                infrastructure: true,
                wireless_direct: false,
                reboot: self.reboot,
            }
            .command()?;
            self.transport.write(&command)
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
        inner: FileTransport,
        pub address: String,
        pub channel: u8,
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
            Ok(Self {
                inner: FileTransport {
                    file,
                    payload_limit,
                },
                address: address.to_owned(),
                channel,
            })
        }
    }
    impl Transport for RfcommTransport {
        fn payload_limit(&self) -> usize {
            self.inner.payload_limit()
        }
        fn subscribe_notifications(&mut self) -> Result<(), String> {
            self.inner.subscribe_notifications()
        }
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.inner.write(bytes)
        }
        fn delay_monotonic(&mut self, milliseconds: u64) {
            self.inner.delay_monotonic(milliseconds)
        }
        fn wait_response(&mut self, timeout_ms: u64) -> Result<WaitOutcome, String> {
            self.inner.wait_response(timeout_ms)
        }
    }
}
