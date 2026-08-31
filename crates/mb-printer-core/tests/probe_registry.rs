// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    discovery::{ObservationOrigin, ProtocolFamily, TransportKind},
    probe::{
        ProbeDefinition, ProbeId, ProbeKind, ProbeLimits, ProbeQualification, ProbeRegistry,
        ProbeRequest, ProbeResponse, ProbeRisk, RegisteredPjlVariable, RegistryError,
        brother_read_only_registry, build_read_only_probe_report, decode_registered_response,
        prepare_registered_probe,
    },
};

fn definition() -> ProbeDefinition {
    ProbeDefinition {
        id: ProbeId::from("brother.raster-status.v1"),
        kind: ProbeKind::BrotherRasterStatus,
        protocols: vec![ProtocolFamily::Brother],
        transports: vec![TransportKind::Usb, TransportKind::RawTcp],
        risk: ProbeRisk::ReadOnly,
        limits: ProbeLimits {
            timeout_ms: 3_000,
            idle_timeout_ms: 0,
            maximum_response_bytes: 32,
        },
        qualification: ProbeQualification {
            qualification_id: "brother-status-qualification-v1".into(),
            manufacturers: vec!["Brother".into()],
            models: vec!["HL-L2375DW".into()],
            firmware_versions: Vec::new(),
        },
    }
}

#[test]
fn registered_kinds_select_protocol_owned_encoders_and_decoders() {
    let registry = brother_read_only_registry();
    let report_id = ProbeId::from("brother.system-report.v1");
    let ProbeRequest::ProtocolPlan { plan } =
        prepare_registered_probe(&registry, &report_id, None).unwrap()
    else {
        panic!("system report must use its reviewed protocol plan")
    };
    assert!(plan.actions.iter().any(|action| matches!(
        action,
        mb_printer_core::protocol::Action::CommandWrite { bytes, .. }
            if bytes == mb_printer_core::protocol::brother::report::SYSTEM_REPORT_COMMAND
    )));

    let device_id = b"\x00\x1dMFG:Brother;MDL:QL-1110NWB;";
    let decoded = decode_registered_response(
        &registry,
        &ProbeId::from("ieee1284.device-id.v1"),
        device_id,
    )
    .unwrap();
    let ProbeResponse::Ieee1284DeviceId(decoded) = decoded else {
        panic!("wrong concrete decoder")
    };
    assert_eq!(decoded.manufacturer.as_deref(), Some("Brother"));
}

#[test]
fn registry_rejects_pjl_command_injection() {
    let mut malicious = definition();
    malicious.id = ProbeId::from("pjl.unsafe");
    malicious.kind = ProbeKind::PjlDinquire {
        variable: RegisteredPjlVariable("COPIES\r\n@PJL DEFAULT PASSWORD=bad".into()),
    };
    assert_eq!(
        ProbeRegistry::default().register(malicious),
        Err(RegistryError::InvalidPjlVariable)
    );
}

#[test]
fn registered_response_limit_is_enforced_before_decoding() {
    let registry = brother_read_only_registry();
    assert!(matches!(
        decode_registered_response(
            &registry,
            &ProbeId::from("brother.raster-status.v1"),
            &[0; 33],
        ),
        Err(mb_printer_core::probe::ProbeCodecError::ResponseTooLarge)
    ));
}

#[test]
fn execution_report_hashes_raw_evidence_and_redacts_device_identifiers() {
    let registry = brother_read_only_registry();
    let id = ProbeId::from("ieee1284.device-id.v1");
    let body = b"MFG:Brother;MDL:QL-1110NWB;SERIALNUMBER:secret-serial;";
    let mut response = Vec::from(((body.len() + 2) as u16).to_be_bytes());
    response.extend_from_slice(body);
    let report = build_read_only_probe_report(
        &registry,
        &id,
        &response,
        ObservationOrigin {
            agent_id: Some("agent-1".into()),
            printer_id: "printer-1".into(),
            endpoint: "usb:04f9:209b:device-1".into(),
            endpoint_generation: 3,
            transport: TransportKind::Usb,
            protocol: ProtocolFamily::Ieee1284,
            request_id: "probe-request-1".into(),
            probe_id: None,
            observed_at: "2026-08-31T12:00:00Z".into(),
            qualification: None,
        },
        12,
    )
    .unwrap();
    assert!(!report.configuration_changed);
    assert_eq!(report.response_bytes, response.len());
    assert_eq!(report.response_hash.len(), 64);
    assert_eq!(
        report
            .origin
            .qualification
            .as_ref()
            .unwrap()
            .response_hash
            .as_deref(),
        Some(report.response_hash.as_str())
    );
    let ProbeResponse::Ieee1284DeviceId(device_id) = report.result else {
        panic!("wrong result kind")
    };
    assert_eq!(device_id.raw, "[REDACTED]");
    assert!(!device_id.fields.contains_key("SERIALNUMBER"));
}

#[test]
fn registry_is_keyed_by_id_and_rejects_duplicates() {
    let mut registry = ProbeRegistry::default();
    registry.register(definition()).unwrap();
    assert!(
        registry
            .get(&ProbeId::from("brother.raster-status.v1"))
            .is_some()
    );
    assert_eq!(
        registry.register(definition()),
        Err(RegistryError::Duplicate("brother.raster-status.v1".into()))
    );
}

#[test]
fn automatic_registry_rejects_state_changing_or_unqualified_probes() {
    let mut unsafe_probe = definition();
    unsafe_probe.risk = ProbeRisk::ConfigurationWrite;
    assert_eq!(
        ProbeRegistry::default().register(unsafe_probe),
        Err(RegistryError::UnsafeAutomaticProbe)
    );
    let mut unqualified = definition();
    unqualified.qualification.qualification_id.clear();
    assert_eq!(
        ProbeRegistry::default().register(unqualified),
        Err(RegistryError::UnsafeAutomaticProbe)
    );
}

#[test]
fn applicability_requires_protocol_transport_and_qualification_match() {
    let definition = definition();
    assert!(definition.applies_to(
        ProtocolFamily::Brother,
        TransportKind::Usb,
        Some("Brother"),
        Some("HL-L2375DW"),
        Some("1.0")
    ));
    assert!(!definition.applies_to(
        ProtocolFamily::Brother,
        TransportKind::Serial,
        Some("Brother"),
        Some("HL-L2375DW"),
        None
    ));
    assert!(!definition.applies_to(
        ProtocolFamily::Brother,
        TransportKind::Usb,
        Some("Brother"),
        Some("Different model"),
        None
    ));
}
