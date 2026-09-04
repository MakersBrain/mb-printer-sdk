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
