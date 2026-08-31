// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::discovery::{
    DeviceSetting, DeviceSnapshot, OutputAuthorization, OutputMode, OutputPolicyError,
    ProtocolObservation, ProtocolSource, RawProtocolValue, SettingValue, prepare_snapshot_output,
};

fn snapshot() -> DeviceSnapshot {
    DeviceSnapshot {
        device_settings: vec![DeviceSetting {
            id: "wifi-ssid".into(),
            current_value: Some(SettingValue::Text("private-network".into())),
            sensitive: true,
            evidence: Vec::new(),
        }],
        observations: vec![ProtocolObservation {
            source: ProtocolSource::IppAttribute {
                name: "wifi-ssid".into(),
            },
            values: vec![RawProtocolValue {
                name: b"wifi-ssid".to_vec(),
                tag: Some(0x41),
                value: b"private-network".to_vec(),
                sensitive: true,
            }],
            original_bytes: Some(b"contains-private-network".to_vec()),
            evidence: mb_printer_core::discovery::Evidence {
                source: ProtocolSource::IppAttribute {
                    name: "wifi-ssid".into(),
                },
                kind: mb_printer_core::discovery::EvidenceKind::Advertised,
                origin: mb_printer_core::discovery::ObservationOrigin {
                    agent_id: None,
                    printer_id: "printer-1".into(),
                    endpoint: "ipp://printer/ipp/print".into(),
                    endpoint_generation: 1,
                    transport: mb_printer_core::discovery::TransportKind::Ipp,
                    protocol: mb_printer_core::discovery::ProtocolFamily::Ipp,
                    request_id: "request-1".into(),
                    probe_id: None,
                    observed_at: "now".into(),
                    qualification: None,
                },
            },
        }],
        ..DeviceSnapshot::default()
    }
}

#[test]
fn normalized_default_removes_raw_and_redacts_sensitive_settings() {
    let output = prepare_snapshot_output(
        snapshot(),
        OutputMode::NormalizedRedacted,
        OutputAuthorization::default(),
    )
    .unwrap();
    assert_eq!(
        output.snapshot.device_settings[0].current_value,
        Some(SettingValue::Text("[REDACTED]".into()))
    );
    assert!(output.snapshot.observations[0].values.is_empty());
    assert!(output.snapshot.observations[0].original_bytes.is_none());
}

#[test]
fn raw_and_sensitive_output_require_separate_authorizations() {
    assert_eq!(
        prepare_snapshot_output(
            snapshot(),
            OutputMode::LocalRawRedacted,
            OutputAuthorization::default()
        ),
        Err(OutputPolicyError::RawLocalNotAuthorized)
    );
    assert_eq!(
        prepare_snapshot_output(
            snapshot(),
            OutputMode::LocalRawSensitive,
            OutputAuthorization {
                raw_local: true,
                ..OutputAuthorization::default()
            }
        ),
        Err(OutputPolicyError::SensitiveNotAuthorized)
    );
}

#[test]
fn cloud_raw_is_explicitly_non_persistent_and_non_logging() {
    let output = prepare_snapshot_output(
        snapshot(),
        OutputMode::CloudRawAuthorized,
        OutputAuthorization {
            sensitive_values: true,
            cloud_raw: true,
            ..OutputAuthorization::default()
        },
    )
    .unwrap();
    assert!(!output.retention.may_persist);
    assert!(!output.retention.may_log);
    assert!(output.retention.audit_required);
}

#[test]
fn redacted_raw_drops_response_bytes_when_a_later_observation_is_sensitive() {
    let mut value = snapshot();
    let non_sensitive_evidence = value.observations[0].evidence.clone();
    value.observations.insert(
        0,
        ProtocolObservation {
            source: ProtocolSource::IppAttribute {
                name: "printer-state".into(),
            },
            values: vec![RawProtocolValue {
                name: b"printer-state".to_vec(),
                tag: Some(0x23),
                value: vec![0, 0, 0, 3],
                sensitive: false,
            }],
            original_bytes: Some(b"whole response contains a later secret".to_vec()),
            evidence: non_sensitive_evidence,
        },
    );
    let prepared = prepare_snapshot_output(
        value,
        OutputMode::LocalRawRedacted,
        OutputAuthorization {
            raw_local: true,
            ..OutputAuthorization::default()
        },
    )
    .unwrap();
    assert!(
        prepared
            .snapshot
            .observations
            .iter()
            .all(|observation| observation.original_bytes.is_none())
    );
}
