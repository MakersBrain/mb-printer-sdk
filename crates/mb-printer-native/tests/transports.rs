// SPDX-License-Identifier: AGPL-3.0-or-later
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
