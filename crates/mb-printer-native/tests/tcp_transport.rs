// SPDX-License-Identifier: AGPL-3.0-or-later

use mb_printer_native::{
    NotificationSupport, Transport, WaitOutcome, WriteKind, transports::TcpTransport,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test]
async fn tcp_transport_uses_async_io_and_distinguishes_response_timeout_and_unavailable() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 3];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(request, [1, 2, 3]);
        stream.write_all(&[9, 8]).await.unwrap();
    });

    let mut transport = TcpTransport::connect(address, 64, 32).await.unwrap();
    assert_eq!(
        transport.subscribe_notifications().await.unwrap(),
        NotificationSupport::Unavailable
    );
    transport
        .write(&[1, 2, 3], WriteKind::Command)
        .await
        .unwrap();
    assert_eq!(
        transport
            .wait_response(Duration::from_secs(1))
            .await
            .unwrap(),
        WaitOutcome::Response(vec![9, 8])
    );
    server.await.unwrap();
    assert_eq!(
        transport
            .wait_response(Duration::from_secs(1))
            .await
            .unwrap(),
        WaitOutcome::Unavailable
    );
    transport.disconnect().await.unwrap();
}

#[tokio::test]
async fn tcp_transport_rejects_zero_limits_before_connecting() {
    let address = "127.0.0.1:9".parse().unwrap();
    assert!(TcpTransport::connect(address, 0, 1).await.is_err());
    assert!(TcpTransport::connect(address, 1, 0).await.is_err());
}
