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
    use std::collections::BTreeMap;

    pub const MAX_IEEE1284_DEVICE_ID_BYTES: usize = 2048;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Ieee1284DeviceId {
        pub raw: String,
        pub manufacturer: Option<String>,
        pub model: Option<String>,
        pub command_sets: Vec<String>,
        pub fields: BTreeMap<String, String>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UsbPortStatus {
        pub selected: bool,
        pub paper_empty: bool,
        pub error: bool,
    }

    pub fn parse_ieee1284_device_id(data: &[u8]) -> Result<Ieee1284DeviceId, String> {
        if data.len() < 2 {
            return Err("short IEEE-1284 device ID".into());
        }
        if data.len() > MAX_IEEE1284_DEVICE_ID_BYTES {
            return Err("IEEE-1284 device ID exceeds limit".into());
        }
        let declared = usize::from(u16::from_be_bytes([data[0], data[1]]));
        if declared < 2 || declared > data.len() || declared > MAX_IEEE1284_DEVICE_ID_BYTES {
            return Err("invalid IEEE-1284 device ID length".into());
        }
        let raw = std::str::from_utf8(&data[2..declared])
            .map_err(|_| "IEEE-1284 device ID is not UTF-8")?
            .trim_matches(char::from(0))
            .trim()
            .to_owned();
        if raw.is_empty() || !raw.contains(';') {
            return Err("malformed IEEE-1284 device ID".into());
        }
        let fields = raw
            .split(';')
            .filter_map(|field| {
                let (key, value) = field.split_once(':')?;
                let key = key.trim().to_ascii_uppercase();
                let value = value.trim().to_owned();
                (!key.is_empty() && !value.is_empty()).then_some((key, value))
            })
            .collect::<BTreeMap<_, _>>();
        if fields.is_empty() {
            return Err("malformed IEEE-1284 device ID fields".into());
        }
        let field =
            |short: &str, long: &str| fields.get(short).or_else(|| fields.get(long)).cloned();
        let manufacturer = field("MFG", "MANUFACTURER");
        let model = field("MDL", "MODEL");
        let command_sets = field("CMD", "COMMAND SET")
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Ieee1284DeviceId {
            raw,
            manufacturer,
            model,
            command_sets,
            fields,
        })
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
        backend: B,
        payload_limit: usize,
        command_limit: usize,
        response_limit: usize,
    }
    impl<B> UsbTransport<B> {
        pub fn new(backend: B, payload_limit: usize, response_limit: usize) -> Self {
            Self {
                backend,
                payload_limit,
                command_limit: payload_limit,
                response_limit,
            }
        }
        pub fn new_with_limits(
            backend: B,
            payload_limit: usize,
            command_limit: usize,
            response_limit: usize,
        ) -> Self {
            Self {
                backend,
                payload_limit,
                command_limit,
                response_limit,
            }
        }
        pub fn backend(&self) -> &B {
            &self.backend
        }
    }
    impl<B: UsbPrinterClassBackend> UsbTransport<B> {
        pub fn get_device_id(
            &mut self,
            timeout_ms: u64,
        ) -> Result<Option<Ieee1284DeviceId>, String> {
            if timeout_ms == 0 {
                return Err("USB Printer Class timeout must be positive".into());
            }
            self.backend
                .get_device_id_raw(timeout_ms, MAX_IEEE1284_DEVICE_ID_BYTES)?
                .map(|data| parse_ieee1284_device_id(&data))
                .transpose()
        }

        pub fn get_port_status(
            &mut self,
            timeout_ms: u64,
        ) -> Result<Option<UsbPortStatus>, String> {
            if timeout_ms == 0 {
                return Err("USB Printer Class timeout must be positive".into());
            }
            self.backend
                .get_port_status_raw(timeout_ms)
                .map(|value| value.map(parse_port_status))
        }
    }
    impl<B: UsbBulkBackend> Transport for UsbTransport<B> {
        fn payload_limit(&self) -> usize {
            self.payload_limit
        }
        fn command_limit(&self) -> usize {
            self.command_limit
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

#[cfg(feature = "ble")]
pub mod ble {
    use super::*;
    use btleplug::api::{
        Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType,
    };
    use futures_util::StreamExt;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use tracing::Instrument as _;
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
    /// Injectable Tokio-native GATT boundary used by services and contract tests.
    pub type AsyncBleFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;
    pub trait AsyncBleGattBackend: Send {
        fn subscribe(&mut self) -> AsyncBleFuture<'_, bool>;
        fn write_without_response<'a>(&'a mut self, bytes: &'a [u8]) -> AsyncBleFuture<'a, ()>;
        fn wait_notification(&mut self, timeout_ms: u64) -> AsyncBleFuture<'_, Option<Vec<u8>>>;
        fn disconnect(&mut self) -> AsyncBleFuture<'_, ()>;
    }
    /// Serializes all GATT effects and exposes notification availability explicitly.
    pub struct AsyncBleTransport<B> {
        backend: tokio::sync::Mutex<B>,
        notifications: AtomicBool,
        payload_limit: usize,
    }
    impl<B: AsyncBleGattBackend> AsyncBleTransport<B> {
        pub fn new(backend: B, payload_limit: usize) -> Result<Self, String> {
            if payload_limit == 0 {
                return Err("BLE payload limit must be positive".into());
            }
            Ok(Self {
                backend: tokio::sync::Mutex::new(backend),
                notifications: AtomicBool::new(false),
                payload_limit,
            })
        }
        pub async fn subscribe_notifications(&self) -> Result<bool, String> {
            let available = self.backend.lock().await.subscribe().await?;
            self.notifications.store(available, Ordering::Release);
            Ok(available)
        }
        pub async fn write(&self, bytes: &[u8]) -> Result<(), String> {
            if bytes.len() > self.payload_limit {
                return Err("BLE write exceeds declared payload limit".into());
            }
            self.backend
                .lock()
                .await
                .write_without_response(bytes)
                .await
        }
        pub async fn wait_notification(&self, timeout_ms: u64) -> Result<WaitOutcome, String> {
            if !self.notifications.load(Ordering::Acquire) {
                return Ok(WaitOutcome::Unavailable);
            }
            Ok(
                match self
                    .backend
                    .lock()
                    .await
                    .wait_notification(timeout_ms)
                    .await?
                {
                    Some(bytes) => WaitOutcome::Response(bytes),
                    None => WaitOutcome::Timeout,
                },
            )
        }
        pub async fn disconnect(&self) -> Result<(), String> {
            self.backend.lock().await.disconnect().await
        }
    }
    pub fn discover_btleplug(timeout_ms: u64) -> Result<Vec<DiscoveredPrinter>, String> {
        let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
        runtime.block_on(discover_btleplug_async(timeout_ms))
    }
    /// Async BLE discovery for applications that already own a Tokio runtime.
    pub async fn discover_btleplug_async(
        timeout_ms: u64,
    ) -> Result<Vec<DiscoveredPrinter>, String> {
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
            runtime.spawn(
                async move {
                    while let Some(notification) = stream.next().await {
                        if tx.send(notification.value).is_err() {
                            break;
                        }
                    }
                }
                .instrument(tracing::Span::current()),
            );
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
    /// Tokio-native BLE transport. It avoids creating or blocking an internal runtime.
    pub struct AsyncBtleplugTransport {
        peripheral: btleplug::platform::Peripheral,
        write: btleplug::api::Characteristic,
        notify: Option<btleplug::api::Characteristic>,
        notifications: tokio::sync::mpsc::Receiver<Vec<u8>>,
        pub payload_limit: usize,
    }
    impl AsyncBtleplugTransport {
        pub async fn connect(
            address: &str,
            write_uuid: Option<uuid::Uuid>,
            notify_uuid: Option<uuid::Uuid>,
            payload_limit: usize,
            scan_timeout_ms: u64,
        ) -> Result<Self, String> {
            if payload_limit == 0 {
                return Err("BLE payload limit must be positive".into());
            }
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
                    .map_err(|error| error.to_string())?;
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
                        item.properties.intersects(
                            CharPropFlags::WRITE_WITHOUT_RESPONSE | CharPropFlags::WRITE,
                        ),
                        |expected| item.uuid == expected,
                    )
                })
                .cloned()
                .ok_or_else(|| "BLE write characteristic not found".to_owned())?;
            let notify = characteristics
                .iter()
                .find(|item| {
                    notify_uuid.map_or(
                        item.properties.contains(CharPropFlags::NOTIFY),
                        |expected| item.uuid == expected,
                    )
                })
                .cloned();
            let mut stream = peripheral
                .notifications()
                .await
                .map_err(|error| error.to_string())?;
            let (sender, notifications) = tokio::sync::mpsc::channel(32);
            tokio::spawn(
                async move {
                    while let Some(notification) = stream.next().await {
                        if sender.send(notification.value).await.is_err() {
                            break;
                        }
                    }
                }
                .instrument(tracing::Span::current()),
            );
            Ok(Self {
                peripheral,
                write,
                notify,
                notifications,
                payload_limit,
            })
        }
        pub async fn subscribe_notifications(&self) -> Result<bool, String> {
            let Some(characteristic) = &self.notify else {
                return Ok(false);
            };
            self.peripheral
                .subscribe(characteristic)
                .await
                .map_err(|error| error.to_string())?;
            Ok(true)
        }
        pub async fn write(&self, bytes: &[u8]) -> Result<(), String> {
            if bytes.len() > self.payload_limit {
                return Err("BLE write exceeds declared payload limit".into());
            }
            self.peripheral
                .write(&self.write, bytes, WriteType::WithoutResponse)
                .await
                .map_err(|error| error.to_string())
        }
        pub async fn wait_notification(
            &mut self,
            timeout_ms: u64,
        ) -> Result<Option<Vec<u8>>, String> {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), self.notifications.recv())
                .await
            {
                Ok(value) => Ok(value),
                Err(_) => Ok(None),
            }
        }
        pub async fn disconnect(&self) -> Result<(), String> {
            self.peripheral
                .disconnect()
                .await
                .map_err(|error| error.to_string())
        }
    }
}

#[cfg(feature = "wifi")]
pub mod wifi {
    use crate::Transport;
    use mb_printer_core::protocol::brother::wifi::{
        self as core_wifi, WirelessSettings as TypedWirelessSettings,
    };
    pub use mb_printer_core::protocol::brother::wifi::{
        AccessPoint, PJL_FOOTER, PJL_HEADER, REBOOT_COMMAND, WirelessAuthentication,
        WirelessEncryption, WirelessField, encode_ssid, ip_address_command, parse_access_points,
        parse_authentication, parse_boolean_field, parse_encryption, parse_ip_address,
        parse_oid_value, parse_wifi_status, wifi_scan_result_command, wifi_scan_start_command,
        wifi_status_command, wireless_scan_plan, wireless_status_plan, xor_password,
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
        fn provision(&mut self, credentials: &WifiCredentials) -> Result<(), String>;
    }
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum IppScheme {
        #[default]
        Ipp,
        Ipps,
    }
    impl IppScheme {
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Ipp => "ipp",
                Self::Ipps => "ipps",
            }
        }
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct IppEndpoint {
        pub scheme: IppScheme,
        pub host: String,
        pub port: u16,
        pub resource: String,
    }
    impl IppEndpoint {
        pub fn ipp(host: impl Into<String>, port: u16, resource: impl Into<String>) -> Self {
            Self {
                scheme: IppScheme::Ipp,
                host: host.into(),
                port,
                resource: resource.into(),
            }
        }
        pub fn ipps(host: impl Into<String>, port: u16, resource: impl Into<String>) -> Self {
            Self {
                scheme: IppScheme::Ipps,
                host: host.into(),
                port,
                resource: resource.into(),
            }
        }
    }
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
        let body = ipp_status_request(endpoint);
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
    fn ipp_attribute(output: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
        output.push(tag);
        output.extend(u16::try_from(name.len()).unwrap_or(u16::MAX).to_be_bytes());
        output.extend(name.as_bytes());
        output.extend(u16::try_from(value.len()).unwrap_or(u16::MAX).to_be_bytes());
        output.extend(value);
    }
    fn ipp_status_request(endpoint: &IppEndpoint) -> Vec<u8> {
        let mut body = vec![2, 0, 0, 0x0b, 0, 0, 0, 1, 1];
        ipp_attribute(&mut body, 0x47, "attributes-charset", b"utf-8");
        ipp_attribute(&mut body, 0x48, "attributes-natural-language", b"en");
        let uri = format!(
            "ipp://{}:{}{}",
            endpoint.host, endpoint.port, endpoint.resource
        );
        ipp_attribute(&mut body, 0x45, "printer-uri", uri.as_bytes());
        for (index, name) in ["printer-state", "printer-state-reasons", "media-ready"]
            .into_iter()
            .enumerate()
        {
            ipp_attribute(
                &mut body,
                0x44,
                if index == 0 {
                    "requested-attributes"
                } else {
                    ""
                },
                name.as_bytes(),
            );
        }
        body.push(3);
        body
    }
    pub fn parse_ipp_status(body: &[u8]) -> Result<IppPrinterStatus, String> {
        if body.len() < 9 || u16::from_be_bytes([body[2], body[3]]) >= 0x0100 {
            return Err("IPP operation failed".into());
        }
        let mut offset = 8usize;
        let mut previous_name = String::new();
        let mut status = IppPrinterStatus {
            printer_state: None,
            reasons: Vec::new(),
            media_ready: Vec::new(),
        };
        while offset < body.len() {
            let tag = body[offset];
            offset += 1;
            if tag == 3 {
                return Ok(status);
            }
            if tag <= 0x0f {
                previous_name.clear();
                continue;
            }
            if offset + 2 > body.len() {
                return Err("truncated IPP attribute".into());
            }
            let name_length = usize::from(u16::from_be_bytes([body[offset], body[offset + 1]]));
            offset += 2;
            if offset + name_length + 2 > body.len() {
                return Err("truncated IPP attribute".into());
            }
            if name_length > 0 {
                previous_name = String::from_utf8(body[offset..offset + name_length].to_vec())
                    .map_err(|_| "IPP attribute name is not UTF-8".to_owned())?;
            }
            offset += name_length;
            let value_length = usize::from(u16::from_be_bytes([body[offset], body[offset + 1]]));
            offset += 2;
            if offset + value_length > body.len() {
                return Err("truncated IPP value".into());
            }
            let value = &body[offset..offset + value_length];
            offset += value_length;
            match previous_name.as_str() {
                "printer-state" if tag == 0x23 && value.len() == 4 => {
                    status.printer_state = Some(u32::from_be_bytes(value.try_into().unwrap()))
                }
                "printer-state-reasons" => status
                    .reasons
                    .push(String::from_utf8_lossy(value).into_owned()),
                "media-ready" => status
                    .media_ready
                    .push(String::from_utf8_lossy(value).into_owned()),
                _ => {}
            }
        }
        Err("IPP response has no end marker".into())
    }
    /// String-valued compatibility adapter for the original native API.
    /// New callers should construct the typed core `WirelessSettings` directly.
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
            TypedWirelessSettings {
                ssid: self.ssid.clone(),
                password: self.password.clone(),
                encryption: WirelessEncryption::try_from(self.encryption.as_str())
                    .map_err(|error| error.to_string())?,
                authentication: WirelessAuthentication::try_from(self.authentication.as_str())
                    .map_err(|error| error.to_string())?,
                infrastructure: self.infrastructure,
                wireless_direct: self.wireless_direct,
                reboot: self.reboot,
            }
            .command()
            .map_err(|error| error.to_string())
        }
    }
    pub fn inquire_command(oid: &str) -> Result<Vec<u8>, String> {
        core_wifi::inquire_command(oid).map_err(|error| error.to_string())
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
        inner: FileTransport,
        response_limit: usize,
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
                response_limit: 64,
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
            let mut descriptor = libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&self.inner.file),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = i32::try_from(timeout_ms).unwrap_or(i32::MAX);
            // SAFETY: descriptor points to one initialized pollfd for the
            // duration of the call.
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
            let mut bytes = vec![0; self.response_limit.max(1)];
            match self.inner.file.read(&mut bytes) {
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
}
