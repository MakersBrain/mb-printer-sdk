// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "snmp")]

use mb_printer_core::snmp::{ObjectId, ObjectRegistry, RegisteredObject};
use mb_printer_native::transports::snmp::{ClientLimits, Community, SnmpClient};
use std::{net::UdpSocket, thread, time::Duration};

fn registry() -> (ObjectRegistry, ObjectId) {
    let oid = ObjectId::parse("1.3.6.1.2.1.43.5.1.1.16.1").unwrap();
    let mut registry = ObjectRegistry::default();
    registry
        .register(RegisteredObject {
            oid: oid.clone(),
            semantic_id: "printer-name".into(),
            sensitive: false,
        })
        .unwrap();
    (registry, oid)
}

#[test]
fn native_get_uses_caller_runtime_and_redacts_credentials() {
    let server = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let endpoint = server.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let mut request = [0; 2048];
        let (length, peer) = server.recv_from(&mut request).unwrap();
        let response = &mut request[..length];
        let pdu = response.iter().position(|byte| *byte == 0xa0).unwrap();
        response[pdu] = 0xa2;
        server.send_to(response, peer).unwrap();
    });
    let (registry, oid) = registry();
    let community = Community::new("sensitive-community").unwrap();
    assert_eq!(format!("{community:?}"), "Community([REDACTED])");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime
        .block_on(SnmpClient.get(
            endpoint,
            &registry,
            &community,
            &oid,
            77,
            ClientLimits {
                timeout: Duration::from_secs(1),
                retries: 0,
                ..ClientLimits::default()
            },
        ))
        .unwrap();
    handle.join().unwrap();
    assert_eq!(response.len(), 1);
    assert_eq!(response[0].oid, oid);
}
