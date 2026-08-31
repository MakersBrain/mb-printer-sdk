// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fail-closed planning and stale-plan validation for printer changes.

use crate::{
    discovery::{ProtocolFamily, SettingValue},
    ipp::{self, Attribute, Message, Value, ValueData, ValueTag},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedChangePlan {
    pub printer_id: String,
    pub endpoint_generation: u64,
    pub setting: String,
    pub expected_old_value_hash: [u8; 32],
    pub requested_value: SettingValue,
    pub requested_protocol_value: Value,
    pub principal: String,
    pub protocol: ProtocolFamily,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeBinding<'a> {
    pub printer_id: &'a str,
    pub endpoint_generation: u64,
    pub principal: &'a str,
    pub protocol: ProtocolFamily,
    pub now_unix_ms: u64,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ChangePlanError {
    #[error("the IPP response was not successful")]
    UnsuccessfulObservation,
    #[error("Set-Printer-Attributes is not advertised")]
    OperationNotAdvertised,
    #[error("printer-settable-attributes-supported is missing or malformed")]
    MissingOrMalformedSettableMetadata,
    #[error("attribute is not explicitly listed as settable: {0}")]
    AttributeNotSettable(String),
    #[error("the current value is not present: {0}")]
    CurrentValueMissing(String),
    #[error("the requested value is incompatible with the current IPP syntax")]
    IncompatibleValue,
    #[error("Get-Printer-Supported-Values is required for a settable xxx-supported attribute")]
    SupportedValuesRequired,
    #[error("the required supported-value constraints are missing or malformed")]
    MissingValueConstraints,
    #[error("the requested value is outside the printer's advertised constraints")]
    RequestedValueNotSupported,
    #[error("the confirmed change has expired")]
    Expired,
    #[error("the confirmed change binding no longer matches: {0}")]
    StaleBinding(&'static str),
    #[error("the current value changed after confirmation")]
    StaleValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedPrinterValues {
    /// Exact settable `xxx-supported` attribute names and their one-set-of
    /// possible values. `admin-define` remains an out-of-band `Value`.
    pub attributes: BTreeMap<String, Vec<Value>>,
    pub unsupported_attributes: Vec<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SupportedValuesError {
    #[error("Get-Printer-Supported-Values response was not successful")]
    Unsuccessful,
    #[error("response included a non-xxx-supported Printer attribute")]
    InvalidAttribute,
    #[error("response included an invalid attribute name")]
    InvalidName,
}

pub fn parse_get_printer_supported_values(
    response: &Message,
) -> Result<SupportedPrinterValues, SupportedValuesError> {
    if response.code >= 0x0100 {
        return Err(SupportedValuesError::Unsuccessful);
    }
    let mut parsed = SupportedPrinterValues {
        attributes: BTreeMap::new(),
        unsupported_attributes: Vec::new(),
    };
    for group in &response.groups {
        for attribute in &group.attributes {
            let name = std::str::from_utf8(&attribute.name)
                .map_err(|_| SupportedValuesError::InvalidName)?
                .to_owned();
            if group.tag == ipp::UNSUPPORTED_ATTRIBUTES_TAG {
                parsed.unsupported_attributes.push(name);
            } else if group.tag == ipp::PRINTER_ATTRIBUTES_TAG {
                if !name.ends_with("-supported") {
                    return Err(SupportedValuesError::InvalidAttribute);
                }
                parsed.attributes.insert(name, attribute.values.clone());
            }
        }
    }
    Ok(parsed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanChangeRequest<'a> {
    pub printer_id: &'a str,
    pub endpoint_generation: u64,
    pub setting: &'a str,
    pub requested_value: Value,
    pub principal: &'a str,
    pub protocol: ProtocolFamily,
    pub expires_at_unix_ms: u64,
}

pub fn plan_ipp_change(
    observation: &Message,
    request: PlanChangeRequest<'_>,
) -> Result<ConfirmedChangePlan, ChangePlanError> {
    plan_ipp_change_with_supported_values(observation, None, request)
}

pub fn plan_ipp_change_with_supported_values(
    observation: &Message,
    supported_values: Option<&SupportedPrinterValues>,
    request: PlanChangeRequest<'_>,
) -> Result<ConfirmedChangePlan, ChangePlanError> {
    if observation.code >= 0x0100 {
        return Err(ChangePlanError::UnsuccessfulObservation);
    }
    if !operation_is_advertised(observation, ipp::SET_PRINTER_ATTRIBUTES) {
        return Err(ChangePlanError::OperationNotAdvertised);
    }
    let settable = settable_attributes(observation)
        .ok_or(ChangePlanError::MissingOrMalformedSettableMetadata)?;
    if !settable.contains(request.setting) {
        return Err(ChangePlanError::AttributeNotSettable(
            request.setting.into(),
        ));
    }
    let current = find_attribute(observation, request.setting.as_bytes())
        .ok_or_else(|| ChangePlanError::CurrentValueMissing(request.setting.into()))?;
    if current.values.is_empty()
        || (!request.setting.ends_with("-supported")
            && !current
                .values
                .iter()
                .all(|value| compatible_syntax(value.tag, request.requested_value.tag)))
    {
        return Err(ChangePlanError::IncompatibleValue);
    }
    validate_requested_value(
        observation,
        supported_values,
        request.setting,
        &request.requested_value,
    )?;
    let requested_value = normalized_setting_value(&request.requested_value)
        .ok_or(ChangePlanError::IncompatibleValue)?;
    Ok(ConfirmedChangePlan {
        printer_id: request.printer_id.into(),
        endpoint_generation: request.endpoint_generation,
        setting: request.setting.into(),
        expected_old_value_hash: attribute_hash(current),
        requested_value,
        requested_protocol_value: request.requested_value,
        principal: request.principal.into(),
        protocol: request.protocol,
        expires_at_unix_ms: request.expires_at_unix_ms,
    })
}

fn validate_requested_value(
    observation: &Message,
    supported_values: Option<&SupportedPrinterValues>,
    setting: &str,
    requested: &Value,
) -> Result<(), ChangePlanError> {
    if setting.ends_with("-supported") {
        if !operation_is_advertised(observation, ipp::GET_PRINTER_SUPPORTED_VALUES) {
            return Err(ChangePlanError::SupportedValuesRequired);
        }
        let supported_values = supported_values.ok_or(ChangePlanError::SupportedValuesRequired)?;
        if supported_values
            .unsupported_attributes
            .iter()
            .any(|name| name == setting)
        {
            return Err(ChangePlanError::RequestedValueNotSupported);
        }
        let allowed = supported_values
            .attributes
            .get(setting)
            .ok_or(ChangePlanError::MissingValueConstraints)?;
        if !value_matches_any(requested, allowed) {
            return Err(ChangePlanError::RequestedValueNotSupported);
        }
    } else if let Some(base) = setting.strip_suffix("-default") {
        let supported_name = format!("{base}-supported");
        let allowed = find_attribute(observation, supported_name.as_bytes())
            .map(|attribute| attribute.values.as_slice())
            .ok_or(ChangePlanError::MissingValueConstraints)?;
        if !value_matches_any(requested, allowed) {
            return Err(ChangePlanError::RequestedValueNotSupported);
        }
    }
    Ok(())
}

fn value_matches_any(requested: &Value, allowed: &[Value]) -> bool {
    allowed
        .iter()
        .any(|candidate| value_matches(requested, candidate))
}

fn value_matches(requested: &Value, allowed: &Value) -> bool {
    if allowed.tag == ValueTag::AdminDefine
        && matches!(
            requested.tag,
            ValueTag::NameWithLanguage | ValueTag::NameWithoutLanguage
        )
    {
        return true;
    }
    if matches!(allowed.data, ValueData::Boolean(true)) {
        return true;
    }
    match (&requested.data, &allowed.data) {
        (ValueData::Integer(value), ValueData::RangeOfInteger { lower, upper }) => {
            value >= lower && value <= upper
        }
        (
            ValueData::RangeOfInteger {
                lower: requested_lower,
                upper: requested_upper,
            },
            ValueData::RangeOfInteger {
                lower: allowed_lower,
                upper: allowed_upper,
            },
        ) => requested_lower >= allowed_lower && requested_upper <= allowed_upper,
        (ValueData::Bytes(uri), ValueData::Bytes(scheme))
            if requested.tag == ValueTag::Uri && allowed.tag == ValueTag::UriScheme =>
        {
            std::str::from_utf8(uri)
                .ok()
                .and_then(|uri| uri.split_once(':').map(|(scheme, _)| scheme))
                .zip(std::str::from_utf8(scheme).ok())
                .is_some_and(|(actual, allowed)| actual.eq_ignore_ascii_case(allowed))
        }
        _ => requested.tag == allowed.tag && requested.data == allowed.data,
    }
}

/// Rebuild a confirmed plan from typed protobuf fields. The caller remains
/// responsible for authenticating the outer request; all authority bindings
/// are retained here for immediate pre-write validation.
pub fn confirmed_ipp_plan_from_wire(
    printer_id: String,
    endpoint_generation: u64,
    setting: String,
    expected_old_value_hash: [u8; 32],
    requested_protocol_value: Value,
    principal: String,
    expires_at_unix_ms: u64,
) -> Result<ConfirmedChangePlan, ChangePlanError> {
    let requested_value = normalized_setting_value(&requested_protocol_value)
        .ok_or(ChangePlanError::IncompatibleValue)?;
    Ok(ConfirmedChangePlan {
        printer_id,
        endpoint_generation,
        setting,
        expected_old_value_hash,
        requested_value,
        requested_protocol_value,
        principal,
        protocol: ProtocolFamily::Ipp,
        expires_at_unix_ms,
    })
}

/// Revalidate every authority binding and the immediately re-read value.
/// Callers must perform this check directly before transmitting a write.
pub fn validate_confirmed_ipp_change(
    plan: &ConfirmedChangePlan,
    current_observation: &Message,
    binding: ChangeBinding<'_>,
) -> Result<(), ChangePlanError> {
    validate_confirmed_ipp_change_with_supported_values(plan, current_observation, None, binding)
}

pub fn validate_confirmed_ipp_change_with_supported_values(
    plan: &ConfirmedChangePlan,
    current_observation: &Message,
    supported_values: Option<&SupportedPrinterValues>,
    binding: ChangeBinding<'_>,
) -> Result<(), ChangePlanError> {
    if binding.now_unix_ms >= plan.expires_at_unix_ms {
        return Err(ChangePlanError::Expired);
    }
    if binding.printer_id != plan.printer_id {
        return Err(ChangePlanError::StaleBinding("printer ID"));
    }
    if binding.endpoint_generation != plan.endpoint_generation {
        return Err(ChangePlanError::StaleBinding("endpoint generation"));
    }
    if binding.principal != plan.principal {
        return Err(ChangePlanError::StaleBinding("principal"));
    }
    if binding.protocol != plan.protocol {
        return Err(ChangePlanError::StaleBinding("protocol"));
    }
    if !operation_is_advertised(current_observation, ipp::SET_PRINTER_ATTRIBUTES) {
        return Err(ChangePlanError::OperationNotAdvertised);
    }
    let settable = settable_attributes(current_observation)
        .ok_or(ChangePlanError::MissingOrMalformedSettableMetadata)?;
    if !settable.contains(&plan.setting) {
        return Err(ChangePlanError::AttributeNotSettable(plan.setting.clone()));
    }
    let current = find_attribute(current_observation, plan.setting.as_bytes())
        .ok_or_else(|| ChangePlanError::CurrentValueMissing(plan.setting.clone()))?;
    if attribute_hash(current) != plan.expected_old_value_hash {
        return Err(ChangePlanError::StaleValue);
    }
    validate_requested_value(
        current_observation,
        supported_values,
        &plan.setting,
        &plan.requested_protocol_value,
    )?;
    Ok(())
}

pub fn set_printer_attributes_request(
    printer_uri: &str,
    setting: &str,
    requested_value: Value,
    request_id: u32,
) -> Message {
    let mut request = ipp::get_printer_attributes_request(
        printer_uri,
        std::iter::empty::<&str>(),
        None,
        request_id,
    );
    request.code = ipp::SET_PRINTER_ATTRIBUTES;
    request.groups.push(ipp::AttributeGroup {
        tag: ipp::PRINTER_ATTRIBUTES_TAG,
        attributes: vec![Attribute::new(setting.as_bytes().to_vec(), requested_value)],
    });
    request
}

pub fn ipp_change_is_applied(plan: &ConfirmedChangePlan, observation: &Message) -> bool {
    find_attribute(observation, plan.setting.as_bytes()).is_some_and(|attribute| {
        attribute.values.len() == 1
            && normalized_setting_value(&attribute.values[0]).as_ref()
                == Some(&plan.requested_value)
    })
}

fn find_attribute<'a>(message: &'a Message, name: &[u8]) -> Option<&'a Attribute> {
    message
        .groups
        .iter()
        .flat_map(|group| group.attributes.iter())
        .find(|attribute| attribute.name == name)
}

fn operation_is_advertised(message: &Message, operation: u16) -> bool {
    find_attribute(message, b"operations-supported").is_some_and(|attribute| {
        attribute.values.iter().any(
            |value| matches!(value.data, ValueData::Enum(value) if value == i32::from(operation)),
        )
    })
}

fn settable_attributes(message: &Message) -> Option<BTreeSet<String>> {
    let attribute = find_attribute(message, b"printer-settable-attributes-supported")?;
    let mut values = BTreeSet::new();
    for value in &attribute.values {
        if value.tag != ValueTag::Keyword {
            return None;
        }
        let ValueData::Bytes(value) = &value.data else {
            return None;
        };
        let value = std::str::from_utf8(value).ok()?;
        if value.is_empty() {
            return None;
        }
        values.insert(value.to_owned());
    }
    (!values.is_empty()).then_some(values)
}

fn compatible_syntax(current: ValueTag, requested: ValueTag) -> bool {
    current == requested
        || matches!(
            (current, requested),
            (ValueTag::TextWithLanguage, ValueTag::TextWithoutLanguage)
                | (ValueTag::TextWithoutLanguage, ValueTag::TextWithLanguage)
                | (ValueTag::NameWithLanguage, ValueTag::NameWithoutLanguage)
                | (ValueTag::NameWithoutLanguage, ValueTag::NameWithLanguage)
        )
}

fn normalized_setting_value(value: &Value) -> Option<SettingValue> {
    match &value.data {
        ValueData::Boolean(value) => Some(SettingValue::Boolean(*value)),
        ValueData::Integer(value) | ValueData::Enum(value) => {
            Some(SettingValue::Integer(i64::from(*value)))
        }
        ValueData::Bytes(bytes) => match value.tag {
            ValueTag::Keyword => std::str::from_utf8(bytes)
                .ok()
                .map(|value| SettingValue::Keyword(value.into())),
            ValueTag::TextWithLanguage
            | ValueTag::TextWithoutLanguage
            | ValueTag::NameWithLanguage
            | ValueTag::NameWithoutLanguage => std::str::from_utf8(bytes)
                .ok()
                .map(|value| SettingValue::Text(value.into())),
            _ => Some(SettingValue::Bytes(bytes.clone())),
        },
        ValueData::RangeOfInteger { lower, upper } => Some(SettingValue::List(vec![
            SettingValue::Integer(i64::from(*lower)),
            SettingValue::Integer(i64::from(*upper)),
        ])),
        ValueData::DateTime(value) => Some(SettingValue::Bytes(value.to_vec())),
        ValueData::Resolution {
            cross_feed,
            feed,
            units,
        } => Some(SettingValue::Text(format!("{cross_feed}x{feed}@{units}"))),
        ValueData::OutOfBand | ValueData::Collection(_) => None,
    }
}

fn attribute_hash(attribute: &Attribute) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((attribute.name.len() as u64).to_be_bytes());
    hasher.update(&attribute.name);
    for value in &attribute.values {
        hasher.update([value.tag.to_byte()]);
        let encoded = serde_json::to_vec(&value.data).expect("IPP value data is serializable");
        hasher.update((encoded.len() as u64).to_be_bytes());
        hasher.update(encoded);
    }
    hasher.finalize().into()
}

/// Stable hash used to bind a cloud confirmation receipt to the exact typed
/// IPP value without echoing that potentially sensitive value in the receipt.
pub fn ipp_value_hash(value: &Value) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([value.tag.to_byte()]);
    let encoded = serde_json::to_vec(&value.data).expect("IPP value data is serializable");
    hasher.update((encoded.len() as u64).to_be_bytes());
    hasher.update(encoded);
    hasher.finalize().into()
}
