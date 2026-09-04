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
