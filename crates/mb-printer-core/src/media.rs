// SPDX-License-Identifier: AGPL-3.0-or-later
//! Media catalogue: which rolls, die-cut labels, and tapes a model can carry.
//!
//! Brother DK geometry follows the reference driver's media table; the Phomemo
//! families follow the preset lists shipped by the mobile and Odoo drivers.

use crate::capabilities::PrinterDefinition;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub width_mm: f64,
    /// Zero for continuous stock, which the caller cuts to length.
    pub height_mm: f64,
    /// `rectangle`, `round`, or `continuous`.
    pub shape: &'static str,
    /// Present for tape stock, so a model only offers the widths it accepts.
    pub tape_width_mm: Option<u16>,
    /// Restricts a preset to specific models; empty means every model in the group.
    #[serde(skip)]
    pub models: &'static [&'static str],
}

const fn die_cut(
    id: &'static str,
    name: &'static str,
    width_mm: f64,
    height_mm: f64,
) -> MediaPreset {
    MediaPreset {
        id,
        name,
        width_mm,
        height_mm,
        shape: "rectangle",
        tape_width_mm: None,
        models: &[],
    }
}
const fn round(id: &'static str, name: &'static str, diameter_mm: f64) -> MediaPreset {
    MediaPreset {
        id,
        name,
        width_mm: diameter_mm,
        height_mm: diameter_mm,
        shape: "round",
        tape_width_mm: None,
        models: &[],
    }
}
const fn continuous(id: &'static str, name: &'static str, width_mm: f64) -> MediaPreset {
    MediaPreset {
        id,
        name,
        width_mm,
        height_mm: 0.,
        shape: "continuous",
        tape_width_mm: None,
        models: &[],
    }
}
const fn wide(preset: MediaPreset, models: &'static [&'static str]) -> MediaPreset {
    MediaPreset { models, ..preset }
}
const fn tape(
    id: &'static str,
    name: &'static str,
    width_mm: f64,
    tape_width_mm: u16,
) -> MediaPreset {
    MediaPreset {
        id,
        name,
        width_mm,
        height_mm: 0.,
        shape: "continuous",
        tape_width_mm: Some(tape_width_mm),
        models: &[],
    }
}

/// Models with the wide 102 mm head.
const WIDE_QL: &[&str] = &["ql-1100", "ql-1110nwb", "ql-1115nwb"];
const WIDE_QL_164: &[&str] = &["ql-1100", "ql-1110nwb"];

const DK: &[MediaPreset] = &[
    continuous("12", "12mm continuous", 12.),
    continuous("29", "29mm continuous", 29.),
    continuous("38", "38mm continuous", 38.),
    continuous("50", "50mm continuous", 50.),
    continuous("54", "54mm continuous", 54.),
    continuous("62", "62mm continuous", 62.),
    wide(continuous("102", "102mm continuous", 102.), WIDE_QL),
    wide(continuous("103", "104mm continuous", 104.), WIDE_QL),
    die_cut("17x54", "17 × 54mm die-cut", 17., 54.),
    die_cut("17x87", "17 × 87mm die-cut", 17., 87.),
    die_cut("23x23", "23 × 23mm die-cut", 23., 23.),
    die_cut("29x42", "29 × 42mm die-cut", 29., 42.),
    die_cut("29x90", "29 × 90mm die-cut", 29., 90.),
    die_cut("39x90", "38 × 90mm die-cut", 38., 90.),
    die_cut("39x48", "39 × 48mm die-cut", 39., 48.),
    die_cut("52x29", "52 × 29mm die-cut", 52., 29.),
    die_cut("60x86", "60 × 87mm die-cut", 60., 87.),
    die_cut("62x29", "62 × 29mm die-cut", 62., 29.),
    die_cut("62x100", "62 × 100mm die-cut", 62., 100.),
    wide(die_cut("102x51", "102 × 51mm die-cut", 102., 51.), WIDE_QL),
    wide(
        die_cut("102x152", "102 × 153mm die-cut", 102., 153.),
        WIDE_QL,
    ),
    wide(
        die_cut("103x164", "104 × 164mm die-cut", 104., 164.),
        WIDE_QL_164,
    ),
    round("d12", "12mm round die-cut", 12.),
    round("d24", "24mm round die-cut", 24.),
    round("d58", "58mm round die-cut", 58.),
];

const M_SERIES: &[MediaPreset] = &[
    die_cut("12x40", "12 × 40mm", 12., 40.),
    die_cut("15x30", "15 × 30mm", 15., 30.),
    die_cut("20x30", "20 × 30mm", 20., 30.),
    die_cut("25x50", "25 × 50mm", 25., 50.),
    die_cut("30x20", "30 × 20mm", 30., 20.),
    die_cut("30x40", "30 × 40mm", 30., 40.),
    die_cut("40x30", "40 × 30mm", 40., 30.),
    die_cut("40x60", "40 × 60mm", 40., 60.),
    die_cut("50x25", "50 × 25mm", 50., 25.),
    die_cut("50x30", "50 × 30mm", 50., 30.),
    die_cut("50x80", "50 × 80mm", 50., 80.),
    die_cut("60x40", "60 × 40mm", 60., 40.),
    round("d20", "20mm round", 20.),
    round("d30", "30mm round", 30.),
    round("d40", "40mm round", 40.),
    round("d50", "50mm round", 50.),
];

const D_SERIES: &[MediaPreset] = &[
    die_cut("40x12", "40 × 12mm", 40., 12.),
    die_cut("30x12", "30 × 12mm", 30., 12.),
    die_cut("22x12", "22 × 12mm", 22., 12.),
    die_cut("12x12", "12 × 12mm", 12., 12.),
    die_cut("30x14", "30 × 14mm", 30., 14.),
    die_cut("22x14", "22 × 14mm", 22., 14.),
    die_cut("40x15", "40 × 15mm", 40., 15.),
    die_cut("30x15", "30 × 15mm", 30., 15.),
    continuous("40x12c", "40 × 12mm continuous", 40.),
    continuous("30x12c", "30 × 12mm continuous", 30.),
    continuous("22x12c", "22 × 12mm continuous", 22.),
    continuous("40x15c", "40 × 15mm continuous", 40.),
    continuous("30x15c", "30 × 15mm continuous", 30.),
    round("d14", "14mm round", 14.),
];

const TAPE: &[MediaPreset] = &[
    tape("40x12", "40 × 12mm tape", 40., 12),
    tape("30x12", "30 × 12mm tape", 30., 12),
    tape("22x12", "22 × 12mm tape", 22., 12),
    tape("12x12", "12 × 12mm tape", 12., 12),
    tape("40x14", "40 × 14mm tape", 40., 14),
    tape("30x14", "30 × 14mm tape", 30., 14),
    tape("22x14", "22 × 14mm tape", 22., 14),
    tape("14x14", "14 × 14mm tape", 14., 14),
    tape("40x15", "40 × 15mm tape", 40., 15),
    tape("30x15", "30 × 15mm tape", 30., 15),
    tape("22x15", "22 × 15mm tape", 22., 15),
    tape("15x15", "15 × 15mm tape", 15., 15),
];

const PM241: &[MediaPreset] = &[
    die_cut("102x152", "102 × 152mm (4 × 6in)", 102., 152.),
    die_cut("102x102", "102 × 102mm (4 × 4in)", 102., 102.),
    die_cut("102x76", "102 × 76mm (4 × 3in)", 102., 76.),
    die_cut("102x51", "102 × 51mm (4 × 2in)", 102., 51.),
    die_cut("100x150", "100 × 150mm", 100., 150.),
    die_cut("100x100", "100 × 100mm", 100., 100.),
];

/// Printable width of the head, which no media may exceed.
pub fn head_width_mm(printer: &PrinterDefinition) -> Option<f64> {
    printer
        .width_bytes
        .map(|bytes| f64::from(bytes) * 8. * 25.4 / f64::from(printer.dpi))
}

/// Every media a model can carry, filtered by head width, tape width, and model.
pub fn presets_for(printer: &PrinterDefinition) -> Vec<MediaPreset> {
    let catalogue: &[MediaPreset] = match printer.label_presets.as_deref() {
        Some("dk") => DK,
        Some("m-series") => M_SERIES,
        Some("d-series") => D_SERIES,
        Some("tape") => TAPE,
        Some("pm241") => PM241,
        _ => return Vec::new(),
    };
    let head = head_width_mm(printer);
    catalogue
        .iter()
        .filter(|preset| preset.models.is_empty() || preset.models.contains(&printer.id.as_str()))
        .filter(
            |preset| match (preset.tape_width_mm, printer.tape_widths.as_ref()) {
                (Some(width), Some(widths)) => widths.contains(&width),
                (Some(_), None) => false,
                _ => true,
            },
        )
        // Nothing wider than the head can be printed. On tape it is the tape
        // width that crosses the head, not the label length.
        .filter(|preset| {
            let across = preset.tape_width_mm.map_or(preset.width_mm, f64::from);
            head.is_none_or(|head| across <= head + 1.)
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
