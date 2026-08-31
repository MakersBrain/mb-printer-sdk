// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::protocol::*;
use mb_printer_native::*;
#[derive(Default)]
struct Mock {
    writes: Vec<Vec<u8>>,
    delays: Vec<u64>,
    fail_write: bool,
    response: Option<Vec<u8>>,
    /// Frames delivered in order before the transport goes quiet.
    frames: Vec<Vec<u8>>,
    /// Stop answering once the queued response/frames are consumed.
    exhaust: bool,
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
        if !self.frames.is_empty() {
            return Ok(WaitOutcome::Response(self.frames.remove(0)));
        }
        match self.response.take() {
            Some(bytes) => Ok(WaitOutcome::Response(bytes)),
            None if self.exhaust => Ok(WaitOutcome::Timeout),
            None => Ok(WaitOutcome::Response(vec![1])),
        }
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
            exhaust: true,
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

#[test]
fn validated_responses_are_captured_for_status_queries() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::Brother,
        source_commit: String::new(),
        actions: vec![Action::WaitForResponse {
            timeout_ms: 1,
            fallback_delay_ms: 0,
            validation: ResponseValidation::BrotherStatus32,
        }],
    };
    let mut status = vec![0; 32];
    status[..3].copy_from_slice(&[0x80, 0x20, 0x42]);
    status[10] = 62;
    let mut transport = Mock {
        response: Some(status.clone()),
        ..Default::default()
    };
    let progress = execute(&plan, &mut transport).unwrap();
    assert_eq!(progress.responses, vec![status]);
}

#[test]
fn a_split_brother_status_frame_is_reassembled() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::Brother,
        source_commit: String::new(),
        actions: vec![Action::WaitForResponse {
            timeout_ms: 1,
            fallback_delay_ms: 0,
            validation: ResponseValidation::BrotherStatus32,
        }],
    };
    let mut head = vec![0; 16];
    head[..3].copy_from_slice(&[0x80, 0x20, 0x42]);
    let mut transport = Mock {
        frames: vec![head, vec![0; 16]],
        exhaust: true,
        ..Default::default()
    };
    let progress = execute(&plan, &mut transport).unwrap();
    assert_eq!(progress.responses.last().map(Vec::len), Some(32));
}

#[test]
fn phomemo_query_echo_is_ignored_before_notification() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::M110,
        source_commit: String::new(),
        actions: vec![Action::WaitForResponse {
            timeout_ms: 1,
            fallback_delay_ms: 0,
            validation: ResponseValidation::PhomemoNotification,
        }],
    };
    let mut transport = Mock {
        frames: vec![vec![0x1f, 0x11, 0x08], vec![0x1a, 0x04, 100]],
        exhaust: true,
        ..Default::default()
    };
    let progress = execute(&plan, &mut transport).unwrap();
    assert_eq!(progress.responses, vec![vec![0x1a, 0x04, 100]]);
}

fn collect_plan(maximum_bytes: usize, validation: ResponseValidation) -> Plan {
    Plan {
        protocol: mb_printer_core::capabilities::Protocol::Brother,
        source_commit: String::new(),
        actions: vec![Action::CollectResponse {
            timeout_ms: 100,
            idle_timeout_ms: 1,
            maximum_bytes,
            validation,
        }],
    }
}

#[test]
fn multipart_response_is_reassembled_at_every_marker_split() {
    let response = b"prefix<<PRINTER CONFIGURATION>>\r\nbody";
    for split in 1..response.len() {
        let mut transport = Mock {
            frames: vec![response[..split].to_vec(), response[split..].to_vec()],
            exhaust: true,
            ..Default::default()
        };
        let progress = execute(
            &collect_plan(response.len(), ResponseValidation::BrotherSystemReport),
            &mut transport,
        )
        .unwrap();
        assert_eq!(progress.responses, [response]);
    }
}

#[test]
fn multipart_response_distinguishes_initial_and_idle_timeout() {
    let mut no_first_packet = Mock {
        exhaust: true,
        ..Default::default()
    };
    assert!(matches!(
        execute(
            &collect_plan(1024, ResponseValidation::BrotherObjbrnet),
            &mut no_first_packet
        ),
        Err(ExecuteError::Timeout { .. })
    ));

    let partial = b"@PJL INFO OBJBRNET\r\n\"458867:1\"\r\n".to_vec();
    let mut idle_after_partial = Mock {
        frames: vec![partial.clone()],
        exhaust: true,
        ..Default::default()
    };
    let progress = execute(
        &collect_plan(partial.len(), ResponseValidation::BrotherObjbrnet),
        &mut idle_after_partial,
    )
    .unwrap();
    assert_eq!(progress.responses, [partial]);
}

#[test]
fn multipart_response_enforces_exact_and_oversized_bounds() {
    let exact = b"VAP,network,0,0,1,-40,0,2".to_vec();
    let mut exact_transport = Mock {
        frames: vec![exact.clone()],
        exhaust: true,
        ..Default::default()
    };
    assert_eq!(
        execute(
            &collect_plan(exact.len(), ResponseValidation::BrotherWifiScan),
            &mut exact_transport,
        )
        .unwrap()
        .responses
        .as_slice(),
        std::slice::from_ref(&exact)
    );

    let exact_length = exact.len();
    let mut oversized = Mock {
        frames: vec![exact, vec![0]],
        exhaust: true,
        ..Default::default()
    };
    let error = execute(
        &collect_plan(exact_length, ResponseValidation::BrotherWifiScan),
        &mut oversized,
    )
    .unwrap_err();
    assert!(matches!(error, ExecuteError::Response { .. }));
    assert!(!error.to_string().contains("network"));
}

#[test]
fn multipart_response_that_never_idles_terminates_at_a_bound() {
    let mut transport = Mock::default();
    let error = execute(
        &collect_plan(3, ResponseValidation::BrotherSystemReport),
        &mut transport,
    )
    .unwrap_err();
    assert!(matches!(error, ExecuteError::Response { .. }));
}

#[test]
fn multipart_response_rejects_zero_bounds_before_transport_io() {
    let plan = Plan {
        protocol: mb_printer_core::capabilities::Protocol::Brother,
        source_commit: String::new(),
        actions: vec![Action::CollectResponse {
            timeout_ms: 0,
            idle_timeout_ms: 1,
            maximum_bytes: 1,
            validation: ResponseValidation::BrotherObjbrnet,
        }],
    };
    assert!(matches!(
        execute(&plan, &mut Mock::default()),
        Err(ExecuteError::InvalidPlan { action: 0, .. })
    ));
}
