// SPDX-License-Identifier: AGPL-3.0-or-later
//! Synchronous, runtime-independent IPP message encoding and decoding.
//!
//! The codec preserves a decoded message's original bounded bytes and retains
//! unknown tags as raw values. Network transports and operation policy belong
//! outside this module.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const END_OF_ATTRIBUTES_TAG: u8 = 0x03;
pub const OPERATION_ATTRIBUTES_TAG: u8 = 0x01;
pub const JOB_ATTRIBUTES_TAG: u8 = 0x02;
pub const PRINTER_ATTRIBUTES_TAG: u8 = 0x04;
pub const UNSUPPORTED_ATTRIBUTES_TAG: u8 = 0x05;
pub const GET_PRINTER_ATTRIBUTES: u16 = 0x000b;
pub const GET_PRINTER_SUPPORTED_VALUES: u16 = 0x0015;
pub const SET_PRINTER_ATTRIBUTES: u16 = 0x0013;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_message_bytes: usize,
    pub max_groups: usize,
    /// Includes top-level attributes and collection members.
    pub max_attributes: usize,
    pub max_values: usize,
    pub max_name_bytes: usize,
    pub max_value_bytes: usize,
    /// Bounds bytes duplicated into decoded names and values.
    pub max_decoded_bytes: usize,
    pub max_collection_depth: usize,
    pub max_collection_members: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: 4 * 1024 * 1024,
            max_groups: 32,
            max_attributes: 4_096,
            max_values: 16_384,
            max_name_bytes: 1_024,
            max_value_bytes: 1024 * 1024,
            max_decoded_bytes: 8 * 1024 * 1024,
            max_collection_depth: 16,
            max_collection_members: 4_096,
        }
    }
}

impl Limits {
    fn validate(self) -> Result<Self, DecodeError> {
        if self.max_message_bytes < 8
            || self.max_groups == 0
            || self.max_attributes == 0
            || self.max_values == 0
            || self.max_name_bytes == 0
            || self.max_value_bytes == 0
            || self.max_decoded_bytes == 0
            || self.max_collection_depth == 0
            || self.max_collection_members == 0
        {
            return Err(DecodeError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub version: Version,
    /// Operation ID in requests and status code in responses.
    pub code: u16,
    pub request_id: u32,
    pub groups: Vec<AttributeGroup>,
    /// Exact input supplied to [`decode`]. Empty for newly constructed values.
    pub original_bytes: Vec<u8>,
}

impl Message {
    pub fn new(version: Version, code: u16, request_id: u32) -> Self {
        Self {
            version,
            code,
            request_id,
            groups: Vec::new(),
            original_bytes: Vec::new(),
        }
    }

    pub fn encode(&self, limits: Limits) -> Result<Vec<u8>, EncodeError> {
        encode(self, limits)
    }
}

/// Construct the operation attributes for a bounded Get-Printer-Attributes
/// request. An empty `requested_attributes` list omits that attribute; callers
/// that want the server-scoped set use `["all"]` explicitly.
pub fn get_printer_attributes_request<I, A>(
    printer_uri: &str,
    requested_attributes: I,
    document_format: Option<&str>,
    request_id: u32,
) -> Message
where
    I: IntoIterator<Item = A>,
    A: AsRef<str>,
{
    let mut attributes = vec![
        Attribute::new(
            b"attributes-charset".to_vec(),
            Value::raw(ValueTag::Charset, b"utf-8"),
        ),
        Attribute::new(
            b"attributes-natural-language".to_vec(),
            Value::raw(ValueTag::NaturalLanguage, b"en"),
        ),
        Attribute::new(
            b"printer-uri".to_vec(),
            Value::raw(ValueTag::Uri, printer_uri.as_bytes()),
        ),
    ];
    if let Some(document_format) = document_format {
        attributes.push(Attribute::new(
            b"document-format".to_vec(),
            Value::raw(ValueTag::MimeMediaType, document_format.as_bytes()),
        ));
    }
    let requested_attributes = requested_attributes
        .into_iter()
        .map(|name| Value::raw(ValueTag::Keyword, name.as_ref().as_bytes()))
        .collect::<Vec<_>>();
    if !requested_attributes.is_empty() {
        attributes.push(Attribute {
            name: b"requested-attributes".to_vec(),
            values: requested_attributes,
        });
    }
    Message {
        version: Version::V2_0,
        code: GET_PRINTER_ATTRIBUTES,
        request_id,
        groups: vec![AttributeGroup {
            tag: OPERATION_ATTRIBUTES_TAG,
            attributes,
        }],
        original_bytes: Vec::new(),
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SupportedValuesRequestError {
    #[error("Get-Printer-Supported-Values only accepts explicit xxx-supported attributes")]
    InvalidRequestedAttribute,
    #[error("Get-Printer-Supported-Values requires at least one requested attribute")]
    MissingRequestedAttribute,
}

/// Construct RFC 3380 Get-Printer-Supported-Values. This administrator
/// operation asks which values may be assigned to explicitly named, settable
/// `xxx-supported` Printer attributes; it is not a generic settings query.
pub fn get_printer_supported_values_request<I, A>(
    printer_uri: &str,
    requested_attributes: I,
    request_id: u32,
) -> Result<Message, SupportedValuesRequestError>
where
    I: IntoIterator<Item = A>,
    A: AsRef<str>,
{
    let requested_attributes = requested_attributes
        .into_iter()
        .map(|attribute| attribute.as_ref().to_owned())
        .collect::<Vec<_>>();
    if requested_attributes.is_empty() {
        return Err(SupportedValuesRequestError::MissingRequestedAttribute);
    }
    if requested_attributes
        .iter()
        .any(|attribute| !attribute.ends_with("-supported") || attribute == "all")
    {
        return Err(SupportedValuesRequestError::InvalidRequestedAttribute);
    }
    let mut request = get_printer_attributes_request(
        printer_uri,
        requested_attributes.iter().map(String::as_str),
        None,
        request_id,
    );
    request.code = GET_PRINTER_SUPPORTED_VALUES;
    Ok(request)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub major: u8,
    pub minor: u8,
}

impl Version {
    pub const V1_1: Self = Self { major: 1, minor: 1 };
    pub const V2_0: Self = Self { major: 2, minor: 0 };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeGroup {
    /// Known and extension delimiter tags are retained verbatim.
    pub tag: u8,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attribute {
    /// Attribute names are bytes because preserving the wire value takes
    /// precedence over assuming the advertised charset is UTF-8.
    pub name: Vec<u8>,
    pub values: Vec<Value>,
}

impl Attribute {
    pub fn new(name: impl Into<Vec<u8>>, value: Value) -> Self {
        Self {
            name: name.into(),
            values: vec![value],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionMember {
    pub name: Vec<u8>,
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Value {
    pub tag: ValueTag,
    pub data: ValueData,
}

impl Value {
    pub fn raw(tag: ValueTag, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            tag,
            data: ValueData::Bytes(bytes.into()),
        }
    }

    pub fn integer(value: i32) -> Self {
        Self {
            tag: ValueTag::Integer,
            data: ValueData::Integer(value),
        }
    }

    pub fn enum_value(value: i32) -> Self {
        Self {
            tag: ValueTag::Enum,
            data: ValueData::Enum(value),
        }
    }

    pub fn boolean(value: bool) -> Self {
        Self {
            tag: ValueTag::Boolean,
            data: ValueData::Boolean(value),
        }
    }

    pub fn collection(members: Vec<CollectionMember>) -> Self {
        Self {
            tag: ValueTag::BegCollection,
            data: ValueData::Collection(members),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ValueData {
    OutOfBand,
    Integer(i32),
    Boolean(bool),
    Enum(i32),
    DateTime([u8; 11]),
    Resolution {
        cross_feed: i32,
        feed: i32,
        units: u8,
    },
    RangeOfInteger {
        lower: i32,
        upper: i32,
    },
    Collection(Vec<CollectionMember>),
    /// Strings, octet strings, unknown tags, and extension values remain exact.
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValueTag {
    Unsupported,
    Unknown,
    NoValue,
    NotSettable,
    DeleteAttribute,
    AdminDefine,
    Integer,
    Boolean,
    Enum,
    OctetString,
    DateTime,
    Resolution,
    RangeOfInteger,
    BegCollection,
    TextWithLanguage,
    NameWithLanguage,
    EndCollection,
    TextWithoutLanguage,
    NameWithoutLanguage,
    Keyword,
    Uri,
    UriScheme,
    Charset,
    NaturalLanguage,
    MimeMediaType,
    MemberAttrName,
    Extension(u8),
}

impl ValueTag {
    pub const fn from_byte(tag: u8) -> Self {
        match tag {
            0x10 => Self::Unsupported,
            0x12 => Self::Unknown,
            0x13 => Self::NoValue,
            0x15 => Self::NotSettable,
            0x16 => Self::DeleteAttribute,
            0x17 => Self::AdminDefine,
            0x21 => Self::Integer,
            0x22 => Self::Boolean,
            0x23 => Self::Enum,
            0x30 => Self::OctetString,
            0x31 => Self::DateTime,
            0x32 => Self::Resolution,
            0x33 => Self::RangeOfInteger,
            0x34 => Self::BegCollection,
            0x35 => Self::TextWithLanguage,
            0x36 => Self::NameWithLanguage,
            0x37 => Self::EndCollection,
            0x41 => Self::TextWithoutLanguage,
            0x42 => Self::NameWithoutLanguage,
            0x44 => Self::Keyword,
            0x45 => Self::Uri,
            0x46 => Self::UriScheme,
            0x47 => Self::Charset,
            0x48 => Self::NaturalLanguage,
            0x49 => Self::MimeMediaType,
            0x4a => Self::MemberAttrName,
            other => Self::Extension(other),
        }
    }

    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Unsupported => 0x10,
            Self::Unknown => 0x12,
            Self::NoValue => 0x13,
            Self::NotSettable => 0x15,
            Self::DeleteAttribute => 0x16,
            Self::AdminDefine => 0x17,
            Self::Integer => 0x21,
            Self::Boolean => 0x22,
            Self::Enum => 0x23,
            Self::OctetString => 0x30,
            Self::DateTime => 0x31,
            Self::Resolution => 0x32,
            Self::RangeOfInteger => 0x33,
            Self::BegCollection => 0x34,
            Self::TextWithLanguage => 0x35,
            Self::NameWithLanguage => 0x36,
            Self::EndCollection => 0x37,
            Self::TextWithoutLanguage => 0x41,
            Self::NameWithoutLanguage => 0x42,
            Self::Keyword => 0x44,
            Self::Uri => 0x45,
            Self::UriScheme => 0x46,
            Self::Charset => 0x47,
            Self::NaturalLanguage => 0x48,
            Self::MimeMediaType => 0x49,
            Self::MemberAttrName => 0x4a,
            Self::Extension(tag) => tag,
        }
    }

    const fn is_out_of_band(self) -> bool {
        matches!(
            self,
            Self::Unsupported
                | Self::Unknown
                | Self::NoValue
                | Self::NotSettable
                | Self::DeleteAttribute
                | Self::AdminDefine
        )
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DecodeError {
    #[error("IPP limits must all be positive and permit the eight-byte header")]
    InvalidLimits,
    #[error("IPP message is {actual} bytes, exceeding the {limit}-byte limit")]
    MessageTooLarge { actual: usize, limit: usize },
    #[error("truncated IPP message at byte {offset}: needed {needed} more bytes")]
    Truncated { offset: usize, needed: usize },
    #[error("expected an attribute group at byte {offset}, found tag 0x{tag:02x}")]
    MissingGroup { offset: usize, tag: u8 },
    #[error("unexpected end-of-attributes marker inside a collection at byte {offset}")]
    EndInsideCollection { offset: usize },
    #[error("attribute value with an empty name has no preceding attribute at byte {offset}")]
    MissingRepeatedAttribute { offset: usize },
    #[error("illegal value tag 0x{tag:02x} at byte {offset}: {reason}")]
    IllegalTag {
        offset: usize,
        tag: u8,
        reason: &'static str,
    },
    #[error("invalid encoded value for tag 0x{tag:02x} at byte {offset}: {reason}")]
    InvalidValue {
        offset: usize,
        tag: u8,
        reason: &'static str,
    },
    #[error("IPP {kind} limit exceeded: {actual} > {limit}")]
    Limit {
        kind: &'static str,
        actual: usize,
        limit: usize,
    },
    #[error("IPP message has trailing bytes after end-of-attributes at byte {offset}")]
    TrailingBytes { offset: usize },
    #[error("IPP message has no end-of-attributes marker")]
    MissingEndOfAttributes,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EncodeError {
    #[error("IPP limits must all be positive and permit the eight-byte header")]
    InvalidLimits,
    #[error("invalid IPP structure: {0}")]
    InvalidStructure(&'static str),
    #[error("IPP {kind} limit exceeded: {actual} > {limit}")]
    Limit {
        kind: &'static str,
        actual: usize,
        limit: usize,
    },
    #[error("IPP name or value cannot exceed 65535 bytes")]
    WireLengthOverflow,
    #[error("IPP value data does not match tag 0x{tag:02x}")]
    TagDataMismatch { tag: u8 },
}

#[derive(Default)]
struct Counters {
    groups: usize,
    attributes: usize,
    values: usize,
    collection_members: usize,
    decoded_bytes: usize,
}

struct Decoder<'a> {
    input: &'a [u8],
    offset: usize,
    limits: Limits,
    counters: Counters,
}

pub fn decode(input: &[u8], limits: Limits) -> Result<Message, DecodeError> {
    let limits = limits.validate()?;
    if input.len() > limits.max_message_bytes {
        return Err(DecodeError::MessageTooLarge {
            actual: input.len(),
            limit: limits.max_message_bytes,
        });
    }
    let mut decoder = Decoder {
        input,
        offset: 0,
        limits,
        counters: Counters::default(),
    };
    let major = decoder.byte()?;
    let minor = decoder.byte()?;
    let code = decoder.u16()?;
    let request_id = decoder.u32()?;
    let mut groups = Vec::new();
    loop {
        let marker_offset = decoder.offset;
        let tag = decoder.byte().map_err(|error| match error {
            DecodeError::Truncated { .. } => DecodeError::MissingEndOfAttributes,
            other => other,
        })?;
        if tag == END_OF_ATTRIBUTES_TAG {
            if decoder.offset != input.len() {
                return Err(DecodeError::TrailingBytes {
                    offset: decoder.offset,
                });
            }
            break;
        }
        if !is_delimiter_tag(tag) {
            return Err(DecodeError::MissingGroup {
                offset: marker_offset,
                tag,
            });
        }
        decoder.bump("groups", 1, decoder.limits.max_groups)?;
        decoder.counters.groups += 1;
        let mut group = AttributeGroup {
            tag,
            attributes: Vec::new(),
        };
        while decoder.offset < input.len() {
            let next = decoder.peek()?;
            if next == END_OF_ATTRIBUTES_TAG || is_delimiter_tag(next) {
                break;
            }
            let entry_offset = decoder.offset;
            let (value_tag, name, value) = decoder.attribute_value(0)?;
            if name.is_empty() {
                let attribute =
                    group
                        .attributes
                        .last_mut()
                        .ok_or(DecodeError::MissingRepeatedAttribute {
                            offset: entry_offset,
                        })?;
                attribute.values.push(value);
            } else {
                decoder.add_attribute()?;
                group.attributes.push(Attribute {
                    name,
                    values: vec![value],
                });
            }
            if matches!(
                value_tag,
                ValueTag::MemberAttrName | ValueTag::EndCollection
            ) {
                return Err(DecodeError::IllegalTag {
                    offset: entry_offset,
                    tag: value_tag.to_byte(),
                    reason: "collection control tag outside a collection",
                });
            }
        }
        groups.push(group);
    }
    Ok(Message {
        version: Version { major, minor },
        code,
        request_id,
        groups,
        original_bytes: input.to_vec(),
    })
}

impl<'a> Decoder<'a> {
    fn byte(&mut self) -> Result<u8, DecodeError> {
        let byte = *self.input.get(self.offset).ok_or(DecodeError::Truncated {
            offset: self.offset,
            needed: 1,
        })?;
        self.offset += 1;
        Ok(byte)
    }

    fn peek(&self) -> Result<u8, DecodeError> {
        self.input
            .get(self.offset)
            .copied()
            .ok_or(DecodeError::MissingEndOfAttributes)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(DecodeError::Truncated {
                offset: self.offset,
                needed: length,
            })?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or_else(|| DecodeError::Truncated {
                offset: self.offset,
                needed: end.saturating_sub(self.input.len()),
            })?;
        self.offset = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes(
            bytes.try_into().expect("four-byte slice"),
        ))
    }

    fn bump(&self, kind: &'static str, add: usize, limit: usize) -> Result<(), DecodeError> {
        let actual = match kind {
            "groups" => self.counters.groups,
            "attributes" => self.counters.attributes,
            "values" => self.counters.values,
            "collection members" => self.counters.collection_members,
            "decoded bytes" => self.counters.decoded_bytes,
            _ => 0,
        }
        .saturating_add(add);
        if actual > limit {
            return Err(DecodeError::Limit {
                kind,
                actual,
                limit,
            });
        }
        Ok(())
    }

    fn add_decoded_bytes(&mut self, length: usize) -> Result<(), DecodeError> {
        self.bump("decoded bytes", length, self.limits.max_decoded_bytes)?;
        self.counters.decoded_bytes += length;
        Ok(())
    }

    fn add_attribute(&mut self) -> Result<(), DecodeError> {
        self.bump("attributes", 1, self.limits.max_attributes)?;
        self.counters.attributes += 1;
        Ok(())
    }

    fn add_value(&mut self) -> Result<(), DecodeError> {
        self.bump("values", 1, self.limits.max_values)?;
        self.counters.values += 1;
        Ok(())
    }

    fn attribute_value(
        &mut self,
        collection_depth: usize,
    ) -> Result<(ValueTag, Vec<u8>, Value), DecodeError> {
        let entry_offset = self.offset;
        let tag = ValueTag::from_byte(self.byte()?);
        let name_length = usize::from(self.u16()?);
        if name_length > self.limits.max_name_bytes {
            return Err(DecodeError::Limit {
                kind: "name bytes",
                actual: name_length,
                limit: self.limits.max_name_bytes,
            });
        }
        let name = self.take(name_length)?.to_vec();
        self.add_decoded_bytes(name_length)?;
        let value_length = usize::from(self.u16()?);
        if value_length > self.limits.max_value_bytes {
            return Err(DecodeError::Limit {
                kind: "value bytes",
                actual: value_length,
                limit: self.limits.max_value_bytes,
            });
        }
        let raw = self.take(value_length)?.to_vec();
        self.add_decoded_bytes(value_length)?;
        self.add_value()?;
        let data = if tag == ValueTag::BegCollection {
            if value_length != 0 {
                return Err(DecodeError::InvalidValue {
                    offset: entry_offset,
                    tag: tag.to_byte(),
                    reason: "begCollection must have an empty value",
                });
            }
            ValueData::Collection(self.collection(collection_depth + 1)?)
        } else {
            decode_scalar(tag, raw, entry_offset)?
        };
        Ok((tag, name, Value { tag, data }))
    }

    fn collection(&mut self, depth: usize) -> Result<Vec<CollectionMember>, DecodeError> {
        if depth > self.limits.max_collection_depth {
            return Err(DecodeError::Limit {
                kind: "collection depth",
                actual: depth,
                limit: self.limits.max_collection_depth,
            });
        }
        let mut members: Vec<CollectionMember> = Vec::new();
        loop {
            let entry_offset = self.offset;
            let tag = self.peek()?;
            if tag == END_OF_ATTRIBUTES_TAG {
                return Err(DecodeError::EndInsideCollection {
                    offset: entry_offset,
                });
            }
            if is_delimiter_tag(tag) {
                return Err(DecodeError::IllegalTag {
                    offset: entry_offset,
                    tag,
                    reason: "attribute group delimiter inside a collection",
                });
            }
            let (tag, wire_name, value) = self.attribute_value(depth)?;
            if !wire_name.is_empty() {
                return Err(DecodeError::InvalidValue {
                    offset: entry_offset,
                    tag: tag.to_byte(),
                    reason: "collection entries must have empty wire names",
                });
            }
            match tag {
                ValueTag::EndCollection => {
                    if value.data != ValueData::Bytes(Vec::new()) {
                        return Err(DecodeError::InvalidValue {
                            offset: entry_offset,
                            tag: tag.to_byte(),
                            reason: "endCollection must have an empty value",
                        });
                    }
                    return Ok(members);
                }
                ValueTag::MemberAttrName => {
                    let ValueData::Bytes(member_name) = value.data else {
                        unreachable!("memberAttrName decodes as bytes")
                    };
                    if member_name.is_empty() {
                        return Err(DecodeError::InvalidValue {
                            offset: entry_offset,
                            tag: tag.to_byte(),
                            reason: "memberAttrName cannot be empty",
                        });
                    }
                    if member_name.len() > self.limits.max_name_bytes {
                        return Err(DecodeError::Limit {
                            kind: "name bytes",
                            actual: member_name.len(),
                            limit: self.limits.max_name_bytes,
                        });
                    }
                    self.bump("collection members", 1, self.limits.max_collection_members)?;
                    self.counters.collection_members += 1;
                    self.add_attribute()?;
                    members.push(CollectionMember {
                        name: member_name,
                        values: Vec::new(),
                    });
                }
                _ => {
                    let member = members.last_mut().ok_or(DecodeError::InvalidValue {
                        offset: entry_offset,
                        tag: tag.to_byte(),
                        reason: "member value precedes memberAttrName",
                    })?;
                    member.values.push(value);
                }
            }
        }
    }
}

fn decode_scalar(tag: ValueTag, raw: Vec<u8>, offset: usize) -> Result<ValueData, DecodeError> {
    let invalid = |reason| DecodeError::InvalidValue {
        offset,
        tag: tag.to_byte(),
        reason,
    };
    if tag.is_out_of_band() {
        if !raw.is_empty() {
            return Err(invalid("out-of-band value must be empty"));
        }
        return Ok(ValueData::OutOfBand);
    }
    match tag {
        ValueTag::Integer => exact_i32(raw, tag, offset).map(ValueData::Integer),
        ValueTag::Enum => exact_i32(raw, tag, offset).map(ValueData::Enum),
        ValueTag::Boolean => match raw.as_slice() {
            [0] => Ok(ValueData::Boolean(false)),
            [1] => Ok(ValueData::Boolean(true)),
            _ => Err(invalid(
                "boolean must be exactly one byte containing zero or one",
            )),
        },
        ValueTag::DateTime => raw
            .try_into()
            .map(ValueData::DateTime)
            .map_err(|_| invalid("dateTime must be exactly 11 bytes")),
        ValueTag::Resolution if raw.len() == 9 => Ok(ValueData::Resolution {
            cross_feed: i32::from_be_bytes(raw[0..4].try_into().expect("four-byte slice")),
            feed: i32::from_be_bytes(raw[4..8].try_into().expect("four-byte slice")),
            units: raw[8],
        }),
        ValueTag::Resolution => Err(invalid("resolution must be exactly 9 bytes")),
        ValueTag::RangeOfInteger if raw.len() == 8 => Ok(ValueData::RangeOfInteger {
            lower: i32::from_be_bytes(raw[0..4].try_into().expect("four-byte slice")),
            upper: i32::from_be_bytes(raw[4..8].try_into().expect("four-byte slice")),
        }),
        ValueTag::RangeOfInteger => Err(invalid("rangeOfInteger must be exactly 8 bytes")),
        ValueTag::BegCollection => Err(invalid("nested collection was not decoded")),
        _ => Ok(ValueData::Bytes(raw)),
    }
}

fn exact_i32(raw: Vec<u8>, tag: ValueTag, offset: usize) -> Result<i32, DecodeError> {
    if raw.len() != 4 {
        return Err(DecodeError::InvalidValue {
            offset,
            tag: tag.to_byte(),
            reason: "integer and enum values must be exactly 4 bytes",
        });
    }
    Ok(i32::from_be_bytes(
        raw.try_into().expect("validated four-byte value"),
    ))
}

const fn is_delimiter_tag(tag: u8) -> bool {
    tag >= 0x01 && tag <= 0x0f && tag != END_OF_ATTRIBUTES_TAG
}

pub fn encode(message: &Message, limits: Limits) -> Result<Vec<u8>, EncodeError> {
    let limits = limits.validate().map_err(|_| EncodeError::InvalidLimits)?;
    let mut encoder = Encoder {
        output: Vec::with_capacity(message.original_bytes.len().max(64)),
        limits,
        counters: Counters::default(),
    };
    encoder.push(message.version.major)?;
    encoder.push(message.version.minor)?;
    encoder.extend(&message.code.to_be_bytes())?;
    encoder.extend(&message.request_id.to_be_bytes())?;
    for group in &message.groups {
        if !is_delimiter_tag(group.tag) {
            return Err(EncodeError::InvalidStructure(
                "attribute group has an invalid delimiter tag",
            ));
        }
        encoder.add("groups", 1, limits.max_groups)?;
        encoder.counters.groups += 1;
        encoder.push(group.tag)?;
        for attribute in &group.attributes {
            encoder.attribute(attribute, 0)?;
        }
    }
    encoder.push(END_OF_ATTRIBUTES_TAG)?;
    Ok(encoder.output)
}

struct Encoder {
    output: Vec<u8>,
    limits: Limits,
    counters: Counters,
}

impl Encoder {
    fn push(&mut self, byte: u8) -> Result<(), EncodeError> {
        self.add_output(1)?;
        self.output.push(byte);
        Ok(())
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        self.add_output(bytes.len())?;
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    fn add_output(&self, length: usize) -> Result<(), EncodeError> {
        let actual = self.output.len().saturating_add(length);
        if actual > self.limits.max_message_bytes {
            return Err(EncodeError::Limit {
                kind: "message bytes",
                actual,
                limit: self.limits.max_message_bytes,
            });
        }
        Ok(())
    }

    fn add(&self, kind: &'static str, add: usize, limit: usize) -> Result<(), EncodeError> {
        let current = match kind {
            "groups" => self.counters.groups,
            "attributes" => self.counters.attributes,
            "values" => self.counters.values,
            "collection members" => self.counters.collection_members,
            "decoded bytes" => self.counters.decoded_bytes,
            _ => 0,
        };
        let actual = current.saturating_add(add);
        if actual > limit {
            return Err(EncodeError::Limit {
                kind,
                actual,
                limit,
            });
        }
        Ok(())
    }

    fn decoded_bytes(&mut self, length: usize) -> Result<(), EncodeError> {
        self.add("decoded bytes", length, self.limits.max_decoded_bytes)?;
        self.counters.decoded_bytes += length;
        Ok(())
    }

    fn attribute(&mut self, attribute: &Attribute, depth: usize) -> Result<(), EncodeError> {
        if attribute.name.is_empty() || attribute.values.is_empty() {
            return Err(EncodeError::InvalidStructure(
                "attributes require a non-empty name and at least one value",
            ));
        }
        self.add("attributes", 1, self.limits.max_attributes)?;
        self.counters.attributes += 1;
        self.check_name(&attribute.name)?;
        for (index, value) in attribute.values.iter().enumerate() {
            self.value(if index == 0 { &attribute.name } else { &[] }, value, depth)?;
        }
        Ok(())
    }

    fn value(&mut self, name: &[u8], value: &Value, depth: usize) -> Result<(), EncodeError> {
        self.add("values", 1, self.limits.max_values)?;
        self.counters.values += 1;
        self.push(value.tag.to_byte())?;
        self.wire_bytes(name)?;
        match (&value.tag, &value.data) {
            (ValueTag::BegCollection, ValueData::Collection(members)) => {
                if depth + 1 > self.limits.max_collection_depth {
                    return Err(EncodeError::Limit {
                        kind: "collection depth",
                        actual: depth + 1,
                        limit: self.limits.max_collection_depth,
                    });
                }
                self.wire_bytes(&[])?;
                for member in members {
                    self.member(member, depth + 1)?;
                }
                self.control(ValueTag::EndCollection, &[])?;
            }
            _ => {
                let encoded = encode_scalar(value)?;
                self.wire_bytes(&encoded)?;
            }
        }
        Ok(())
    }

    fn member(&mut self, member: &CollectionMember, depth: usize) -> Result<(), EncodeError> {
        if member.name.is_empty() || member.values.is_empty() {
            return Err(EncodeError::InvalidStructure(
                "collection members require a name and at least one value",
            ));
        }
        self.add("collection members", 1, self.limits.max_collection_members)?;
        self.counters.collection_members += 1;
        self.add("attributes", 1, self.limits.max_attributes)?;
        self.counters.attributes += 1;
        self.check_name(&member.name)?;
        self.control(ValueTag::MemberAttrName, &member.name)?;
        for value in &member.values {
            self.value(&[], value, depth)?;
        }
        Ok(())
    }

    fn control(&mut self, tag: ValueTag, bytes: &[u8]) -> Result<(), EncodeError> {
        self.add("values", 1, self.limits.max_values)?;
        self.counters.values += 1;
        self.push(tag.to_byte())?;
        self.wire_bytes(&[])?;
        self.wire_bytes(bytes)
    }

    fn check_name(&self, name: &[u8]) -> Result<(), EncodeError> {
        if name.len() > self.limits.max_name_bytes {
            return Err(EncodeError::Limit {
                kind: "name bytes",
                actual: name.len(),
                limit: self.limits.max_name_bytes,
            });
        }
        Ok(())
    }

    fn wire_bytes(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        if bytes.len() > u16::MAX as usize {
            return Err(EncodeError::WireLengthOverflow);
        }
        if bytes.len() > self.limits.max_value_bytes {
            return Err(EncodeError::Limit {
                kind: "value bytes",
                actual: bytes.len(),
                limit: self.limits.max_value_bytes,
            });
        }
        self.decoded_bytes(bytes.len())?;
        self.extend(&(bytes.len() as u16).to_be_bytes())?;
        self.extend(bytes)
    }
}

fn encode_scalar(value: &Value) -> Result<Vec<u8>, EncodeError> {
    let mismatch = || EncodeError::TagDataMismatch {
        tag: value.tag.to_byte(),
    };
    match (&value.tag, &value.data) {
        (tag, ValueData::OutOfBand) if tag.is_out_of_band() => Ok(Vec::new()),
        (ValueTag::Integer, ValueData::Integer(integer)) => Ok(integer.to_be_bytes().to_vec()),
        (ValueTag::Enum, ValueData::Enum(integer)) => Ok(integer.to_be_bytes().to_vec()),
        (ValueTag::Boolean, ValueData::Boolean(boolean)) => Ok(vec![u8::from(*boolean)]),
        (ValueTag::DateTime, ValueData::DateTime(date_time)) => Ok(date_time.to_vec()),
        (
            ValueTag::Resolution,
            ValueData::Resolution {
                cross_feed,
                feed,
                units,
            },
        ) => {
            let mut bytes = Vec::with_capacity(9);
            bytes.extend(cross_feed.to_be_bytes());
            bytes.extend(feed.to_be_bytes());
            bytes.push(*units);
            Ok(bytes)
        }
        (ValueTag::RangeOfInteger, ValueData::RangeOfInteger { lower, upper }) => {
            let mut bytes = Vec::with_capacity(8);
            bytes.extend(lower.to_be_bytes());
            bytes.extend(upper.to_be_bytes());
            Ok(bytes)
        }
        (tag, ValueData::Bytes(bytes))
            if !tag.is_out_of_band()
                && !matches!(
                    tag,
                    ValueTag::Integer
                        | ValueTag::Boolean
                        | ValueTag::Enum
                        | ValueTag::DateTime
                        | ValueTag::Resolution
                        | ValueTag::RangeOfInteger
                        | ValueTag::BegCollection
                ) =>
        {
            Ok(bytes.clone())
        }
        _ => Err(mismatch()),
    }
}
