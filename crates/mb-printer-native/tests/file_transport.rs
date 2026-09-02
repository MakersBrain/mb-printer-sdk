// SPDX-License-Identifier: AGPL-3.0-or-later

use mb_printer_native::{
    NotificationSupport, Transport, WaitOutcome, WriteKind, transports::FileTransport,
};
use std::time::Duration;

#[tokio::test]
async fn file_transport_is_async_and_flushes_on_disconnect() {
    let path = std::env::temp_dir().join(format!("mb-printer-file-{}", std::process::id()));
    std::fs::write(&path, []).unwrap();
    let mut transport = FileTransport::open(&path, 64).await.unwrap();
    assert_eq!(
        transport.subscribe_notifications().await.unwrap(),
        NotificationSupport::Unavailable
    );
    transport
        .write(&[1, 2, 3], WriteKind::Raster)
        .await
        .unwrap();
    assert_eq!(
        transport.wait_response(Duration::ZERO).await.unwrap(),
        WaitOutcome::Unavailable
    );
    transport.disconnect().await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3]);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn file_transport_rejects_zero_payload_limit() {
    let path = std::env::temp_dir().join(format!("mb-printer-file-zero-{}", std::process::id()));
    std::fs::write(&path, []).unwrap();
    assert!(FileTransport::open(&path, 0).await.is_err());
    std::fs::remove_file(path).unwrap();
}
