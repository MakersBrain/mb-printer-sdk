// SPDX-License-Identifier: AGPL-3.0-or-later

use mb_printer_core::{
    capabilities::Protocol,
    protocol::{Plan, SOURCE_COMMIT},
};
use mb_printer_native::{
    FileReplayStore, NotificationSupport, Transport, TransportError, TransportFuture, WaitOutcome,
    WriteKind, execute_once_with_store,
};
use std::time::Duration;

struct NoIo;
impl Transport for NoIo {
    fn payload_limit(&self) -> usize {
        1
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
        Box::pin(async { Ok(WaitOutcome::Unavailable) })
    }
    fn delay(&mut self, _: Duration) -> TransportFuture<'_, ()> {
        Box::pin(async {})
    }
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn filesystem_replay_claim_survives_store_reopen() {
    let directory = std::env::temp_dir().join(format!("mb-printer-replay-{}", std::process::id()));
    let plan = Plan {
        protocol: Protocol::M02,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![],
    };
    let mut first = FileReplayStore::new(&directory).unwrap();
    execute_once_with_store(&mut first, "durable-job", &plan, &mut NoIo)
        .await
        .unwrap();
    let mut reopened = FileReplayStore::new(&directory).unwrap();
    assert!(
        execute_once_with_store(&mut reopened, "durable-job", &plan, &mut NoIo)
            .await
            .is_err()
    );
    std::fs::remove_dir_all(directory).unwrap();
}
