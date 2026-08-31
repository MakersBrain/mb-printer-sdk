// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_agent_proto::{
    ContractError,
    v1::{
        IppInspect, OperationKind, ProtocolOperationLimits, ProtocolRequest, ReadSetting,
        protocol_request,
    },
    validate_initial_release_request, validate_request,
};
use prost::Message as _;

fn request(operation: protocol_request::Operation) -> ProtocolRequest {
    ProtocolRequest {
        request_id: "request-1".into(),
        contract_version: 1,
        authenticated_principal: "principal-1".into(),
        printer_id: "published-printer-1".into(),
        endpoint_generation: 4,
        expires_at_unix_ms: 2_000,
        limits: Some(ProtocolOperationLimits {
            timeout_ms: 1_000,
            maximum_response_bytes: 65_536,
        }),
        operation: Some(operation),
    }
}

#[test]
fn protobuf_round_trip_preserves_short_lived_typed_request() {
    let request = request(protocol_request::Operation::IppInspect(IppInspect {
        requested_attributes: vec!["all".into()],
        document_format: None,
        output_mode: 0,
    }));
    let decoded = ProtocolRequest::decode(request.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, request);
    validate_request(
        &decoded,
        1_000,
        5_000,
        1024 * 1024,
        &[OperationKind::IppInspect],
    )
    .unwrap();
    validate_initial_release_request(&decoded).unwrap();
}

#[test]
fn unsupported_version_limits_expiry_and_operations_fail_closed() {
    let mut invalid = request(protocol_request::Operation::IppInspect(
        IppInspect::default(),
    ));
    invalid.contract_version = 0;
    assert_eq!(
        validate_request(
            &invalid,
            1_000,
            5_000,
            1024 * 1024,
            &[OperationKind::IppInspect]
        ),
        Err(ContractError::MissingIdentity)
    );

    let expired = request(protocol_request::Operation::IppInspect(
        IppInspect::default(),
    ));
    assert_eq!(
        validate_request(
            &expired,
            2_000,
            5_000,
            1024 * 1024,
            &[OperationKind::IppInspect]
        ),
        Err(ContractError::Expired)
    );

    let setting = request(protocol_request::Operation::ReadSetting(ReadSetting {
        setting_id: "printer-location".into(),
    }));
    assert_eq!(
        validate_request(
            &setting,
            1_000,
            5_000,
            1024 * 1024,
            &[OperationKind::IppInspect]
        ),
        Err(ContractError::UnsupportedOperation)
    );
    assert_eq!(
        validate_initial_release_request(&setting),
        Err(ContractError::InitialReleaseIppInspectOnly)
    );
}

#[test]
fn contract_has_printer_id_but_no_arbitrary_endpoint_or_request_bytes() {
    let descriptor = include_str!("../proto/agent_session.proto");
    let request_block = descriptor
        .split("message ProtocolRequest {")
        .nth(1)
        .unwrap()
        .split("message IppInspect")
        .next()
        .unwrap();
    assert!(request_block.contains("string printer_id"));
    assert!(!request_block.contains("string endpoint ="));
    assert!(!request_block.contains("request_bytes"));
    assert!(!request_block.contains("host"));
    assert!(!request_block.contains("port"));
}
