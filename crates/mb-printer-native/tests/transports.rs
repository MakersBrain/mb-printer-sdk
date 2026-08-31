// SPDX-License-Identifier: AGPL-3.0-or-later
#[cfg(any(
    feature = "serial",
    feature = "usb",
    feature = "ble",
    feature = "wifi",
    feature = "native-input"
))]
use mb_printer_native::{Transport, WaitOutcome, transports::*};

#[cfg(feature = "serial")]
#[test]
fn serial_discovery_is_filtered_and_sorted() {
    let directory = std::env::temp_dir().join(format!("mb-printer-serial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("ttyUSB2"), []).unwrap();
    std::fs::write(directory.join("ttyACM0"), []).unwrap();
    std::fs::write(directory.join("not-a-port"), []).unwrap();
    let found = serial::SerialDiscovery {
        directories: vec![directory.clone()],
    }
    .discover()
    .unwrap();
    assert_eq!(
        found
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["ttyACM0", "ttyUSB2"]
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(feature = "usb")]
#[test]
fn usb_backend_preserves_writes_and_timeout() {
    #[derive(Default)]
    struct Fake {
        writes: Vec<Vec<u8>>,
        reply: Option<Vec<u8>>,
    }
    impl usb::UsbBulkBackend for Fake {
        fn write_bulk(&mut self, b: &[u8]) -> Result<(), String> {
            self.writes.push(b.into());
            Ok(())
        }
        fn read_bulk(&mut self, _: u64, _: usize) -> Result<Option<Vec<u8>>, String> {
            Ok(self.reply.take())
        }
    }
    let mut transport = usb::UsbTransport::new(Fake::default(), 64, 32);
    transport.write(&[1, 2]).unwrap();
    assert_eq!(transport.backend().writes, vec![vec![1, 2]]);
    assert_eq!(transport.wait_response(5).unwrap(), WaitOutcome::Timeout);
}

#[cfg(feature = "usb")]
#[test]
fn rusb_discovery_never_uses_panicking_global_context() {
    let result = std::panic::catch_unwind(usb::discover_rusb);
    assert!(
        result.is_ok(),
        "USB discovery must return Result instead of panicking"
    );
    assert!(
        std::panic::catch_unwind(usb::discover_rusb_bulk).is_ok(),
        "bulk-interface discovery must also return Result instead of panicking"
    );
}

#[cfg(feature = "usb")]
#[test]
fn usb_candidate_selection_is_injectable_and_deterministic() {
    let wanted = usb::UsbIdentity {
        vendor_id: 0x04f9,
        product_id: 0x209b,
        bus: 2,
        address: 7,
    };
    let candidate = |identity, class, interface, alternate, endpoint| usb::UsbBulkCandidate {
        identity,
        interface,
        alternate_setting: alternate,
        out_endpoint: endpoint,
        in_endpoint: None,
        max_packet_size: 64,
        interface_class: class,
        manufacturer: None,
        product: None,
        serial_number: None,
    };
    let other = usb::UsbIdentity {
        address: 8,
        ..wanted
    };
    let candidates = vec![
        candidate(wanted, 255, 0, 0, 1),
        candidate(other, 7, 0, 0, 2),
        candidate(wanted, 7, 2, 1, 4),
        candidate(wanted, 7, 1, 0, 3),
    ];
    let selected = usb::select_bulk_candidate(&candidates, wanted).unwrap();
    assert_eq!((selected.interface_class, selected.interface), (7, 1));
    assert!(
        usb::select_bulk_candidate(
            &candidates,
            usb::UsbIdentity {
                address: 9,
                ..wanted
            }
        )
        .is_none()
    );
}

#[cfg(feature = "usb")]
#[test]
fn usb_printer_class_device_id_is_bounded_and_typed() {
    let payload = b"MFG:Brother;MDL:QL-1110NWB;CMD:PJL,RASTER;CLS:PRINTER;";
    let mut bytes = u16::try_from(payload.len() + 2)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(payload);
    let parsed = usb::parse_ieee1284_device_id(&bytes).unwrap();
    assert_eq!(parsed.manufacturer.as_deref(), Some("Brother"));
    assert_eq!(parsed.model.as_deref(), Some("QL-1110NWB"));
    assert_eq!(parsed.command_sets, ["PJL", "RASTER"]);
    assert_eq!(parsed.fields["CLS"], "PRINTER");
    assert_eq!(parsed.raw, String::from_utf8(payload.to_vec()).unwrap());

    for malformed in [
        vec![],
        vec![0],
        vec![0, 1],
        vec![0, 10, b'M'],
        vec![0, 4, 0xff, b';'],
        vec![0, 4, b'x', b';'],
    ] {
        assert!(usb::parse_ieee1284_device_id(&malformed).is_err());
    }
    let oversized = vec![0; usb::MAX_IEEE1284_DEVICE_ID_BYTES + 1];
    assert!(usb::parse_ieee1284_device_id(&oversized).is_err());
}

#[cfg(feature = "usb")]
#[test]
fn usb_printer_class_queries_are_injectable_and_bounded() {
    #[derive(Default)]
    struct Fake {
        device_id: Option<Vec<u8>>,
        port_status: Option<u8>,
        device_calls: Vec<(u64, usize)>,
        port_calls: Vec<u64>,
    }
    impl usb::UsbPrinterClassBackend for Fake {
        fn get_device_id_raw(
            &mut self,
            timeout_ms: u64,
            maximum: usize,
        ) -> Result<Option<Vec<u8>>, String> {
            self.device_calls.push((timeout_ms, maximum));
            Ok(self.device_id.take())
        }

        fn get_port_status_raw(&mut self, timeout_ms: u64) -> Result<Option<u8>, String> {
            self.port_calls.push(timeout_ms);
            Ok(self.port_status.take())
        }
    }

    let payload = b"MANUFACTURER:Brother;MODEL:QL-1100;COMMAND SET:PJL;";
    let mut device_id = u16::try_from(payload.len() + 2)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    device_id.extend_from_slice(payload);
    let mut transport = usb::UsbTransport::new(
        Fake {
            device_id: Some(device_id),
            port_status: Some(0x18),
            ..Default::default()
        },
        64,
        64,
    );
    let identifier = transport.get_device_id(250).unwrap().unwrap();
    assert_eq!(identifier.manufacturer.as_deref(), Some("Brother"));
    assert_eq!(identifier.model.as_deref(), Some("QL-1100"));
    assert_eq!(
        transport.get_port_status(300).unwrap(),
        Some(usb::UsbPortStatus {
            selected: true,
            paper_empty: false,
            error: false,
        })
    );
    assert_eq!(
        transport.backend().device_calls,
        [(250, usb::MAX_IEEE1284_DEVICE_ID_BYTES)]
    );
    assert_eq!(transport.backend().port_calls, [300]);

    let mut invalid = usb::UsbTransport::new(Fake::default(), 64, 64);
    assert!(invalid.get_device_id(0).is_err());
    assert!(invalid.get_port_status(0).is_err());
    assert!(invalid.backend().device_calls.is_empty());
    assert!(invalid.backend().port_calls.is_empty());
}

#[cfg(feature = "usb")]
#[test]
fn usb_port_status_bits_and_serial_revalidation_are_explicit() {
    assert_eq!(
        usb::parse_port_status(0x38),
        usb::UsbPortStatus {
            selected: true,
            paper_empty: true,
            error: false,
        }
    );
    assert!(usb::parse_port_status(0).error);
    assert!(usb::verify_expected_serial("QL-A", "QL-A").is_ok());
    assert!(usb::verify_expected_serial("QL-A", "QL-B").is_err());
}

#[cfg(feature = "serial")]
#[test]
fn serial_configuration_has_explicit_printer_defaults() {
    let config = serial::SerialConfig::default();
    assert_eq!(
        (
            config.baud_rate,
            config.timeout_ms,
            config.payload_limit,
            config.response_limit
        ),
        (115_200, 500, 512, 64)
    );
}

#[cfg(feature = "ble")]
#[test]
fn ble_backend_distinguishes_unavailable_timeout_and_notification() {
    struct Fake {
        available: bool,
        reply: Option<Vec<u8>>,
    }
    impl ble::BleGattBackend for Fake {
        fn subscribe(&mut self) -> Result<bool, String> {
            Ok(self.available)
        }
        fn write_without_response(&mut self, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn wait_notification(&mut self, _: u64) -> Result<Option<Vec<u8>>, String> {
            Ok(self.reply.take())
        }
    }
    let mut unavailable = ble::BleTransport::new(
        Fake {
            available: false,
            reply: None,
        },
        20,
    );
    unavailable.subscribe_notifications().unwrap();
    assert_eq!(
        unavailable.wait_response(1).unwrap(),
        WaitOutcome::Unavailable
    );
    let mut connected = ble::BleTransport::new(
        Fake {
            available: true,
            reply: Some(vec![9]),
        },
        20,
    );
    connected.subscribe_notifications().unwrap();
    assert_eq!(
        connected.wait_response(1).unwrap(),
        WaitOutcome::Response(vec![9])
    );
    assert_eq!(connected.wait_response(1).unwrap(), WaitOutcome::Timeout);
}

#[cfg(feature = "ble")]
#[test]
fn injectable_async_ble_serializes_notifies_and_disconnects() {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct State {
        events: Arc<Mutex<Vec<String>>>,
        in_flight: Arc<AtomicUsize>,
        maximum_in_flight: Arc<AtomicUsize>,
    }
    struct Fake {
        state: State,
        reply: Option<Vec<u8>>,
    }
    impl ble::AsyncBleGattBackend for Fake {
        fn subscribe(&mut self) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>> {
            Box::pin(async {
                self.state.events.lock().unwrap().push("subscribe".into());
                Ok(true)
            })
        }
        fn write_without_response<'a>(
            &'a mut self,
            bytes: &'a [u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async move {
                let current = self.state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                self.state
                    .maximum_in_flight
                    .fetch_max(current, Ordering::SeqCst);
                self.state
                    .events
                    .lock()
                    .unwrap()
                    .push(format!("write:{}", bytes[0]));
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        }
        fn wait_notification(
            &mut self,
            timeout_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, String>> + Send + '_>> {
            Box::pin(async move {
                self.state
                    .events
                    .lock()
                    .unwrap()
                    .push(format!("wait:{timeout_ms}"));
                Ok(self.reply.take())
            })
        }
        fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            Box::pin(async {
                self.state.events.lock().unwrap().push("disconnect".into());
                Ok(())
            })
        }
    }
    let state = State {
        events: Arc::new(Mutex::new(vec![])),
        in_flight: Arc::new(AtomicUsize::new(0)),
        maximum_in_flight: Arc::new(AtomicUsize::new(0)),
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let transport = Arc::new(
            ble::AsyncBleTransport::new(
                Fake {
                    state: state.clone(),
                    reply: Some(vec![9]),
                },
                20,
            )
            .unwrap(),
        );
        assert_eq!(
            transport.wait_notification(1).await.unwrap(),
            WaitOutcome::Unavailable
        );
        assert!(transport.subscribe_notifications().await.unwrap());
        let (left, right) =
            futures_util::future::join(transport.write(&[1]), transport.write(&[2])).await;
        left.unwrap();
        right.unwrap();
        assert_eq!(
            transport.wait_notification(25).await.unwrap(),
            WaitOutcome::Response(vec![9])
        );
        assert_eq!(
            transport.wait_notification(25).await.unwrap(),
            WaitOutcome::Timeout
        );
        assert!(transport.write(&[0; 21]).await.is_err());
        transport.disconnect().await.unwrap();
    });
    assert_eq!(state.maximum_in_flight.load(Ordering::SeqCst), 1);
    let events = state.events.lock().unwrap();
    assert_eq!(events.first().map(String::as_str), Some("subscribe"));
    assert_eq!(events.last().map(String::as_str), Some("disconnect"));
}

#[cfg(feature = "wifi")]
#[test]
fn wifi_credentials_never_debug_the_secret() {
    let value = wifi::WifiCredentials {
        ssid: "label-net".into(),
        password: "secret".into(),
    };
    let debug = format!("{value:?}");
    assert!(debug.contains("label-net"));
    assert!(!debug.contains("secret"));
}

#[cfg(feature = "wifi")]
#[test]
fn brother_wifi_commands_and_parsers_match_reference_contract() {
    let settings = wifi::WirelessSettings {
        ssid: "Café".into(),
        password: "secret".into(),
        encryption: "tkip-aes".into(),
        authentication: "wpa-psk".into(),
        infrastructure: true,
        wireless_direct: false,
        reboot: false,
    };
    let command = settings.command().unwrap();
    assert!(command.starts_with(wifi::PJL_HEADER));
    let encoded_ssid = b"458877:-43-61-66-c3";
    assert!(
        command
            .windows(encoded_ssid.len())
            .any(|part| part == encoded_ssid)
    );
    assert!(!command.windows(6).any(|part| part == b"secret"));
    assert_eq!(wifi::parse_wifi_status(b"\"458867 : 1\r\n"), Some(true));
    assert_eq!(
        wifi::parse_ip_address(b"458967.2:-c0-a8-01-2a\r\n"),
        Some("192.168.1.42".into())
    );
    let points = wifi::parse_access_points(b"VAP,\"-43-61-66-c3-a9\",x,x,6,-42,3,2\r\n");
    assert_eq!(points[0].ssid, "Café");
    assert!(points[0].enterprise && points[0].encrypted);
}

#[cfg(feature = "wifi")]
#[test]
fn ipp_status_parser_preserves_state_reasons_and_media() {
    fn attribute(output: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
        output.push(tag);
        output.extend((name.len() as u16).to_be_bytes());
        output.extend(name.as_bytes());
        output.extend((value.len() as u16).to_be_bytes());
        output.extend(value);
    }
    let mut body = vec![2, 0, 0, 0, 0, 0, 0, 1, 4];
    attribute(&mut body, 0x23, "printer-state", &4u32.to_be_bytes());
    attribute(&mut body, 0x44, "printer-state-reasons", b"media-empty");
    attribute(&mut body, 0x44, "media-ready", b"roll_62x29mm");
    body.push(3);
    let status = wifi::parse_ipp_status(&body).unwrap();
    assert_eq!(status.printer_state, Some(4));
    assert_eq!(status.reasons, ["media-empty"]);
    assert_eq!(status.media_ready, ["roll_62x29mm"]);
    assert!(wifi::parse_ipp_status(&body[..body.len() - 2]).is_err());
}

#[cfg(feature = "wifi")]
#[test]
fn ipp_query_and_discovery_use_the_reusable_loopback_boundary() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    fn server() -> (wifi::IppEndpoint, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            let header_end = loop {
                let length = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..length]);
                if let Some(split) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break split + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap();
            while request.len() < header_end + content_length {
                let length = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..length]);
            }
            assert!(
                request
                    .windows(20)
                    .any(|part| part == b"POST /ipp/print HTTP")
            );
            assert!(request.windows(11).any(|part| part == b"media-ready"));
            let mut body = vec![2, 0, 0, 0, 0, 0, 0, 1, 4];
            let mut attribute = |tag: u8, name: &str, value: &[u8]| {
                body.push(tag);
                body.extend((name.len() as u16).to_be_bytes());
                body.extend(name.as_bytes());
                body.extend((value.len() as u16).to_be_bytes());
                body.extend(value);
            };
            attribute(0x23, "printer-state", &3u32.to_be_bytes());
            attribute(0x44, "media-ready", b"roll_62x29mm");
            body.push(3);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        (
            wifi::IppEndpoint {
                scheme: wifi::IppScheme::Ipp,
                host: "127.0.0.1".into(),
                port,
                resource: "/ipp/print".into(),
            },
            handle,
        )
    }
    let (endpoint, handle) = server();
    let status = wifi::query_ipp_status(&endpoint, 1_000).unwrap();
    handle.join().unwrap();
    assert_eq!(status.printer_state, Some(3));
    assert_eq!(status.media_ready, ["roll_62x29mm"]);
    let (endpoint, handle) = server();
    let discovered = wifi::discover_ipp(std::slice::from_ref(&endpoint), 1_000);
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].0, endpoint);
    handle.join().unwrap();
}

#[cfg(feature = "native-input")]
#[test]
fn native_input_is_allowlisted_and_bounded() {
    use input::NativeInputBackend;
    let directory = std::env::temp_dir().join(format!("mb-printer-input-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory).unwrap();
    let pdf = directory.join("label.pdf");
    std::fs::write(&pdf, b"%PDF").unwrap();
    let backend = input::FileInputBackend { maximum_bytes: 4 };
    assert_eq!(backend.read(&pdf).unwrap().media_type, "application/pdf");
    assert!(backend.read(&directory.join("unknown.bin")).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}
