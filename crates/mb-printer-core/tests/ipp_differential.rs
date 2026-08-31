// SPDX-License-Identifier: AGPL-3.0-or-later
//! Development-only interoperability checks against ipp.rs. The oracle is a
//! dev-dependency and is not linked into mb-printer-core artifacts.

use ipp::{
    model::{DelimiterTag, IppVersion, Operation},
    operation::{GetPrinterAttributes, IppOperation},
    parser::IppParser,
    prelude::Uri,
    reader::IppReader,
};
use mb_printer_core::ipp::{
    self as core_ipp, Attribute, AttributeGroup, Limits, Message, Value, ValueTag, Version,
};
use std::io::Cursor;

#[test]
fn ipp_rs_generated_get_printer_attributes_round_trips_through_core() {
    let uri: Uri = "ipp://printer.example:631/ipp/print".parse().unwrap();
    let oracle = GetPrinterAttributes::with_attributes(
        uri,
        ["printer-state", "printer-state-reasons", "media-ready"],
    )
    .unwrap()
    .into_ipp_request();
    let oracle_bytes = oracle.to_bytes().to_vec();

    let decoded = core_ipp::decode(&oracle_bytes, Limits::default()).unwrap();
    assert_eq!(decoded.version, Version::V1_1);
    assert_eq!(decoded.code, Operation::GetPrinterAttributes as u16);
    assert_eq!(decoded.original_bytes, oracle_bytes);
    assert_eq!(decoded.encode(Limits::default()).unwrap(), oracle_bytes);
}

#[test]
fn core_generated_response_decodes_to_the_same_shape_in_ipp_rs() {
    let response = Message {
        version: Version::V2_0,
        code: 0,
        request_id: 42,
        groups: vec![
            AttributeGroup {
                tag: core_ipp::OPERATION_ATTRIBUTES_TAG,
                attributes: vec![
                    Attribute::new(
                        b"attributes-charset".to_vec(),
                        Value::raw(ValueTag::Charset, b"utf-8"),
                    ),
                    Attribute::new(
                        b"attributes-natural-language".to_vec(),
                        Value::raw(ValueTag::NaturalLanguage, b"en"),
                    ),
                ],
            },
            AttributeGroup {
                tag: core_ipp::PRINTER_ATTRIBUTES_TAG,
                attributes: vec![
                    Attribute::new(b"printer-state".to_vec(), Value::enum_value(3)),
                    Attribute {
                        name: b"printer-state-reasons".to_vec(),
                        values: vec![
                            Value::raw(ValueTag::Keyword, b"none"),
                            Value::raw(ValueTag::Keyword, b"other"),
                        ],
                    },
                ],
            },
        ],
        original_bytes: Vec::new(),
    };
    let bytes = response.encode(Limits::default()).unwrap();

    let oracle = IppParser::new(IppReader::new(Cursor::new(bytes)))
        .parse()
        .unwrap();
    assert_eq!(oracle.header().version, IppVersion::v2_0());
    assert_eq!(oracle.header().operation_or_status, 0);
    assert_eq!(oracle.header().request_id, 42);
    let printer = oracle
        .attributes()
        .first_of(DelimiterTag::PrinterAttributes)
        .unwrap();
    assert_eq!(printer.attributes().len(), 2);
    assert!(printer.get("printer-state").is_some());
    assert!(printer.get("printer-state-reasons").is_some());
}
