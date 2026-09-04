// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "blocking")]

use mb_printer_core::{
    capabilities::Protocol,
    protocol::{Action, Plan, SOURCE_COMMIT},
};
use mb_printer_native::{
    ExecuteError, NotificationSupport, Transport, TransportError, TransportErrorKind,
    TransportFuture, WaitOutcome, WriteKind, blocking::BlockingPrinterClient,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

struct FakeTransport {
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Transport for FakeTransport {
    fn payload_limit(&self) -> usize {
        16
    }
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        Box::pin(async { Ok(NotificationSupport::Unavailable) })
    }
    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        _: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        self.writes.lock().unwrap().push(bytes.to_vec());
        Box::pin(async { Ok(()) })
    }
    fn wait_response(
        &mut self,
        _: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        Box::pin(async { Ok(WaitOutcome::Timeout) })
    }
    fn delay(&mut self, _: Duration) -> TransportFuture<'_, ()> {
        Box::pin(async {})
    }
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }
}

fn plan() -> Plan {
    Plan {
        protocol: Protocol::M02,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![Action::CommandWrite {
            bytes: vec![1, 2, 3],
            atomic: true,
            name: "test".into(),
        }],
    }
}

fn wait_plan() -> Plan {
    Plan {
        protocol: Protocol::M02,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![Action::WaitForResponse {
            timeout_ms: 1,
            fallback_delay_ms: 0,
            validation: mb_printer_core::protocol::ResponseValidation::AnyNotification,
        }],
    }
}

#[test]
fn blocking_client_executes_and_reports_progress_from_sync_code() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut client = BlockingPrinterClient::from_transport(FakeTransport {
        writes: writes.clone(),
    })
    .unwrap();
    let mut progress = Vec::new();
    let result = client
        .execute_with_progress(plan(), |event| progress.push(event.clone()))
        .unwrap();
    assert_eq!(result.bytes_written, 3);
    assert_eq!(progress.last(), Some(&result));
    assert_eq!(*writes.lock().unwrap(), [vec![1, 2, 3]]);
    client.disconnect().unwrap();
}

#[tokio::test]
async fn blocking_client_never_nests_the_callers_runtime() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut client = BlockingPrinterClient::from_transport(FakeTransport { writes }).unwrap();
    assert_eq!(client.execute(plan()).unwrap().bytes_written, 3);
    client.disconnect().unwrap();
}

#[test]
fn drop_performs_bounded_best_effort_shutdown() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let client = BlockingPrinterClient::from_transport(FakeTransport { writes }).unwrap();
    drop(client);
}

#[test]
fn response_timeout_crosses_the_blocking_boundary_with_progress() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut client = BlockingPrinterClient::from_transport(FakeTransport { writes }).unwrap();
    assert!(matches!(
        client.execute(wait_plan()),
        Err(ExecuteError::Timeout { .. })
    ));
    client.disconnect().unwrap();
}

struct CancelledWait;
impl Transport for CancelledWait {
    fn payload_limit(&self) -> usize {
        16
    }
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        Box::pin(async { Ok(NotificationSupport::Unavailable) })
    }
    fn write<'a>(
        &'a mut self,
        _: &'a [u8],
        _: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }
    fn wait_response(
        &mut self,
        _: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        Box::pin(async {
            Err(TransportError::new(
                TransportErrorKind::Cancelled,
                "test cancellation",
            ))
        })
    }
    fn delay(&mut self, _: Duration) -> TransportFuture<'_, ()> {
        Box::pin(async {})
    }
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn cancellation_crosses_the_blocking_boundary_canonically() {
    let mut client = BlockingPrinterClient::from_transport(CancelledWait).unwrap();
    assert!(matches!(
        client.execute(wait_plan()),
        Err(ExecuteError::Cancelled { .. })
    ));
    client.disconnect().unwrap();
}

struct TerminatingTransport;
impl Transport for TerminatingTransport {
    fn payload_limit(&self) -> usize {
        16
    }
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        Box::pin(async { Ok(NotificationSupport::Unavailable) })
    }
    fn write<'a>(
        &'a mut self,
        _: &'a [u8],
        _: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        panic!("simulated worker termination")
    }
    fn wait_response(
        &mut self,
        _: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        Box::pin(async { Ok(WaitOutcome::Unavailable) })
    }
    fn delay(&mut self, _: Duration) -> TransportFuture<'_, ()> {
        Box::pin(async {})
    }
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn worker_termination_is_reported_without_hanging_the_caller() {
    let mut client = BlockingPrinterClient::from_transport(TerminatingTransport).unwrap();
    assert!(matches!(
        client.execute(plan()),
        Err(ExecuteError::Transport { .. })
    ));
}
