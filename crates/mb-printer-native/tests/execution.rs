// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::protocol::*;
use mb_printer_native::*;
#[derive(Default)]
struct Mock {
    writes: Vec<Vec<u8>>,
    delays: Vec<u64>,
    fail_write: bool,
    response: Option<Vec<u8>>,
}
impl Transport for Mock {
    fn payload_limit(&self) -> usize {
        2
    }
    fn subscribe_notifications(&mut self) -> Result<(), String> {
        Ok(())
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
