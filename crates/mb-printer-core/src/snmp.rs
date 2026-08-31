// SPDX-License-Identifier: AGPL-3.0-or-later
//! Small bounded SNMPv2c read codec for registered printer-management OIDs.
//! It intentionally implements only GET/GETNEXT requests and RESPONSE values.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub Vec<u32>);

impl ObjectId {
    pub fn parse(value: &str) -> Result<Self, SnmpError> {
        let arcs = value
            .trim_start_matches('.')
            .split('.')
            .map(|arc| arc.parse::<u32>().map_err(|_| SnmpError::InvalidOid))
            .collect::<Result<Vec<_>, _>>()?;
        if arcs.len() < 2 || arcs[0] > 2 || (arcs[0] < 2 && arcs[1] > 39) {
            return Err(SnmpError::InvalidOid);
        }
        Ok(Self(arcs))
    }

    pub fn is_within(&self, root: &Self) -> bool {
        self.0.starts_with(&root.0)
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, arc) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str(".")?;
            }
            write!(formatter, "{arc}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredObject {
    pub oid: ObjectId,
    pub semantic_id: String,
    pub sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectKey(String);

impl ObjectKey {
    pub fn new(value: impl Into<String>) -> Result<Self, SnmpError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            });
        valid
            .then_some(Self(value))
            .ok_or(SnmpError::InvalidObjectKey)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ObjectKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sensitivity {
    Public,
    Identifier,
    Secret,
}

impl Sensitivity {
    pub const fn is_sensitive(self) -> bool {
        !matches!(self, Self::Public)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObjectSyntax {
    Integer,
    Octets,
    Utf8 { trim_trailing_nul: bool },
    Ipv4,
    ObjectIdentifier,
    Counter,
    BrotherFirmwareRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceQualification {
    pub manufacturer: String,
    pub models: Vec<String>,
    pub firmware: Option<String>,
    pub qualification_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValueConstraint {
    IntegerRange { minimum: i64, maximum: i64 },
    OctetLength { minimum: usize, maximum: usize },
    Utf8Length { minimum: usize, maximum: usize },
    Values(Vec<SetValue>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum SetValue {
    Integer(i64),
    Octets(Vec<u8>),
    Text(String),
    IpAddress([u8; 4]),
    ObjectId(ObjectId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriteRisk {
    Low,
    Configuration,
    Connectivity,
    Destructive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verification {
    ReadBackSameObject,
    ReadBack { key: ObjectKey, expected: SetValue },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteDefinition {
    pub constraint: ValueConstraint,
    pub risk: WriteRisk,
    pub verification: Verification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "access")]
pub enum ObjectAccess {
    ReadOnly,
    ConfirmedWrite { definition: WriteDefinition },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDefinition {
    pub key: ObjectKey,
    pub oid: ObjectId,
    pub syntax: ObjectSyntax,
    pub sensitivity: Sensitivity,
    pub access: ObjectAccess,
    pub qualification: DeviceQualification,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectRegistry {
    objects: BTreeMap<ObjectId, RegisteredObject>,
    definitions: BTreeMap<ObjectKey, ObjectDefinition>,
}

impl ObjectRegistry {
    pub fn register(&mut self, object: RegisteredObject) -> Result<(), SnmpError> {
        if object.semantic_id.is_empty() || self.objects.contains_key(&object.oid) {
            return Err(SnmpError::UnregisteredObject);
        }
        self.objects.insert(object.oid.clone(), object);
        Ok(())
    }

    pub fn get(&self, oid: &ObjectId) -> Option<&RegisteredObject> {
        self.objects.get(oid)
    }

    pub fn register_definition(&mut self, definition: ObjectDefinition) -> Result<(), SnmpError> {
        if self.objects.contains_key(&definition.oid)
            || self.definitions.contains_key(&definition.key)
        {
            return Err(SnmpError::DuplicateObject);
        }
        self.objects.insert(
            definition.oid.clone(),
            RegisteredObject {
                oid: definition.oid.clone(),
                semantic_id: definition.key.to_string(),
                sensitive: definition.sensitivity.is_sensitive(),
            },
        );
        self.definitions.insert(definition.key.clone(), definition);
        Ok(())
    }

    pub fn definition(&self, key: &ObjectKey) -> Option<&ObjectDefinition> {
        self.definitions.get(key)
    }

    pub fn definition_for_oid(&self, oid: &ObjectId) -> Option<&ObjectDefinition> {
        self.definitions
            .values()
            .find(|definition| &definition.oid == oid)
    }

    pub fn definitions(&self) -> impl Iterator<Item = &ObjectDefinition> {
        self.definitions.values()
    }

    pub fn permits_root(&self, root: &ObjectId) -> bool {
        self.objects.keys().any(|oid| oid.is_within(root))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    pub maximum_message_bytes: usize,
    pub maximum_varbinds: usize,
    pub maximum_value_bytes: usize,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            maximum_message_bytes: 64 * 1024,
            maximum_varbinds: 64,
            maximum_value_bytes: 16 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum ObjectValue {
    Integer(i64),
    Bytes(Vec<u8>),
    Null,
    ObjectId(ObjectId),
    IpAddress([u8; 4]),
    Counter32(u32),
    Gauge32(u32),
    Unsigned32(u32),
    TimeTicks(u32),
    Opaque(Vec<u8>),
    Nsap(Vec<u8>),
    Counter64(u64),
    /// Compatibility representation retained for older callers.
    Counter(u64),
    NoSuchObject,
    NoSuchInstance,
    EndOfMibView,
    Unknown {
        tag: u8,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VarBind {
    pub oid: ObjectId,
    pub value: ObjectValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub request_id: i32,
    pub error_status: i32,
    pub error_index: i32,
    pub varbinds: Vec<VarBind>,
    pub evidence: ResponseEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseEvidence {
    pub credential_elided_hash: [u8; 32],
    pub original_length: usize,
    pub sanitized_bytes: Option<Vec<u8>>,
}

impl ResponseEvidence {
    pub fn from_structured(varbinds: &[VarBind]) -> Self {
        let bytes = serde_json::to_vec(varbinds).expect("SNMP varbinds are serializable");
        let mut digest = Sha256::new();
        digest.update(b"mb-printer-snmp-structured-evidence-v1\0");
        digest.update(&bytes);
        Self {
            credential_elided_hash: digest.finalize().into(),
            original_length: 0,
            sanitized_bytes: None,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SnmpError {
    #[error("invalid SNMP object identifier")]
    InvalidOid,
    #[error("SNMP object is not registered for reading")]
    UnregisteredObject,
    #[error("SNMP message exceeds a configured limit")]
    LimitExceeded,
    #[error("truncated or malformed SNMP BER")]
    Malformed,
    #[error("SNMP response request ID did not match")]
    RequestIdMismatch,
    #[error("invalid semantic SNMP object key")]
    InvalidObjectKey,
    #[error("duplicate SNMP object definition")]
    DuplicateObject,
    #[error("SNMP value does not satisfy the registered syntax or constraint")]
    InvalidValue,
}

pub fn encode_get(
    registry: &ObjectRegistry,
    community: &[u8],
    request_id: i32,
    oid: &ObjectId,
) -> Result<Vec<u8>, SnmpError> {
    if registry.get(oid).is_none() {
        return Err(SnmpError::UnregisteredObject);
    }
    encode_request(registry, community, request_id, oid, 0xa0)
}

pub fn encode_get_next(
    registry: &ObjectRegistry,
    community: &[u8],
    request_id: i32,
    oid: &ObjectId,
) -> Result<Vec<u8>, SnmpError> {
    encode_request(registry, community, request_id, oid, 0xa1)
}

fn encode_request(
    registry: &ObjectRegistry,
    community: &[u8],
    request_id: i32,
    oid: &ObjectId,
    pdu_tag: u8,
) -> Result<Vec<u8>, SnmpError> {
    if community.is_empty()
        || community.len() > 255
        || (pdu_tag == 0xa1 && !registry.permits_root(oid))
    {
        return Err(SnmpError::UnregisteredObject);
    }
    let varbind = sequence([tlv(0x06, &encode_oid(oid)?), tlv(0x05, &[])].concat());
    let varbinds = sequence(varbind);
    let pdu = tlv(
        pdu_tag,
        &[
            integer_tlv(i64::from(request_id)),
            integer_tlv(0),
            integer_tlv(0),
            varbinds,
        ]
        .concat(),
    );
    Ok(sequence(
        [integer_tlv(1), tlv(0x04, community), pdu].concat(),
    ))
}

pub fn decode_response(
    bytes: &[u8],
    expected_request_id: i32,
    limits: DecodeLimits,
) -> Result<Response, SnmpError> {
    if bytes.len() > limits.maximum_message_bytes {
        return Err(SnmpError::LimitExceeded);
    }
    let mut outer = Cursor::new(bytes).nested(0x30)?;
    let version = outer.integer()?;
    if version != 1 {
        return Err(SnmpError::Malformed);
    }
    outer.value(0x04, 255)?; // community is deliberately discarded
    let mut pdu = outer.nested(0xa2)?;
    outer.finish()?;
    let request_id = i32::try_from(pdu.integer()?).map_err(|_| SnmpError::Malformed)?;
    if request_id != expected_request_id {
        return Err(SnmpError::RequestIdMismatch);
    }
    let error_status = i32::try_from(pdu.integer()?).map_err(|_| SnmpError::Malformed)?;
    let error_index = i32::try_from(pdu.integer()?).map_err(|_| SnmpError::Malformed)?;
    let mut bindings = pdu.nested(0x30)?;
    pdu.finish()?;
    let mut varbinds = Vec::new();
    while !bindings.remaining().is_empty() {
        if varbinds.len() >= limits.maximum_varbinds {
            return Err(SnmpError::LimitExceeded);
        }
        let mut binding = bindings.nested(0x30)?;
        let oid = ObjectId(decode_oid(binding.value(0x06, 256)?)?);
        let (tag, value) = binding.any(limits.maximum_value_bytes)?;
        binding.finish()?;
        varbinds.push(VarBind {
            oid,
            value: decode_value(tag, value)?,
        });
    }
    let sanitized = sanitize_v2c_message(bytes)?;
    let mut digest = Sha256::new();
    digest.update(b"mb-printer-snmp-v2c-evidence-v1\0");
    digest.update(&sanitized);
    Ok(Response {
        request_id,
        error_status,
        error_index,
        varbinds,
        evidence: ResponseEvidence {
            credential_elided_hash: digest.finalize().into(),
            original_length: bytes.len(),
            sanitized_bytes: Some(sanitized),
        },
    })
}

fn sanitize_v2c_message(bytes: &[u8]) -> Result<Vec<u8>, SnmpError> {
    let mut sanitized = bytes.to_vec();
    let mut outer = Cursor::new(bytes).nested(0x30)?;
    outer.integer()?;
    let value_start = outer.offset;
    let community = outer.value(0x04, 255)?;
    let value_end = outer.offset;
    let header_length = value_end
        .checked_sub(value_start)
        .and_then(|length| length.checked_sub(community.len()))
        .ok_or(SnmpError::Malformed)?;
    let outer_header = bytes.len() - outer.bytes.len();
    let start = outer_header + value_start + header_length;
    let end = start
        .checked_add(community.len())
        .ok_or(SnmpError::Malformed)?;
    let target = sanitized.get_mut(start..end).ok_or(SnmpError::Malformed)?;
    target.fill(b'*');
    Ok(sanitized)
}

pub fn validate_set_value(
    definition: &ObjectDefinition,
    value: &SetValue,
) -> Result<(), SnmpError> {
    let syntax_matches = matches!(
        (&definition.syntax, value),
        (ObjectSyntax::Integer, SetValue::Integer(_))
            | (ObjectSyntax::Octets, SetValue::Octets(_))
            | (ObjectSyntax::Utf8 { .. }, SetValue::Text(_))
            | (ObjectSyntax::Ipv4, SetValue::IpAddress(_))
            | (ObjectSyntax::ObjectIdentifier, SetValue::ObjectId(_))
    );
    if !syntax_matches {
        return Err(SnmpError::InvalidValue);
    }
    let ObjectAccess::ConfirmedWrite { definition: write } = &definition.access else {
        return Err(SnmpError::UnregisteredObject);
    };
    let valid = match (&write.constraint, value) {
        (ValueConstraint::IntegerRange { minimum, maximum }, SetValue::Integer(value)) => {
            (minimum..=maximum).contains(&value)
        }
        (ValueConstraint::OctetLength { minimum, maximum }, SetValue::Octets(value)) => {
            (*minimum..=*maximum).contains(&value.len())
        }
        (ValueConstraint::Utf8Length { minimum, maximum }, SetValue::Text(value)) => {
            (*minimum..=*maximum).contains(&value.len())
        }
        (ValueConstraint::Values(values), value) => values.contains(value),
        _ => false,
    };
    valid.then_some(()).ok_or(SnmpError::InvalidValue)
}

fn decode_value(tag: u8, bytes: &[u8]) -> Result<ObjectValue, SnmpError> {
    Ok(match tag {
        0x02 => ObjectValue::Integer(decode_integer(bytes)?),
        0x04 => ObjectValue::Bytes(bytes.to_vec()),
        0x05 if bytes.is_empty() => ObjectValue::Null,
        0x06 => ObjectValue::ObjectId(ObjectId(decode_oid(bytes)?)),
        0x40 if bytes.len() == 4 => {
            ObjectValue::IpAddress(bytes.try_into().map_err(|_| SnmpError::Malformed)?)
        }
        0x41 => ObjectValue::Counter32(
            u32::try_from(decode_unsigned(bytes)?).map_err(|_| SnmpError::Malformed)?,
        ),
        0x42 => ObjectValue::Gauge32(
            u32::try_from(decode_unsigned(bytes)?).map_err(|_| SnmpError::Malformed)?,
        ),
        0x43 => ObjectValue::TimeTicks(
            u32::try_from(decode_unsigned(bytes)?).map_err(|_| SnmpError::Malformed)?,
        ),
        0x44 => ObjectValue::Opaque(bytes.to_vec()),
        0x45 => ObjectValue::Nsap(bytes.to_vec()),
        0x46 => ObjectValue::Counter64(decode_unsigned(bytes)?),
        0x47 => ObjectValue::Unsigned32(
            u32::try_from(decode_unsigned(bytes)?).map_err(|_| SnmpError::Malformed)?,
        ),
        0x80 if bytes.is_empty() => ObjectValue::NoSuchObject,
        0x81 if bytes.is_empty() => ObjectValue::NoSuchInstance,
        0x82 if bytes.is_empty() => ObjectValue::EndOfMibView,
        // Known tags with invalid encodings are malformed, not unknown values.
        0x05 | 0x40 | 0x80..=0x82 => {
            return Err(SnmpError::Malformed);
        }
        _ => ObjectValue::Unknown {
            tag,
            bytes: bytes.to_vec(),
        },
    })
}

fn sequence(content: Vec<u8>) -> Vec<u8> {
    tlv(0x30, &content)
}

fn integer_tlv(value: i64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let mut start = 0;
    while start < 7
        && ((bytes[start] == 0 && bytes[start + 1] & 0x80 == 0)
            || (bytes[start] == 0xff && bytes[start + 1] & 0x80 != 0))
    {
        start += 1;
    }
    tlv(0x02, &bytes[start..])
}

fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut output = vec![tag];
    if value.len() < 128 {
        output.push(value.len() as u8);
    } else {
        let bytes = value.len().to_be_bytes();
        let first = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len() - 1);
        output.push(0x80 | (bytes.len() - first) as u8);
        output.extend_from_slice(&bytes[first..]);
    }
    output.extend_from_slice(value);
    output
}

fn encode_oid(oid: &ObjectId) -> Result<Vec<u8>, SnmpError> {
    if oid.0.len() < 2 || oid.0[0] > 2 || (oid.0[0] < 2 && oid.0[1] > 39) {
        return Err(SnmpError::InvalidOid);
    }
    let mut output = Vec::new();
    encode_base128(oid.0[0] * 40 + oid.0[1], &mut output);
    for arc in &oid.0[2..] {
        encode_base128(*arc, &mut output);
    }
    Ok(output)
}

fn encode_base128(value: u32, output: &mut Vec<u8>) {
    let mut encoded = [0; 5];
    let mut index = encoded.len();
    let mut value = value;
    loop {
        index -= 1;
        encoded[index] = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            break;
        }
    }
    let last = encoded.len() - 1;
    for byte in &mut encoded[index..last] {
        *byte |= 0x80;
    }
    output.extend_from_slice(&encoded[index..]);
}

fn decode_oid(bytes: &[u8]) -> Result<Vec<u32>, SnmpError> {
    let mut arcs = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let mut value = 0u32;
        let mut count = 0;
        loop {
            let byte = *bytes.get(cursor).ok_or(SnmpError::Malformed)?;
            cursor += 1;
            count += 1;
            if count > 5 || value > (u32::MAX >> 7) {
                return Err(SnmpError::Malformed);
            }
            value = (value << 7) | u32::from(byte & 0x7f);
            if byte & 0x80 == 0 {
                break;
            }
        }
        arcs.push(value);
    }
    let first = *arcs.first().ok_or(SnmpError::Malformed)?;
    let (first_arc, second_arc) = if first < 40 {
        (0, first)
    } else if first < 80 {
        (1, first - 40)
    } else {
        (2, first - 80)
    };
    arcs[0] = second_arc;
    arcs.insert(0, first_arc);
    Ok(arcs)
}

fn decode_integer(bytes: &[u8]) -> Result<i64, SnmpError> {
    if bytes.is_empty() || bytes.len() > 8 {
        return Err(SnmpError::Malformed);
    }
    let mut output = if bytes[0] & 0x80 == 0 {
        [0; 8]
    } else {
        [0xff; 8]
    };
    output[8 - bytes.len()..].copy_from_slice(bytes);
    Ok(i64::from_be_bytes(output))
}

fn decode_unsigned(bytes: &[u8]) -> Result<u64, SnmpError> {
    if bytes.is_empty() || bytes.len() > 9 || (bytes.len() == 9 && bytes[0] != 0) {
        return Err(SnmpError::Malformed);
    }
    let bytes = bytes.strip_prefix(&[0]).unwrap_or(bytes);
    let mut output = [0; 8];
    output[8 - bytes.len()..].copy_from_slice(bytes);
    Ok(u64::from_be_bytes(output))
}

#[derive(Clone, Copy)]
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }

    fn any(&mut self, maximum: usize) -> Result<(u8, &'a [u8]), SnmpError> {
        let tag = *self.bytes.get(self.offset).ok_or(SnmpError::Malformed)?;
        self.offset += 1;
        let length = self.length()?;
        if length > maximum {
            return Err(SnmpError::LimitExceeded);
        }
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SnmpError::Malformed)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(SnmpError::Malformed)?;
        self.offset = end;
        Ok((tag, value))
    }

    fn value(&mut self, tag: u8, maximum: usize) -> Result<&'a [u8], SnmpError> {
        let (actual, value) = self.any(maximum)?;
        (actual == tag).then_some(value).ok_or(SnmpError::Malformed)
    }

    fn nested(&mut self, tag: u8) -> Result<Self, SnmpError> {
        Ok(Self::new(self.value(tag, self.bytes.len())?))
    }

    fn integer(&mut self) -> Result<i64, SnmpError> {
        decode_integer(self.value(0x02, 8)?)
    }

    fn length(&mut self) -> Result<usize, SnmpError> {
        let first = *self.bytes.get(self.offset).ok_or(SnmpError::Malformed)?;
        self.offset += 1;
        if first & 0x80 == 0 {
            return Ok(usize::from(first));
        }
        let count = usize::from(first & 0x7f);
        if count == 0 || count > std::mem::size_of::<usize>() {
            return Err(SnmpError::Malformed);
        }
        let end = self.offset.checked_add(count).ok_or(SnmpError::Malformed)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(SnmpError::Malformed)?;
        if bytes.first() == Some(&0) {
            return Err(SnmpError::Malformed);
        }
        self.offset = end;
        Ok(bytes
            .iter()
            .fold(0usize, |value, byte| (value << 8) | usize::from(*byte)))
    }

    fn finish(self) -> Result<(), SnmpError> {
        self.remaining()
            .is_empty()
            .then_some(())
            .ok_or(SnmpError::Malformed)
    }
}
