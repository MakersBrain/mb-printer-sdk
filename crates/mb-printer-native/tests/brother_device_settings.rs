// SPDX-License-Identifier: AGPL-3.0-or-later

use mb_printer_core::discovery::SettingValue;
use mb_printer_native::{
    Transport, WaitOutcome,
    brother_device_settings::{DeviceSettingKind, model_profile, retrieve_device_settings},
};
use std::collections::VecDeque;

#[derive(Default)]
struct FakeTransport {
    writes: Vec<Vec<u8>>,
    responses: VecDeque<Vec<u8>>,
    identity: (u8, u8),
    fail_setting: Option<Vec<u8>>,
}

impl FakeTransport {
    fn matching(series: u8, model: u8) -> Self {
        Self {
            identity: (series, model),
            ..Self::default()
        }
    }
}

impl Transport for FakeTransport {
    fn payload_limit(&self) -> usize {
        4096
    }

    fn subscribe_notifications(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.writes.push(bytes.to_vec());
        let response = match bytes {
            b"\x1b@\x1biS" => {
                let mut status = vec![0; 32];
                status[3] = self.identity.0;
                status[4] = self.identity.1;
                Some(status)
            }
            b"\x1biUa\x01" => {
                let mut response = vec![0; 32];
                response[30] = 1;
                response[31] = 0xff;
                Some(response)
            }
            b"\x1biXc1\x00\x00" => Some(vec![0, 0, 9]),
            b"\x1biXe1\x00\x00\x01" => Some(vec![0, 0, 130]),
            b"\x1biXe1\x00\x00\x02" => Some(vec![0, 0, 126]),
            b"\x1biXO1\x00\x00" => Some(vec![0, 0, 1]),
            b"\x1bia\x01" | b"\x1bia\xff" => None,
            _ => return Err("unexpected command".into()),
        };
        if self.fail_setting.as_deref() == Some(bytes) {
            return Ok(());
        }
        if let Some(response) = response {
            self.responses.push_back(response);
        }
        Ok(())
    }

    fn delay_monotonic(&mut self, _: u64) {}

    fn wait_response(&mut self, _: u64) -> Result<WaitOutcome, String> {
        Ok(self
            .responses
            .pop_front()
            .map(WaitOutcome::Response)
            .unwrap_or(WaitOutcome::Timeout))
    }
}

#[test]
fn ql_800_retrieval_uses_verified_commands_and_decodes_values() {
    let profile = model_profile("QL-800").unwrap();
    let mut transport = FakeTransport::matching(0x34, 0x38);

    let inspection = retrieve_device_settings(&mut transport, profile);

    assert_eq!(inspection.error, None);
    assert_eq!(inspection.observations.len(), 5);
    assert_eq!(
        transport.writes,
        vec![
            b"\x1b@\x1biS".to_vec(),
            b"\x1bia\x01".to_vec(),
            b"\x1biUa\x01".to_vec(),
            b"\x1biXc1\x00\x00".to_vec(),
            b"\x1biXe1\x00\x00\x01".to_vec(),
            b"\x1biXe1\x00\x00\x02".to_vec(),
            b"\x1biXO1\x00\x00".to_vec(),
            b"\x1bia\xff".to_vec(),
        ]
    );
    assert_eq!(
        inspection.observations[0].value,
        Some(SettingValue::Keyword("raster".into()))
    );
    assert_eq!(
        inspection.observations[1].value,
        Some(SettingValue::Keyword("auto-cut-and-cut-at-end".into()))
    );
    assert_eq!(
        inspection.observations[2].value,
        Some(SettingValue::Integer(2))
    );
    assert_eq!(
        inspection.observations[3].value,
        Some(SettingValue::Integer(-2))
    );
    assert_eq!(
        inspection.observations[4].value,
        Some(SettingValue::Keyword("continue-from-last".into()))
    );
}

#[test]
fn identity_mismatch_fails_closed_before_settings_mode() {
    let profile = model_profile("ql-1110nwb").unwrap();
    let mut transport = FakeTransport::matching(0x34, 0x43);

    let inspection = retrieve_device_settings(&mut transport, profile);

    assert!(inspection.error.unwrap().contains("identity mismatch"));
    assert!(inspection.observations.is_empty());
    assert_eq!(transport.writes, vec![b"\x1b@\x1biS".to_vec()]);
}

#[test]
fn failed_read_stops_correlation_and_still_exits_settings_mode() {
    let profile = model_profile("pt-p710bt").unwrap();
    let failed = b"\x1biXc1\x00\x00".to_vec();
    let mut transport = FakeTransport {
        fail_setting: Some(failed),
        ..FakeTransport::matching(0x30, 0x76)
    };

    let inspection = retrieve_device_settings(&mut transport, profile);

    assert_eq!(inspection.observations.len(), 2);
    assert_eq!(
        inspection.observations[1].setting,
        DeviceSettingKind::AutoCut
    );
    assert!(inspection.observations[1].error.is_some());
    assert_eq!(transport.writes.last().unwrap(), b"\x1bia\xff");
    assert!(
        !transport
            .writes
            .iter()
            .any(|write| write == b"\x1biXe1\x00\x00\x01")
    );
}

#[test]
fn model_profiles_cover_both_compared_native_binaries() {
    for id in [
        "ql-800",
        "ql-810w",
        "ql-820nwb",
        "ql-1100",
        "ql-1110nwb",
        "ql-1115nwb",
        "ql-1115nwb-cp",
        "pt-p710bt",
        "pt-p715ebt",
        "pt-e720bt",
    ] {
        assert!(model_profile(id).is_some(), "missing {id}");
    }
}
