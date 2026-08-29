// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::protocol::*;
use mb_printer_native::*;
#[derive(Default)]
struct Mock {
    writes: Vec<Vec<u8>>,
    delays: Vec<u64>,
    fail_write: bool,
    response: Option<Vec<u8>>,
    wait: Option<WaitOutcome>,
    command_limit: Option<usize>,
    raster_limit: Option<usize>,
}
impl Transport for Mock {
    fn payload_limit(&self) -> usize {
        self.raster_limit.unwrap_or(2)
    }
    fn subscribe_notifications(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn command_limit(&self) -> usize {
        self.command_limit.unwrap_or_else(|| self.payload_limit())
    }
    fn write(&mut self, b: &[u8]) -> Result<(), String> {
        if self.fail_write {
            return Err("ambiguous disconnect".into());
        }
        self.writes.push(b.into());
        Ok(())
    }
    fn delay_monotonic(&mut self, n: u64) {
        self.delays.push(n)
    }
    fn wait_response(&mut self, _: u64) -> Result<WaitOutcome, String> {
        if let Some(outcome) = self.wait.take() {
            return Ok(outcome);
        }
        Ok(WaitOutcome::Response(
            self.response.take().unwrap_or_else(|| vec![1]),
        ))
    }
}
#[test]
fn first_write_error_is_marked_potentially_accepted() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::MSeries,
        source_commit: String::new(),
        actions: vec![Action::CommandWrite {
            name: "first".into(),
            bytes: vec![1],
            atomic: true,
        }],
    };
    let mut transport = Mock {
        fail_write: true,
        ..Default::default()
    };
    let Err(ExecuteError::Transport { progress, .. }) = execute(&plan, &mut transport) else {
        panic!("expected transport failure")
    };
    assert_eq!(progress.bytes_written, 0);
    assert!(progress.potentially_accepted_write);
    assert!(transport.writes.is_empty());
}
#[test]
fn brother_status_policy_requires_exactly_32_bytes() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::Brother,
        source_commit: String::new(),
        actions: vec![Action::WaitForResponse {
            timeout_ms: 1,
            fallback_delay_ms: 0,
            validation: ResponseValidation::BrotherStatus32,
        }],
    };
    for length in [31, 33] {
        let mut bytes = vec![0; length];
        bytes[..3].copy_from_slice(&[0x80, 0x20, 0x42]);
        let mut transport = Mock {
            response: Some(bytes),
            ..Default::default()
        };
        assert!(matches!(
            execute(&plan, &mut transport),
            Err(ExecuteError::Response { .. })
        ));
    }
    let mut bytes = vec![0; 32];
    bytes[..3].copy_from_slice(&[0x80, 0x20, 0x42]);
    let mut transport = Mock {
        response: Some(bytes),
        ..Default::default()
    };
    assert!(execute(&plan, &mut transport).is_ok());

    // The Python and JavaScript Brother drivers treat an absent preflight
    // status as optional (notably on raw TCP port 9100).
    for response in [WaitOutcome::Unavailable, WaitOutcome::Timeout] {
        let mut transport = Mock {
            response: None,
            wait: Some(response),
            ..Default::default()
        };
        assert!(execute(&plan, &mut transport).is_ok());
    }
}
#[test]
fn raster_is_physically_split_and_paced() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::MSeries,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![Action::RasterWrite {
            bytes: vec![1, 2, 3, 4, 5],
            logical_chunk: 4,
            delay_after_each_physical_write_ms: 20,
        }],
    };
    let mut m = Mock::default();
    let p = execute(&plan, &mut m).unwrap();
    assert_eq!(m.writes, vec![vec![1, 2], vec![3, 4], vec![5]]);
    assert_eq!(m.delays, vec![20, 20, 20]);
    assert_eq!(p.bytes_written, 5)
}
#[test]
fn physical_writes_restart_at_every_logical_chunk_boundary() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::MSeries,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![Action::RasterWrite {
            bytes: (0..10).collect(),
            logical_chunk: 4,
            delay_after_each_physical_write_ms: 20,
        }],
    };
    let mut transport = Mock {
        raster_limit: Some(3),
        ..Default::default()
    };
    execute(&plan, &mut transport).unwrap();
    assert_eq!(
        transport.writes.iter().map(Vec::len).collect::<Vec<_>>(),
        [3, 1, 3, 1, 2]
    );
    assert_eq!(transport.delays, [20; 5]);
}
#[test]
fn atomic_preflight_occurs_before_writes() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::MSeries,
        source_commit: String::new(),
        actions: vec![Action::CommandWrite {
            name: "x".into(),
            bytes: vec![1, 2, 3],
            atomic: true,
        }],
    };
    let mut m = Mock::default();
    assert!(matches!(
        execute(&plan, &mut m),
        Err(ExecuteError::AtomicTooLarge { .. })
    ));
    assert!(m.writes.is_empty())
}

#[test]
fn command_and_raster_limits_are_independent() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::Brother,
        source_commit: String::new(),
        actions: vec![
            Action::CommandWrite {
                name: "invalidate".into(),
                bytes: vec![0; 200],
                atomic: true,
            },
            Action::RasterWrite {
                bytes: vec![1; 130],
                logical_chunk: 1024,
                delay_after_each_physical_write_ms: 0,
            },
        ],
    };
    let mut transport = Mock {
        command_limit: Some(512),
        raster_limit: Some(64),
        ..Default::default()
    };
    execute(&plan, &mut transport).unwrap();
    assert_eq!(
        transport.writes.iter().map(Vec::len).collect::<Vec<_>>(),
        [200, 64, 64, 2]
    );
}

#[test]
fn timing_policy_preserves_or_only_explicitly_changes_reference_delays() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::MSeries,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![
            Action::Delay { milliseconds: 20 },
            Action::RasterWrite {
                bytes: vec![1, 2, 3],
                logical_chunk: 3,
                delay_after_each_physical_write_ms: 10,
            },
        ],
    };
    let mut safe = Mock::default();
    execute_with_timing(&plan, &mut safe, ReferenceTiming::IncreaseBy(5)).unwrap();
    assert_eq!(safe.delays, [25, 15, 15]);
    let mut diagnostic = Mock::default();
    execute_with_timing(
        &plan,
        &mut diagnostic,
        ReferenceTiming::UnsafeDiagnosticReduceBy(7),
    )
    .unwrap();
    assert_eq!(diagnostic.delays, [13, 3, 3]);
}
