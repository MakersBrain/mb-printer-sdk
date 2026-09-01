// SPDX-License-Identifier: AGPL-3.0-or-later
//! Protocol-neutral discovery snapshots, evidence, and identity reconciliation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::ipp::{Attribute, Message as IppMessage, Value as IppValue, ValueData, ValueTag};
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    pub identity: PrinterIdentity,
    pub state: DeviceState,
    pub supplies: Vec<Supply>,
    pub job_capabilities: Vec<JobCapability>,
    pub device_settings: Vec<DeviceSetting>,
    pub mutation_support: Vec<MutationSupport>,
    pub operations: Vec<OperationCapability>,
    pub observations: Vec<ProtocolObservation>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrinterIdentity {
    pub printer_id: String,
    pub uuid: Option<String>,
    pub serial_number: Option<String>,
    pub device_id: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceState {
    pub state: Option<String>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Supply {
    pub id: String,
    pub level_percent: Option<u8>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobCapability {
    pub id: String,
    pub current_default: Option<SettingValue>,
    pub supported_values: Option<ValueConstraint>,
    pub format_scope: Option<String>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSetting {
    pub id: String,
    pub current_value: Option<SettingValue>,
    pub sensitive: bool,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationSupport {
    pub setting: String,
    pub access: MutationAccess,
    pub constraints: Option<ValueConstraint>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MutationAccess {
    ReadOnly,
    ConfirmedWrite,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationCapability {
    pub operation: String,
    pub availability: CapabilityAvailability,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityAvailability {
    Advertised,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum SettingValue {
    Boolean(bool),
    Integer(i64),
    Text(String),
    Keyword(String),
    Bytes(Vec<u8>),
    List(Vec<SettingValue>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ValueConstraint {
    Values(Vec<SettingValue>),
    IntegerRange { lower: i64, upper: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolObservation {
    pub source: ProtocolSource,
    pub values: Vec<RawProtocolValue>,
    pub original_bytes: Option<Vec<u8>>,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawProtocolValue {
    pub name: Vec<u8>,
    pub tag: Option<u8>,
    pub value: Vec<u8>,
    pub sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub source: ProtocolSource,
    pub kind: EvidenceKind,
    pub origin: ObservationOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ProtocolSource {
    IppAttribute { name: String },
    DnsSdTxt { key: String },
    Ieee1284DeviceId,
    PjlVariable { name: String },
    SnmpObject { oid: String },
    RegisteredProbe { probe_id: String },
    ModelCatalogue { catalogue: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum EvidenceKind {
    Advertised,
    Observed,
    Inferred,
    HardwareQualified { qualification_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationOrigin {
    pub agent_id: Option<String>,
    pub printer_id: String,
    pub endpoint: String,
    pub endpoint_generation: u64,
    pub transport: TransportKind,
    pub protocol: ProtocolFamily,
    pub request_id: String,
    pub probe_id: Option<String>,
    pub observed_at: String,
    pub qualification: Option<QualificationMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationMetadata {
    pub qualification_id: String,
    pub firmware: Option<String>,
    pub response_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportKind {
    Ipp,
    Ipps,
    DnsSd,
    RawTcp,
    Usb,
    IppOverUsb,
    Serial,
    BluetoothRfcomm,
    Ble,
    Snmp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProtocolFamily {
    Ipp,
    Pjl,
    Ieee1284,
    Brother,
    Snmp,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeReason {
    MatchingUuid,
    CompatibleSerialOrDeviceId,
    ExplicitUserAssociation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeDecision {
    Merge(MergeReason),
    IdentityConflict,
    RequiresUserAssociation,
}

/// Decide whether two endpoint observations may represent one printer. Network
/// addresses, service names, and model names are intentionally not inputs.
pub fn reconcile_identity(
    left: &PrinterIdentity,
    right: &PrinterIdentity,
    explicitly_associated: bool,
) -> MergeDecision {
    if explicitly_associated {
        return MergeDecision::Merge(MergeReason::ExplicitUserAssociation);
    }
    if conflicting(&left.uuid, &right.uuid)
        || conflicting(&left.serial_number, &right.serial_number)
        || conflicting(&left.device_id, &right.device_id)
        || conflicting(&left.manufacturer, &right.manufacturer)
    {
        return MergeDecision::IdentityConflict;
    }
    if matches_nonempty(&left.uuid, &right.uuid) {
        return MergeDecision::Merge(MergeReason::MatchingUuid);
    }
    if matches_nonempty(&left.serial_number, &right.serial_number)
        || matches_nonempty(&left.device_id, &right.device_id)
    {
        return MergeDecision::Merge(MergeReason::CompatibleSerialOrDeviceId);
    }
    MergeDecision::RequiresUserAssociation
}

fn matches_nonempty(left: &Option<String>, right: &Option<String>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if !left.is_empty() && left == right)
}

fn conflicting(left: &Option<String>, right: &Option<String>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if !left.is_empty() && !right.is_empty() && left != right)
}

/// Normalize one IPP response without discarding its protocol representation.
/// The optional format applies only to this observation and prevents
/// format-qualified results from overwriting base capabilities.
pub fn normalize_ipp(
    message: &IppMessage,
    origin: &ObservationOrigin,
    format: Option<&str>,
) -> DeviceSnapshot {
    let attributes = message
        .groups
        .iter()
        .flat_map(|group| group.attributes.iter())
        .collect::<Vec<_>>();
    let mut snapshot = DeviceSnapshot {
        identity: PrinterIdentity {
            printer_id: origin.printer_id.clone(),
            ..PrinterIdentity::default()
        },
        ..DeviceSnapshot::default()
    };

    snapshot.identity.uuid = first_text(&attributes, b"printer-uuid");
    snapshot.identity.serial_number = first_text(&attributes, b"printer-serial-number");
    snapshot.identity.device_id = first_text(&attributes, b"printer-device-id");
    snapshot.identity.model = first_text(&attributes, b"printer-make-and-model");
    snapshot.state.state = first_value(&attributes, b"printer-state").and_then(ipp_printer_state);
    snapshot.state.reasons = values(&attributes, b"printer-state-reasons")
        .filter_map(value_text)
        .collect();

    let marker_names = values(&attributes, b"marker-names")
        .filter_map(value_text)
        .collect::<Vec<_>>();
    let marker_levels = values(&attributes, b"marker-levels")
        .map(|value| value_i64(value).and_then(|level| u8::try_from(level).ok()))
        .collect::<Vec<_>>();
    for (index, name) in marker_names.into_iter().enumerate() {
        let source = ProtocolSource::IppAttribute {
            name: "marker-levels".into(),
        };
        snapshot.supplies.push(Supply {
            id: name,
            level_percent: marker_levels
                .get(index)
                .copied()
                .flatten()
                .filter(|level| *level <= 100),
            evidence: vec![evidence(source, origin)],
        });
    }

    let operation_values = values(&attributes, b"operations-supported")
        .filter_map(value_i64)
        .collect::<BTreeSet<_>>();
    for operation in &operation_values {
        let source = ProtocolSource::IppAttribute {
            name: "operations-supported".into(),
        };
        snapshot.operations.push(OperationCapability {
            operation: ipp_operation_name(*operation),
            availability: CapabilityAvailability::Advertised,
            evidence: vec![evidence(source, origin)],
        });
    }

    let settable = valid_settable_attributes(&attributes);
    let persistent_write_advertised =
        operation_values.contains(&i64::from(crate::ipp::SET_PRINTER_ATTRIBUTES));

    for base in [
        "copies",
        "sides",
        "orientation-requested",
        "print-quality",
        "printer-resolution",
        "media",
        "media-col",
        "print-color-mode",
        "print-scaling",
        "document-format",
        "finishings",
    ] {
        let default_name = format!("{base}-default");
        let supported_name = format!("{base}-supported");
        let current_default =
            first_value(&attributes, default_name.as_bytes()).and_then(setting_value);
        let supported = values(&attributes, supported_name.as_bytes())
            .filter_map(setting_value)
            .collect::<Vec<_>>();
        if current_default.is_none() && supported.is_empty() {
            continue;
        }
        let evidence_name = if !supported.is_empty() {
            supported_name
        } else {
            default_name
        };
        snapshot.job_capabilities.push(JobCapability {
            id: base.into(),
            current_default,
            supported_values: (!supported.is_empty()).then_some(ValueConstraint::Values(supported)),
            format_scope: format.map(str::to_owned),
            evidence: vec![evidence(
                ProtocolSource::IppAttribute {
                    name: evidence_name,
                },
                origin,
            )],
        });
    }

    let media_ready = values(&attributes, b"media-ready")
        .filter_map(setting_value)
        .collect::<Vec<_>>();
    if !media_ready.is_empty() {
        snapshot.job_capabilities.push(JobCapability {
            id: "media-ready".into(),
            current_default: None,
            supported_values: Some(ValueConstraint::Values(media_ready)),
            format_scope: format.map(str::to_owned),
            evidence: vec![evidence(
                ProtocolSource::IppAttribute {
                    name: "media-ready".into(),
                },
                origin,
            )],
        });
    }

    for name in ["printer-name", "printer-info", "printer-location"] {
        let Some(value) = first_value(&attributes, name.as_bytes()).and_then(setting_value) else {
            continue;
        };
        let source = ProtocolSource::IppAttribute { name: name.into() };
        let attribute_is_settable = persistent_write_advertised
            && settable
                .as_ref()
                .is_some_and(|attributes| attributes.contains(name));
        snapshot.device_settings.push(DeviceSetting {
            id: name.into(),
            current_value: Some(value),
            sensitive: is_sensitive_ipp_attribute(name.as_bytes()),
            evidence: vec![evidence(source.clone(), origin)],
        });
        snapshot.mutation_support.push(MutationSupport {
            setting: name.into(),
            access: if attribute_is_settable {
                MutationAccess::ConfirmedWrite
            } else {
                MutationAccess::ReadOnly
            },
            constraints: None,
            evidence: vec![evidence(source, origin)],
        });
    }

    for name in [
        "printer-firmware-name",
        "printer-firmware-string-version",
        "printer-firmware-version",
        "printer-up-time",
        "printer-config-change-time",
        "uri-authentication-supported",
        "uri-security-supported",
        "ipp-features-supported",
    ] {
        let observed = values(&attributes, name.as_bytes())
            .filter_map(setting_value)
            .collect::<Vec<_>>();
        if observed.is_empty() {
            continue;
        }
        let source = ProtocolSource::IppAttribute { name: name.into() };
        snapshot.device_settings.push(DeviceSetting {
            id: name.into(),
            current_value: Some(if observed.len() == 1 {
                observed[0].clone()
            } else {
                SettingValue::List(observed)
            }),
            sensitive: is_sensitive_ipp_attribute(name.as_bytes()),
            evidence: vec![evidence(source.clone(), origin)],
        });
        snapshot.mutation_support.push(MutationSupport {
            setting: name.into(),
            access: MutationAccess::ReadOnly,
            constraints: None,
            evidence: vec![evidence(source, origin)],
        });
    }

    for (index, attribute) in attributes.into_iter().enumerate() {
        let name = String::from_utf8_lossy(&attribute.name).into_owned();
        let source = ProtocolSource::IppAttribute { name };
        snapshot.observations.push(ProtocolObservation {
            source: source.clone(),
            values: attribute
                .values
                .iter()
                .map(|value| RawProtocolValue {
                    name: attribute.name.clone(),
                    tag: Some(value.tag.to_byte()),
                    value: scalar_wire_bytes(value),
                    sensitive: is_sensitive_ipp_attribute(&attribute.name),
                })
                .collect(),
            // The response bytes are response-level evidence. Retain one bounded
            // copy instead of duplicating the complete message per attribute.
            original_bytes: (index == 0).then(|| message.original_bytes.clone()),
            evidence: evidence(source, origin),
        });
    }
    snapshot
}

fn ipp_printer_state(value: &IppValue) -> Option<String> {
    match value.data {
        ValueData::Enum(3) => Some("idle".into()),
        ValueData::Enum(4) => Some("processing".into()),
        ValueData::Enum(5) => Some("stopped".into()),
        ValueData::Enum(value) => Some(value.to_string()),
        _ => setting_value(value).map(|value| format_setting(&value)),
    }
}

/// Normalize only registered SNMP objects. Unknown walk results never become
/// semantic settings, while their bounded protocol observations remain
/// available to an explicitly raw local caller.
pub fn normalize_snmp(
    response: &crate::snmp::Response,
    registry: &crate::snmp::ObjectRegistry,
    origin: &ObservationOrigin,
) -> DeviceSnapshot {
    let mut snapshot = DeviceSnapshot {
        identity: PrinterIdentity {
            printer_id: origin.printer_id.clone(),
            ..PrinterIdentity::default()
        },
        ..DeviceSnapshot::default()
    };
    for (index, binding) in response.varbinds.iter().enumerate() {
        let source = ProtocolSource::SnmpObject {
            oid: binding.oid.to_string(),
        };
        let evidence = Evidence {
            source: source.clone(),
            kind: EvidenceKind::Observed,
            origin: origin.clone(),
        };
        if let Some(object) = registry.get(&binding.oid)
            && let Some(value) = snmp_setting_value(&binding.value)
        {
            snapshot.device_settings.push(DeviceSetting {
                id: object.semantic_id.clone(),
                current_value: Some(value),
                sensitive: object.sensitive,
                evidence: vec![evidence.clone()],
            });
            snapshot.mutation_support.push(MutationSupport {
                setting: object.semantic_id.clone(),
                access: MutationAccess::ReadOnly,
                constraints: None,
                evidence: vec![evidence.clone()],
            });
        }
        snapshot.observations.push(ProtocolObservation {
            source,
            values: vec![RawProtocolValue {
                name: binding.oid.to_string().into_bytes(),
                tag: Some(snmp_value_tag(&binding.value)),
                value: serde_json::to_vec(&binding.value)
                    .expect("SNMP value representation is serializable"),
                sensitive: registry
                    .get(&binding.oid)
                    .is_some_and(|object| object.sensitive),
            }],
            original_bytes: (index == 0).then(|| response.original_bytes.clone()),
            evidence,
        });
    }
    snapshot
}

fn snmp_setting_value(value: &crate::snmp::ObjectValue) -> Option<SettingValue> {
    use crate::snmp::ObjectValue;
    match value {
        ObjectValue::Integer(value) => Some(SettingValue::Integer(*value)),
        ObjectValue::Bytes(value) => std::str::from_utf8(value)
            .ok()
            .map(|value| SettingValue::Text(value.trim_end_matches('\0').into()))
            .or_else(|| Some(SettingValue::Bytes(value.clone()))),
        ObjectValue::ObjectId(value) => Some(SettingValue::Text(value.to_string())),
        ObjectValue::IpAddress(value) => Some(SettingValue::Text(format!(
            "{}.{}.{}.{}",
            value[0], value[1], value[2], value[3]
        ))),
        ObjectValue::Counter(value) => i64::try_from(*value).ok().map(SettingValue::Integer),
        ObjectValue::NoSuchObject
        | ObjectValue::NoSuchInstance
        | ObjectValue::EndOfMibView
        | ObjectValue::Unknown { .. } => None,
    }
}

fn snmp_value_tag(value: &crate::snmp::ObjectValue) -> u8 {
    use crate::snmp::ObjectValue;
    match value {
        ObjectValue::Integer(_) => 0x02,
        ObjectValue::Bytes(_) => 0x04,
        ObjectValue::ObjectId(_) => 0x06,
        ObjectValue::IpAddress(_) => 0x40,
        ObjectValue::Counter(_) => 0x41,
        ObjectValue::NoSuchObject => 0x80,
        ObjectValue::NoSuchInstance => 0x81,
        ObjectValue::EndOfMibView => 0x82,
        ObjectValue::Unknown { tag, .. } => *tag,
    }
}

fn evidence(source: ProtocolSource, origin: &ObservationOrigin) -> Evidence {
    Evidence {
        source,
        kind: EvidenceKind::Advertised,
        origin: origin.clone(),
    }
}

fn values<'a>(attributes: &'a [&Attribute], name: &'a [u8]) -> impl Iterator<Item = &'a IppValue> {
    attributes
        .iter()
        .filter(move |attribute| attribute.name == name)
        .flat_map(|attribute| attribute.values.iter())
}

fn first_value<'a>(attributes: &'a [&Attribute], name: &[u8]) -> Option<&'a IppValue> {
    attributes
        .iter()
        .find(|attribute| attribute.name == name)
        .and_then(|attribute| attribute.values.first())
}

fn first_text(attributes: &[&Attribute], name: &[u8]) -> Option<String> {
    first_value(attributes, name).and_then(value_text)
}

fn value_text(value: &IppValue) -> Option<String> {
    let ValueData::Bytes(bytes) = &value.data else {
        return None;
    };
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

fn value_i64(value: &IppValue) -> Option<i64> {
    match value.data {
        ValueData::Integer(value) | ValueData::Enum(value) => Some(i64::from(value)),
        _ => None,
    }
}

fn setting_value(value: &IppValue) -> Option<SettingValue> {
    match &value.data {
        ValueData::OutOfBand => None,
        ValueData::Integer(value) | ValueData::Enum(value) => {
            Some(SettingValue::Integer(i64::from(*value)))
        }
        ValueData::Boolean(value) => Some(SettingValue::Boolean(*value)),
        ValueData::Bytes(bytes) => match value.tag {
            ValueTag::Keyword => std::str::from_utf8(bytes)
                .ok()
                .map(|value| SettingValue::Keyword(value.into())),
            ValueTag::TextWithLanguage
            | ValueTag::NameWithLanguage
            | ValueTag::TextWithoutLanguage
            | ValueTag::NameWithoutLanguage
            | ValueTag::Uri
            | ValueTag::UriScheme
            | ValueTag::Charset
            | ValueTag::NaturalLanguage
            | ValueTag::MimeMediaType => std::str::from_utf8(bytes)
                .ok()
                .map(|value| SettingValue::Text(value.into())),
            _ => Some(SettingValue::Bytes(bytes.clone())),
        },
        ValueData::RangeOfInteger { lower, upper } => Some(SettingValue::List(vec![
            SettingValue::Integer(i64::from(*lower)),
            SettingValue::Integer(i64::from(*upper)),
        ])),
        ValueData::Resolution {
            cross_feed,
            feed,
            units,
        } => Some(SettingValue::Text(format!("{cross_feed}x{feed}@{units}"))),
        ValueData::DateTime(bytes) => Some(SettingValue::Bytes(bytes.to_vec())),
        ValueData::Collection(_) => None,
    }
}

fn format_setting(value: &SettingValue) -> String {
    match value {
        SettingValue::Boolean(value) => value.to_string(),
        SettingValue::Integer(value) => value.to_string(),
        SettingValue::Text(value) | SettingValue::Keyword(value) => value.clone(),
        SettingValue::Bytes(_) => "bytes".into(),
        SettingValue::List(_) => "list".into(),
    }
}

fn valid_settable_attributes(attributes: &[&Attribute]) -> Option<BTreeSet<String>> {
    let attribute = attributes
        .iter()
        .find(|attribute| attribute.name == b"printer-settable-attributes-supported")?;
    let mut result = BTreeSet::new();
    for value in &attribute.values {
        if value.tag != ValueTag::Keyword {
            return None;
        }
        let value = value_text(value)?;
        if value.is_empty() {
            return None;
        }
        result.insert(value);
    }
    (!result.is_empty()).then_some(result)
}

fn ipp_operation_name(operation: i64) -> String {
    match u16::try_from(operation).ok() {
        Some(crate::ipp::GET_PRINTER_ATTRIBUTES) => "get-printer-attributes".into(),
        Some(crate::ipp::GET_PRINTER_SUPPORTED_VALUES) => "get-printer-supported-values".into(),
        Some(crate::ipp::SET_PRINTER_ATTRIBUTES) => "set-printer-attributes".into(),
        _ => format!("ipp-operation-0x{operation:04x}"),
    }
}

fn scalar_wire_bytes(value: &IppValue) -> Vec<u8> {
    match &value.data {
        ValueData::OutOfBand | ValueData::Collection(_) => Vec::new(),
        ValueData::Integer(value) | ValueData::Enum(value) => value.to_be_bytes().to_vec(),
        ValueData::Boolean(value) => vec![u8::from(*value)],
        ValueData::DateTime(value) => value.to_vec(),
        ValueData::Resolution {
            cross_feed,
            feed,
            units,
        } => [
            cross_feed.to_be_bytes().as_slice(),
            feed.to_be_bytes().as_slice(),
            &[*units],
        ]
        .concat(),
        ValueData::RangeOfInteger { lower, upper } => {
            [lower.to_be_bytes(), upper.to_be_bytes()].concat()
        }
        ValueData::Bytes(bytes) => bytes.clone(),
    }
}

fn is_sensitive_ipp_attribute(name: &[u8]) -> bool {
    let name = String::from_utf8_lossy(name).to_ascii_lowercase();
    [
        "password",
        "credential",
        "certificate",
        "private-key",
        "printer-uuid",
        "serial-number",
        "device-id",
        "wifi",
        "ssid",
        "address",
    ]
    .iter()
    .any(|needle| name.contains(needle))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMode {
    NormalizedRedacted,
    LocalRawRedacted,
    LocalRawSensitive,
    CloudRawAuthorized,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputAuthorization {
    pub raw_local: bool,
    pub sensitive_values: bool,
    pub cloud_raw: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPolicy {
    pub may_persist: bool,
    pub may_log: bool,
    pub audit_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSnapshot {
    pub snapshot: DeviceSnapshot,
    pub mode: OutputMode,
    pub retention: RetentionPolicy,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OutputPolicyError {
    #[error("raw local output was not authorized")]
    RawLocalNotAuthorized,
    #[error("sensitive-value output was not separately authorized")]
    SensitiveNotAuthorized,
    #[error("raw cloud output requires dedicated authorization")]
    CloudRawNotAuthorized,
}

pub fn prepare_snapshot_output(
    mut snapshot: DeviceSnapshot,
    mode: OutputMode,
    authorization: OutputAuthorization,
) -> Result<PreparedSnapshot, OutputPolicyError> {
    let retention = match mode {
        OutputMode::NormalizedRedacted => {
            redact_sensitive_snapshot(&mut snapshot);
            for observation in &mut snapshot.observations {
                observation.values.clear();
                observation.original_bytes = None;
            }
            RetentionPolicy {
                may_persist: true,
                may_log: true,
                audit_required: false,
            }
        }
        OutputMode::LocalRawRedacted => {
            if !authorization.raw_local {
                return Err(OutputPolicyError::RawLocalNotAuthorized);
            }
            redact_sensitive_snapshot(&mut snapshot);
            let response_contains_sensitive = snapshot
                .observations
                .iter()
                .flat_map(|observation| &observation.values)
                .any(|value| value.sensitive);
            for observation in &mut snapshot.observations {
                for value in &mut observation.values {
                    if value.sensitive {
                        value.value = b"[REDACTED]".to_vec();
                    }
                }
                // Original bytes cover the whole response, not only the
                // observation carrying them. Any sensitive value makes every
                // original-byte copy unsafe for redacted output.
                if response_contains_sensitive {
                    observation.original_bytes = None;
                }
            }
            RetentionPolicy {
                may_persist: true,
                may_log: false,
                audit_required: true,
            }
        }
        OutputMode::LocalRawSensitive => {
            if !authorization.raw_local {
                return Err(OutputPolicyError::RawLocalNotAuthorized);
            }
            if !authorization.sensitive_values {
                return Err(OutputPolicyError::SensitiveNotAuthorized);
            }
            RetentionPolicy {
                may_persist: false,
                may_log: false,
                audit_required: true,
            }
        }
        OutputMode::CloudRawAuthorized => {
            if !authorization.cloud_raw {
                return Err(OutputPolicyError::CloudRawNotAuthorized);
            }
            if !authorization.sensitive_values {
                return Err(OutputPolicyError::SensitiveNotAuthorized);
            }
            RetentionPolicy {
                may_persist: false,
                may_log: false,
                audit_required: true,
            }
        }
    };
    Ok(PreparedSnapshot {
        snapshot,
        mode,
        retention,
    })
}

fn redact_sensitive_snapshot(snapshot: &mut DeviceSnapshot) {
    for value in [
        &mut snapshot.identity.uuid,
        &mut snapshot.identity.serial_number,
        &mut snapshot.identity.device_id,
    ]
    .into_iter()
    .flatten()
    {
        *value = redact_identifier(value);
    }
    for setting in &mut snapshot.device_settings {
        if setting.sensitive {
            setting.current_value = Some(SettingValue::Text("[REDACTED]".into()));
        }
    }
    for evidence in all_evidence_mut(snapshot) {
        evidence.origin.endpoint = redact_identifier(&evidence.origin.endpoint);
    }
}

fn all_evidence_mut(snapshot: &mut DeviceSnapshot) -> Vec<&mut Evidence> {
    let mut evidence = Vec::new();
    for supply in &mut snapshot.supplies {
        evidence.extend(&mut supply.evidence);
    }
    for capability in &mut snapshot.job_capabilities {
        evidence.extend(&mut capability.evidence);
    }
    for setting in &mut snapshot.device_settings {
        evidence.extend(&mut setting.evidence);
    }
    for mutation in &mut snapshot.mutation_support {
        evidence.extend(&mut mutation.evidence);
    }
    for operation in &mut snapshot.operations {
        evidence.extend(&mut operation.evidence);
    }
    for observation in &mut snapshot.observations {
        evidence.push(&mut observation.evidence);
    }
    evidence
}

/// Produce a stable pseudonymous identifier for normalized/redacted output.
/// This is intentionally one-way and does not include a secret or raw prefix.
pub fn redact_identifier(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut encoded = String::with_capacity(7 + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}
