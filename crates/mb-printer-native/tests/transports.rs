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
