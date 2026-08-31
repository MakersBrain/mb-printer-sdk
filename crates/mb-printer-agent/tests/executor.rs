// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_agent::{
    AgentExecutor, AgentPolicy, CancellationToken, CloudChangeReceipt, GuardedWritePolicy,
    InitialExecution, ProbeRunOutput, ProbeRunnerFuture, PublishedPrinter, PublishedProbeTarget,
    RegisteredProbeRunner,
};
use mb_printer_agent_proto::v1::{
    ApplyChange, IppInspect, OperationKind, OutputMode, PlanChange, ProtocolOperationLimits,
    ProtocolRequest, RejectionReason, ResultOutcome, RunProbe, protocol_request,
};
use mb_printer_core::{
    discovery::{DeviceSnapshot, ProtocolFamily, TransportKind},
    ipp::{self, Attribute, AttributeGroup, Limits, Message, Value, ValueTag, Version},
    probe::{ProbeLimits, ProbeRequest as PreparedProbeRequest, brother_read_only_registry},
};
use mb_printer_native::transports::ipp::IppEndpoint;
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    net::TcpListener,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

#[derive(Default)]
struct Ieee1284Runner {
    calls: AtomicUsize,
}

impl RegisteredProbeRunner for Ieee1284Runner {
    fn run(&self, request: PreparedProbeRequest, limits: ProbeLimits) -> ProbeRunnerFuture<'_> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            assert_eq!(request, PreparedProbeRequest::Ieee1284DeviceId);
            assert_eq!(limits.timeout_ms, 5_000);
            let text = b"MFG:Brother;MDL:HL-L2375DW;CMD:PJL,PCL;SN:secret;";
            let mut response = Vec::with_capacity(text.len() + 2);
            response.extend_from_slice(&u16::try_from(text.len() + 2).unwrap().to_be_bytes());
            response.extend_from_slice(text);
            Ok(ProbeRunOutput {
                response,
                duration_ms: 7,
            })
        })
    }
}

fn policy() -> AgentPolicy {
    AgentPolicy {
        agent_id: "agent-1".into(),
        contract_version: 1,
        maximum_timeout_ms: 5_000,
        maximum_response_bytes: 1024 * 1024,
        allow_cloud_raw_redacted: false,
        allow_cloud_raw_sensitive: false,
    }
}

fn request() -> ProtocolRequest {
    ProtocolRequest {
        request_id: "request-1".into(),
        contract_version: 1,
        authenticated_principal: "principal-1".into(),
        printer_id: "printer-1".into(),
        endpoint_generation: 4,
        expires_at_unix_ms: 2_000,
        limits: Some(ProtocolOperationLimits {
            timeout_ms: 1_000,
            maximum_response_bytes: 64 * 1024,
        }),
        operation: Some(protocol_request::Operation::IppInspect(IppInspect {
            requested_attributes: vec!["all".into()],
            document_format: None,
            output_mode: OutputMode::NormalizedRedacted as i32,
        })),
    }
}

fn server() -> (IppEndpoint, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        let header_end = loop {
            let count = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..count]);
            if let Some(offset) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap();
        while request.len() < header_end + content_length {
            let count = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..count]);
        }
        let request = ipp::decode(
            &request[header_end..header_end + content_length],
            Limits::default(),
        )
        .unwrap();
        let response = Message {
            version: Version::V2_0,
            code: 0,
            request_id: request.request_id,
            groups: vec![AttributeGroup {
                tag: ipp::PRINTER_ATTRIBUTES_TAG,
                attributes: vec![
                    Attribute::new(
                        b"printer-uuid".to_vec(),
                        Value::raw(ValueTag::Uri, b"urn:uuid:sensitive-device-id"),
                    ),
                    Attribute::new(b"printer-state".to_vec(), Value::enum_value(3)),
                ],
            }],
            original_bytes: Vec::new(),
        }
        .encode(Limits::default())
        .unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.len()
        )
        .unwrap();
        stream.write_all(&response).unwrap();
    });
    (IppEndpoint::ipp("127.0.0.1", port, "/ipp/print"), handle)
}

fn plan_server() -> (IppEndpoint, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        let header_end = loop {
            let count = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..count]);
            if let Some(offset) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap();
        while request.len() < header_end + content_length {
            let count = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..count]);
        }
        let request = ipp::decode(
            &request[header_end..header_end + content_length],
            Limits::default(),
        )
        .unwrap();
        let response = Message {
            version: Version::V2_0,
            code: 0,
            request_id: request.request_id,
            groups: vec![AttributeGroup {
                tag: ipp::PRINTER_ATTRIBUTES_TAG,
                attributes: vec![
                    Attribute::new(
                        b"operations-supported".to_vec(),
                        Value::enum_value(i32::from(ipp::SET_PRINTER_ATTRIBUTES)),
                    ),
                    Attribute::new(
                        b"printer-settable-attributes-supported".to_vec(),
                        Value::raw(ValueTag::Keyword, b"printer-location"),
                    ),
                    Attribute::new(
                        b"printer-location".to_vec(),
                        Value::raw(ValueTag::TextWithoutLanguage, b"old location"),
                    ),
                ],
            }],
            original_bytes: Vec::new(),
        }
        .encode(Limits::default())
        .unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.len()
        )
        .unwrap();
        stream.write_all(&response).unwrap();
    });
    (IppEndpoint::ipp("127.0.0.1", port, "/ipp/print"), handle)
}

#[test]
fn capabilities_and_initial_execution_are_typed_bounded_and_redacted() {
    let (endpoint, server) = server();
    let executor = AgentExecutor::new(policy()).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 4,
            endpoint,
        })
        .unwrap();
    let capabilities = executor.capabilities();
    assert_eq!(capabilities.operations, [OperationKind::IppInspect as i32]);
    assert_eq!(capabilities.printers[0].printer_id, "printer-1");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let execution =
        runtime.block_on(executor.execute_initial(request(), 1_000, CancellationToken::default()));
    server.join().unwrap();
    let InitialExecution::Accepted { accepted, result } = execution else {
        panic!("request should be accepted");
    };
    assert_eq!(accepted.request_id, "request-1");
    assert_eq!(result.outcome, ResultOutcome::Succeeded as i32);
    assert!(result.logging_allowed);
    let snapshot: DeviceSnapshot = serde_json::from_slice(&result.bounded_response).unwrap();
    assert!(
        snapshot
            .identity
            .uuid
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256:"))
    );
    assert!(
        snapshot
            .observations
            .iter()
            .all(|value| value.values.is_empty())
    );
    assert!(
        snapshot
            .observations
            .iter()
            .all(|value| value.original_bytes.is_none())
    );
    assert_eq!(result.evidence[0].agent_id, "agent-1");
    assert_eq!(result.evidence[0].printer_id, "printer-1");
    assert!(result.evidence[0].endpoint.starts_with("sha256:"));
}

#[test]
fn stale_endpoint_and_cancelled_requests_terminate_without_io() {
    let executor = AgentExecutor::new(policy()).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 5,
            endpoint: IppEndpoint::ipp("127.0.0.1", 9, "/ipp/print"),
        })
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let stale =
        runtime.block_on(executor.execute_initial(request(), 1_000, CancellationToken::default()));
    let InitialExecution::Rejected(rejected) = stale else {
        panic!("stale endpoint must be rejected");
    };
    assert_eq!(rejected.reason, RejectionReason::StaleEndpoint as i32);

    let mut cancelled_request = request();
    cancelled_request.endpoint_generation = 5;
    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let cancelled =
        runtime.block_on(executor.execute_initial(cancelled_request, 1_000, cancellation));
    let InitialExecution::Accepted { result, .. } = cancelled else {
        panic!("valid request is accepted before cancellation result");
    };
    assert_eq!(result.outcome, ResultOutcome::Cancelled as i32);
}

#[test]
fn cloud_raw_modes_require_dedicated_agent_policy() {
    let executor = AgentExecutor::new(policy()).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 4,
            endpoint: IppEndpoint::ipp("127.0.0.1", 9, "/ipp/print"),
        })
        .unwrap();
    let mut raw = request();
    let Some(protocol_request::Operation::IppInspect(operation)) = &mut raw.operation else {
        unreachable!()
    };
    operation.output_mode = OutputMode::RawSensitive as i32;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let execution =
        runtime.block_on(executor.execute_initial(raw, 1_000, CancellationToken::default()));
    let InitialExecution::Rejected(rejected) = execution else {
        panic!("raw output should be rejected by default");
    };
    assert_eq!(rejected.reason, RejectionReason::Unauthorized as i32);
}

#[test]
fn authorized_cloud_raw_redacted_is_ephemeral_and_hides_sensitive_values() {
    let (endpoint, server) = server();
    let mut agent_policy = policy();
    agent_policy.allow_cloud_raw_redacted = true;
    let executor = AgentExecutor::new(agent_policy).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 4,
            endpoint,
        })
        .unwrap();
    let mut raw = request();
    let Some(protocol_request::Operation::IppInspect(operation)) = &mut raw.operation else {
        unreachable!()
    };
    operation.output_mode = OutputMode::RawRedacted as i32;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let execution =
        runtime.block_on(executor.execute_initial(raw, 1_000, CancellationToken::default()));
    server.join().unwrap();
    let InitialExecution::Accepted { result, .. } = execution else {
        panic!("authorized raw response should be accepted");
    };
    assert_eq!(result.outcome, ResultOutcome::Succeeded as i32);
    assert!(!result.persistence_allowed);
    assert!(!result.logging_allowed);
    let snapshot: DeviceSnapshot = serde_json::from_slice(&result.bounded_response).unwrap();
    assert!(
        snapshot
            .observations
            .iter()
            .any(|value| !value.values.is_empty())
    );
    assert!(snapshot.observations.iter().all(|observation| {
        observation
            .values
            .iter()
            .filter(|value| value.sensitive)
            .all(|value| value.value == b"[REDACTED]")
    }));
    assert!(snapshot.observations.iter().all(|observation| {
        !observation.values.iter().any(|value| value.sensitive)
            || observation.original_bytes.is_none()
    }));
}

#[test]
fn guarded_cloud_planning_preserves_every_authority_binding() {
    let (endpoint, server) = plan_server();
    let executor = AgentExecutor::new(policy()).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 4,
            endpoint,
        })
        .unwrap();
    let requested_value =
        serde_json::to_vec(&Value::raw(ValueTag::TextWithoutLanguage, b"new location")).unwrap();
    let mut change = request();
    change.operation = Some(protocol_request::Operation::PlanChange(PlanChange {
        setting_id: "printer-location".into(),
        requested_value,
        protocol: "ipp".into(),
    }));
    let write_policy = GuardedWritePolicy {
        allowed_settings: BTreeSet::from(["printer-location".into()]),
    };
    assert!(
        executor
            .guarded_ipp_capabilities(&write_policy)
            .operations
            .contains(&(OperationKind::ApplyChange as i32))
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let execution =
        runtime.block_on(executor.execute_guarded_ipp_change(change, 1_000, &write_policy));
    server.join().unwrap();
    let InitialExecution::Accepted { result, .. } = execution else {
        panic!("authorized plan should be accepted")
    };
    assert_eq!(result.outcome, ResultOutcome::Succeeded as i32);
    assert!(!result.persistence_allowed);
    assert!(!result.logging_allowed);
    let plan: CloudChangeReceipt = serde_json::from_slice(&result.bounded_response).unwrap();
    assert_eq!(plan.printer_id, "printer-1");
    assert_eq!(plan.endpoint_generation, 4);
    assert_eq!(plan.principal, "principal-1");
    assert_eq!(
        plan.protocol,
        mb_printer_core::discovery::ProtocolFamily::Ipp
    );
    assert_eq!(plan.expires_at_unix_ms, 2_000);
    assert_eq!(plan.setting, "printer-location");
    assert_ne!(plan.expected_requested_value_hash, [0; 32]);
    assert!(!String::from_utf8_lossy(&result.bounded_response).contains("new location"));
}

#[test]
fn guarded_apply_rejects_a_changed_requested_value_before_io() {
    let executor = AgentExecutor::new(policy()).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 4,
            endpoint: IppEndpoint::ipp("127.0.0.1", 9, "/ipp/print"),
        })
        .unwrap();
    let changed_value =
        serde_json::to_vec(&Value::raw(ValueTag::TextWithoutLanguage, b"changed")).unwrap();
    let mut apply = request();
    apply.operation = Some(protocol_request::Operation::ApplyChange(ApplyChange {
        setting_id: "printer-location".into(),
        expected_old_value_hash: vec![7; 32],
        requested_value: changed_value,
        protocol: "ipp".into(),
        plan_expires_at_unix_ms: 1_900,
        expected_requested_value_hash: vec![8; 32],
    }));
    let write_policy = GuardedWritePolicy {
        allowed_settings: BTreeSet::from(["printer-location".into()]),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let execution =
        runtime.block_on(executor.execute_guarded_ipp_change(apply.clone(), 1_000, &write_policy));
    let InitialExecution::Accepted { result, .. } = execution else {
        panic!("well-formed request is accepted before value validation")
    };
    assert_eq!(result.outcome, ResultOutcome::Rejected as i32);
    assert_eq!(
        result.safe_error,
        "requested value no longer matches the confirmation"
    );
    let replay = runtime.block_on(executor.execute_guarded_ipp_change(apply, 1_000, &write_policy));
    let InitialExecution::Rejected(rejected) = replay else {
        panic!("consumed write request IDs must never replay")
    };
    assert_eq!(rejected.reason, RejectionReason::Policy as i32);
}

#[test]
fn guarded_write_cancellation_before_execution_performs_no_io() {
    let executor = AgentExecutor::new(policy()).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 4,
            endpoint: IppEndpoint::ipp("127.0.0.1", 9, "/ipp/print"),
        })
        .unwrap();
    let value = Value::raw(ValueTag::TextWithoutLanguage, b"new location");
    let mut apply = request();
    apply.request_id = "cancelled-write-1".into();
    apply.operation = Some(protocol_request::Operation::ApplyChange(ApplyChange {
        setting_id: "printer-location".into(),
        expected_old_value_hash: vec![7; 32],
        requested_value: serde_json::to_vec(&value).unwrap(),
        protocol: "ipp".into(),
        plan_expires_at_unix_ms: 1_900,
        expected_requested_value_hash: mb_printer_core::administration::ipp_value_hash(&value)
            .to_vec(),
    }));
    let write_policy = GuardedWritePolicy {
        allowed_settings: BTreeSet::from(["printer-location".into()]),
    };
    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let execution = runtime.block_on(executor.execute_guarded_ipp_change_with_cancellation(
        apply,
        1_000,
        &write_policy,
        cancellation,
    ));
    let InitialExecution::Accepted { result, .. } = execution else {
        panic!("valid request is accepted before cancellation result")
    };
    assert_eq!(result.outcome, ResultOutcome::Cancelled as i32);
}

fn probe_request() -> ProtocolRequest {
    ProtocolRequest {
        request_id: "probe-request-1".into(),
        contract_version: 1,
        authenticated_principal: "principal-1".into(),
        printer_id: "printer-1".into(),
        endpoint_generation: 4,
        expires_at_unix_ms: 10_000,
        limits: Some(ProtocolOperationLimits {
            timeout_ms: 5_000,
            maximum_response_bytes: 64 * 1024,
        }),
        operation: Some(protocol_request::Operation::RunProbe(RunProbe {
            probe_id: "ieee1284.device-id.v1".into(),
        })),
    }
}

fn probe_target(manufacturer: &str) -> PublishedProbeTarget {
    PublishedProbeTarget {
        printer_id: "printer-1".into(),
        endpoint_generation: 4,
        endpoint_identity: "usb:04f9:009e:port-3".into(),
        transport: TransportKind::Usb,
        protocol: ProtocolFamily::Ieee1284,
        manufacturer: Some(manufacturer.into()),
        model: Some("HL-L2375DW".into()),
        firmware: Some("1.72".into()),
        printer_definition: None,
    }
}

#[test]
fn registered_probe_is_typed_qualified_bounded_and_redacted() {
    let registry = brother_read_only_registry();
    let executor = AgentExecutor::new(policy()).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 4,
            endpoint: IppEndpoint::ipp("127.0.0.1", 9, "/ipp/print"),
        })
        .unwrap();
    let capabilities = executor.registered_probe_capabilities(&registry);
    assert!(
        capabilities
            .operations
            .contains(&(OperationKind::RunProbe as i32))
    );
    let runner = Ieee1284Runner::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let execution = runtime.block_on(executor.execute_registered_probe(
        probe_request(),
        1_000,
        &registry,
        &probe_target("Brother"),
        &runner,
        CancellationToken::default(),
    ));
    let InitialExecution::Accepted { result, .. } = execution else {
        panic!("qualified registered probe should be accepted")
    };
    assert_eq!(runner.calls.load(Ordering::Relaxed), 1);
    assert_eq!(result.outcome, ResultOutcome::Succeeded as i32);
    assert!(!result.persistence_allowed);
    assert!(!result.logging_allowed);
    let report: serde_json::Value = serde_json::from_slice(&result.bounded_response).unwrap();
    assert_eq!(report["probeId"], "ieee1284.device-id.v1");
    assert_eq!(report["configurationChanged"], false);
    assert_eq!(report["result"]["value"]["raw"], "[REDACTED]");
    assert!(report["result"]["value"]["fields"].get("SN").is_none());
    assert_eq!(report["origin"]["qualification"]["firmware"], "1.72");
    assert!(
        report["endpoint"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:"))
    );
    assert_eq!(report["endpoint"], report["origin"]["endpoint"]);
    assert!(
        report["responseHash"]
            .as_str()
            .is_some_and(|value| value.len() == 64)
    );
}

#[test]
fn unqualified_or_cancelled_registered_probe_never_reaches_hardware() {
    let registry = brother_read_only_registry();
    let executor = AgentExecutor::new(policy()).unwrap();
    executor
        .publish(PublishedPrinter {
            printer_id: "printer-1".into(),
            endpoint_generation: 4,
            endpoint: IppEndpoint::ipp("127.0.0.1", 9, "/ipp/print"),
        })
        .unwrap();
    let runner = Ieee1284Runner::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let unqualified = runtime.block_on(executor.execute_registered_probe(
        probe_request(),
        1_000,
        &registry,
        &probe_target("Other"),
        &runner,
        CancellationToken::default(),
    ));
    let InitialExecution::Rejected(rejected) = unqualified else {
        panic!("unqualified target must be rejected")
    };
    assert_eq!(rejected.reason, RejectionReason::Policy as i32);

    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let cancelled = runtime.block_on(executor.execute_registered_probe(
        probe_request(),
        1_000,
        &registry,
        &probe_target("Brother"),
        &runner,
        cancellation,
    ));
    let InitialExecution::Accepted { result, .. } = cancelled else {
        panic!("qualified request is accepted before cancellation result")
    };
    assert_eq!(result.outcome, ResultOutcome::Cancelled as i32);
    assert_eq!(runner.calls.load(Ordering::Relaxed), 0);
}
