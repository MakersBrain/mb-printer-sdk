// SPDX-License-Identifier: AGPL-3.0-or-later

use mb_printer_core::{
    discovery::SettingValue,
    protocol::brother::wifi::{PJL_FOOTER, PJL_HEADER, WirelessField},
};
use mb_printer_native::{
    Transport, WaitOutcome,
    brother_settings::{field_id, is_sensitive, retrieve_wireless_settings},
};

#[derive(Default)]
struct FakeObjbrnetTransport {
    writes: Vec<Vec<u8>>,
    pending: Option<Vec<u8>>,
}

impl Transport for FakeObjbrnetTransport {
    fn payload_limit(&self) -> usize {
        4096
    }

    fn subscribe_notifications(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.writes.push(bytes.to_vec());
        if bytes == [0; 200] || bytes == b"\x1b@\x1bia\x01" || bytes == b"\x1biS" {
            return Ok(());
        }
        let field = WirelessField::ALL
            .into_iter()
            .find(|field| {
                bytes
                    .windows(field.oid().len())
                    .any(|part| part == field.oid().as_bytes())
            })
            .ok_or("request did not contain an allowlisted OID")?;
        let value = match field {
            WirelessField::Connected => "1",
            WirelessField::Ipv4 => "c0-a8-01-2a",
            WirelessField::Ssid => "-54-65-73-74",
            WirelessField::Encryption => "4",
            WirelessField::Authentication => "19",
            WirelessField::Infrastructure => "1",
            WirelessField::WirelessDirect => "0",
        };
        self.pending =
            Some(format!("@PJL INFO OBJBRNET\r\n\"{}:{value}\"\r\n", field.oid()).into_bytes());
        Ok(())
    }

    fn delay_monotonic(&mut self, _: u64) {}

    fn wait_response(&mut self, _: u64) -> Result<WaitOutcome, String> {
        Ok(self
            .pending
            .take()
            .map(WaitOutcome::Response)
            .unwrap_or(WaitOutcome::Timeout))
    }
}

#[test]
fn retrieval_uses_only_the_fixed_read_only_allowlist() {
    let mut transport = FakeObjbrnetTransport::default();
    let inspection = retrieve_wireless_settings(&mut transport);

    assert_eq!(inspection.observations.len(), WirelessField::ALL.len());
    assert_eq!(transport.writes.len(), WirelessField::ALL.len() * 4);
    for (parts, field) in transport.writes.chunks_exact(4).zip(WirelessField::ALL) {
        let request = &parts[0];
        assert!(request.starts_with(PJL_HEADER));
        assert!(request.ends_with(PJL_FOOTER));
        assert!(
            request
                .windows(field.oid().len())
                .any(|part| part == field.oid().as_bytes())
        );
        assert!(
            request
                .windows(b"@PJL INQUIRE OBJBRNET".len())
                .any(|part| part == b"@PJL INQUIRE OBJBRNET")
        );
        assert!(
            !request
                .windows(b"OBJBRNET=\"994588".len())
                .any(|part| part == b"OBJBRNET=\"994588")
        );
        assert_eq!(parts[1], [0; 200]);
        assert_eq!(parts[2], b"\x1b@\x1bia\x01");
        assert_eq!(parts[3], b"\x1biS");
    }

    assert_eq!(
        inspection.observations[0].value,
        Some(SettingValue::Integer(1))
    );
    assert_eq!(
        inspection.observations[1].value,
        Some(SettingValue::Text("192.168.1.42".into()))
    );
    assert_eq!(
        inspection.observations[2].value,
        Some(SettingValue::Text("Test".into()))
    );
    assert_eq!(
        inspection.observations[3].value,
        Some(SettingValue::Keyword("aes".into()))
    );
    assert_eq!(
        inspection.observations[4].value,
        Some(SettingValue::Keyword("wpa2-only".into()))
    );
    assert_eq!(
        inspection.observations[5].value,
        Some(SettingValue::Boolean(true))
    );
    assert_eq!(
        inspection.observations[6].value,
        Some(SettingValue::Boolean(false))
    );
    assert!(
        inspection
            .observations
            .iter()
            .all(|item| item.error.is_none())
    );
}

#[test]
fn field_metadata_does_not_claim_oid_458867_is_connection_status() {
    assert_eq!(field_id(WirelessField::Connected), "wireless-state");
    assert!(is_sensitive(WirelessField::Ipv4));
    assert!(is_sensitive(WirelessField::Ssid));
    assert!(!is_sensitive(WirelessField::Authentication));
}
