// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::ipp::{
    self, Attribute, AttributeGroup, CollectionMember, DecodeError, Limits, Message, Value,
    ValueData, ValueTag, Version,
};

fn request_with(values: Vec<Value>) -> Message {
    Message {
        version: Version::V2_0,
        code: 0x000b,
        request_id: 17,
        groups: vec![AttributeGroup {
            tag: ipp::OPERATION_ATTRIBUTES_TAG,
            attributes: vec![Attribute {
                name: b"requested-attributes".to_vec(),
                values,
            }],
        }],
        original_bytes: Vec::new(),
    }
}

#[test]
fn deterministic_round_trip_preserves_repeated_and_unknown_values() {
    let message = request_with(vec![
        Value::raw(ValueTag::Keyword, b"printer-state"),
        Value::raw(ValueTag::Extension(0x7f), [0, 1, 2, 255]),
    ]);
    let encoded = message.encode(Limits::default()).unwrap();
    let decoded = ipp::decode(&encoded, Limits::default()).unwrap();
    assert_eq!(decoded.original_bytes, encoded);
    assert_eq!(decoded.groups, message.groups);
    assert_eq!(decoded.encode(Limits::default()).unwrap(), encoded);
}

#[test]
fn all_standard_scalar_and_out_of_band_tags_round_trip() {
    let mut values = vec![
        Value {
            tag: ValueTag::Unsupported,
            data: ValueData::OutOfBand,
        },
        Value {
            tag: ValueTag::Unknown,
            data: ValueData::OutOfBand,
        },
        Value {
            tag: ValueTag::NoValue,
            data: ValueData::OutOfBand,
        },
        Value {
            tag: ValueTag::NotSettable,
            data: ValueData::OutOfBand,
        },
        Value {
            tag: ValueTag::DeleteAttribute,
            data: ValueData::OutOfBand,
        },
        Value {
            tag: ValueTag::AdminDefine,
            data: ValueData::OutOfBand,
        },
        Value::integer(-7),
        Value::boolean(true),
        Value::enum_value(4),
        Value {
            tag: ValueTag::DateTime,
            data: ValueData::DateTime([0x07, 0xe8, 1, 2, 3, 4, 5, 0, b'+', 0, 0]),
        },
        Value {
            tag: ValueTag::Resolution,
            data: ValueData::Resolution {
                cross_feed: 300,
                feed: 600,
                units: 3,
            },
        },
        Value {
            tag: ValueTag::RangeOfInteger,
            data: ValueData::RangeOfInteger {
                lower: 1,
                upper: 99,
            },
        },
    ];
    for tag in [
        ValueTag::OctetString,
        ValueTag::TextWithLanguage,
        ValueTag::NameWithLanguage,
        ValueTag::TextWithoutLanguage,
        ValueTag::NameWithoutLanguage,
        ValueTag::Keyword,
        ValueTag::Uri,
        ValueTag::UriScheme,
        ValueTag::Charset,
        ValueTag::NaturalLanguage,
        ValueTag::MimeMediaType,
    ] {
        values.push(Value::raw(tag, [1, 2, 3]));
    }
    let message = request_with(values);
    let bytes = message.encode(Limits::default()).unwrap();
    assert_eq!(
        ipp::decode(&bytes, Limits::default()).unwrap().groups,
        message.groups
    );
}

#[test]
fn nested_collections_and_repeated_members_round_trip() {
    let nested = Value::collection(vec![CollectionMember {
        name: b"inner".to_vec(),
        values: vec![Value::integer(5)],
    }]);
    let collection = Value::collection(vec![
        CollectionMember {
            name: b"media-size".to_vec(),
            values: vec![nested],
        },
        CollectionMember {
            name: b"media-type".to_vec(),
            values: vec![
                Value::raw(ValueTag::Keyword, b"labels"),
                Value::raw(ValueTag::Keyword, b"stationery"),
            ],
        },
    ]);
    let message = request_with(vec![collection]);
    let bytes = message.encode(Limits::default()).unwrap();
    let decoded = ipp::decode(&bytes, Limits::default()).unwrap();
    assert_eq!(decoded.groups, message.groups);
    assert_eq!(decoded.encode(Limits::default()).unwrap(), bytes);
}

#[test]
fn every_truncation_is_rejected_without_panicking() {
    let bytes = request_with(vec![Value::collection(vec![CollectionMember {
        name: b"x-dimension".to_vec(),
        values: vec![Value::integer(100)],
    }])])
    .encode(Limits::default())
    .unwrap();
    for end in 0..bytes.len() {
        assert!(
            ipp::decode(&bytes[..end], Limits::default()).is_err(),
            "{end}"
        );
    }
}

#[test]
fn explicit_message_value_count_and_depth_limits_fail_closed() {
    let bytes = request_with(vec![Value::raw(ValueTag::Keyword, vec![0; 32])])
        .encode(Limits::default())
        .unwrap();
    let limits = Limits {
        max_message_bytes: bytes.len() - 1,
        ..Limits::default()
    };
    assert!(matches!(
        ipp::decode(&bytes, limits),
        Err(DecodeError::MessageTooLarge { .. })
    ));

    let limits = Limits {
        max_value_bytes: 4,
        ..Limits::default()
    };
    assert!(matches!(
        ipp::decode(&bytes, limits),
        Err(DecodeError::Limit {
            kind: "value bytes",
            ..
        })
    ));

    let nested = request_with(vec![Value::collection(vec![CollectionMember {
        name: b"outer".to_vec(),
        values: vec![Value::collection(vec![CollectionMember {
            name: b"inner".to_vec(),
            values: vec![Value::integer(1)],
        }])],
    }])]);
    let nested = nested.encode(Limits::default()).unwrap();
    let limits = Limits {
        max_collection_depth: 1,
        ..Limits::default()
    };
    assert!(matches!(
        ipp::decode(&nested, limits),
        Err(DecodeError::Limit {
            kind: "collection depth",
            ..
        })
    ));
}

#[test]
fn malformed_collection_control_and_scalar_lengths_are_rejected() {
    let mut end_outside = vec![2, 0, 0, 0, 0, 0, 0, 1, 1];
    end_outside.extend([0x37, 0, 1, b'x', 0, 0, 3]);
    assert!(matches!(
        ipp::decode(&end_outside, Limits::default()),
        Err(DecodeError::IllegalTag { .. })
    ));

    let mut bad_integer = vec![2, 0, 0, 0, 0, 0, 0, 1, 1];
    bad_integer.extend([0x21, 0, 1, b'x', 0, 1, 0, 3]);
    assert!(matches!(
        ipp::decode(&bad_integer, Limits::default()),
        Err(DecodeError::InvalidValue { .. })
    ));
}

#[test]
fn every_structural_and_decoded_allocation_limit_is_enforced() {
    let message = Message {
        version: Version::V2_0,
        code: ipp::GET_PRINTER_ATTRIBUTES,
        request_id: 1,
        groups: vec![
            AttributeGroup {
                tag: ipp::OPERATION_ATTRIBUTES_TAG,
                attributes: vec![
                    Attribute {
                        name: b"first-name".to_vec(),
                        values: vec![
                            Value::raw(ValueTag::Keyword, b"one"),
                            Value::raw(ValueTag::Keyword, b"two"),
                        ],
                    },
                    Attribute::new(b"second".to_vec(), Value::integer(2)),
                ],
            },
            AttributeGroup {
                tag: ipp::PRINTER_ATTRIBUTES_TAG,
                attributes: vec![Attribute::new(
                    b"collection".to_vec(),
                    Value::collection(vec![
                        CollectionMember {
                            name: b"one".to_vec(),
                            values: vec![Value::integer(1)],
                        },
                        CollectionMember {
                            name: b"two".to_vec(),
                            values: vec![Value::integer(2)],
                        },
                    ]),
                )],
            },
        ],
        original_bytes: Vec::new(),
    };
    let bytes = message.encode(Limits::default()).unwrap();
    for (limits, kind) in [
        (
            Limits {
                max_groups: 1,
                ..Limits::default()
            },
            "groups",
        ),
        (
            Limits {
                max_attributes: 2,
                ..Limits::default()
            },
            "attributes",
        ),
        (
            Limits {
                max_values: 2,
                ..Limits::default()
            },
            "values",
        ),
        (
            Limits {
                max_name_bytes: 4,
                ..Limits::default()
            },
            "name bytes",
        ),
        (
            Limits {
                max_decoded_bytes: 8,
                ..Limits::default()
            },
            "decoded bytes",
        ),
        (
            Limits {
                max_collection_members: 1,
                ..Limits::default()
            },
            "collection members",
        ),
    ] {
        assert!(matches!(
            ipp::decode(&bytes, limits),
            Err(DecodeError::Limit { kind: actual, .. }) if actual == kind
        ));
    }
}

#[test]
fn bounded_arbitrary_byte_corpus_never_panics() {
    let mut state = 0x9e37_79b9u32;
    for length in 0..=256 {
        let mut bytes = vec![0; length];
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        let result = std::panic::catch_unwind(|| ipp::decode(&bytes, Limits::default()));
        assert!(
            result.is_ok(),
            "decoder panicked for corpus length {length}"
        );
    }
}
