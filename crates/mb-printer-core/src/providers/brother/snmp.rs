// SPDX-License-Identifier: AGPL-3.0-or-later
//! Qualified Brother firmware inventory exposed through registered SNMP objects.

use crate::{
    discovery::Evidence,
    snmp::{
        DeviceQualification, ObjectAccess, ObjectDefinition, ObjectId, ObjectKey, ObjectRegistry,
        ObjectSyntax, ObjectValue, Sensitivity, SnmpError, VarBind,
    },
};
use serde::{Deserialize, Serialize};

pub const FIRMWARE_RECORD_BASE: &str = "1.3.6.1.4.1.2435.2.4.3.99.3.1.6.1.2";
pub const MAX_FIRMWARE_RECORDS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observed<T> {
    pub value: T,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "status", content = "detail")]
pub enum FieldResult<T> {
    Observed(Observed<T>),
    Unsupported,
    Missing,
    Malformed { evidence: Vec<Evidence> },
    Conflict { observations: Vec<Observed<T>> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareInventory {
    pub update_model: FieldResult<String>,
    pub specification: FieldResult<String>,
    pub schema_version: FieldResult<String>,
    pub components: FieldResult<Vec<FirmwareComponent>>,
    pub diagnostic_records: Vec<Observed<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareComponent {
    pub id: String,
    pub version: String,
    pub compatibility_key: FieldResult<String>,
    pub evidence: Vec<Evidence>,
}

pub fn mfp_inventory_catalogue(
    qualification: DeviceQualification,
) -> Result<ObjectRegistry, SnmpError> {
    let mut registry = ObjectRegistry::default();
    for index in 1..=MAX_FIRMWARE_RECORDS {
        registry.register_definition(ObjectDefinition {
            key: ObjectKey::new(format!("brother.firmware.record.{index}"))?,
            oid: ObjectId::parse(&format!("{FIRMWARE_RECORD_BASE}.{index}"))?,
            syntax: ObjectSyntax::BrotherFirmwareRecord,
            sensitivity: Sensitivity::Public,
            access: ObjectAccess::ReadOnly,
            qualification: qualification.clone(),
        })?;
    }
    Ok(registry)
}

/// Frozen, read-only objects shared by the qualified Brother MFP profile.
/// Adding an OID here is a reviewed product change; callers cannot extend it.
pub fn mfp_read_catalogue(qualification: DeviceQualification) -> Result<ObjectRegistry, SnmpError> {
    let mut registry = mfp_inventory_catalogue(qualification.clone())?;
    for (key, oid, syntax, sensitivity) in [
        (
            "printer.serial-number",
            "1.3.6.1.2.1.43.5.1.1.17.1",
            ObjectSyntax::Utf8 {
                trim_trailing_nul: true,
            },
            Sensitivity::Identifier,
        ),
        (
            "printer.system-contact",
            "1.3.6.1.2.1.1.4.0",
            ObjectSyntax::Utf8 {
                trim_trailing_nul: true,
            },
            Sensitivity::Identifier,
        ),
        (
            "printer.system-name",
            "1.3.6.1.2.1.1.5.0",
            ObjectSyntax::Utf8 {
                trim_trailing_nul: true,
            },
            Sensitivity::Identifier,
        ),
        (
            "printer.system-location",
            "1.3.6.1.2.1.1.6.0",
            ObjectSyntax::Utf8 {
                trim_trailing_nul: true,
            },
            Sensitivity::Identifier,
        ),
        (
            "brother.firmware-update.supported",
            "1.3.6.1.4.1.2435.2.4.3.2435.5.101.1.0",
            ObjectSyntax::Integer,
            Sensitivity::Public,
        ),
        (
            "brother.firmware-update.enabled",
            "1.3.6.1.4.1.2435.2.4.3.2435.5.101.2.0",
            ObjectSyntax::Integer,
            Sensitivity::Public,
        ),
        (
            "brother.phoenix.capabilities",
            "1.3.6.1.4.1.2435.2.4.3.2435.5.39.1.0",
            ObjectSyntax::Octets,
            Sensitivity::Public,
        ),
        (
            "brother.firmware-update.keyword-count",
            "1.3.6.1.4.1.2435.2.3.9.4.2.1.5.5.55.1.0",
            ObjectSyntax::Integer,
            Sensitivity::Public,
        ),
    ] {
        registry.register_definition(ObjectDefinition {
            key: ObjectKey::new(key)?,
            oid: ObjectId::parse(oid)?,
            syntax,
            sensitivity,
            access: ObjectAccess::ReadOnly,
            qualification: qualification.clone(),
        })?;
    }
    Ok(registry)
}

pub fn parse_mfp_inventory(
    bindings: &[VarBind],
    evidence_for: impl Fn(&ObjectId) -> Vec<Evidence>,
) -> FirmwareInventory {
    let mut models = Vec::new();
    let mut specifications = Vec::new();
    let mut ids = Vec::new();
    let mut versions = Vec::new();
    let mut malformed_evidence = Vec::new();
    let mut malformed = false;
    let mut diagnostic_records = Vec::new();

    for (offset, binding) in bindings.iter().take(MAX_FIRMWARE_RECORDS).enumerate() {
        let expected_oid = ObjectId::parse(&format!("{FIRMWARE_RECORD_BASE}.{}", offset + 1))
            .expect("the frozen Brother OID is valid");
        if binding.oid != expected_oid {
            malformed = true;
            malformed_evidence.extend(evidence_for(&binding.oid));
            break;
        }
        let bytes = match &binding.value {
            ObjectValue::Bytes(bytes) => bytes,
            ObjectValue::NoSuchObject | ObjectValue::NoSuchInstance | ObjectValue::EndOfMibView => {
                break;
            }
            _ => {
                malformed = true;
                malformed_evidence.extend(evidence_for(&binding.oid));
                break;
            }
        };
        let Ok(record) = std::str::from_utf8(bytes) else {
            malformed = true;
            malformed_evidence.extend(evidence_for(&binding.oid));
            break;
        };
        let record = record.trim_end_matches(['\0', '\r', '\n']);
        let Some((key, raw_value)) = record.split_once('=') else {
            break;
        };
        let value = raw_value.trim().trim_matches('"').to_owned();
        if value.is_empty() {
            malformed = true;
            malformed_evidence.extend(evidence_for(&binding.oid));
            continue;
        }
        let observed = Observed {
            value,
            evidence: evidence_for(&binding.oid),
        };
        match key {
            "MODEL" => models.push(observed),
            "SPEC" => specifications.push(observed),
            "FIRMID" => ids.push(observed),
            "FIRMVER" => versions.push(observed),
            _ => diagnostic_records.push(Observed {
                value: format!("{key}={}", observed.value),
                evidence: observed.evidence,
            }),
        }
    }

    let components = if malformed || ids.len() != versions.len() {
        let mut evidence = malformed_evidence;
        evidence.extend(ids.iter().flat_map(|value| value.evidence.clone()));
        evidence.extend(versions.iter().flat_map(|value| value.evidence.clone()));
        FieldResult::Malformed { evidence }
    } else if ids.is_empty() {
        FieldResult::Missing
    } else {
        FieldResult::Observed(Observed {
            value: ids
                .into_iter()
                .zip(versions)
                .map(|(id, version)| {
                    let mut evidence = id.evidence;
                    evidence.extend(version.evidence);
                    FirmwareComponent {
                        id: id.value,
                        version: version.value,
                        compatibility_key: FieldResult::Unsupported,
                        evidence,
                    }
                })
                .collect(),
            evidence: Vec::new(),
        })
    };

    FirmwareInventory {
        update_model: singular_field(models),
        specification: singular_field(specifications),
        schema_version: FieldResult::Unsupported,
        components,
        diagnostic_records,
    }
}

fn singular_field(mut observations: Vec<Observed<String>>) -> FieldResult<String> {
    let Some(first) = observations.first().cloned() else {
        return FieldResult::Missing;
    };
    if observations
        .iter()
        .all(|observation| observation.value == first.value)
    {
        let mut combined = first;
        combined.evidence = observations
            .drain(..)
            .flat_map(|observation| observation.evidence)
            .collect();
        FieldResult::Observed(combined)
    } else {
        FieldResult::Conflict { observations }
    }
}
