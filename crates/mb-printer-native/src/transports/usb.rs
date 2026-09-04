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
    mb_printer_core::protocol::ieee1284::parse_device_id(data).map_err(|error| error.to_string())
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
    fn read_bulk(&mut self, timeout_ms: u64, maximum: usize) -> Result<Option<Vec<u8>>, String>;
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
        self.backend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    fn read_bulk(&mut self, timeout_ms: u64, maximum: usize) -> Result<Option<Vec<u8>>, String> {
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
