// SPDX-License-Identifier: AGPL-3.0-or-later
//! Media catalogue: which rolls, die-cut labels, and tapes a model can carry.
//!
//! Both catalogues are imported rather than transcribed:
//! `data/brother-media.json` by `scripts/import_brother_ptd.py` from Brother's
//! printer descriptors, and `data/phomemo-media.json` by
//! `scripts/import_phomemo_paper.py` from the Phomemo paper API and the offline
//! table inside their application. `data/phomemo-capabilities.json` states the
//! media width those models accept, which exceeds what the head prints.

use crate::capabilities::PrinterDefinition;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPreset {
    pub id: String,
    pub name: String,
    pub width_mm: f64,
    /// Zero for continuous stock, which the caller cuts to length.
    pub height_mm: f64,
    /// `rectangle`, `round`, or `continuous`.
    pub shape: String,
    /// Present for tape stock, so a model only offers the widths it accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape_width_mm: Option<u16>,
    /// Printable area and placement, where the source descriptor states them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printable_width_dots: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printable_length_dots: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_right_dots: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_margin_dots: Option<u32>,
}

#[derive(Deserialize)]
struct BrotherMedia {
    models: BTreeMap<String, Vec<MediaPreset>>,
}
#[derive(Deserialize)]
struct PhomemoMedia {
    models: BTreeMap<String, Vec<MediaPreset>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capability {
    max_media_width_mm: f64,
}
#[derive(Deserialize)]
struct Capabilities {
    models: BTreeMap<String, Capability>,
}

static BROTHER: LazyLock<BrotherMedia> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../data/brother-media.json"))
        .expect("bundled Brother media parses")
});
static PHOMEMO: LazyLock<PhomemoMedia> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../data/phomemo-media.json"))
        .expect("bundled Phomemo media parses")
});
static CAPABILITIES: LazyLock<Capabilities> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../data/phomemo-capabilities.json"))
        .expect("bundled Phomemo capabilities parse")
});

/// Printable width of the head, which no media may exceed.
pub fn head_width_mm(printer: &PrinterDefinition) -> Option<f64> {
    printer
        .width_bytes
        .map(|bytes| f64::from(bytes) * 8. * 25.4 / f64::from(printer.dpi))
}

/// The widest media a model accepts. Vendors state it where the printer takes
/// stock wider than the head prints, so fall back to the head only when unstated.
pub fn max_media_width_mm(printer: &PrinterDefinition) -> Option<f64> {
    CAPABILITIES
        .models
        .get(&printer.id)
        .map(|capability| capability.max_media_width_mm)
        .or_else(|| head_width_mm(printer))
}

/// Every media a model can carry, filtered by what it physically accepts.
pub fn presets_for(printer: &PrinterDefinition) -> Vec<MediaPreset> {
    if printer.label_presets.as_deref() == Some("dk") {
        return BROTHER.models.get(&printer.id).cloned().unwrap_or_default();
    }
    let Some(catalogue) = PHOMEMO.models.get(&printer.id) else {
        return Vec::new();
    };
    let widest = max_media_width_mm(printer);
    catalogue
        .iter()
        // On tape it is the tape width that crosses the head, not the label length.
        .filter(|preset| {
            let across = preset.tape_width_mm.map_or(preset.width_mm, f64::from);
            widest.is_none_or(|widest| across <= widest + 1.)
        })
        .cloned()
        .collect()
}

/// Names the media a printer reported. Die-cut wins over continuous, and both
/// orientations are tried, since a status reply describes the tape, not the label.
pub fn match_media(
    printer: &PrinterDefinition,
    width_mm: f64,
    height_mm: f64,
) -> Option<MediaPreset> {
    const TOLERANCE: f64 = 1.5;
    let close = |left: f64, right: f64| (left - right).abs() <= TOLERANCE;
    let presets = presets_for(printer);
    presets
        .iter()
        .find(|preset| {
            preset.height_mm > 0.
                && ((close(preset.width_mm, width_mm) && close(preset.height_mm, height_mm))
                    || (close(preset.width_mm, height_mm) && close(preset.height_mm, width_mm)))
        })
        .or_else(|| {
            presets
                .iter()
                .find(|preset| preset.height_mm == 0. && close(preset.width_mm, width_mm))
        })
        .cloned()
}
