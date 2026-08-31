// SPDX-License-Identifier: AGPL-3.0-or-later
use super::wifi::{IppEndpoint, IppScheme};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    time::{Duration, Instant},
};
use thiserror::Error;

pub const IPP_SERVICE_TYPE: &str = "_ipp._tcp.local.";
pub const IPPS_SERVICE_TYPE: &str = "_ipps._tcp.local.";
const MAX_DNS_NAME_BYTES: usize = 1024;
const MAX_RESOURCE_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryLimits {
    pub timeout_ms: u64,
    pub maximum_services: usize,
    pub maximum_txt_bytes: usize,
    pub maximum_addresses: usize,
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            timeout_ms: 3000,
            maximum_services: 64,
            maximum_txt_bytes: 4096,
            maximum_addresses: 16,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DiscoveryError {
    #[error("DNS-SD discovery bounds must be positive")]
    InvalidLimits,
    #[error("DNS-SD backend failed: {0}")]
    Backend(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedService {
    pub service_type: String,
    pub fullname: String,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
    pub txt: Vec<(String, Vec<u8>)>,
}

pub trait DnsSdBackend {
    fn browse(
        &mut self,
        service_type: &str,
        limits: DiscoveryLimits,
    ) -> Result<Vec<ResolvedService>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredIppPrinter {
    pub fullname: String,
    pub endpoint: IppEndpoint,
    pub addresses: BTreeSet<IpAddr>,
    /// TXT keys are normalized to ASCII lowercase; values remain the exact
    /// bounded bytes advertised by the service.
    pub txt: BTreeMap<String, Vec<u8>>,
}

impl DiscoveredIppPrinter {
    pub fn txt_utf8(&self, key: &str) -> Option<&str> {
        std::str::from_utf8(self.txt.get(&key.to_ascii_lowercase())?).ok()
    }
}

pub fn discover_with_backend<B: DnsSdBackend>(
    backend: &mut B,
    limits: DiscoveryLimits,
) -> Result<Vec<DiscoveredIppPrinter>, DiscoveryError> {
    if limits.timeout_ms == 0
        || limits.maximum_services == 0
        || limits.maximum_txt_bytes == 0
        || limits.maximum_addresses == 0
    {
        return Err(DiscoveryError::InvalidLimits);
    }
    let started = Instant::now();
    let total = Duration::from_millis(limits.timeout_ms);
    let mut discovered = BTreeMap::<(IppScheme, String), DiscoveredIppPrinter>::new();

    for (index, service_type) in [IPP_SERVICE_TYPE, IPPS_SERVICE_TYPE]
        .into_iter()
        .enumerate()
    {
        let remaining = total.saturating_sub(started.elapsed());
        if remaining.is_zero() || discovered.len() >= limits.maximum_services {
            break;
        }
        let remaining_ms = u64::try_from(remaining.as_millis())
            .unwrap_or(u64::MAX)
            .max(1)
            / u64::try_from(2 - index).expect("two service types");
        let records = backend
            .browse(
                service_type,
                DiscoveryLimits {
                    timeout_ms: remaining_ms.max(1),
                    maximum_services: limits.maximum_services - discovered.len(),
                    ..limits
                },
            )
            .map_err(DiscoveryError::Backend)?;
        for record in records {
            let Some(printer) = normalize(record, limits) else {
                continue;
            };
            let key = (
                printer.endpoint.scheme,
                printer.fullname.to_ascii_lowercase(),
            );
            discovered.insert(key, printer);
            if discovered.len() >= limits.maximum_services {
                break;
            }
        }
    }
    Ok(discovered.into_values().collect())
}

fn normalize(record: ResolvedService, limits: DiscoveryLimits) -> Option<DiscoveredIppPrinter> {
    let scheme = match record.service_type.as_str() {
        IPP_SERVICE_TYPE => IppScheme::Ipp,
        IPPS_SERVICE_TYPE => IppScheme::Ipps,
        _ => return None,
    };
    if record.fullname.is_empty()
        || record.fullname.len() > MAX_DNS_NAME_BYTES
        || record.host.is_empty()
        || record.host.len() > MAX_DNS_NAME_BYTES
        || record.port == 0
        || record.addresses.is_empty()
        || record.addresses.len() > limits.maximum_addresses
    {
        return None;
    }
    let mut txt_size = 0usize;
    let mut txt = BTreeMap::new();
    for (key, value) in record.txt {
        if key.is_empty() {
            return None;
        }
        txt_size = txt_size.checked_add(key.len())?.checked_add(value.len())?;
        if txt_size > limits.maximum_txt_bytes {
            return None;
        }
        txt.insert(key.to_ascii_lowercase(), value);
    }
    let resource = match txt.get("rp") {
        Some(value) => {
            let value = std::str::from_utf8(value).ok()?.trim();
            if value.is_empty() || value.len() > MAX_RESOURCE_BYTES || value.contains(['\r', '\n'])
            {
                return None;
            }
            if value.starts_with('/') {
                value.to_owned()
            } else {
                format!("/{value}")
            }
        }
        None => "/ipp/print".into(),
    };
    Some(DiscoveredIppPrinter {
        fullname: record.fullname,
        endpoint: IppEndpoint {
            scheme,
            host: record.host,
            port: record.port,
            resource,
        },
        addresses: record.addresses.into_iter().collect(),
        txt,
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MdnsSdBackend;

impl DnsSdBackend for MdnsSdBackend {
    fn browse(
        &mut self,
        service_type: &str,
        limits: DiscoveryLimits,
    ) -> Result<Vec<ResolvedService>, String> {
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|error| error.to_string())?;
        let receiver = daemon
            .browse(service_type)
            .map_err(|error| error.to_string())?;
        let started = Instant::now();
        let timeout = Duration::from_millis(limits.timeout_ms);
        let mut services = Vec::new();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            match receiver.recv_timeout(remaining) {
                Ok(mdns_sd::ServiceEvent::ServiceResolved(service)) => {
                    if service.get_addresses().len() > limits.maximum_addresses {
                        continue;
                    }
                    let mut txt_size = 0usize;
                    let mut txt = Vec::new();
                    let mut txt_within_limit = true;
                    for property in service.get_properties().iter() {
                        let value = property.val().unwrap_or_default();
                        let Some(new_size) = txt_size
                            .checked_add(property.key().len())
                            .and_then(|size| size.checked_add(value.len()))
                        else {
                            txt_within_limit = false;
                            break;
                        };
                        if new_size > limits.maximum_txt_bytes {
                            txt_within_limit = false;
                            break;
                        }
                        txt_size = new_size;
                        txt.push((property.key().to_owned(), value.to_vec()));
                    }
                    if !txt_within_limit {
                        continue;
                    }
                    services.push(ResolvedService {
                        service_type: service.ty_domain.clone(),
                        fullname: service.get_fullname().to_owned(),
                        host: service.get_hostname().to_owned(),
                        port: service.get_port(),
                        addresses: service
                            .get_addresses()
                            .iter()
                            .map(mdns_sd::ScopedIp::to_ip_addr)
                            .collect(),
                        txt,
                    });
                    if services.len() >= limits.maximum_services {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = daemon.stop_browse(service_type);
        let _ = daemon.shutdown();
        Ok(services)
    }
}

pub fn discover(limits: DiscoveryLimits) -> Result<Vec<DiscoveredIppPrinter>, DiscoveryError> {
    discover_with_backend(&mut MdnsSdBackend, limits)
}
