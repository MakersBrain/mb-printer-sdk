// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    capabilities::Protocol,
    protocol::{Action, Plan, ResponseValidation, SOURCE_COMMIT},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

pub const SYSTEM_REPORT_COMMAND: &[u8] = b"\x1biXG";
pub const SYSTEM_REPORT_MARKER: &str = "<<PRINTER CONFIGURATION>>";
pub const MAX_SYSTEM_REPORT_BYTES: usize = 64 * 1024;
pub const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum SystemReportError {
    #[error("Brother system report exceeds the 64 KiB limit")]
    TooLarge,
    #[error("response is not a Brother printer configuration report")]
    MissingMarker,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemReport {
    pub text: String,
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl std::fmt::Debug for SystemReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SystemReport")
            .field("sections", &self.sections.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl SystemReport {
    pub fn redacted(&self) -> Self {
        let sections = self
            .sections
            .iter()
            .map(|(section, values)| {
                let values = values
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            if sensitive_key(key) {
                                REDACTED.into()
                            } else {
                                value.clone()
                            },
                        )
                    })
                    .collect();
                (section.clone(), values)
            })
            .collect();
        Self {
            text: redact_text(&self.text),
            sections,
        }
    }
}

pub fn decode_system_report(data: &[u8]) -> Result<String, SystemReportError> {
    if data.len() > MAX_SYSTEM_REPORT_BYTES {
        return Err(SystemReportError::TooLarge);
    }
    let text = String::from_utf8_lossy(data).replace('\0', "");
    let marker = text
        .find(SYSTEM_REPORT_MARKER)
        .ok_or(SystemReportError::MissingMarker)?;
    Ok(text[marker..].trim().into())
}

pub fn parse_system_report(data: &[u8]) -> Result<SystemReport, SystemReportError> {
    let text = decode_system_report(data)?;
    let mut sections = BTreeMap::<String, BTreeMap<String, String>>::new();
    let mut current = None::<String>;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let section = section.trim().to_owned();
            sections.entry(section.clone()).or_default();
            current = Some(section);
        } else if let (Some(section), Some((key, value))) = (&current, line.split_once('=')) {
            sections
                .entry(section.clone())
                .or_default()
                .insert(key.trim().into(), value.trim().into());
        }
    }
    Ok(SystemReport { text, sections })
}

fn redact_text(text: &str) -> String {
    text.lines()
        .map(|line| {
            let Some((key, _)) = line.split_once('=') else {
                return line.to_owned();
            };
            if sensitive_key(key) {
                format!("{}={REDACTED}", key.trim_end())
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "serial",
        "ssid",
        "ipaddress",
        "ipv4",
        "ipv6",
        "gateway",
        "subnet",
        "macaddress",
        "nodename",
        "bluetoothaddress",
        "password",
        "networkkey",
    ]
    .iter()
    .any(|sensitive| normalized.contains(sensitive))
}

pub fn system_report_plan() -> Plan {
    Plan {
        protocol: Protocol::Brother,
        source_commit: SOURCE_COMMIT.into(),
        actions: vec![
            Action::CommandWrite {
                name: "ESC i X G system report".into(),
                bytes: SYSTEM_REPORT_COMMAND.to_vec(),
                atomic: true,
            },
            Action::CollectResponse {
                timeout_ms: 5000,
                idle_timeout_ms: 300,
                maximum_bytes: MAX_SYSTEM_REPORT_BYTES,
                validation: ResponseValidation::BrotherSystemReport,
            },
        ],
    }
}
