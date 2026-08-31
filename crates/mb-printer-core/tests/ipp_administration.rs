// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    administration::{
        ChangeBinding, ChangePlanError, PlanChangeRequest, parse_get_printer_supported_values,
        plan_ipp_change, plan_ipp_change_with_supported_values, set_printer_attributes_request,
        validate_confirmed_ipp_change,
    },
    discovery::{ProtocolFamily, SettingValue},
    ipp::{self, Attribute, AttributeGroup, Message, Value, ValueData, ValueTag, Version},
};

fn observation(location: &str, settable_tag: ValueTag) -> Message {
    Message {
        version: Version::V2_0,
        code: 0,
        request_id: 1,
        groups: vec![AttributeGroup {
            tag: ipp::PRINTER_ATTRIBUTES_TAG,
            attributes: vec![
                Attribute::new(
                    b"operations-supported".to_vec(),
                    Value::enum_value(i32::from(ipp::SET_PRINTER_ATTRIBUTES)),
                ),
                Attribute::new(
                    b"printer-settable-attributes-supported".to_vec(),
                    Value::raw(settable_tag, b"printer-location"),
                ),
                Attribute::new(
                    b"printer-location".to_vec(),
                    Value::raw(ValueTag::TextWithoutLanguage, location.as_bytes()),
                ),
                Attribute::new(
                    b"printer-location-default".to_vec(),
                    Value::raw(ValueTag::TextWithoutLanguage, location.as_bytes()),
                ),
            ],
        }],
        original_bytes: Vec::new(),
    }
}

fn supported_attribute_observation() -> Message {
    Message {
        version: Version::V2_0,
        code: 0,
        request_id: 1,
        groups: vec![AttributeGroup {
            tag: ipp::PRINTER_ATTRIBUTES_TAG,
            attributes: vec![
                Attribute {
                    name: b"operations-supported".to_vec(),
                    values: vec![
                        Value::enum_value(i32::from(ipp::SET_PRINTER_ATTRIBUTES)),
                        Value::enum_value(i32::from(ipp::GET_PRINTER_SUPPORTED_VALUES)),
                    ],
                },
                Attribute::new(
                    b"printer-settable-attributes-supported".to_vec(),
                    Value::raw(ValueTag::Keyword, b"media-supported"),
                ),
                Attribute::new(
                    b"media-supported".to_vec(),
                    Value::raw(ValueTag::Keyword, b"iso_a4_210x297mm"),
                ),
            ],
        }],
        original_bytes: Vec::new(),
    }
}

fn parsed_supported_values(
    values: Vec<Value>,
) -> mb_printer_core::administration::SupportedPrinterValues {
    parse_get_printer_supported_values(&Message {
        version: Version::V2_0,
        code: 0,
        request_id: 2,
        groups: vec![AttributeGroup {
            tag: ipp::PRINTER_ATTRIBUTES_TAG,
            attributes: vec![Attribute {
                name: b"media-supported".to_vec(),
                values,
            }],
        }],
        original_bytes: Vec::new(),
    })
    .unwrap()
}

fn request() -> PlanChangeRequest<'static> {
    PlanChangeRequest {
        printer_id: "printer-1",
        endpoint_generation: 4,
        setting: "printer-location",
        requested_value: Value::raw(ValueTag::TextWithoutLanguage, b"Workshop"),
        principal: "user-1",
        protocol: ProtocolFamily::Ipp,
        expires_at_unix_ms: 2_000,
    }
}

fn binding(now: u64) -> ChangeBinding<'static> {
    ChangeBinding {
        printer_id: "printer-1",
        endpoint_generation: 4,
        principal: "user-1",
        protocol: ProtocolFamily::Ipp,
        now_unix_ms: now,
    }
}

#[test]
fn plans_and_revalidates_every_confirmation_binding() {
    let observed = observation("Office", ValueTag::Keyword);
    let plan = plan_ipp_change(&observed, request()).unwrap();
    assert_eq!(plan.requested_value, SettingValue::Text("Workshop".into()));
    validate_confirmed_ipp_change(&plan, &observed, binding(1_000)).unwrap();

    let mut rebound = binding(1_000);
    rebound.endpoint_generation = 5;
    assert_eq!(
        validate_confirmed_ipp_change(&plan, &observed, rebound),
        Err(ChangePlanError::StaleBinding("endpoint generation"))
    );
    assert_eq!(
        validate_confirmed_ipp_change(&plan, &observed, binding(2_000)),
        Err(ChangePlanError::Expired)
    );
    assert_eq!(
        validate_confirmed_ipp_change(
            &plan,
            &observation("Warehouse", ValueTag::Keyword),
            binding(1_000)
        ),
        Err(ChangePlanError::StaleValue)
    );
}

#[test]
fn missing_malformed_or_unadvertised_write_support_fails_closed() {
    assert_eq!(
        plan_ipp_change(
            &observation("Office", ValueTag::TextWithoutLanguage),
            request()
        ),
        Err(ChangePlanError::MissingOrMalformedSettableMetadata)
    );
    let mut missing = observation("Office", ValueTag::Keyword);
    missing.groups[0]
        .attributes
        .retain(|attribute| attribute.name != b"printer-settable-attributes-supported");
    assert_eq!(
        plan_ipp_change(&missing, request()),
        Err(ChangePlanError::MissingOrMalformedSettableMetadata)
    );
    let mut unsupported = observation("Office", ValueTag::Keyword);
    unsupported.groups[0]
        .attributes
        .retain(|attribute| attribute.name != b"operations-supported");
    assert_eq!(
        plan_ipp_change(&unsupported, request()),
        Err(ChangePlanError::OperationNotAdvertised)
    );
}

#[test]
fn defaults_do_not_make_an_attribute_settable() {
    let mut observed = observation("Office", ValueTag::Keyword);
    observed.groups[0]
        .attributes
        .retain(|attribute| attribute.name != b"printer-settable-attributes-supported");
    assert!(
        observed.groups[0]
            .attributes
            .iter()
            .any(|attribute| attribute.name == b"printer-location-default")
    );
    assert!(plan_ipp_change(&observed, request()).is_err());
}

#[test]
fn set_request_is_protocol_scoped_and_contains_only_the_confirmed_attribute() {
    let request = set_printer_attributes_request(
        "ipp://printer/ipp/print",
        "printer-location",
        Value::raw(ValueTag::TextWithoutLanguage, b"Workshop"),
        9,
    );
    assert_eq!(request.code, ipp::SET_PRINTER_ATTRIBUTES);
    assert_eq!(request.groups.len(), 2);
    assert_eq!(request.groups[1].attributes.len(), 1);
    assert_eq!(request.groups[1].attributes[0].name, b"printer-location");
}

#[test]
fn rfc_3380_supported_values_preserve_admin_define_and_unsupported_results() {
    let response = Message {
        version: Version::V2_0,
        code: 0,
        request_id: 4,
        groups: vec![
            AttributeGroup {
                tag: ipp::PRINTER_ATTRIBUTES_TAG,
                attributes: vec![Attribute {
                    name: b"media-supported".to_vec(),
                    values: vec![
                        Value::raw(ValueTag::Keyword, b"iso_a4_210x297mm"),
                        Value {
                            tag: ValueTag::AdminDefine,
                            data: mb_printer_core::ipp::ValueData::OutOfBand,
                        },
                    ],
                }],
            },
            AttributeGroup {
                tag: ipp::UNSUPPORTED_ATTRIBUTES_TAG,
                attributes: vec![Attribute::new(
                    b"finishings-supported".to_vec(),
                    Value {
                        tag: ValueTag::Unsupported,
                        data: mb_printer_core::ipp::ValueData::OutOfBand,
                    },
                )],
            },
        ],
        original_bytes: Vec::new(),
    };
    let parsed = parse_get_printer_supported_values(&response).unwrap();
    assert_eq!(parsed.attributes["media-supported"].len(), 2);
    assert_eq!(
        parsed.attributes["media-supported"][1].tag,
        ValueTag::AdminDefine
    );
    assert_eq!(parsed.unsupported_attributes, ["finishings-supported"]);
}

#[test]
fn supported_values_request_rejects_generic_or_implicit_attribute_queries() {
    assert!(
        ipp::get_printer_supported_values_request(
            "ipp://printer/ipp/print",
            ["media-supported"],
            1
        )
        .is_ok()
    );
    assert!(
        ipp::get_printer_supported_values_request(
            "ipp://printer/ipp/print",
            ["printer-location"],
            1
        )
        .is_err()
    );
    assert!(
        ipp::get_printer_supported_values_request("ipp://printer/ipp/print", ["all"], 1).is_err()
    );
}

#[test]
fn rfc_3380_constraints_fail_closed_and_admin_define_accepts_only_name_syntax() {
    let observation = supported_attribute_observation();
    let request_for = |value| PlanChangeRequest {
        printer_id: "printer-1",
        endpoint_generation: 1,
        setting: "media-supported",
        requested_value: value,
        principal: "admin-1",
        protocol: ProtocolFamily::Ipp,
        expires_at_unix_ms: 2_000,
    };
    assert_eq!(
        plan_ipp_change(
            &observation,
            request_for(Value::raw(ValueTag::Keyword, b"iso_a4_210x297mm"))
        ),
        Err(ChangePlanError::SupportedValuesRequired)
    );

    let explicit =
        parsed_supported_values(vec![Value::raw(ValueTag::Keyword, b"iso_a4_210x297mm")]);
    assert!(
        plan_ipp_change_with_supported_values(
            &observation,
            Some(&explicit),
            request_for(Value::raw(ValueTag::Keyword, b"iso_a4_210x297mm")),
        )
        .is_ok()
    );
    assert_eq!(
        plan_ipp_change_with_supported_values(
            &observation,
            Some(&explicit),
            request_for(Value::raw(ValueTag::Keyword, b"vendor-media")),
        ),
        Err(ChangePlanError::RequestedValueNotSupported)
    );

    let admin_define = parsed_supported_values(vec![Value {
        tag: ValueTag::AdminDefine,
        data: ValueData::OutOfBand,
    }]);
    assert!(
        plan_ipp_change_with_supported_values(
            &observation,
            Some(&admin_define),
            request_for(Value::raw(ValueTag::NameWithoutLanguage, b"custom stock")),
        )
        .is_ok()
    );
    assert_eq!(
        plan_ipp_change_with_supported_values(
            &observation,
            Some(&admin_define),
            request_for(Value::raw(ValueTag::Keyword, b"custom-stock")),
        ),
        Err(ChangePlanError::RequestedValueNotSupported)
    );
}

#[test]
fn default_values_must_match_the_corresponding_supported_attribute() {
    let mut observed = observation("Office", ValueTag::Keyword);
    observed.groups[0]
        .attributes
        .iter_mut()
        .find(|attribute| attribute.name == b"printer-settable-attributes-supported")
        .unwrap()
        .values
        .push(Value::raw(ValueTag::Keyword, b"sides-default"));
    observed.groups[0].attributes.extend([
        Attribute::new(
            b"sides-default".to_vec(),
            Value::raw(ValueTag::Keyword, b"one-sided"),
        ),
        Attribute {
            name: b"sides-supported".to_vec(),
            values: vec![
                Value::raw(ValueTag::Keyword, b"one-sided"),
                Value::raw(ValueTag::Keyword, b"two-sided-long-edge"),
            ],
        },
    ]);
    let request_for = |value: &'static [u8]| PlanChangeRequest {
        printer_id: "printer-1",
        endpoint_generation: 1,
        setting: "sides-default",
        requested_value: Value::raw(ValueTag::Keyword, value),
        principal: "admin-1",
        protocol: ProtocolFamily::Ipp,
        expires_at_unix_ms: 2_000,
    };
    assert!(plan_ipp_change(&observed, request_for(b"two-sided-long-edge")).is_ok());
    assert_eq!(
        plan_ipp_change(&observed, request_for(b"two-sided-short-edge")),
        Err(ChangePlanError::RequestedValueNotSupported)
    );
}
