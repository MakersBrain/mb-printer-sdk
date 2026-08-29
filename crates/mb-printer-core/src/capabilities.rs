// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PrinterDefinition {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub width_bytes: Option<u16>,
    #[serde(default = "dpi")]
    pub dpi: u16,
    #[serde(default)]
    pub alignment: Alignment,
    #[serde(default)]
    pub rotated: bool,
    #[serde(default)]
    pub tape: bool,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub name_patterns: Vec<String>,
    #[serde(default)]
    pub tape_widths: Option<Vec<u16>>,
    #[serde(default)]
    pub default_tape_width: Option<u16>,
    #[serde(default)]
    pub label_presets: Option<String>,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub additional_offset_r: i32,
    #[serde(default = "invalidate")]
    pub invalidate_bytes: u16,
    #[serde(default)]
    pub compression: bool,
    #[serde(default)]
    pub min_rows: u32,
    #[serde(default)]
    pub max_rows: u32,
}
fn dpi() -> u16 {
    203
}
fn invalidate() -> u16 {
    200
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    MSeries,
    M02,
    M04,
    M110,
    DSeries,
    P12,
    Tspl,
    Brother,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Alignment {
    Left,
    #[default]
    Center,
    Right,
}
#[derive(Deserialize)]
struct File {
    printers: Vec<PrinterDefinition>,
}
pub fn bundled() -> Vec<PrinterDefinition> {
    serde_json::from_str::<File>(include_str!("../data/printers.json"))
        .expect("bundled printer definitions must be valid")
        .printers
}
pub fn by_id(id: &str) -> Option<PrinterDefinition> {
    bundled().into_iter().find(|p| p.id == id)
}
pub fn detect(name: &str) -> Option<PrinterDefinition> {
    let u = name.to_uppercase();
    let mut all: Vec<_> = bundled()
        .into_iter()
        .flat_map(|p| {
            p.name_patterns
                .clone()
                .into_iter()
                .map(move |n| (n, p.clone()))
        })
        .collect();
    all.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
    all.into_iter()
        .find(|(n, _)| u.starts_with(&n.to_uppercase()))
        .map(|x| x.1)
}
impl PrinterDefinition {
    pub fn width_px(&self) -> Option<u32> {
        self.width_bytes.map(|x| x as u32 * 8)
    }
    pub fn chunk_size(&self) -> usize {
        match self.protocol {
            Protocol::Brother => 1024,
            Protocol::M04 => 256,
            Protocol::Tspl => 512,
            _ => 128,
        }
    }
    pub fn chunk_delay_ms(&self) -> u64 {
        match self.protocol {
            Protocol::Brother => 0,
            Protocol::Tspl => 10,
            _ => 20,
        }
    }
}
