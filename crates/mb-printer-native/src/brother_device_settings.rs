// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded, read-only Brother Printer Setting Tool device-setting retrieval.

use mb_printer_core::discovery::SettingValue;
use std::time::{Duration, Instant};

use crate::{Transport, WaitOutcome, WriteKind};

const STATUS_REQUEST: &[u8] = b"\x1b@\x1biS";
const ENTER_SETTINGS_MODE: &[u8] = b"\x1bia\x01";
const EXIT_SETTINGS_MODE: &[u8] = b"\x1bia\xff";
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const DRAIN_TIMEOUT_MS: u64 = 200;
const MAX_DRAIN_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceSettingKind {
    CommandMode,
    AutoCut,
    PrintDensityColor1,
    PrintDensityColor2,
    SerializeMode,
}

impl DeviceSettingKind {
    pub const COMMON: [Self; 4] = [
        Self::CommandMode,
        Self::AutoCut,
        Self::PrintDensityColor1,
        Self::SerializeMode,
    ];
    pub const MULTICOLOR: [Self; 5] = [
        Self::CommandMode,
        Self::AutoCut,
        Self::PrintDensityColor1,
        Self::PrintDensityColor2,
        Self::SerializeMode,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::CommandMode => "command-mode",
            Self::AutoCut => "auto-cut",
            Self::PrintDensityColor1 => "print-density-color-1",
            Self::PrintDensityColor2 => "print-density-color-2",
            Self::SerializeMode => "serialize-mode",
        }
    }

    const fn command(self) -> &'static [u8] {
        match self {
            Self::CommandMode => b"\x1biUa\x01",
            Self::AutoCut => b"\x1biXc1\x00\x00",
            Self::PrintDensityColor1 => b"\x1biXe1\x00\x00\x01",
            Self::PrintDensityColor2 => b"\x1biXe1\x00\x00\x02",
            Self::SerializeMode => b"\x1biXO1\x00\x00",
        }
    }

    const fn response_size(self) -> usize {
        match self {
            Self::CommandMode => 32,
            Self::AutoCut
            | Self::PrintDensityColor1
            | Self::PrintDensityColor2
            | Self::SerializeMode => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrotherModelProfile {
    pub id: &'static str,
    pub display_name: &'static str,
    pub series_code: u8,
    pub model_code: u8,
    pub settings: &'static [DeviceSettingKind],
}

const COMMON: &[DeviceSettingKind] = &DeviceSettingKind::COMMON;
const MULTICOLOR: &[DeviceSettingKind] = &DeviceSettingKind::MULTICOLOR;

pub const MODEL_PROFILES: &[BrotherModelProfile] = &[
    profile("ql-800", "Brother QL-800", 0x34, 0x38, MULTICOLOR),
    profile("ql-810w", "Brother QL-810W", 0x34, 0x39, MULTICOLOR),
    profile("ql-820nwb", "Brother QL-820NWB", 0x34, 0x41, MULTICOLOR),
    profile("ql-1100", "Brother QL-1100", 0x34, 0x43, COMMON),
    profile("ql-1110nwb", "Brother QL-1110NWB", 0x34, 0x44, COMMON),
    profile("ql-1115nwb", "Brother QL-1115NWB", 0x34, 0x45, COMMON),
    profile("ql-1115nwb-cp", "Brother QL-1115NWB CP", 0x34, 0x45, COMMON),
    profile("pt-p710bt", "Brother PT-P710BT", 0x30, 0x76, COMMON),
    profile("pt-p715ebt", "Brother PT-P715eBT", 0x30, 0x77, COMMON),
    profile("pt-e720bt", "Brother PT-E720BT", 0x30, 0x81, COMMON),
];

const fn profile(
    id: &'static str,
    display_name: &'static str,
    series_code: u8,
    model_code: u8,
    settings: &'static [DeviceSettingKind],
) -> BrotherModelProfile {
    BrotherModelProfile {
        id,
        display_name,
        series_code,
        model_code,
        settings,
    }
}

pub fn model_profile(id: &str) -> Option<&'static BrotherModelProfile> {
    MODEL_PROFILES
        .iter()
        .find(|profile| profile.id.eq_ignore_ascii_case(id))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSettingObservation {
    pub setting: DeviceSettingKind,
    pub id: &'static str,
    pub value: Option<SettingValue>,
    pub raw_response: Option<Vec<u8>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSettingsInspection {
    pub model: &'static str,
    pub identity_response: Option<Vec<u8>>,
    pub observations: Vec<DeviceSettingObservation>,
    pub error: Option<String>,
}

/// Retrieves the commands confirmed in both the QL800/QL1100 and PTP710BT
/// native `brdvset.exe` builds. The caller selects a fixed model profile and
/// the 32-byte status identity must match before settings mode is entered.
pub async fn retrieve_device_settings<T: Transport>(
    transport: &mut T,
    profile: &'static BrotherModelProfile,
) -> DeviceSettingsInspection {
    let mut inspection = DeviceSettingsInspection {
        model: profile.id,
        identity_response: None,
        observations: Vec::new(),
        error: None,
    };

    if let Err(error) = drain_stale_responses(transport).await {
        inspection.error = Some(format!("input drain failed: {error}"));
        return inspection;
    }
    if let Err(error) = write_command(transport, STATUS_REQUEST).await {
        inspection.error = Some(error);
        return inspection;
    }
    let status = match read_exact_response(transport, 32).await {
        Ok(status) => status,
        Err(error) => {
            inspection.error = Some(format!("status request failed: {error}"));
            return inspection;
        }
    };
    inspection.identity_response = Some(status.clone());
    if status[3] != profile.series_code || status[4] != profile.model_code {
        inspection.error = Some(format!(
            "printer identity mismatch: expected series/model {:02x}/{:02x}, got {:02x}/{:02x}",
            profile.series_code, profile.model_code, status[3], status[4]
        ));
        return inspection;
    }
    if let Err(error) = write_command(transport, ENTER_SETTINGS_MODE).await {
        inspection.error = Some(format!("could not enter device-settings mode: {error}"));
        return inspection;
    }

    for &setting in profile.settings {
        let mut observation = DeviceSettingObservation {
            setting,
            id: setting.id(),
            value: None,
            raw_response: None,
            error: None,
        };
        let result = match write_command(transport, setting.command()).await {
            Ok(()) => read_exact_response(transport, setting.response_size()).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(response) => {
                observation.value = decode(setting, &response);
                observation.raw_response = Some(response);
                if observation.value.is_none() {
                    observation.error = Some("response contained an unknown value".into());
                }
            }
            Err(error) => {
                observation.error = Some(error.clone());
                inspection.observations.push(observation);
                inspection.error = Some(format!(
                    "stopped after {} because the response stream can no longer be safely correlated",
                    setting.id()
                ));
                break;
            }
        }
        inspection.observations.push(observation);
    }

    if let Err(error) = write_command(transport, EXIT_SETTINGS_MODE).await {
        let message = format!("could not exit device-settings mode: {error}");
        inspection.error = Some(match inspection.error.take() {
            Some(previous) => format!("{previous}; {message}"),
            None => message,
        });
    }
    inspection
}

async fn write_command<T: Transport>(transport: &mut T, command: &[u8]) -> Result<(), String> {
    if command.len() > transport.command_limit() {
        return Err(format!(
            "command is {} bytes but transport command limit is {}",
            command.len(),
            transport.command_limit()
        ));
    }
    transport
        .write(command, WriteKind::Command)
        .await
        .map_err(|error| error.to_string())
}

async fn read_exact_response<T: Transport>(
    transport: &mut T,
    expected: usize,
) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    let mut response = Vec::with_capacity(expected);
    while response.len() < expected && started.elapsed() < RESPONSE_TIMEOUT {
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
                if response.len().saturating_add(bytes.len()) > expected {
                    return Err(format!("response exceeded expected {expected}-byte size"));
                }
                response.extend(bytes);
            }
            WaitOutcome::Timeout | WaitOutcome::Unavailable => break,
        }
    }
    if response.len() != expected {
        return Err(format!(
            "response was {} bytes, expected {expected}",
            response.len()
        ));
    }
    Ok(response)
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

fn decode(setting: DeviceSettingKind, response: &[u8]) -> Option<SettingValue> {
    let value = match setting {
        DeviceSettingKind::CommandMode => *response.get(30)?,
        DeviceSettingKind::AutoCut
        | DeviceSettingKind::PrintDensityColor1
        | DeviceSettingKind::PrintDensityColor2
        | DeviceSettingKind::SerializeMode => *response.get(2)?,
    };
    match setting {
        DeviceSettingKind::CommandMode => command_mode(value)
            .map(str::to_owned)
            .map(SettingValue::Keyword),
        DeviceSettingKind::AutoCut => auto_cut(value)
            .map(str::to_owned)
            .map(SettingValue::Keyword),
        DeviceSettingKind::PrintDensityColor1 | DeviceSettingKind::PrintDensityColor2 => (123
            ..=133)
            .contains(&value)
            .then(|| SettingValue::Integer(i64::from(value) - 128)),
        DeviceSettingKind::SerializeMode => match value {
            0 => Some(SettingValue::Keyword("from-starting-value".into())),
            1 => Some(SettingValue::Keyword("continue-from-last".into())),
            _ => None,
        },
    }
}

const fn command_mode(value: u8) -> Option<&'static str> {
    match value {
        0 => Some("esc-p"),
        1 => Some("raster"),
        3 => Some("p-touch-template"),
        4 => Some("cpcl-emulation"),
        5 => Some("cpcl-line-print-emulation"),
        6 => Some("sbpl-emulation"),
        7 => Some("epl"),
        8 => Some("dpl"),
        _ => None,
    }
}

const fn auto_cut(value: u8) -> Option<&'static str> {
    match value {
        0 => Some("off"),
        1 => Some("auto-cut"),
        8 => Some("cut-at-end"),
        9 => Some("auto-cut-and-cut-at-end"),
        _ => None,
    }
}
