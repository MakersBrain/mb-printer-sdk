// SPDX-License-Identifier: AGPL-3.0-or-later
//! Registered probe metadata. Definitions identify reviewed protocol code;
//! they never contain caller-supplied request bytes or decoder objects.

use crate::{
    capabilities::PrinterDefinition,
    discovery::{ObservationOrigin, ProtocolFamily, QualificationMetadata, TransportKind},
    protocol::{self, Plan},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProbeId(pub String);

impl From<&str> for ProbeId {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeDefinition {
    pub id: ProbeId,
    pub kind: ProbeKind,
    pub protocols: Vec<ProtocolFamily>,
    pub transports: Vec<TransportKind>,
    pub risk: ProbeRisk,
    pub limits: ProbeLimits,
    pub qualification: ProbeQualification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ProbeKind {
    Ieee1284DeviceId,
    BrotherRasterStatus,
    BrotherSystemReport,
    BrotherWirelessStatus,
    PjlInfo { info: PjlInfoKind },
    PjlDinquire { variable: RegisteredPjlVariable },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PjlInfoKind {
    Id,
    Status,
    Config,
    Variables,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RegisteredPjlVariable(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeRisk {
    ReadOnly,
    BenignStateChange,
    ConfigurationWrite,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeLimits {
    pub timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub maximum_response_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeQualification {
    pub qualification_id: String,
    pub manufacturers: Vec<String>,
    pub models: Vec<String>,
    pub firmware_versions: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ProbeRegistry {
    definitions: BTreeMap<ProbeId, ProbeDefinition>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RegistryError {
    #[error("probe ID is empty")]
    EmptyId,
    #[error("probe definition has no protocol or transport")]
    MissingApplicability,
    #[error("probe limits must be positive and idle timeout cannot exceed total timeout")]
    InvalidLimits,
    #[error("automatic probes must be qualified read-only operations")]
    UnsafeAutomaticProbe,
    #[error("probe ID is already registered: {0}")]
    Duplicate(String),
    #[error("registered PJL variable contains unsafe characters")]
    InvalidPjlVariable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ProbeRequest {
    /// The transport performs the standard IEEE 1284 GET_DEVICE_ID request.
    Ieee1284DeviceId,
    /// A reviewed protocol transaction containing any required delays and
    /// response boundaries.
    ProtocolPlan { plan: Plan },
    /// A single reviewed command. Bytes are produced by this module and can
    /// never be supplied by a registry definition or remote caller.
    Command { bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum ProbeResponse {
    Ieee1284DeviceId(protocol::ieee1284::DeviceId),
    BrotherRasterStatus(protocol::BrotherStatus),
    BrotherSystemReport(protocol::brother::report::SystemReport),
    BrotherWirelessStatus(bool),
    PjlText(String),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProbeCodecError {
    #[error("unknown registered probe: {0}")]
    UnknownProbe(String),
    #[error("probe response exceeds its registered byte limit")]
    ResponseTooLarge,
    #[error("Brother raster status requires a qualified printer definition")]
    MissingPrinterDefinition,
    #[error("probe response is malformed: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeExecutionReport {
    pub probe_id: ProbeId,
    pub qualification_id: String,
    pub printer_id: String,
    pub endpoint: String,
    pub endpoint_generation: u64,
    pub transport: TransportKind,
    pub protocol: ProtocolFamily,
    pub duration_ms: u64,
    pub limits: ProbeLimits,
    pub response_bytes: usize,
    pub response_hash: String,
    pub configuration_changed: bool,
    pub result: ProbeResponse,
    pub origin: ObservationOrigin,
}

impl ProbeRegistry {
    pub fn register(&mut self, definition: ProbeDefinition) -> Result<(), RegistryError> {
        if definition.id.0.is_empty() {
            return Err(RegistryError::EmptyId);
        }
        if definition.protocols.is_empty() || definition.transports.is_empty() {
            return Err(RegistryError::MissingApplicability);
        }
        if definition.limits.timeout_ms == 0
            || definition.limits.idle_timeout_ms > definition.limits.timeout_ms
            || definition.limits.maximum_response_bytes == 0
        {
            return Err(RegistryError::InvalidLimits);
        }
        if definition.risk != ProbeRisk::ReadOnly
            || definition.qualification.qualification_id.is_empty()
        {
            return Err(RegistryError::UnsafeAutomaticProbe);
        }
        if let ProbeKind::PjlDinquire { variable } = &definition.kind
            && !valid_pjl_identifier(&variable.0)
        {
            return Err(RegistryError::InvalidPjlVariable);
        }
        if self.definitions.contains_key(&definition.id) {
            return Err(RegistryError::Duplicate(definition.id.0));
        }
        self.definitions.insert(definition.id.clone(), definition);
        Ok(())
    }

    pub fn get(&self, id: &ProbeId) -> Option<&ProbeDefinition> {
        self.definitions.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ProbeId, &ProbeDefinition)> {
        self.definitions.iter()
    }
}

/// Resolve a registered kind to protocol-owned request encoding.
pub fn prepare_registered_probe(
    registry: &ProbeRegistry,
    id: &ProbeId,
    printer: Option<&PrinterDefinition>,
) -> Result<ProbeRequest, ProbeCodecError> {
    let definition = registry
        .get(id)
        .ok_or_else(|| ProbeCodecError::UnknownProbe(id.0.clone()))?;
    match &definition.kind {
        ProbeKind::Ieee1284DeviceId => Ok(ProbeRequest::Ieee1284DeviceId),
        ProbeKind::BrotherRasterStatus => {
            let printer = printer.ok_or(ProbeCodecError::MissingPrinterDefinition)?;
            Ok(ProbeRequest::ProtocolPlan {
                plan: protocol::brother::status::plan(printer),
            })
        }
        ProbeKind::BrotherSystemReport => Ok(ProbeRequest::ProtocolPlan {
            plan: protocol::brother::report::system_report_plan(),
        }),
        ProbeKind::BrotherWirelessStatus => Ok(ProbeRequest::Command {
            bytes: protocol::brother::wifi::wifi_status_command(),
        }),
        ProbeKind::PjlInfo { info } => Ok(ProbeRequest::Command {
            bytes: pjl_info_command(*info),
        }),
        ProbeKind::PjlDinquire { variable } => Ok(ProbeRequest::Command {
            bytes: pjl_dinquire_command(&variable.0),
        }),
    }
}

/// Decode with the concrete protocol decoder selected by the registered kind.
pub fn decode_registered_response(
    registry: &ProbeRegistry,
    id: &ProbeId,
    response: &[u8],
) -> Result<ProbeResponse, ProbeCodecError> {
    let definition = registry
        .get(id)
        .ok_or_else(|| ProbeCodecError::UnknownProbe(id.0.clone()))?;
    if response.len() > definition.limits.maximum_response_bytes {
        return Err(ProbeCodecError::ResponseTooLarge);
    }
    match definition.kind {
        ProbeKind::Ieee1284DeviceId => protocol::ieee1284::parse_device_id(response)
            .map(ProbeResponse::Ieee1284DeviceId)
            .map_err(|error| ProbeCodecError::Malformed(error.to_string())),
        ProbeKind::BrotherRasterStatus => protocol::brother_parse_status(response)
            .map(ProbeResponse::BrotherRasterStatus)
            .map_err(|error| ProbeCodecError::Malformed(error.into())),
        ProbeKind::BrotherSystemReport => protocol::brother::report::parse_system_report(response)
            .map(ProbeResponse::BrotherSystemReport)
            .map_err(|error| ProbeCodecError::Malformed(error.to_string())),
        ProbeKind::BrotherWirelessStatus => protocol::brother::wifi::parse_wifi_status(response)
            .map(ProbeResponse::BrotherWirelessStatus)
            .ok_or_else(|| ProbeCodecError::Malformed("invalid wireless status".into())),
        ProbeKind::PjlInfo { .. } | ProbeKind::PjlDinquire { .. } => {
            decode_pjl_text(response).map(ProbeResponse::PjlText)
        }
    }
}

pub fn build_read_only_probe_report(
    registry: &ProbeRegistry,
    id: &ProbeId,
    response: &[u8],
    mut origin: ObservationOrigin,
    duration_ms: u64,
) -> Result<ProbeExecutionReport, ProbeCodecError> {
    let definition = registry
        .get(id)
        .ok_or_else(|| ProbeCodecError::UnknownProbe(id.0.clone()))?;
    let result = redact_probe_response(decode_registered_response(registry, id, response)?);
    let response_hash = hex_sha256(response);
    origin.probe_id = Some(id.0.clone());
    origin.qualification = Some(QualificationMetadata {
        qualification_id: definition.qualification.qualification_id.clone(),
        firmware: origin
            .qualification
            .as_ref()
            .and_then(|qualification| qualification.firmware.clone()),
        response_hash: Some(response_hash.clone()),
    });
    Ok(ProbeExecutionReport {
        probe_id: id.clone(),
        qualification_id: definition.qualification.qualification_id.clone(),
        printer_id: origin.printer_id.clone(),
        endpoint: origin.endpoint.clone(),
        endpoint_generation: origin.endpoint_generation,
        transport: origin.transport,
        protocol: origin.protocol,
        duration_ms,
        limits: definition.limits,
        response_bytes: response.len(),
        response_hash,
        configuration_changed: false,
        result,
        origin,
    })
}

fn redact_probe_response(response: ProbeResponse) -> ProbeResponse {
    match response {
        ProbeResponse::Ieee1284DeviceId(mut value) => {
            value.raw = "[REDACTED]".into();
            value.fields.retain(|key, _| {
                !["SERIALNUMBER", "SERN", "SN", "MAC", "MACADDRESS"].contains(&key.as_str())
            });
            ProbeResponse::Ieee1284DeviceId(value)
        }
        ProbeResponse::BrotherSystemReport(value) => {
            ProbeResponse::BrotherSystemReport(value.redacted())
        }
        ProbeResponse::PjlText(_) => ProbeResponse::PjlText("[REDACTED]".into()),
        value => value,
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

pub fn brother_read_only_registry() -> ProbeRegistry {
    let mut registry = ProbeRegistry::default();
    for (id, kind, protocol, transports, maximum_response_bytes) in [
        (
            "ieee1284.device-id.v1",
            ProbeKind::Ieee1284DeviceId,
            ProtocolFamily::Ieee1284,
            vec![TransportKind::Usb, TransportKind::IppOverUsb],
            protocol::ieee1284::MAX_DEVICE_ID_BYTES,
        ),
        (
            "brother.raster-status.v1",
            ProbeKind::BrotherRasterStatus,
            ProtocolFamily::Brother,
            vec![TransportKind::Usb, TransportKind::RawTcp],
            32,
        ),
        (
            "brother.system-report.v1",
            ProbeKind::BrotherSystemReport,
            ProtocolFamily::Brother,
            vec![TransportKind::Usb, TransportKind::RawTcp],
            protocol::brother::report::MAX_SYSTEM_REPORT_BYTES,
        ),
        (
            "brother.wireless-status.v1",
            ProbeKind::BrotherWirelessStatus,
            ProtocolFamily::Brother,
            vec![TransportKind::Usb, TransportKind::RawTcp],
            4 * 1024,
        ),
    ] {
        registry
            .register(ProbeDefinition {
                id: ProbeId(id.into()),
                kind,
                protocols: vec![protocol],
                transports,
                risk: ProbeRisk::ReadOnly,
                limits: ProbeLimits {
                    timeout_ms: 5_000,
                    idle_timeout_ms: 300,
                    maximum_response_bytes,
                },
                qualification: ProbeQualification {
                    qualification_id: format!("{id}.qualified"),
                    manufacturers: vec!["Brother".into()],
                    models: Vec::new(),
                    firmware_versions: Vec::new(),
                },
            })
            .expect("built-in probe definitions are valid");
    }
    for info in [
        PjlInfoKind::Id,
        PjlInfoKind::Status,
        PjlInfoKind::Config,
        PjlInfoKind::Variables,
    ] {
        let suffix = match info {
            PjlInfoKind::Id => "id",
            PjlInfoKind::Status => "status",
            PjlInfoKind::Config => "config",
            PjlInfoKind::Variables => "variables",
        };
        let id = format!("pjl.info-{suffix}.v1");
        registry
            .register(ProbeDefinition {
                id: ProbeId(id.clone()),
                kind: ProbeKind::PjlInfo { info },
                protocols: vec![ProtocolFamily::Pjl],
                transports: vec![TransportKind::Usb, TransportKind::RawTcp],
                risk: ProbeRisk::ReadOnly,
                limits: ProbeLimits {
                    timeout_ms: 5_000,
                    idle_timeout_ms: 300,
                    maximum_response_bytes: 64 * 1024,
                },
                qualification: ProbeQualification {
                    qualification_id: format!("{id}.qualified"),
                    manufacturers: vec!["Brother".into()],
                    models: Vec::new(),
                    firmware_versions: Vec::new(),
                },
            })
            .expect("built-in PJL probe definitions are valid");
    }
    registry
}

fn pjl_info_command(info: PjlInfoKind) -> Vec<u8> {
    let name = match info {
        PjlInfoKind::Id => "ID",
        PjlInfoKind::Status => "STATUS",
        PjlInfoKind::Config => "CONFIG",
        PjlInfoKind::Variables => "VARIABLES",
    };
    format!("\x1b%-12345X@PJL\r\n@PJL INFO {name}\r\n\x1b%-12345X").into_bytes()
}

fn pjl_dinquire_command(variable: &str) -> Vec<u8> {
    debug_assert!(valid_pjl_identifier(variable));
    format!("\x1b%-12345X@PJL\r\n@PJL DINQUIRE {variable}\r\n\x1b%-12345X").into_bytes()
}

fn valid_pjl_identifier(variable: &str) -> bool {
    !variable.is_empty()
        && variable.len() <= 64
        && variable
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn decode_pjl_text(response: &[u8]) -> Result<String, ProbeCodecError> {
    let text = std::str::from_utf8(response)
        .map_err(|_| ProbeCodecError::Malformed("PJL response is not UTF-8".into()))?;
    let text = text.trim_matches(|character: char| character == '\0' || character.is_whitespace());
    if text.is_empty() {
        return Err(ProbeCodecError::Malformed("empty PJL response".into()));
    }
    Ok(text.into())
}

impl ProbeDefinition {
    pub fn applies_to(
        &self,
        protocol: ProtocolFamily,
        transport: TransportKind,
        manufacturer: Option<&str>,
        model: Option<&str>,
        firmware: Option<&str>,
    ) -> bool {
        self.risk == ProbeRisk::ReadOnly
            && self.protocols.contains(&protocol)
            && self.transports.contains(&transport)
            && matches_allowlist(&self.qualification.manufacturers, manufacturer)
            && matches_allowlist(&self.qualification.models, model)
            && matches_allowlist(&self.qualification.firmware_versions, firmware)
    }
}

fn matches_allowlist(allowlist: &[String], actual: Option<&str>) -> bool {
    allowlist.is_empty()
        || actual.is_some_and(|actual| {
            allowlist
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(actual))
        })
}
