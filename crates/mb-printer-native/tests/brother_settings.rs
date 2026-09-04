// SPDX-License-Identifier: AGPL-3.0-or-later

use mb_printer_core::{
    discovery::SettingValue,
    protocol::brother::wifi::{PJL_FOOTER, PJL_HEADER, WirelessField},
};
use mb_printer_native::{
    NotificationSupport, Transport, TransportError, TransportErrorKind, TransportFuture,
    WaitOutcome, WriteKind,
    brother_settings::{field_id, is_sensitive, retrieve_wireless_settings},
};
use std::time::Duration;

#[derive(Default)]
struct FakeObjbrnetTransport {
    writes: Vec<Vec<u8>>,
    pending: Option<Vec<u8>>,
}

impl Transport for FakeObjbrnetTransport {
    fn payload_limit(&self) -> usize {
        4096
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
        Box::pin(async move {
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
                .ok_or_else(|| {
                    TransportError::new(
                        TransportErrorKind::InvalidConfiguration,
                        "request did not contain an allowlisted OID",
                    )
                })?;
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
        })
    }

    fn delay(&mut self, _: Duration) -> TransportFuture<'_, ()> {
        Box::pin(async {})
    }

    fn wait_response(
        &mut self,
        _: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        Box::pin(async move {
            Ok(self
                .pending
                .take()
                .map(WaitOutcome::Response)
                .unwrap_or(WaitOutcome::Timeout))
        })
    }
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn retrieval_uses_only_the_fixed_read_only_allowlist() {
    let mut transport = FakeObjbrnetTransport::default();
    let inspection = retrieve_wireless_settings(&mut transport).await;

    assert_eq!(inspection.observations.len(), WirelessField::ALL.len());
    assert_eq!(transport.writes.len(), WirelessField::ALL.len() * 4);
    for (parts, field) in transport
        .writes
        .as_chunks::<4>()
        .0
        .iter()
        .zip(WirelessField::ALL)
    {
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
