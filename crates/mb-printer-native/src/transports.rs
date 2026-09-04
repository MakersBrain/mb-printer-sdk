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

#[cfg(feature = "dns-sd")]
pub mod dns_sd;
#[cfg(feature = "serial")]
#[path = "transports/serial.rs"]
pub mod serial;
#[cfg(feature = "usb")]
#[path = "transports/usb.rs"]
pub mod usb;

#[cfg(feature = "ipp")]
pub mod ipp;

#[cfg(feature = "ble")]
#[path = "transports/ble.rs"]
pub mod ble;
#[cfg(feature = "native-input")]
#[path = "transports/input.rs"]
pub mod input;
#[cfg(all(feature = "bluetooth-rfcomm", target_os = "linux"))]
#[path = "transports/rfcomm.rs"]
pub mod rfcomm;
#[cfg(feature = "wifi")]
#[path = "transports/wifi.rs"]
pub mod wifi;
