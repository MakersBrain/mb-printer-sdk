// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded, read-only retrieval of known Brother OBJBRNET settings.

use mb_printer_core::{
    discovery::SettingValue,
    protocol::brother::wifi::{self, WirelessAuthentication, WirelessEncryption, WirelessField},
};
use std::time::{Duration, Instant};

use crate::{Transport, WaitOutcome, WriteKind};

/// The largest response retained for any single OBJBRNET inquiry.
pub const MAX_RESPONSE_BYTES: usize = 4_000;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const DRAIN_TIMEOUT_MS: u64 = 200;
const MAX_DRAIN_BYTES: usize = 32 * 1024;
const MAX_ATTEMPTS: usize = 2;
// Windows' bidirectional spooler closes/flushes the PJL job before reading.
// Raw TCP/USB needs the QL's document-free status preflight to make firmware
// process and return the preceding OBJBRNET inquiry.
const INVALIDATE_BYTES: usize = 200;
const STATUS_PREAMBLE: &[u8] = b"\x1b@\x1bia\x01";
const STATUS_REQUEST: &[u8] = b"\x1biS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrotherSettingObservation {
    pub field: WirelessField,
    pub id: &'static str,
    pub oid: &'static str,
    pub sensitive: bool,
    pub value: Option<SettingValue>,
    pub raw_response: Option<Vec<u8>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrotherSettingsInspection {
    pub observations: Vec<BrotherSettingObservation>,
}

/// Retrieves the fixed, reverse-engineered wireless field allowlist.
///
/// Each field is queried independently so unsupported fields are reported
/// without hiding later observations. No mutation command and no credential
/// or arbitrary-OID query can be constructed through this API.
pub async fn retrieve_wireless_settings<T: Transport>(
    transport: &mut T,
) -> BrotherSettingsInspection {
    // Brother's helper vacuums the input before beginning. This is important
    // on USB, where a previous timed-out command can leave a late reply queued.
    let drain_error = drain_stale_responses(transport).await.err();
    let mut observations = Vec::with_capacity(WirelessField::ALL.len());
    for field in WirelessField::ALL {
        observations.push(match &drain_error {
            Some(error) => failed_observation(field, format!("input drain failed: {error}")),
            None => retrieve_field(transport, field).await,
        });
    }
    BrotherSettingsInspection { observations }
}

/// Retrieves every field using a fresh transport, matching a print spooler's
/// one-job-per-query lifecycle. This is the preferred raw TCP/USB adapter when
/// the firmware stops processing after returning one PJL response.
pub async fn retrieve_wireless_settings_with<T, F>(mut open: F) -> BrotherSettingsInspection
where
    T: Transport,
    F: FnMut() -> Result<T, String>,
{
    let mut observations = Vec::with_capacity(WirelessField::ALL.len());
    for field in WirelessField::ALL {
        observations.push(match open() {
            // A newly opened raw-print socket has no stale input. Reading
            // before its first write can make strict port-9100 firmware close
            // what it sees as an empty job.
            Ok(mut transport) => retrieve_field(&mut transport, field).await,
            Err(error) => failed_observation(field, error),
        });
    }
    BrotherSettingsInspection { observations }
}

/// Retrieves one known field after discarding any bounded stale input.
pub async fn retrieve_wireless_setting<T: Transport>(
    transport: &mut T,
    field: WirelessField,
) -> BrotherSettingObservation {
    match drain_stale_responses(transport).await {
        Ok(()) => retrieve_field(transport, field).await,
        Err(error) => failed_observation(field, format!("input drain failed: {error}")),
    }
}

async fn retrieve_field<T: Transport>(
    transport: &mut T,
    field: WirelessField,
) -> BrotherSettingObservation {
    let mut observation = base_observation(field);
    let command = field.command();
    if command.len() > transport.command_limit() {
        observation.error = Some(format!(
            "query is {} bytes but transport command limit is {}",
            command.len(),
            transport.command_limit()
        ));
        return observation;
    }
    let mut last_error = "response timed out".to_owned();
    for _ in 0..MAX_ATTEMPTS {
        if let Err(error) = transport.write(&command, WriteKind::Command).await {
            last_error = error.to_string();
            continue;
        }
        if let Err(error) = transport
            .write(&[0; INVALIDATE_BYTES], WriteKind::Command)
            .await
        {
            last_error = error.to_string();
            continue;
        }
        if let Err(error) = transport.write(STATUS_PREAMBLE, WriteKind::Command).await {
            last_error = error.to_string();
            continue;
        }
        transport.delay(Duration::from_millis(100)).await;
        if let Err(error) = transport.write(STATUS_REQUEST, WriteKind::Command).await {
            last_error = error.to_string();
            continue;
        }
        match read_matching_response(transport, field).await {
            Ok(response) => {
                observation.raw_response = Some(response.clone());
                observation.value = decode_field(field, &response);
                if observation.value.is_none() {
                    observation.error =
                        Some("response did not contain a valid value for the requested OID".into());
                }
                return observation;
            }
            Err(error) => last_error = error,
        }
    }
    observation.error = Some(last_error);
    observation
}

fn base_observation(field: WirelessField) -> BrotherSettingObservation {
    BrotherSettingObservation {
        field,
        id: field_id(field),
        oid: field.oid(),
        sensitive: is_sensitive(field),
        value: None,
        raw_response: None,
        error: None,
    }
}

fn failed_observation(field: WirelessField, error: String) -> BrotherSettingObservation {
    BrotherSettingObservation {
        error: Some(error),
        ..base_observation(field)
    }
}

async fn drain_stale_responses<T: Transport>(transport: &mut T) -> Result<(), String> {
    let mut drained = 0usize;
    loop {
        match transport
            .wait_response(Duration::from_millis(DRAIN_TIMEOUT_MS))
            .await
            .map_err(|error| error.to_string())?
        {
            WaitOutcome::Response(bytes) => {
                drained = drained
                    .checked_add(bytes.len())
                    .ok_or("stale response byte count overflow")?;
                if drained > MAX_DRAIN_BYTES {
                    return Err("stale responses exceeded 32768-byte drain limit".into());
                }
            }
            WaitOutcome::Timeout | WaitOutcome::Unavailable => return Ok(()),
        }
    }
}

async fn read_matching_response<T: Transport>(
    transport: &mut T,
    field: WirelessField,
) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    let mut response = Vec::new();
    while started.elapsed() < RESPONSE_TIMEOUT {
        let remaining = RESPONSE_TIMEOUT.saturating_sub(started.elapsed());
        let timeout_ms = u64::try_from(remaining.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        match transport
            .wait_response(Duration::from_millis(timeout_ms))
            .await
            .map_err(|error| error.to_string())?
        {
            WaitOutcome::Response(bytes) => {
                if response.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
                    return Err("response exceeded 4000-byte limit".into());
                }
                response.extend(bytes);
                if wifi::parse_oid_value(&response, field.oid()).is_some() {
                    return Ok(response);
                }
                // A complete late response for a previous OID is deliberately
                // ignored. Keep waiting for the requested OID within the same
                // deadline so it cannot be misbound to this observation.
                if WirelessField::ALL.into_iter().any(|other| {
                    other != field && wifi::parse_oid_value(&response, other.oid()).is_some()
                }) {
                    response.clear();
                }
            }
            WaitOutcome::Timeout | WaitOutcome::Unavailable => break,
        }
    }
    Err("response timed out".into())
}

pub const fn field_id(field: WirelessField) -> &'static str {
    match field {
        // Reverse engineering establishes that this is a 0/1 WLAN state,
        // but not that it means live association/connection status.
        WirelessField::Connected => "wireless-state",
        WirelessField::Ipv4 => "ipv4-address",
        WirelessField::Ssid => "ssid",
        WirelessField::Encryption => "encryption",
        WirelessField::Authentication => "authentication",
        WirelessField::Infrastructure => "infrastructure-enabled",
        WirelessField::WirelessDirect => "wireless-direct-enabled",
    }
}

pub const fn is_sensitive(field: WirelessField) -> bool {
    matches!(field, WirelessField::Ipv4 | WirelessField::Ssid)
}

fn decode_field(field: WirelessField, response: &[u8]) -> Option<SettingValue> {
    match field {
        WirelessField::Connected => wifi::parse_oid_value(response, field.oid())
            .and_then(|value| value.trim().parse::<i64>().ok())
            .map(SettingValue::Integer),
        WirelessField::Ipv4 => wifi::parse_ip_address(response).map(SettingValue::Text),
        WirelessField::Ssid => wifi::parse_oid_value(response, field.oid()).map(SettingValue::Text),
        WirelessField::Encryption => wifi::parse_encryption(response)
            .map(encryption_name)
            .map(str::to_owned)
            .map(SettingValue::Keyword),
        WirelessField::Authentication => wifi::parse_authentication(response)
            .map(authentication_name)
            .map(str::to_owned)
            .map(SettingValue::Keyword),
        WirelessField::Infrastructure | WirelessField::WirelessDirect => {
            wifi::parse_boolean_field(response, field).map(SettingValue::Boolean)
        }
    }
}

const fn encryption_name(value: WirelessEncryption) -> &'static str {
    match value {
        WirelessEncryption::None => "none",
        WirelessEncryption::Wep => "wep",
        WirelessEncryption::Tkip => "tkip",
        WirelessEncryption::Aes => "aes",
        WirelessEncryption::Ckip => "ckip",
        WirelessEncryption::Cmic => "cmic",
        WirelessEncryption::CkipCmic => "ckip-cmic",
        WirelessEncryption::TkipAes => "tkip-aes",
    }
}

const fn authentication_name(value: WirelessAuthentication) -> &'static str {
    match value {
        WirelessAuthentication::Open => "open",
        WirelessAuthentication::SharedKey => "shared-key",
        WirelessAuthentication::WpaPsk => "wpa-psk",
        WirelessAuthentication::Leap => "leap",
        WirelessAuthentication::EapFast => "eap-fast",
        WirelessAuthentication::Peap => "peap",
        WirelessAuthentication::EapTtls => "eap-ttls",
        WirelessAuthentication::EapTls => "eap-tls",
        WirelessAuthentication::WpaOnly => "wpa-only",
        WirelessAuthentication::Wpa2Only => "wpa2-only",
    }
}
