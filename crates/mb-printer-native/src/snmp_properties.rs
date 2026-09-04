// SPDX-License-Identifier: AGPL-3.0-or-later
//! Semantic, qualified SNMP property access.

use crate::transports::snmp::{ClientError, ClientLimits, Credentials, SnmpClient};
use mb_printer_core::{
    discovery::{
        Evidence, EvidenceKind, ObservationOrigin, ProtocolFamily, ProtocolSource,
        QualificationMetadata, TransportKind,
    },
    providers::brother::snmp::{FirmwareInventory, mfp_read_catalogue, parse_mfp_inventory},
    snmp::{DeviceQualification, ObjectKey, ObjectRegistry, ObjectSyntax, ObjectValue},
};
use serde::Serialize;
use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

/// A network endpoint bound to the identity and qualification used to select
/// its immutable SNMP catalogue.
#[derive(Clone, Debug)]
pub struct QualifiedPrinter {
    pub printer_id: String,
    pub endpoint: SocketAddr,
    pub endpoint_generation: u64,
    pub manufacturer: String,
    pub model: String,
    pub firmware: Option<String>,
    pub qualification: DeviceQualification,
    pub credentials: Credentials,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyObservation {
    pub property: ObjectKey,
    pub value: PropertyValue,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum PropertyValue {
    Integer(i64),
    Octets(Vec<u8>),
    Text(String),
    Ipv4([u8; 4]),
    ObjectIdentifier(String),
    Counter(u64),
    VendorRecord(String),
}

#[derive(Debug, Error)]
pub enum ReadPropertyError {
    #[error("SNMP property is unsupported for this qualified printer")]
    Unsupported,
    #[error("printer identity does not match the selected SNMP qualification")]
    QualificationMismatch,
    #[error(transparent)]
    Client(#[from] ClientError),
}

pub type FirmwareInspectionError = ReadPropertyError;

#[derive(Debug, Clone)]
pub struct PrinterClient {
    transport: SnmpClient,
    registry: ObjectRegistry,
    limits: ClientLimits,
}

impl PrinterClient {
    pub fn new(registry: ObjectRegistry, limits: ClientLimits) -> Self {
        Self {
            transport: SnmpClient,
            registry,
            limits,
        }
    }

    /// Construct the initial, read-only Brother MFP firmware inventory client.
    pub fn brother_mfp(
        qualification: DeviceQualification,
        limits: ClientLimits,
    ) -> Result<Self, mb_printer_core::snmp::SnmpError> {
        Ok(Self::new(mfp_read_catalogue(qualification)?, limits))
    }

    pub async fn read_property(
        &self,
        printer: &QualifiedPrinter,
        property: &ObjectKey,
    ) -> Result<PropertyObservation, ReadPropertyError> {
        self.check_qualification(printer)?;
        let definition = self
            .registry
            .definition(property)
            .ok_or(ReadPropertyError::Unsupported)?;
        let result = self
            .transport
            .get_many(
                printer.endpoint,
                &self.registry,
                &printer.credentials,
                std::slice::from_ref(&definition.oid),
                self.limits,
            )
            .await?;
        let binding = result
            .varbinds
            .into_iter()
            .next()
            .ok_or(ClientError::Transport)?;
        Ok(PropertyObservation {
            property: property.clone(),
            value: convert_value(&definition.syntax, binding.value)?,
            evidence: vec![evidence(printer, binding.oid.to_string(), &result.evidence)],
        })
    }

    pub async fn inspect_firmware(
        &self,
        printer: &QualifiedPrinter,
    ) -> Result<FirmwareInventory, FirmwareInspectionError> {
        self.check_qualification(printer)?;
        let mut oids = self
            .registry
            .definitions()
            .filter(|definition| {
                definition
                    .key
                    .as_str()
                    .starts_with("brother.firmware.record.")
            })
            .map(|definition| definition.oid.clone())
            .collect::<Vec<_>>();
        oids.sort();
        if oids.is_empty() {
            return Err(ReadPropertyError::Unsupported);
        }
        let result = self
            .transport
            .get_many(
                printer.endpoint,
                &self.registry,
                &printer.credentials,
                &oids,
                self.limits,
            )
            .await?;
        let shared = result.evidence;
        Ok(parse_mfp_inventory(&result.varbinds, |oid| {
            vec![evidence(printer, oid.to_string(), &shared)]
        }))
    }

    fn check_qualification(&self, printer: &QualifiedPrinter) -> Result<(), ReadPropertyError> {
        let selected = self
            .registry
            .definitions()
            .next()
            .ok_or(ReadPropertyError::Unsupported)?;
        let qualification = &selected.qualification;
        let manufacturer_matches =
            normalized(&printer.manufacturer) == normalized(&qualification.manufacturer);
        let model_matches = qualification
            .models
            .iter()
            .any(|model| normalized_model(model) == normalized_model(&printer.model));
        let firmware_matches = qualification.firmware.as_ref().is_none_or(|required| {
            printer
                .firmware
                .as_ref()
                .is_some_and(|actual| actual == required)
        });
        let binding_matches =
            printer.qualification.qualification_id == qualification.qualification_id;
        if manufacturer_matches && model_matches && firmware_matches && binding_matches {
            Ok(())
        } else {
            Err(ReadPropertyError::QualificationMismatch)
        }
    }
}

fn convert_value(
    syntax: &ObjectSyntax,
    value: ObjectValue,
) -> Result<PropertyValue, ReadPropertyError> {
    let malformed = || ClientError::Protocol(mb_printer_core::snmp::SnmpError::Malformed).into();
    match (syntax, value) {
        (ObjectSyntax::Integer, ObjectValue::Integer(value)) => Ok(PropertyValue::Integer(value)),
        (ObjectSyntax::Octets, ObjectValue::Bytes(value)) => Ok(PropertyValue::Octets(value)),
        (ObjectSyntax::Utf8 { trim_trailing_nul }, ObjectValue::Bytes(value)) => {
            let value = String::from_utf8(value).map_err(|_| malformed())?;
            let value = if *trim_trailing_nul {
                value.trim_end_matches('\0').to_owned()
            } else {
                value
            };
            Ok(PropertyValue::Text(value))
        }
        (ObjectSyntax::Ipv4, ObjectValue::IpAddress(value)) => Ok(PropertyValue::Ipv4(value)),
        (ObjectSyntax::ObjectIdentifier, ObjectValue::ObjectId(value)) => {
            Ok(PropertyValue::ObjectIdentifier(value.to_string()))
        }
        (
            ObjectSyntax::Counter,
            ObjectValue::Counter32(value)
            | ObjectValue::Gauge32(value)
            | ObjectValue::Unsigned32(value)
            | ObjectValue::TimeTicks(value),
        ) => Ok(PropertyValue::Counter(u64::from(value))),
        (ObjectSyntax::Counter, ObjectValue::Counter64(value) | ObjectValue::Counter(value)) => {
            Ok(PropertyValue::Counter(value))
        }
        (ObjectSyntax::BrotherFirmwareRecord, ObjectValue::Bytes(value)) => {
            let value = String::from_utf8(value).map_err(|_| malformed())?;
            Ok(PropertyValue::VendorRecord(
                value.trim_end_matches(['\0', '\r', '\n']).to_owned(),
            ))
        }
        (
            _,
            ObjectValue::NoSuchObject | ObjectValue::NoSuchInstance | ObjectValue::EndOfMibView,
        ) => Err(ReadPropertyError::Unsupported),
        _ => Err(malformed()),
    }
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalized_model(value: &str) -> String {
    let value = normalized(value);
    value.strip_prefix("brother").unwrap_or(&value).to_owned()
}

fn evidence(
    printer: &QualifiedPrinter,
    oid: String,
    response: &mb_printer_core::snmp::ResponseEvidence,
) -> Evidence {
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();
    Evidence {
        source: ProtocolSource::SnmpObject { oid },
        kind: EvidenceKind::HardwareQualified {
            qualification_id: printer.qualification.qualification_id.clone(),
        },
        origin: ObservationOrigin {
            agent_id: None,
            printer_id: printer.printer_id.clone(),
            endpoint: printer.endpoint.to_string(),
            endpoint_generation: printer.endpoint_generation,
            transport: TransportKind::Snmp,
            protocol: ProtocolFamily::Snmp,
            request_id: response_hash(response),
            probe_id: None,
            observed_at,
            qualification: Some(QualificationMetadata {
                qualification_id: printer.qualification.qualification_id.clone(),
                firmware: printer.firmware.clone(),
                response_hash: Some(response_hash(response)),
            }),
        },
    }
}

fn response_hash(response: &mb_printer_core::snmp::ResponseEvidence) -> String {
    response
        .credential_elided_hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_aliases_are_normalized_without_weakening_qualification_id() {
        assert_eq!(
            normalized_model("Brother HL-L2375DW"),
            normalized_model("HL-L2375DW")
        );
    }
}
