// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    discovery::{
        MutationAccess, ObservationOrigin, ProtocolFamily, SettingValue, TransportKind,
        normalize_ipp,
    },
    ipp::{self, Attribute, AttributeGroup, Message, Value, ValueTag, Version},
};

fn origin() -> ObservationOrigin {
    ObservationOrigin {
        agent_id: Some("agent-1".into()),
        printer_id: "printer-1".into(),
        endpoint: "ipp://printer/ipp/print".into(),
        endpoint_generation: 7,
        transport: TransportKind::Ipp,
        protocol: ProtocolFamily::Ipp,
        request_id: "request-1".into(),
        probe_id: None,
        observed_at: "2026-08-31T12:00:00Z".into(),
        qualification: None,
    }
}

fn response(settable_tag: ValueTag) -> Message {
    Message {
        version: Version::V2_0,
        code: 0,
        request_id: 1,
        groups: vec![AttributeGroup {
            tag: ipp::PRINTER_ATTRIBUTES_TAG,
            attributes: vec![
                Attribute::new(
                    b"printer-uuid".to_vec(),
                    Value::raw(ValueTag::Uri, b"urn:uuid:device-1"),
                ),
                Attribute::new(
                    b"printer-make-and-model".to_vec(),
                    Value::raw(ValueTag::TextWithoutLanguage, b"Brother HL-L2375DW"),
                ),
                Attribute {
                    name: b"operations-supported".to_vec(),
                    values: vec![
                        Value::enum_value(i32::from(ipp::GET_PRINTER_ATTRIBUTES)),
                        Value::enum_value(i32::from(ipp::SET_PRINTER_ATTRIBUTES)),
                    ],
                },
                Attribute::new(
                    b"printer-settable-attributes-supported".to_vec(),
                    Value::raw(settable_tag, b"printer-location"),
                ),
                Attribute::new(
                    b"printer-location".to_vec(),
                    Value::raw(ValueTag::TextWithoutLanguage, b"Office"),
                ),
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
                Attribute::new(
                    b"x-secret-ssid".to_vec(),
                    Value::raw(ValueTag::TextWithoutLanguage, b"private-network"),
                ),
                Attribute {
                    name: b"marker-names".to_vec(),
                    values: vec![Value::raw(ValueTag::NameWithoutLanguage, b"Black Toner")],
                },
                Attribute {
                    name: b"marker-levels".to_vec(),
                    values: vec![Value::integer(73)],
                },
                Attribute::new(
                    b"printer-firmware-string-version".to_vec(),
                    Value::raw(ValueTag::TextWithoutLanguage, b"1.23"),
                ),
            ],
        }],
        original_bytes: vec![2, 0, 0, 0, 0, 0, 0, 1, 3],
    }
}

#[test]
fn normalizes_capabilities_without_losing_protocol_observations() {
    let snapshot = normalize_ipp(
        &response(ValueTag::Keyword),
        &origin(),
        Some("application/pdf"),
    );
    assert_eq!(snapshot.identity.uuid.as_deref(), Some("urn:uuid:device-1"));
    assert_eq!(snapshot.job_capabilities.len(), 1);
    assert_eq!(snapshot.job_capabilities[0].id, "sides");
    assert_eq!(
        snapshot.job_capabilities[0].current_default,
        Some(SettingValue::Keyword("one-sided".into()))
    );
    assert_eq!(
        snapshot.job_capabilities[0].format_scope.as_deref(),
        Some("application/pdf")
    );
    assert_eq!(
        snapshot.mutation_support[0].access,
        MutationAccess::ConfirmedWrite
    );
    assert_eq!(snapshot.observations.len(), 11);
    assert_eq!(snapshot.supplies[0].id, "Black Toner");
    assert_eq!(snapshot.supplies[0].level_percent, Some(73));
    assert!(
        snapshot
            .device_settings
            .iter()
            .any(|setting| setting.id == "printer-firmware-string-version")
    );
    assert!(snapshot.observations.iter().any(|observation| {
        observation.values.iter().any(|value| value.sensitive)
            && observation.original_bytes.as_deref() == Some(&[2, 0, 0, 0, 0, 0, 0, 1, 3])
    }));
}

#[test]
fn malformed_or_missing_settable_metadata_fails_closed() {
    let malformed = normalize_ipp(&response(ValueTag::TextWithoutLanguage), &origin(), None);
    assert_eq!(
        malformed.mutation_support[0].access,
        MutationAccess::ReadOnly
    );

    let mut missing = response(ValueTag::Keyword);
    missing.groups[0]
        .attributes
        .retain(|attribute| attribute.name != b"printer-settable-attributes-supported");
    let missing = normalize_ipp(&missing, &origin(), None);
    assert_eq!(missing.mutation_support[0].access, MutationAccess::ReadOnly);
}

#[test]
fn defaults_never_imply_writability() {
    let mut message = response(ValueTag::Keyword);
    message.groups[0]
        .attributes
        .retain(|attribute| attribute.name != b"operations-supported");
    let snapshot = normalize_ipp(&message, &origin(), None);
    assert_eq!(
        snapshot.mutation_support[0].access,
        MutationAccess::ReadOnly
    );
}
