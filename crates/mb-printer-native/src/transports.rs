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
    pub struct SerialTransport(pub FileTransport);
    impl SerialTransport {
        /// Opens an OS-configured serial/RFCOMM TTY. Baud/parity setup remains with the platform layer.
        pub fn open(path: impl AsRef<Path>, payload_limit: usize) -> Result<Self, String> {
            FileTransport::open(path, payload_limit).map(Self)
        }
    }
    impl Transport for SerialTransport {
        fn payload_limit(&self) -> usize {
            self.0.payload_limit()
        }
        fn subscribe_notifications(&mut self) -> Result<(), String> {
            self.0.subscribe_notifications()
        }
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.0.write(bytes)
        }
        fn delay_monotonic(&mut self, milliseconds: u64) {
            self.0.delay_monotonic(milliseconds)
        }
        fn wait_response(&mut self, timeout_ms: u64) -> Result<WaitOutcome, String> {
            self.0.wait_response(timeout_ms)
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
}

#[cfg(feature = "ble")]
pub mod ble {
    use super::*;
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
}

#[cfg(feature = "wifi")]
pub mod wifi {
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
    use std::{path::PathBuf, process::Command};
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
        pub device: PathBuf,
    }
    impl RfcommTransport {
        /// Bind through BlueZ's `rfcomm` utility, then open the resulting TTY.
        pub fn bind(
            index: u8,
            address: &str,
            channel: u8,
            payload_limit: usize,
        ) -> Result<Self, String> {
            let device = PathBuf::from(format!("/dev/rfcomm{index}"));
            let channel = channel.to_string();
            let status = Command::new("rfcomm")
                .args(["bind", device.to_str().unwrap(), address, &channel])
                .status()
                .map_err(|error| error.to_string())?;
            if !status.success() {
                return Err(format!("rfcomm bind exited with {status}"));
            }
            Ok(Self {
                inner: FileTransport::open(&device, payload_limit)?,
                device,
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
