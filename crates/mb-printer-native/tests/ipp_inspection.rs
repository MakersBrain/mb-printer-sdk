// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "ipp")]

use mb_printer_core::ipp::{
    self, Attribute, AttributeGroup, Limits, Message, Value, ValueTag, Version,
};
use mb_printer_core::{
    administration::{ChangeBinding, PlanChangeRequest, plan_ipp_change},
    discovery::{ObservationOrigin, ProtocolFamily, TransportKind},
};
use mb_printer_native::transports::ipp::{
    ApplyChangeOutcome, AsyncIppOverUsbBackend, DiscoveryOptions, InspectLimits, IppClient,
    IppClientError, IppEndpoint, IppOverUsbClient, IppOverUsbError, IppOverUsbFuture,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Default)]
struct FakeIppOverUsb {
    calls: AtomicUsize,
    oversized: bool,
}

impl AsyncIppOverUsbBackend for FakeIppOverUsb {
    fn transact(&self, request: Vec<u8>, maximum_response_bytes: usize) -> IppOverUsbFuture<'_> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let oversized = self.oversized;
        Box::pin(async move {
            let request = ipp::decode(&request, Limits::default()).unwrap();
            if oversized {
                return Ok(vec![0; maximum_response_bytes + 1]);
            }
            Message {
                version: Version::V2_0,
                code: 0,
                request_id: request.request_id,
                groups: vec![AttributeGroup {
                    tag: ipp::PRINTER_ATTRIBUTES_TAG,
                    attributes: vec![Attribute::new(
                        b"printer-state".to_vec(),
                        Value::enum_value(3),
                    )],
                }],
                original_bytes: Vec::new(),
            }
            .encode(Limits::default())
            .map_err(|error| error.to_string())
        })
    }
}

fn response() -> Vec<u8> {
    Message {
        version: Version::V2_0,
        code: 0,
        request_id: 9,
        groups: vec![AttributeGroup {
            tag: ipp::PRINTER_ATTRIBUTES_TAG,
            attributes: vec![Attribute::new(
                b"x-vendor-setting".to_vec(),
                Value::raw(ValueTag::Extension(0x7f), [0xde, 0xad, 0xbe, 0xef]),
            )],
        }],
        original_bytes: Vec::new(),
    }
    .encode(Limits::default())
    .unwrap()
}

#[test]
fn ipp_over_usb_uses_portable_codec_and_enforces_response_limit() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let request = ipp::get_printer_attributes_request(
        "ipp://localhost/ipp/print",
        ["printer-state"],
        None,
        44,
    );
    let limits = InspectLimits {
        timeout: Duration::from_secs(1),
        maximum_response_bytes: 1024,
        codec: Limits {
            max_message_bytes: 1024,
            ..Limits::default()
        },
    };
    let client = IppOverUsbClient::new(FakeIppOverUsb::default());
    let decoded = runtime.block_on(client.inspect(&request, limits)).unwrap();
    assert_eq!(decoded.request_id, 44);
    assert_eq!(
        decoded.original_bytes,
        decoded.encode(limits.codec).unwrap()
    );

    let oversized = IppOverUsbClient::new(FakeIppOverUsb {
        calls: AtomicUsize::new(0),
        oversized: true,
    });
    assert!(matches!(
        runtime.block_on(oversized.inspect(&request, limits)),
        Err(IppOverUsbError::ResponseTooLarge { limit: 1024 })
    ));
}

fn chunked_server(body: Vec<u8>) -> (IppEndpoint, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 8192];
        let read = stream.read(&mut request).unwrap();
        assert!(
            request[..read]
                .windows(15)
                .any(|part| part == b"application/ipp")
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        stream.write_all(b"\r\n0\r\n\r\n").unwrap();
    });
    (IppEndpoint::ipp("127.0.0.1", port, "/ipp/print"), handle)
}

fn status_server(status: &str) -> (IppEndpoint, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let status = status.to_owned();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });
    (IppEndpoint::ipp("127.0.0.1", port, "/ipp/print"), handle)
}

fn setting_observation(location: &str) -> Vec<u8> {
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
                    Value::raw(ValueTag::Keyword, b"printer-location"),
                ),
                Attribute::new(
                    b"printer-location".to_vec(),
                    Value::raw(ValueTag::TextWithoutLanguage, location.as_bytes()),
                ),
            ],
        }],
        original_bytes: Vec::new(),
    }
    .encode(Limits::default())
    .unwrap()
}

fn supported_setting_observation() -> Vec<u8> {
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
    .encode(Limits::default())
    .unwrap()
}

fn supported_values_response() -> Vec<u8> {
    Message {
        version: Version::V2_0,
        code: 0,
        request_id: 2,
        groups: vec![AttributeGroup {
            tag: ipp::PRINTER_ATTRIBUTES_TAG,
            attributes: vec![Attribute {
                name: b"media-supported".to_vec(),
                values: vec![Value {
                    tag: ValueTag::AdminDefine,
                    data: mb_printer_core::ipp::ValueData::OutOfBand,
                }],
            }],
        }],
        original_bytes: Vec::new(),
    }
    .encode(Limits::default())
    .unwrap()
}

fn sequence_server(
    responses: Vec<Vec<u8>>,
) -> (IppEndpoint, Arc<Mutex<Vec<u16>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let operations = Arc::new(Mutex::new(Vec::new()));
    let recorded = operations.clone();
    let handle = thread::spawn(move || {
        for response in responses {
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
            let body = &request[header_end..header_end + content_length];
            recorded
                .lock()
                .unwrap()
                .push(u16::from_be_bytes([body[2], body[3]]));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(&response).unwrap();
        }
    });
    (
        IppEndpoint::ipp("127.0.0.1", port, "/ipp/print"),
        operations,
        handle,
    )
}

fn disconnect_after_write_server(
    before: Vec<u8>,
) -> (IppEndpoint, Arc<Mutex<Vec<u16>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let operations = Arc::new(Mutex::new(Vec::new()));
    let recorded = operations.clone();
    let handle = thread::spawn(move || {
        for index in 0..2 {
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
            let body = &request[header_end..header_end + content_length];
            recorded
                .lock()
                .unwrap()
                .push(u16::from_be_bytes([body[2], body[3]]));
            if index == 0 {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    before.len()
                )
                .unwrap();
                stream.write_all(&before).unwrap();
            }
            // The second connection closes only after the complete write was
            // received. Its outcome is therefore ambiguous, never retryable.
        }
    });
    (
        IppEndpoint::ipp("127.0.0.1", port, "/ipp/print"),
        operations,
        handle,
    )
}

fn discovery_server(
    responses: Vec<Message>,
) -> (
    IppEndpoint,
    Arc<Mutex<Vec<Message>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let handle = thread::spawn(move || {
        for mut response in responses {
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
            response.request_id = request.request_id;
            recorded.lock().unwrap().push(request);
            let response = response.encode(Limits::default()).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(&response).unwrap();
        }
    });
    (
        IppEndpoint::ipp("127.0.0.1", port, "/ipp/print"),
        requests,
        handle,
    )
}

fn capability_response(attributes: Vec<Attribute>) -> Message {
    Message {
        version: Version::V2_0,
        code: 0,
        request_id: 0,
        groups: vec![AttributeGroup {
            tag: ipp::PRINTER_ATTRIBUTES_TAG,
            attributes,
        }],
        original_bytes: Vec::new(),
    }
}

#[test]
fn async_inspection_decodes_chunked_responses_and_preserves_original_bytes() {
    let body = response();
    let (endpoint, server) = chunked_server(body.clone());
    let request =
        ipp::get_printer_attributes_request(&endpoint.printer_uri().unwrap(), ["all"], None, 9);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let decoded = runtime
        .block_on(
            IppClient::new()
                .unwrap()
                .inspect(&endpoint, &request, InspectLimits::default()),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(decoded.original_bytes, body);
    assert_eq!(decoded.groups[0].attributes[0].name, b"x-vendor-setting");
    assert_eq!(
        decoded.groups[0].attributes[0].values[0].tag,
        ValueTag::Extension(0x7f)
    );
}

#[test]
fn authentication_failure_is_distinct_and_does_not_decode_a_body() {
    let (endpoint, server) = status_server("401 Unauthorized");
    let request =
        ipp::get_printer_attributes_request("ipp://127.0.0.1/ipp/print", ["all"], None, 9);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(
            IppClient::new()
                .unwrap()
                .inspect(&endpoint, &request, InspectLimits::default()),
        )
        .unwrap_err();
    server.join().unwrap();
    assert!(matches!(
        error,
        IppClientError::HttpStatus(reqwest::StatusCode::UNAUTHORIZED)
    ));
}

#[test]
fn response_limit_is_enforced_before_decoding() {
    let body = response();
    let (endpoint, server) = chunked_server(body);
    let request =
        ipp::get_printer_attributes_request(&endpoint.printer_uri().unwrap(), ["all"], None, 9);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let limits = InspectLimits {
        maximum_response_bytes: 8,
        ..InspectLimits::default()
    };
    let error = runtime
        .block_on(
            IppClient::new()
                .unwrap()
                .inspect(&endpoint, &request, limits),
        )
        .unwrap_err();
    server.join().unwrap();
    assert!(matches!(
        error,
        IppClientError::ResponseTooLarge { limit: 8 }
    ));
}

#[test]
fn confirmed_write_rereads_writes_once_and_verifies() {
    let before_bytes = setting_observation("Office");
    let before = ipp::decode(&before_bytes, Limits::default()).unwrap();
    let plan = plan_ipp_change(
        &before,
        PlanChangeRequest {
            printer_id: "printer-1",
            endpoint_generation: 4,
            setting: "printer-location",
            requested_value: Value::raw(ValueTag::TextWithoutLanguage, b"Workshop"),
            principal: "user-1",
            protocol: ProtocolFamily::Ipp,
            expires_at_unix_ms: 2_000,
        },
    )
    .unwrap();
    let write_ok = Message::new(Version::V2_0, 0, 2)
        .encode(Limits::default())
        .unwrap();
    let (endpoint, operations, server) = sequence_server(vec![
        before_bytes,
        write_ok,
        setting_observation("Workshop"),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime
        .block_on(IppClient::new().unwrap().apply_confirmed_change(
            &endpoint,
            &plan,
            ChangeBinding {
                printer_id: "printer-1",
                endpoint_generation: 4,
                principal: "user-1",
                protocol: ProtocolFamily::Ipp,
                now_unix_ms: 1_000,
            },
            InspectLimits::default(),
        ))
        .unwrap();
    server.join().unwrap();
    assert!(matches!(outcome, ApplyChangeOutcome::Verified { .. }));
    assert_eq!(
        *operations.lock().unwrap(),
        vec![
            ipp::GET_PRINTER_ATTRIBUTES,
            ipp::SET_PRINTER_ATTRIBUTES,
            ipp::GET_PRINTER_ATTRIBUTES
        ]
    );
}

#[test]
fn planning_settable_supported_attributes_uses_rfc_3380_before_confirmation() {
    let (endpoint, operations, server) = sequence_server(vec![
        supported_setting_observation(),
        supported_values_response(),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let plan = runtime
        .block_on(IppClient::new().unwrap().plan_change(
            &endpoint,
            PlanChangeRequest {
                printer_id: "printer-1",
                endpoint_generation: 4,
                setting: "media-supported",
                requested_value: Value::raw(ValueTag::NameWithoutLanguage, b"Custom Stock"),
                principal: "admin-1",
                protocol: ProtocolFamily::Ipp,
                expires_at_unix_ms: 2_000,
            },
            InspectLimits::default(),
        ))
        .unwrap();
    server.join().unwrap();
    assert_eq!(plan.setting, "media-supported");
    assert_eq!(
        *operations.lock().unwrap(),
        vec![
            ipp::GET_PRINTER_ATTRIBUTES,
            ipp::GET_PRINTER_SUPPORTED_VALUES
        ]
    );
}

#[test]
fn disconnect_after_write_is_ambiguous_and_never_retried() {
    let before_bytes = setting_observation("Office");
    let before = ipp::decode(&before_bytes, Limits::default()).unwrap();
    let plan = plan_ipp_change(
        &before,
        PlanChangeRequest {
            printer_id: "printer-1",
            endpoint_generation: 4,
            setting: "printer-location",
            requested_value: Value::raw(ValueTag::TextWithoutLanguage, b"Workshop"),
            principal: "user-1",
            protocol: ProtocolFamily::Ipp,
            expires_at_unix_ms: 2_000,
        },
    )
    .unwrap();
    let (endpoint, operations, server) = disconnect_after_write_server(before_bytes);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime
        .block_on(IppClient::new().unwrap().apply_confirmed_change(
            &endpoint,
            &plan,
            ChangeBinding {
                printer_id: "printer-1",
                endpoint_generation: 4,
                principal: "user-1",
                protocol: ProtocolFamily::Ipp,
                now_unix_ms: 1_000,
            },
            InspectLimits::default(),
        ))
        .unwrap();
    server.join().unwrap();
    assert!(matches!(
        outcome,
        ApplyChangeOutcome::Ambiguous { stage: "write", .. }
    ));
    assert_eq!(
        *operations.lock().unwrap(),
        vec![ipp::GET_PRINTER_ATTRIBUTES, ipp::SET_PRINTER_ATTRIBUTES]
    );
}

#[test]
fn rejected_write_is_not_verified_or_retried() {
    let before_bytes = setting_observation("Office");
    let before = ipp::decode(&before_bytes, Limits::default()).unwrap();
    let plan = plan_ipp_change(
        &before,
        PlanChangeRequest {
            printer_id: "printer-1",
            endpoint_generation: 4,
            setting: "printer-location",
            requested_value: Value::raw(ValueTag::TextWithoutLanguage, b"Workshop"),
            principal: "user-1",
            protocol: ProtocolFamily::Ipp,
            expires_at_unix_ms: 2_000,
        },
    )
    .unwrap();
    let rejected = Message::new(Version::V2_0, 0x0402, 2)
        .encode(Limits::default())
        .unwrap();
    let (endpoint, operations, server) = sequence_server(vec![before_bytes, rejected]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime
        .block_on(IppClient::new().unwrap().apply_confirmed_change(
            &endpoint,
            &plan,
            ChangeBinding {
                printer_id: "printer-1",
                endpoint_generation: 4,
                principal: "user-1",
                protocol: ProtocolFamily::Ipp,
                now_unix_ms: 1_000,
            },
            InspectLimits::default(),
        ))
        .unwrap();
    server.join().unwrap();
    assert!(matches!(outcome, ApplyChangeOutcome::Rejected { .. }));
    assert_eq!(
        *operations.lock().unwrap(),
        vec![ipp::GET_PRINTER_ATTRIBUTES, ipp::SET_PRINTER_ATTRIBUTES]
    );
}

#[test]
fn readback_mismatch_is_a_verified_failure_without_retry() {
    let before_bytes = setting_observation("Office");
    let before = ipp::decode(&before_bytes, Limits::default()).unwrap();
    let plan = plan_ipp_change(
        &before,
        PlanChangeRequest {
            printer_id: "printer-1",
            endpoint_generation: 4,
            setting: "printer-location",
            requested_value: Value::raw(ValueTag::TextWithoutLanguage, b"Workshop"),
            principal: "user-1",
            protocol: ProtocolFamily::Ipp,
            expires_at_unix_ms: 2_000,
        },
    )
    .unwrap();
    let write_ok = Message::new(Version::V2_0, 0, 2)
        .encode(Limits::default())
        .unwrap();
    let (endpoint, operations, server) = sequence_server(vec![
        before_bytes,
        write_ok,
        setting_observation("Unexpected"),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime
        .block_on(IppClient::new().unwrap().apply_confirmed_change(
            &endpoint,
            &plan,
            ChangeBinding {
                printer_id: "printer-1",
                endpoint_generation: 4,
                principal: "user-1",
                protocol: ProtocolFamily::Ipp,
                now_unix_ms: 1_000,
            },
            InspectLimits::default(),
        ))
        .unwrap();
    server.join().unwrap();
    assert!(matches!(
        outcome,
        ApplyChangeOutcome::ReadBackMismatch { .. }
    ));
    assert_eq!(
        *operations.lock().unwrap(),
        vec![
            ipp::GET_PRINTER_ATTRIBUTES,
            ipp::SET_PRINTER_ATTRIBUTES,
            ipp::GET_PRINTER_ATTRIBUTES
        ]
    );
}

#[test]
fn discovery_queries_only_advertised_varying_formats_without_overwriting_base() {
    let base = capability_response(vec![
        Attribute {
            name: b"document-format-supported".to_vec(),
            values: vec![
                Value::raw(ValueTag::MimeMediaType, b"application/pdf"),
                Value::raw(ValueTag::MimeMediaType, b"image/png"),
            ],
        },
        Attribute::new(
            b"document-format-varying-attributes".to_vec(),
            Value::raw(ValueTag::Keyword, b"sides-supported"),
        ),
        Attribute::new(
            b"sides-default".to_vec(),
            Value::raw(ValueTag::Keyword, b"one-sided"),
        ),
    ]);
    let pdf = capability_response(vec![Attribute {
        name: b"sides-supported".to_vec(),
        values: vec![
            Value::raw(ValueTag::Keyword, b"one-sided"),
            Value::raw(ValueTag::Keyword, b"two-sided-long-edge"),
        ],
    }]);
    let png = capability_response(vec![Attribute::new(
        b"sides-supported".to_vec(),
        Value::raw(ValueTag::Keyword, b"one-sided"),
    )]);
    let (endpoint, requests, server) = discovery_server(vec![base, pdf, png]);
    let origin = ObservationOrigin {
        agent_id: None,
        printer_id: "printer-1".into(),
        endpoint: endpoint.printer_uri().unwrap(),
        endpoint_generation: 1,
        transport: TransportKind::Ipp,
        protocol: ProtocolFamily::Ipp,
        request_id: "discovery-1".into(),
        probe_id: None,
        observed_at: "2026-08-31T12:00:00Z".into(),
        qualification: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime
        .block_on(IppClient::new().unwrap().discover(
            &endpoint,
            &origin,
            DiscoveryOptions::default(),
        ))
        .unwrap();
    server.join().unwrap();
    assert_eq!(result.format_responses.len(), 2);
    assert!(!result.formats_truncated);
    let scopes = result
        .snapshot
        .job_capabilities
        .iter()
        .filter(|capability| capability.id == "sides")
        .map(|capability| capability.format_scope.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        scopes,
        vec![None, Some("application/pdf"), Some("image/png")]
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0].groups[0]
            .attributes
            .iter()
            .all(|attribute| attribute.name != b"document-format")
    );
    assert!(
        requests[1].groups[0]
            .attributes
            .iter()
            .any(|attribute| attribute.name == b"document-format")
    );
}

#[test]
fn discovery_runs_bounded_focused_queries_only_when_the_base_advertises_need() {
    let base = capability_response(vec![Attribute {
        name: b"requested-attributes-supported".to_vec(),
        values: vec![
            Value::raw(ValueTag::Keyword, b"marker-names"),
            Value::raw(ValueTag::Keyword, b"marker-levels"),
            Value::raw(ValueTag::Keyword, b"marker-types"),
        ],
    }]);
    let focused = capability_response(vec![
        Attribute::new(
            b"marker-names".to_vec(),
            Value::raw(ValueTag::NameWithoutLanguage, b"Black Toner"),
        ),
        Attribute::new(b"marker-levels".to_vec(), Value::integer(61)),
        Attribute::new(
            b"marker-types".to_vec(),
            Value::raw(ValueTag::Keyword, b"toner"),
        ),
    ]);
    let (endpoint, requests, server) = discovery_server(vec![base, focused]);
    let origin = ObservationOrigin {
        agent_id: None,
        printer_id: "printer-1".into(),
        endpoint: endpoint.printer_uri().unwrap(),
        endpoint_generation: 1,
        transport: TransportKind::Ipp,
        protocol: ProtocolFamily::Ipp,
        request_id: "focused-1".into(),
        probe_id: None,
        observed_at: "2026-08-31T12:00:00Z".into(),
        qualification: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime
        .block_on(IppClient::new().unwrap().discover(
            &endpoint,
            &origin,
            DiscoveryOptions::default(),
        ))
        .unwrap();
    server.join().unwrap();
    assert_eq!(result.focused_responses.len(), 1);
    assert!(!result.focused_queries_truncated);
    assert_eq!(result.snapshot.supplies[0].id, "Black Toner");
    assert_eq!(result.snapshot.supplies[0].level_percent, Some(61));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let requested = requests[1].groups[0]
        .attributes
        .iter()
        .find(|attribute| attribute.name == b"requested-attributes")
        .unwrap();
    assert!(requested.values.iter().any(|value| {
        matches!(&value.data, mb_printer_core::ipp::ValueData::Bytes(bytes) if bytes == b"marker-levels")
    }));
}
