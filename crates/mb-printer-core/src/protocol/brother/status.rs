// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    capabilities::{PrinterDefinition, Protocol},
    protocol::{Action, Plan, ResponseValidation, SOURCE_COMMIT},
};
use serde::{Deserialize, Serialize};

const INITIALIZE: &[u8] = &[0x1b, 0x40];
const RASTER_MODE: &[u8] = &[0x1b, 0x69, 0x61, 1];
const STATUS_REQUEST: &[u8] = &[0x1b, 0x69, 0x53];

/// Builds the document-free Brother status transaction.
pub fn plan(printer: &PrinterDefinition) -> Plan {
    debug_assert_eq!(printer.protocol, Protocol::Brother);
    Plan {
        protocol: Protocol::Brother,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![
            command("invalidate", vec![0; printer.invalidate_bytes as usize]),
            command("ESC @ init", INITIALIZE.to_vec()),
            command("switch to raster mode", RASTER_MODE.to_vec()),
            // The printer discards a status request that arrives while it is
            // still consuming the invalidate/init preamble.
            Action::Delay { milliseconds: 100 },
            command("ESC i S status request", STATUS_REQUEST.to_vec()),
            Action::WaitForResponse {
                timeout_ms: 3000,
                fallback_delay_ms: 0,
                validation: ResponseValidation::BrotherStatus32,
            },
        ],
    }
}

fn command(name: &str, bytes: Vec<u8>) -> Action {
    Action::CommandWrite {
        name: name.into(),
        bytes,
        atomic: true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrotherStatus {
    pub media_width_mm: u8,
    pub media_length_mm: u8,
    pub media_type: &'static str,
    pub status_type: &'static str,
    pub phase: &'static str,
    pub errors: Vec<&'static str>,
}

pub fn parse_status(data: &[u8]) -> Result<BrotherStatus, &'static str> {
    if data.len() != 32 {
        return Err("Brother status must be exactly 32 bytes");
    }
    if !data.starts_with(&[0x80, 0x20, 0x42]) {
        return Err("unexpected Brother status header");
    }
    let mut errors = Vec::new();
    for (bit, name) in [
        (0, "no media"),
        (1, "end of media"),
        (2, "cutter jam"),
        (4, "unit in use"),
        (5, "printer off"),
        (7, "fan failure"),
    ] {
        if data[8] & (1 << bit) != 0 {
            errors.push(name)
        }
    }
    for (bit, name) in [
        (0, "replace media"),
        (1, "expansion buffer full"),
        (2, "transmission error"),
        (4, "cover opened while printing"),
        (6, "media cannot be fed"),
        (7, "system error"),
    ] {
        if data[9] & (1 << bit) != 0 {
            errors.push(name)
        }
    }
    Ok(BrotherStatus {
        media_width_mm: data[10],
        media_length_mm: data[17],
        media_type: match data[11] {
            0 => "no media",
            0x0a => "continuous",
            0x0b => "die-cut",
            _ => "unknown",
        },
        status_type: match data[18] {
            0 => "reply to status request",
            1 => "printing completed",
            2 => "error",
            5 => "notification",
            6 => "phase change",
            _ => "unknown",
        },
        phase: match data[19] {
            0 => "waiting to receive",
            1 => "printing",
            _ => "unknown",
        },
        errors,
    })
}
