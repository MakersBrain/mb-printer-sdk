// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{collections::VecDeque, time::Duration};

use futures_executor::block_on;
use mb_printer_core::{
    capabilities::Protocol,
    protocol::{Action, Plan, ResponseValidation, SOURCE_COMMIT},
};
use mb_printer_executor::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Subscribe,
    Write(WriteKind, Vec<u8>),
    Wait(Duration),
    Delay(Duration),
    Disconnect,
}

struct FakeTransport {
    payload_limit: usize,
    command_limit: Option<usize>,
    events: Vec<Event>,
    waits: VecDeque<Result<WaitOutcome, TransportError>>,
    write_error: Option<TransportError>,
    subscribe_error: Option<TransportError>,
}

impl Default for FakeTransport {
    fn default() -> Self {
        Self {
            payload_limit: 2,
            command_limit: None,
            events: Vec::new(),
            waits: VecDeque::new(),
            write_error: None,
            subscribe_error: None,
        }
    }
}

impl Transport for FakeTransport {
    fn payload_limit(&self) -> usize {
        self.payload_limit
    }

    fn command_limit(&self) -> usize {
        self.command_limit.unwrap_or(self.payload_limit)
    }

    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        self.events.push(Event::Subscribe);
        let result = self
            .subscribe_error
            .take()
            .map_or(Ok(NotificationSupport::Subscribed), Err);
        Box::pin(async move { result })
    }

    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        kind: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        self.events.push(Event::Write(kind, bytes.to_vec()));
        let result = self.write_error.take().map_or(Ok(()), Err);
        Box::pin(async move { result })
    }

    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        self.events.push(Event::Wait(timeout));
        let result = self.waits.pop_front().unwrap_or(Ok(WaitOutcome::Timeout));
        Box::pin(async move { result })
    }

    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
        self.events.push(Event::Delay(duration));
        Box::pin(async {})
    }

    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        self.events.push(Event::Disconnect);
        Box::pin(async { Ok(()) })
    }
}

fn plan(protocol: Protocol, actions: Vec<Action>) -> Plan {
    Plan {
        protocol,
        source_commit: SOURCE_COMMIT.into(),
        actions,
    }
}

fn command(bytes: Vec<u8>) -> Action {
    Action::CommandWrite {
        name: "command".into(),
        bytes,
        atomic: true,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn assert_send<T: Send>(_: &T) {}

#[test]
fn native_transport_and_execution_future_are_send_and_dyn_safe() {
    let mut fake = FakeTransport::default();
    #[cfg(not(target_arch = "wasm32"))]
    assert_send(&fake);
    let transport: &mut dyn Transport = &mut fake;
    let job = plan(Protocol::MSeries, vec![]);
    let future = execute(&job, transport);
    #[cfg(not(target_arch = "wasm32"))]
    assert_send(&future);
    assert_eq!(block_on(future).unwrap(), Progress::default());
}

#[test]
fn raster_boundaries_kinds_and_reference_delays_are_preserved() {
    let job = plan(
        Protocol::MSeries,
        vec![Action::RasterWrite {
            bytes: (0..10).collect(),
            logical_chunk: 4,
            delay_after_each_physical_write_ms: 20,
        }],
    );
    let mut fake = FakeTransport {
        payload_limit: 3,
        ..Default::default()
    };
    let progress = block_on(execute(&job, &mut fake)).unwrap();
    let writes = fake
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Write(kind, bytes) => Some((*kind, bytes.len())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        writes,
        [
            (WriteKind::Raster, 3),
            (WriteKind::Raster, 1),
            (WriteKind::Raster, 3),
            (WriteKind::Raster, 1),
            (WriteKind::Raster, 2),
        ]
    );
    assert_eq!(
        fake.events
            .iter()
            .filter(|event| {
                matches!(event, Event::Delay(duration) if *duration == Duration::from_millis(20))
            })
            .count(),
        5
    );
    assert_eq!(progress.bytes_written, 10);
}

#[test]
fn command_and_raster_limits_are_independent() {
    let job = plan(
        Protocol::Brother,
        vec![
            command(vec![0; 200]),
            Action::RasterWrite {
                bytes: vec![1; 130],
                logical_chunk: 1024,
                delay_after_each_physical_write_ms: 0,
            },
        ],
    );
    let mut fake = FakeTransport {
        payload_limit: 64,
        command_limit: Some(512),
        ..Default::default()
    };
    block_on(execute(&job, &mut fake)).unwrap();
    assert_eq!(
        fake.events
            .iter()
            .filter_map(|event| match event {
                Event::Write(kind, bytes) => Some((*kind, bytes.len())),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [
            (WriteKind::Command, 200),
            (WriteKind::Raster, 64),
            (WriteKind::Raster, 64),
            (WriteKind::Raster, 2),
        ]
    );
}

#[test]
fn progress_callback_reports_only_completed_actions() {
    let job = plan(
        Protocol::MSeries,
        vec![
            command(vec![1]),
            Action::Delay { milliseconds: 1 },
            command(vec![2, 3]),
        ],
    );
    let mut fake = FakeTransport {
        command_limit: Some(8),
        ..Default::default()
    };
    let mut observed = Vec::new();
    let progress = block_on(execute_with_options(
        &job,
        &mut fake,
        ExecutionOptions {
            timing: ReferenceTiming::Preserve,
            cancellation: &NeverCancelled,
        },
        |progress| observed.push((progress.last_completed_action, progress.bytes_written)),
    ))
    .unwrap();
    assert_eq!(observed, [(Some(0), 1), (Some(1), 1), (Some(2), 3)]);
    assert_eq!(progress.bytes_written, 3);
}

#[test]
fn timing_policy_changes_every_reference_delay() {
    let job = plan(
        Protocol::MSeries,
        vec![
            Action::Delay { milliseconds: 20 },
            Action::RasterWrite {
                bytes: vec![1, 2, 3],
                logical_chunk: 3,
                delay_after_each_physical_write_ms: 10,
            },
        ],
    );
    let mut fake = FakeTransport::default();
    block_on(execute_with_options(
        &job,
        &mut fake,
        ExecutionOptions {
            timing: ReferenceTiming::IncreaseBy(5),
            cancellation: &NeverCancelled,
        },
        |_| {},
    ))
    .unwrap();
    assert_eq!(
        fake.events
            .iter()
            .filter_map(|event| match event {
                Event::Delay(duration) => Some(duration.as_millis()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [25, 15, 15]
    );

    let mut diagnostic = FakeTransport::default();
    block_on(execute_with_options(
        &job,
        &mut diagnostic,
        ExecutionOptions {
            timing: ReferenceTiming::UnsafeDiagnosticReduceBy(7),
            cancellation: &NeverCancelled,
        },
        |_| {},
    ))
    .unwrap();
    assert_eq!(
        diagnostic
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Delay(duration) => Some(duration.as_millis()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [13, 3, 3]
    );
}

#[test]
fn whole_plan_preflight_precedes_all_effects() {
    let cases = [
        plan(Protocol::MSeries, vec![command(vec![1, 2, 3])]),
        plan(
            Protocol::MSeries,
            vec![Action::RasterWrite {
                bytes: vec![1],
                logical_chunk: 0,
                delay_after_each_physical_write_ms: 0,
            }],
        ),
        plan(
            Protocol::Brother,
            vec![Action::WaitForResponse {
                timeout_ms: 1,
                fallback_delay_ms: 0,
                validation: ResponseValidation::BrotherSystemReport,
            }],
        ),
        plan(
            Protocol::Brother,
            vec![Action::CollectResponse {
                timeout_ms: 0,
                idle_timeout_ms: 1,
                maximum_bytes: 1,
                validation: ResponseValidation::BrotherObjbrnet,
            }],
        ),
    ];
    for job in cases {
        let mut fake = FakeTransport::default();
        assert!(block_on(execute(&job, &mut fake)).is_err());
        assert!(fake.events.is_empty());
    }
}

#[test]
fn preflight_bounds_plan_actions_and_declared_retained_bytes() {
    let mut fake = FakeTransport::default();
    let too_many = plan(
        Protocol::MSeries,
        (0..=MAX_PLAN_ACTIONS)
            .map(|_| Action::JobBoundary {
                kind: mb_printer_core::protocol::Boundary::Start,
            })
            .collect(),
    );
    assert!(matches!(
        block_on(execute(&too_many, &mut fake)),
        Err(ExecuteError::InvalidPlan { .. })
    ));
    assert!(fake.events.is_empty());

    let too_much_response = plan(
        Protocol::Brother,
        vec![
            Action::CollectResponse {
                timeout_ms: 1,
                idle_timeout_ms: 1,
                maximum_bytes: MAX_RETAINED_RESPONSE_BYTES,
                validation: ResponseValidation::BrotherObjbrnet,
            },
            Action::CollectResponse {
                timeout_ms: 1,
                idle_timeout_ms: 1,
                maximum_bytes: 1,
                validation: ResponseValidation::BrotherWifiScan,
            },
        ],
    );
    assert!(matches!(
        block_on(execute(&too_much_response, &mut fake)),
        Err(ExecuteError::InvalidPlan { action: 1, .. })
    ));
    assert!(fake.events.is_empty());
}

#[test]
fn notification_timeout_and_unavailable_use_ordered_fallback_delay() {
    let job = plan(
        Protocol::MSeries,
        vec![
            Action::SubscribeNotifications,
            command(vec![1]),
            Action::WaitForResponse {
                timeout_ms: 500,
                fallback_delay_ms: 80,
                validation: ResponseValidation::AnyNotification,
            },
        ],
    );
    for absent in [WaitOutcome::Unavailable, WaitOutcome::Timeout] {
        let mut fake = FakeTransport {
            command_limit: Some(8),
            ..Default::default()
        };
        fake.waits.push_back(Ok(absent));
        block_on(execute(&job, &mut fake)).unwrap();
        assert_eq!(
            fake.events,
            [
                Event::Subscribe,
                Event::Write(WriteKind::Command, vec![1]),
                Event::Wait(Duration::from_millis(500)),
                Event::Delay(Duration::from_millis(80)),
            ]
        );
    }
}

#[test]
fn mtu_matrix_chunks_exactly_and_zero_limits_are_rejected() {
    for mtu in [23, 64, 185, 517] {
        let bytes = vec![0x55; 1100];
        let mut fake = FakeTransport {
            payload_limit: mtu,
            ..Default::default()
        };
        block_on(execute(
            &plan(
                Protocol::MSeries,
                vec![Action::RasterWrite {
                    bytes: bytes.clone(),
                    logical_chunk: 600,
                    delay_after_each_physical_write_ms: 0,
                }],
            ),
            &mut fake,
        ))
        .unwrap();
        let sizes = fake.events.iter().filter_map(|event| match event {
            Event::Write(WriteKind::Raster, bytes) => Some(bytes.len()),
            _ => None,
        });
        assert_eq!(sizes.clone().sum::<usize>(), bytes.len());
        assert!(sizes.into_iter().all(|size| size <= mtu));
    }
    for (payload_limit, command_limit) in [(0, None), (23, Some(0))] {
        let mut fake = FakeTransport {
            payload_limit,
            command_limit,
            ..Default::default()
        };
        assert!(matches!(
            block_on(execute(&plan(Protocol::MSeries, vec![]), &mut fake)),
            Err(ExecuteError::InvalidPlan { .. })
        ));
    }
}

#[test]
fn write_failures_are_outcome_unknown_and_do_not_increment_bytes() {
    let mut fake = FakeTransport {
        write_error: Some(TransportError::new(
            TransportErrorKind::Disconnected,
            "connection closed",
        )),
        ..Default::default()
    };
    let error = block_on(execute(
        &plan(Protocol::MSeries, vec![command(vec![1])]),
        &mut fake,
    ))
    .unwrap_err();
    let ExecuteError::WriteOutcomeUnknown { progress, source } = error else {
        panic!("unexpected error")
    };
    assert_eq!(progress.bytes_written, 0);
    assert!(progress.potentially_accepted_write);
    assert_eq!(source.unwrap().kind, TransportErrorKind::Disconnected);
}

#[test]
fn split_fixed_responses_alignment_and_brother_best_effort_match_policy() {
    let mut status_head = vec![0; 16];
    status_head[..3].copy_from_slice(&[0x80, 0x20, 0x42]);
    let status_job = plan(
        Protocol::Brother,
        vec![Action::WaitForResponse {
            timeout_ms: 100,
            fallback_delay_ms: 0,
            validation: ResponseValidation::BrotherStatus32,
        }],
    );
    let mut fake = FakeTransport::default();
    fake.waits.extend([
        Ok(WaitOutcome::Response(status_head)),
        Ok(WaitOutcome::Response(vec![0; 16])),
    ]);
    assert_eq!(
        block_on(execute(&status_job, &mut fake)).unwrap().responses[0].len(),
        32
    );

    for absent in [WaitOutcome::Unavailable, WaitOutcome::Timeout] {
        let mut fake = FakeTransport::default();
        fake.waits.push_back(Ok(absent));
        assert!(block_on(execute(&status_job, &mut fake)).is_ok());
    }

    let phomemo_job = plan(
        Protocol::M110,
        vec![Action::WaitForResponse {
            timeout_ms: 100,
            fallback_delay_ms: 0,
            validation: ResponseValidation::PhomemoNotification,
        }],
    );
    let mut fake = FakeTransport::default();
    fake.waits.extend([
        Ok(WaitOutcome::Response(vec![0x1f, 0x11, 0x08])),
        Ok(WaitOutcome::Response(vec![0x1a, 0x04, 100])),
    ]);
    assert_eq!(
        block_on(execute(&phomemo_job, &mut fake))
            .unwrap()
            .responses,
        [vec![0x1a, 0x04, 100]]
    );

    for length in [31, 33] {
        let mut bytes = vec![0; length];
        bytes[..3].copy_from_slice(&[0x80, 0x20, 0x42]);
        let mut fake = FakeTransport::default();
        fake.waits
            .extend([Ok(WaitOutcome::Response(bytes)), Ok(WaitOutcome::Timeout)]);
        assert!(matches!(
            block_on(execute(&status_job, &mut fake)),
            Err(ExecuteError::Response { .. })
        ));
    }
}

fn collect_plan(maximum_bytes: usize, validation: ResponseValidation) -> Plan {
    plan(
        Protocol::Brother,
        vec![Action::CollectResponse {
            timeout_ms: 1_000,
            idle_timeout_ms: 1,
            maximum_bytes,
            validation,
        }],
    )
}

#[test]
fn multipart_responses_reassemble_validate_and_enforce_bounds() {
    let response = b"prefix<<PRINTER CONFIGURATION>>\r\nbody";
    for split in 1..response.len() {
        let mut fake = FakeTransport::default();
        fake.waits.extend([
            Ok(WaitOutcome::Response(response[..split].to_vec())),
            Ok(WaitOutcome::Response(response[split..].to_vec())),
            Ok(WaitOutcome::Timeout),
        ]);
        assert_eq!(
            block_on(execute(
                &collect_plan(response.len(), ResponseValidation::BrotherSystemReport),
                &mut fake,
            ))
            .unwrap()
            .responses,
            [response]
        );
    }

    let bytes = b"VAP,network".to_vec();
    let mut fake = FakeTransport::default();
    fake.waits.extend([
        Ok(WaitOutcome::Response(bytes.clone())),
        Ok(WaitOutcome::Response(vec![0])),
    ]);
    assert!(matches!(
        block_on(execute(
            &collect_plan(bytes.len(), ResponseValidation::BrotherWifiScan),
            &mut fake,
        )),
        Err(ExecuteError::Response { .. })
    ));
}

#[test]
fn multipart_distinguishes_initial_timeout_from_idle_after_partial() {
    let job = collect_plan(1024, ResponseValidation::BrotherObjbrnet);
    let mut no_first_packet = FakeTransport::default();
    assert!(matches!(
        block_on(execute(&job, &mut no_first_packet)),
        Err(ExecuteError::Timeout { .. })
    ));

    let partial = b"@PJL INFO OBJBRNET\r\n\"458867:1\"\r\n".to_vec();
    let mut idle_after_partial = FakeTransport::default();
    idle_after_partial.waits.extend([
        Ok(WaitOutcome::Response(partial.clone())),
        Ok(WaitOutcome::Timeout),
    ]);
    assert_eq!(
        block_on(execute(&job, &mut idle_after_partial))
            .unwrap()
            .responses,
        [partial]
    );
}

#[test]
fn multipart_response_that_never_idles_hits_read_ceiling() {
    let mut fake = FakeTransport::default();
    fake.waits.extend(std::iter::repeat_n(
        Ok(WaitOutcome::Response(vec![b'x'])),
        4096,
    ));
    let job = plan(
        Protocol::Brother,
        vec![Action::CollectResponse {
            timeout_ms: 10_000,
            idle_timeout_ms: 10_000,
            maximum_bytes: 8192,
            validation: ResponseValidation::BrotherSystemReport,
        }],
    );
    assert!(matches!(
        block_on(execute(&job, &mut fake)),
        Err(ExecuteError::Response { .. })
    ));
    assert_eq!(
        fake.events
            .iter()
            .filter(|event| matches!(event, Event::Wait(_)))
            .count(),
        4096
    );
}

#[test]
fn memory_replay_store_claims_before_transport_effect() {
    let mut store = MemoryReplayStore::default();
    let job = plan(Protocol::MSeries, vec![command(vec![1])]);
    let mut first = FakeTransport::default();
    block_on(execute_once_with_store(
        &mut store,
        "durable-id",
        &job,
        &mut first,
    ))
    .unwrap();
    let mut retry = FakeTransport::default();
    assert!(matches!(
        block_on(execute_once_with_store(
            &mut store,
            "durable-id",
            &job,
            &mut retry,
        )),
        Err(ExecuteError::Replay(_))
    ));
    assert!(retry.events.is_empty());
}

#[test]
fn replay_claim_happens_before_execution_and_survives_failure() {
    let job = plan(Protocol::MSeries, vec![command(vec![1])]);
    let mut guard = ReplayGuard::default();
    let mut failed = FakeTransport {
        write_error: Some(TransportError::new(
            TransportErrorKind::Disconnected,
            "closed",
        )),
        ..Default::default()
    };
    assert!(matches!(
        block_on(guard.execute_once("job-1", &job, &mut failed)),
        Err(ExecuteError::WriteOutcomeUnknown { .. })
    ));
    let mut retry = FakeTransport::default();
    assert!(matches!(
        block_on(guard.execute_once("job-1", &job, &mut retry)),
        Err(ExecuteError::Replay(_))
    ));
    assert!(retry.events.is_empty());
}

#[test]
fn error_display_does_not_leak_progress_or_response_frames() {
    let progress = Progress {
        last_completed_action: Some(7),
        bytes_written: 12_345,
        potentially_accepted_write: true,
        responses: vec![vec![222, 173, 190, 239]],
    };
    let errors = [
        ExecuteError::Cancelled {
            progress: progress.clone(),
        },
        ExecuteError::WriteOutcomeUnknown {
            progress: progress.clone(),
            source: None,
        },
        ExecuteError::Timeout {
            progress: progress.clone(),
        },
        ExecuteError::Response {
            progress,
            message: "validator mismatch".into(),
        },
    ];
    for error in errors {
        let display = error.to_string();
        for forbidden in ["Progress", "responses", "222", "12345", "12_345"] {
            assert!(
                !display.contains(forbidden),
                "leaked {forbidden}: {display}"
            );
        }
    }
}
