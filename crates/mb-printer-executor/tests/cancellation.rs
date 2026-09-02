// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(not(target_arch = "wasm32"))]

use std::{sync::mpsc, time::Duration};

use futures_executor::block_on;
use mb_printer_core::{
    capabilities::Protocol,
    protocol::{Action, Plan, ResponseValidation},
};
use mb_printer_executor::*;

struct BlockedResponse {
    started: Option<mpsc::Sender<()>>,
}

struct BlockedWrite {
    started: Option<mpsc::Sender<()>>,
}

impl Transport for BlockedWrite {
    fn payload_limit(&self) -> usize {
        23
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
        let started = self.started.take();
        Box::pin(async move {
            if let Some(started) = started {
                started.send(()).unwrap();
            }
            std::future::pending().await
        })
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

impl Transport for BlockedResponse {
    fn payload_limit(&self) -> usize {
        23
    }

    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        Box::pin(async { Ok(NotificationSupport::Subscribed) })
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
        let started = self.started.take();
        Box::pin(async move {
            if let Some(started) = started {
                started.send(()).unwrap();
            }
            std::future::pending().await
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
fn cancellation_wakes_an_executor_blocked_in_response_wait() {
    let token = CancellationToken::default();
    let cancel_from_main = token.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let plan = Plan {
            protocol: Protocol::MSeries,
            source_commit: String::new(),
            actions: vec![Action::WaitForResponse {
                timeout_ms: 60_000,
                fallback_delay_ms: 0,
                validation: ResponseValidation::AnyNotification,
            }],
        };
        let mut transport = BlockedResponse {
            started: Some(started_tx),
        };
        block_on(execute_with_options(
            &plan,
            &mut transport,
            ExecutionOptions {
                timing: ReferenceTiming::Preserve,
                cancellation: &token,
            },
            |_| {},
        ))
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    cancel_from_main.cancel();
    let error = worker.join().unwrap().unwrap_err();
    let ExecuteError::Cancelled { progress } = error else {
        panic!("expected cancellation")
    };
    assert_eq!(progress.bytes_written, 0);
    assert!(!progress.potentially_accepted_write);
}

#[test]
fn already_cancelled_stops_before_any_transport_effect() {
    let token = CancellationToken::default();
    token.cancel();
    let (tx, _rx) = mpsc::channel();
    let mut transport = BlockedResponse { started: Some(tx) };
    let plan = Plan {
        protocol: Protocol::MSeries,
        source_commit: String::new(),
        actions: vec![Action::SubscribeNotifications],
    };
    assert!(matches!(
        block_on(execute_with_options(
            &plan,
            &mut transport,
            ExecutionOptions {
                timing: ReferenceTiming::Preserve,
                cancellation: &token,
            },
            |_| {},
        )),
        Err(ExecuteError::Cancelled { .. })
    ));
}

#[test]
fn cancellation_of_an_in_flight_write_has_unknown_outcome() {
    let token = CancellationToken::default();
    let cancel_from_main = token.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let plan = Plan {
            protocol: Protocol::MSeries,
            source_commit: String::new(),
            actions: vec![Action::CommandWrite {
                name: "write".into(),
                bytes: vec![1, 2, 3],
                atomic: true,
            }],
        };
        let mut transport = BlockedWrite {
            started: Some(started_tx),
        };
        block_on(execute_with_options(
            &plan,
            &mut transport,
            ExecutionOptions {
                timing: ReferenceTiming::Preserve,
                cancellation: &token,
            },
            |_| {},
        ))
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    cancel_from_main.cancel();
    let error = worker.join().unwrap().unwrap_err();
    let ExecuteError::WriteOutcomeUnknown { progress, source } = error else {
        panic!("expected uncertain write")
    };
    assert!(source.is_none());
    assert_eq!(progress.bytes_written, 0);
    assert!(progress.potentially_accepted_write);
}
