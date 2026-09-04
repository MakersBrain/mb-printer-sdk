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
