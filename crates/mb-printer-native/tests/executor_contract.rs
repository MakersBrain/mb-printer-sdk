// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{capabilities::Protocol, protocol::*};
use mb_printer_native::*;

#[derive(Clone)]
struct FakeClockTransport {
    mtu: usize,
    operation: usize,
    fail_at: Option<usize>,
    events: Vec<String>,
    wait: WaitOutcome,
}
impl FakeClockTransport {
    fn succeed(mtu: usize) -> Self {
        Self {
            mtu,
            operation: 0,
            fail_at: None,
            events: vec![],
            wait: WaitOutcome::Response(vec![1]),
        }
    }
    fn operation(&mut self, event: String) -> Result<(), String> {
        let current = self.operation;
        self.operation += 1;
        self.events.push(event);
        if self.fail_at == Some(current) {
            Err("disconnect".into())
        } else {
            Ok(())
        }
    }
}
impl Transport for FakeClockTransport {
    fn payload_limit(&self) -> usize {
        self.mtu
    }
    fn subscribe_notifications(&mut self) -> Result<(), String> {
        self.operation("subscribe".into())
    }
    fn write(&mut self, b: &[u8]) -> Result<(), String> {
        self.operation(format!("write:{}", b.len()))
    }
    fn delay_monotonic(&mut self, n: u64) {
        self.events.push(format!("delay:{n}"))
    }
    fn wait_response(&mut self, n: u64) -> Result<WaitOutcome, String> {
        self.operation(format!("wait:{n}"))?;
        Ok(self.wait.clone())
    }
}
fn plan(actions: Vec<Action>) -> Plan {
    Plan {
        protocol: Protocol::MSeries,
        source_commit: SOURCE_COMMIT.into(),
        actions,
    }
}

#[test]
fn notifications_fake_clock_timeout_and_fallback_are_ordered() {
    let actions = vec![
        Action::SubscribeNotifications,
        Action::CommandWrite {
            name: "q".into(),
            bytes: vec![1],
            atomic: true,
        },
        Action::WaitForResponse {
            timeout_ms: 500,
            fallback_delay_ms: 80,
            validation: ResponseValidation::AnyNotification,
        },
    ];
    let mut unavailable = FakeClockTransport::succeed(23);
    unavailable.wait = WaitOutcome::Unavailable;
    assert!(execute(&plan(actions.clone()), &mut unavailable).is_ok());
    assert_eq!(
        unavailable.events,
        vec!["subscribe", "write:1", "wait:500", "delay:80"]
    );
    let mut timeout = FakeClockTransport::succeed(23);
    timeout.wait = WaitOutcome::Timeout;
    assert!(matches!(
        execute(&plan(actions), &mut timeout),
        Err(ExecuteError::Timeout { .. })
    ));
    assert!(
        !timeout.events.contains(&"delay:80".into()),
        "a real timeout never uses the unavailable fallback"
    );
}

#[test]
fn disconnect_at_each_io_boundary_stops_without_replay() {
    let actions = vec![
        Action::SubscribeNotifications,
        Action::CommandWrite {
            name: "a".into(),
            bytes: vec![1],
            atomic: true,
        },
        Action::RasterWrite {
            bytes: vec![2, 3],
            logical_chunk: 1,
            delay_after_each_physical_write_ms: 4,
        },
        Action::WaitForResponse {
            timeout_ms: 7,
            fallback_delay_ms: 0,
            validation: ResponseValidation::AnyNotification,
        },
    ];
    for fail_at in 0..5 {
        let mut transport = FakeClockTransport {
            fail_at: Some(fail_at),
            ..FakeClockTransport::succeed(23)
        };
        assert!(
            matches!(
                execute(&plan(actions.clone()), &mut transport),
                Err(ExecuteError::Transport { .. })
            ),
            "boundary {fail_at}"
        );
        assert_eq!(
            transport.operation,
            fail_at + 1,
            "must stop at first disconnect"
        );
    }
}

#[test]
fn mtu_matrix_chunks_exactly_and_rejects_invalid_limits() {
    for mtu in [23, 64, 185, 517] {
        let bytes = vec![0x55; 1100];
        let mut transport = FakeClockTransport::succeed(mtu);
        execute(
            &plan(vec![Action::RasterWrite {
                bytes: bytes.clone(),
                logical_chunk: 600,
                delay_after_each_physical_write_ms: 0,
            }]),
            &mut transport,
        )
        .unwrap();
        let sizes = transport
            .events
            .iter()
            .filter_map(|event| event.strip_prefix("write:")?.parse::<usize>().ok())
            .collect::<Vec<_>>();
        assert_eq!(sizes.iter().sum::<usize>(), bytes.len());
        assert!(sizes.iter().all(|size| *size <= mtu));
    }
    let mut zero = FakeClockTransport::succeed(0);
    assert!(matches!(
        execute(&plan(vec![]), &mut zero),
        Err(ExecuteError::InvalidPlan { .. })
    ));
    let mut transport = FakeClockTransport::succeed(23);
    assert!(matches!(
        execute(
            &plan(vec![Action::RasterWrite {
                bytes: vec![1],
                logical_chunk: 0,
                delay_after_each_physical_write_ms: 0
            }]),
            &mut transport
        ),
        Err(ExecuteError::InvalidPlan { .. })
    ));
}

#[test]
fn replay_guard_marks_before_first_transport_effect() {
    let job = plan(vec![Action::CommandWrite {
        name: "once".into(),
        bytes: vec![1],
        atomic: true,
    }]);
    let mut guard = ReplayGuard::default();
    let mut failed = FakeClockTransport {
        fail_at: Some(0),
        ..FakeClockTransport::succeed(23)
    };
    assert!(matches!(
        guard.execute_once("job-1", &job, &mut failed),
        Err(ExecuteError::Transport { .. })
    ));
    let mut retry = FakeClockTransport::succeed(23);
    assert!(matches!(
        guard.execute_once("job-1", &job, &mut retry),
        Err(ExecuteError::Replay(_))
    ));
    assert!(
        retry.events.is_empty(),
        "replay rejection precedes transport access"
    );
}
