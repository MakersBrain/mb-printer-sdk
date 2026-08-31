// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded IEEE 1284 device-ID decoding shared by native USB and WASM hosts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

pub const MAX_DEVICE_ID_BYTES: usize = 2_048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceId {
    pub raw: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub command_sets: Vec<String>,
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DeviceIdError {
    #[error("short IEEE-1284 device ID")]
    TooShort,
    #[error("IEEE-1284 device ID exceeds limit")]
    TooLarge,
    #[error("invalid IEEE-1284 device ID length")]
    InvalidLength,
    #[error("IEEE-1284 device ID is not UTF-8")]
    InvalidUtf8,
    #[error("malformed IEEE-1284 device ID")]
    Malformed,
}

pub fn parse_device_id(data: &[u8]) -> Result<DeviceId, DeviceIdError> {
    if data.len() < 2 {
        return Err(DeviceIdError::TooShort);
    }
    if data.len() > MAX_DEVICE_ID_BYTES {
        return Err(DeviceIdError::TooLarge);
    }
    let declared = usize::from(u16::from_be_bytes([data[0], data[1]]));
    if !(2..=data.len()).contains(&declared) || declared > MAX_DEVICE_ID_BYTES {
        return Err(DeviceIdError::InvalidLength);
    }
    let raw = std::str::from_utf8(&data[2..declared])
        .map_err(|_| DeviceIdError::InvalidUtf8)?
        .trim_matches(char::from(0))
        .trim()
        .to_owned();
    if raw.is_empty() || !raw.contains(';') {
        return Err(DeviceIdError::Malformed);
    }
    let fields = raw
        .split(';')
        .filter_map(|field| {
            let (key, value) = field.split_once(':')?;
            let key = key.trim().to_ascii_uppercase();
            let value = value.trim().to_owned();
            (!key.is_empty() && !value.is_empty()).then_some((key, value))
        })
        .collect::<BTreeMap<_, _>>();
    if fields.is_empty() {
        return Err(DeviceIdError::Malformed);
    }
    let field = |short: &str, long: &str| fields.get(short).or_else(|| fields.get(long)).cloned();
    let manufacturer = field("MFG", "MANUFACTURER");
    let model = field("MDL", "MODEL");
    let command_sets = field("CMD", "COMMAND SET")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(DeviceId {
        raw,
        manufacturer,
        model,
        command_sets,
        fields,
    })
}
