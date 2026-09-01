// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::discovery::{
    ObservationOrigin, ProtocolFamily, ProtocolSource, SettingValue, TransportKind, normalize_snmp,
};
use mb_printer_core::snmp::{
    DecodeLimits, ObjectId, ObjectRegistry, ObjectValue, RegisteredObject, Response,
    ResponseEvidence, SnmpError, VarBind, decode_response, encode_get, encode_get_next,
};

fn registry() -> (ObjectRegistry, ObjectId) {
    let oid = ObjectId::parse("1.3.6.1.2.1.43.5.1.1.16.1").unwrap();
    let mut registry = ObjectRegistry::default();
    registry
        .register(RegisteredObject {
            oid: oid.clone(),
            semantic_id: "printer-name".into(),
            sensitive: false,
        })
        .unwrap();
    (registry, oid)
}

#[test]
fn registered_snmp_values_use_protocol_neutral_evidence() {
    let (registry, oid) = registry();
    let response = Response {
        request_id: 42,
        error_status: 0,
        error_index: 0,
        varbinds: vec![VarBind {
            oid: oid.clone(),
            value: ObjectValue::Bytes(b"Office printer".to_vec()),
        }],
        evidence: ResponseEvidence {
            credential_elided_hash: [7; 32],
            original_length: 20,
            sanitized_bytes: Some(b"bounded-snmp-message".to_vec()),
        },
    };
    let snapshot = normalize_snmp(
        &response,
        &registry,
        &ObservationOrigin {
            agent_id: Some("agent-1".into()),
            printer_id: "printer-1".into(),
            endpoint: "192.0.2.4:161".into(),
            endpoint_generation: 2,
            transport: TransportKind::Snmp,
            protocol: ProtocolFamily::Snmp,
            request_id: "snmp-1".into(),
            probe_id: Some("snmp.printer-name.v1".into()),
            observed_at: "2026-08-31T00:00:00Z".into(),
            qualification: None,
        },
    );
    assert_eq!(snapshot.identity.printer_id, "printer-1");
    assert_eq!(
        snapshot.device_settings[0].current_value,
        Some(SettingValue::Text("Office printer".into()))
    );
    assert!(matches!(
        snapshot.observations[0].source,
        ProtocolSource::SnmpObject { .. }
    ));
    assert_eq!(
        snapshot.observations[0].original_bytes.as_deref(),
        Some(b"bounded-snmp-message".as_slice())
    );
}

#[test]
fn only_registered_read_oids_can_be_encoded() {
    let (registry, oid) = registry();
    let get = encode_get(&registry, b"private", 42, &oid).unwrap();
    let next = encode_get_next(&registry, b"private", 42, &oid).unwrap();
    assert!(get.contains(&0xa0));
    assert!(next.contains(&0xa1));
    assert_eq!(
        encode_get(
            &registry,
            b"private",
            42,
            &ObjectId::parse("1.3.6.1.4.1.99999").unwrap(),
        ),
        Err(SnmpError::UnregisteredObject)
    );
}

#[test]
fn bounded_response_decoder_checks_correlation_and_preserves_unknown_values() {
    let (registry, oid) = registry();
    let mut response = encode_get(&registry, b"private", 42, &oid).unwrap();
    let pdu = response.iter().position(|byte| *byte == 0xa0).unwrap();
    response[pdu] = 0xa2;
    let decoded = decode_response(&response, 42, DecodeLimits::default()).unwrap();
    assert_eq!(decoded.varbinds[0].oid, oid);
    assert_eq!(decoded.varbinds[0].value, ObjectValue::Null);
    let sanitized = decoded.evidence.sanitized_bytes.as_deref().unwrap();
    assert!(
        !sanitized
            .windows(b"private".len())
            .any(|value| value == b"private")
    );
    assert_eq!(
        decode_response(&response, 43, DecodeLimits::default()),
        Err(SnmpError::RequestIdMismatch)
    );
    assert_eq!(
        decode_response(
            &response,
            42,
            DecodeLimits {
                maximum_message_bytes: response.len() - 1,
                ..DecodeLimits::default()
            },
        ),
        Err(SnmpError::LimitExceeded)
    );
}
