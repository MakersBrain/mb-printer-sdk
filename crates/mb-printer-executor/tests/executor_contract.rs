// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use futures_executor::block_on;
use mb_printer_core::{
    capabilities::Protocol,
    protocol::{Action, Plan, ResponseValidation},
};
use mb_printer_executor::*;

struct BoundaryTransport {
    operation: usize,
    fail_at: usize,
    events: Vec<String>,
}

impl BoundaryTransport {
    fn operation(&mut self, event: String) -> Result<(), TransportError> {
        let current = self.operation;
        self.operation += 1;
        self.events.push(event);
        if self.fail_at == current {
            Err(TransportError::new(
                TransportErrorKind::Disconnected,
                "connection closed",
            ))
        } else {
            Ok(())
        }
    }
}

impl Transport for BoundaryTransport {
    fn payload_limit(&self) -> usize {
        23
    }

    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        let result = self.operation("subscribe".into());
        Box::pin(async move { result.map(|()| NotificationSupport::Subscribed) })
    }

    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        _: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        let result = self.operation(format!("write:{}", bytes.len()));
        Box::pin(async move { result })
    }

    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        let result = self.operation(format!("wait:{}", timeout.as_millis()));
        Box::pin(async move { result.map(|()| WaitOutcome::Response(vec![1])) })
    }

    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
        self.events.push(format!("delay:{}", duration.as_millis()));
        Box::pin(async {})
    }

    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn failure_at_each_io_boundary_stops_without_retry() {
    let plan = Plan {
        protocol: Protocol::MSeries,
        source_commit: String::new(),
        actions: vec![
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
        ],
    };
    for fail_at in 0..5 {
        let mut transport = BoundaryTransport {
            operation: 0,
            fail_at,
            events: vec![],
        };
        let error = block_on(execute(&plan, &mut transport)).unwrap_err();
        if fail_at == 0 || fail_at == 4 {
            assert!(matches!(error, ExecuteError::Transport { .. }));
        } else {
            assert!(matches!(error, ExecuteError::WriteOutcomeUnknown { .. }));
        }
        assert_eq!(transport.operation, fail_at + 1);
    }
}
