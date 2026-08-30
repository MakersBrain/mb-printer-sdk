// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::capabilities::{PrinterDefinition, Protocol};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SOURCE_COMMIT: &str = "1f58d3f0e7f941b9143277cda828380149e56855";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    JobBoundary {
        kind: Boundary,
    },
    SubscribeNotifications,
    CommandWrite {
        name: String,
        bytes: Vec<u8>,
        atomic: bool,
    },
    RasterWrite {
        bytes: Vec<u8>,
        logical_chunk: usize,
        delay_after_each_physical_write_ms: u64,
    },
    Delay {
        milliseconds: u64,
    },
    WaitForResponse {
        timeout_ms: u64,
        fallback_delay_ms: u64,
        validation: ResponseValidation,
    },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Boundary {
    Start,
    End,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseValidation {
    AnyNotification,
    BrotherStatus32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub protocol: Protocol,
    pub source_commit: String,
    pub actions: Vec<Action>,
}
#[derive(Debug, Clone)]
pub struct Raster {
    pub width_bytes: u16,
    pub height: u32,
    pub data: Vec<u8>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub density: u8,
    pub feed: u8,
    pub continuous: bool,
    pub speed: u8,
    pub copies: u16,
    pub gap_tenths_mm: i16,
    pub offset_tenths_mm: i16,
    pub offset_x: u16,
    pub offset_y: u16,
    pub label_width_tenths_mm: Option<u16>,
    pub label_height_tenths_mm: Option<u16>,
    pub brother_media: Option<BrotherMedia>,
    pub cut: bool,
    pub cut_every: u8,
    pub compress: bool,
    pub high_quality: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrotherMedia {
    pub width_mm: u8,
    pub length_mm: u8,
    pub continuous: bool,
    pub feed_margin: u16,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            density: 6,
            feed: 32,
            continuous: false,
            speed: 5,
            copies: 1,
            gap_tenths_mm: 30,
            offset_tenths_mm: -30,
            offset_x: 0,
            offset_y: 0,
            label_width_tenths_mm: None,
            label_height_tenths_mm: None,
            brother_media: None,
            cut: true,
            cut_every: 1,
            compress: true,
            high_quality: true,
        }
    }
}
#[derive(Debug, Error, PartialEq)]
pub enum PlanError {
    #[error("raster length does not equal width_bytes * height")]
    RasterLength,
    #[error("value is outside protocol range: {0}")]
    Range(&'static str),
    #[error("protocol does not support this operation: {0}")]
    Unsupported(&'static str),
}

/// Builds a document-free plan that only asks the printer for its status.
/// The executor captures the reply, which `brother_parse_status` decodes.
pub fn status_plan(printer: &PrinterDefinition) -> Result<Plan, PlanError> {
    let mut a = Vec::new();
    match printer.protocol {
        Protocol::Brother => {
            cmd(
                &mut a,
                "invalidate",
                vec![0; printer.invalidate_bytes as usize],
            );
            cmd(&mut a, "ESC @ init", vec![0x1b, 0x40]);
            // The printer discards a status request that arrives while it is
            // still consuming the invalidate/init preamble.
            delay(&mut a, 100);
            cmd(&mut a, "ESC i S status request", vec![0x1b, 0x69, 0x53]);
            a.push(Action::WaitForResponse {
                timeout_ms: 3000,
                fallback_delay_ms: 0,
                validation: ResponseValidation::BrotherStatus32,
            });
        }
        // Phomemo families answer 1f 11 <code> on the notification channel.
        Protocol::MSeries
        | Protocol::M02
        | Protocol::M04
        | Protocol::M110
        | Protocol::DSeries
        | Protocol::P12 => {
            a.push(Action::SubscribeNotifications);
            for (name, code) in PHOMEMO_QUERIES {
                cmd(&mut a, name, vec![0x1f, 0x11, *code]);
                a.push(Action::WaitForResponse {
                    timeout_ms: 800,
                    // A model that does not answer one query must not fail the rest.
                    fallback_delay_ms: 100,
                    validation: ResponseValidation::AnyNotification,
                });
            }
        }
        _ => return Err(PlanError::Unsupported("status request")),
    }
    Ok(Plan {
        protocol: printer.protocol,
        source_commit: SOURCE_COMMIT.into(),
        actions: a,
    })
}

pub fn plan(printer: &PrinterDefinition, r: &Raster, o: &Options) -> Result<Plan, PlanError> {
    if r.data.len() != r.width_bytes as usize * r.height as usize {
        return Err(PlanError::RasterLength);
    }
    if !(1..=8).contains(&o.density) {
        return Err(PlanError::Range("density"));
    }
    if o.copies == 0 {
        return Err(PlanError::Range("copies"));
    }
    if o.cut && o.cut_every == 0 {
        return Err(PlanError::Range("cut every"));
    }
    if printer.protocol != Protocol::Tspl && o.copies > 1 {
        let mut single = o.clone();
        single.copies = 1;
        let base = plan(printer, r, &single)?;
        let mut actions = vec![Action::JobBoundary {
            kind: Boundary::Start,
        }];
        for _ in 0..o.copies {
            actions.extend_from_slice(&base.actions[1..base.actions.len() - 1])
        }
        actions.push(Action::JobBoundary {
            kind: Boundary::End,
        });
        return Ok(Plan {
            protocol: printer.protocol,
            source_commit: SOURCE_COMMIT.into(),
            actions,
        });
    }
    let mut a = vec![Action::JobBoundary {
        kind: Boundary::Start,
    }];
    match printer.protocol {
        Protocol::MSeries => {
            cmd(&mut a, "ESC @ init", vec![0x1b, 0x40]);
            delay(&mut a, 100);
            cmd(&mut a, "ESC 7 heat", heat(o.density));
            delay(&mut a, 30);
            cmd(&mut a, "GS | density", vec![0x1d, 0x7c, o.density]);
            delay(&mut a, 50);
            cmd(&mut a, "GS v 0 raster header", header(r));
            raster(&mut a, printer, r.data.clone());
            delay(&mut a, 300);
            cmd(&mut a, "ESC J feed", vec![0x1b, 0x4a, o.feed]);
            delay(&mut a, 800)
        }
        Protocol::M02 => {
            cmd(&mut a, "M02 prefix", vec![0x10, 0xff, 0xfe, 1]);
            delay(&mut a, 50);
            cmd(&mut a, "ESC @ init", vec![0x1b, 0x40]);
            delay(&mut a, 100);
            cmd(&mut a, "ESC 7 heat", heat(o.density));
            delay(&mut a, 30);
            cmd(&mut a, "GS v 0 raster header", header(r));
            raster(&mut a, printer, r.data.clone());
            delay(&mut a, 300);
            cmd(&mut a, "ESC J feed", vec![0x1b, 0x4a, o.feed.min(8)]);
            delay(&mut a, 500)
        }
        Protocol::M04 => {
            cmd(
                &mut a,
                "M04 density",
                vec![0x1f, 0x11, 2, round_div(o.density as u16 * 15, 8) as u8],
            );
            delay(&mut a, 30);
            cmd(
                &mut a,
                "M04 heat",
                vec![
                    0x1f,
                    0x11,
                    0x37,
                    (100 + round_div((o.density - 1) as u16 * 50, 3)) as u8,
                ],
            );
            delay(&mut a, 30);
            cmd(&mut a, "M04 init", vec![0x1f, 0x11, 0x0b]);
            delay(&mut a, 30);
            cmd(&mut a, "M04 compression", vec![0x1f, 0x11, 0x35, 0]);
            delay(&mut a, 30);
            cmd(&mut a, "GS v 0 raster header", header(r));
            raster(&mut a, printer, r.data.clone());
            delay(&mut a, 300);
            for _ in 0..round_div(o.feed.max(1) as u16, 16).max(1) {
                cmd(&mut a, "M04 feed", vec![0x1b, 0x64, 2]);
                delay(&mut a, 30)
            }
            delay(&mut a, 500)
        }
        Protocol::M110 => {
            cmd(&mut a, "M110 speed", vec![0x1b, 0x4e, 0x0d, o.speed]);
            delay(&mut a, 30);
            cmd(
                &mut a,
                "M110 density",
                vec![
                    0x1b,
                    0x4e,
                    4,
                    // Match Python's round(5 + density * 1.25), including
                    // ties-to-even at the default density (12.5 -> 12).
                    [0, 6, 8, 9, 10, 11, 12, 14, 15][o.density as usize],
                ],
            );
            delay(&mut a, 30);
            cmd(
                &mut a,
                "media type",
                vec![0x1f, 0x11, if o.continuous { 0x0b } else { 0x0a }],
            );
            delay(&mut a, 30);
            cmd(&mut a, "GS v 0 raster header", header(r));
            raster(&mut a, printer, r.data.clone());
            delay(&mut a, 300);
            cmd(
                &mut a,
                "M110 footer",
                vec![0x1f, 0xf0, 5, 0, 0x1f, 0xf0, 3, 0],
            );
            delay(&mut a, 500)
        }
        Protocol::DSeries => {
            let padded;
            let raster_data = if o.continuous && o.feed > 0 {
                let extra_rows = 56u32 + u32::from(o.feed);
                let mut data = r.data.clone();
                data.resize(data.len() + extra_rows as usize * r.width_bytes as usize, 0);
                padded = Raster {
                    width_bytes: r.width_bytes,
                    height: r.height + extra_rows,
                    data,
                };
                &padded
            } else {
                r
            };
            cmd(&mut a, "ESC 7 heat", heat(o.density));
            delay(&mut a, 30);
            cmd(
                &mut a,
                "media type",
                vec![0x1f, 0x11, if o.continuous { 0x0b } else { 0x0a }],
            );
            delay(&mut a, 30);
            let mut h = vec![0x1b, 0x40];
            h.extend(header(raster_data));
            cmd(&mut a, "ESC @ init + GS v 0 raster header", h);
            raster(&mut a, printer, raster_data.data.clone());
            delay(&mut a, 100);
            cmd(&mut a, "ESC d 0 print + gap detect", vec![0x1b, 0x64, 0])
        }
        Protocol::P12 => {
            a.push(Action::SubscribeNotifications);
            for p in [
                vec![0x1f, 0x11, 0x38],
                vec![
                    0x1f, 0x11, 0x11, 0x1f, 0x11, 0x12, 0x1f, 0x11, 9, 0x1f, 0x11, 0x13,
                ],
                vec![0x1f, 0x11, 9],
                vec![0x1f, 0x11, 0x19, 0x1f, 0x11, 0x11],
                vec![0x1f, 0x11, 0x19],
                vec![0x1f, 0x11, 7],
            ] {
                cmd(&mut a, "P12 init packet", p);
                a.push(Action::WaitForResponse {
                    timeout_ms: 500,
                    fallback_delay_ms: 500,
                    validation: ResponseValidation::AnyNotification,
                })
            }
            let mut h = vec![0x1b, 0x40];
            h.extend(header(r));
            cmd(&mut a, "ESC @ init + GS v 0 raster header", h);
            raster(&mut a, printer, r.data.clone());
            delay(&mut a, 100);
            cmd(&mut a, "P12 feed", vec![0x1b, 0x64, 0x0d]);
            delay(&mut a, 50);
            cmd(&mut a, "P12 feed", vec![0x1b, 0x64, 0x0d])
        }
        Protocol::Tspl => {
            let w = o
                .label_width_tenths_mm
                .unwrap_or_else(|| tspl_tenths_mm(u64::from(r.width_bytes) * 8, printer.dpi));
            let h = o
                .label_height_tenths_mm
                .unwrap_or_else(|| tspl_tenths_mm(u64::from(r.height), printer.dpi));
            for s in [
                fmt_mm("SIZE", w, Some(h)),
                fmt_mm("GAP", o.gap_tenths_mm.max(0) as u16, Some(0)),
                format!("OFFSET {} mm", decimal(o.offset_tenths_mm)),
                format!("DENSITY {}", round_div(o.density as u16 * 15, 8)),
                format!("SPEED {}", o.speed),
                "DIRECTION 0".into(),
                "CLS".into(),
            ] {
                cmd(&mut a, "TSPL", tspl(&s));
                delay(&mut a, 50)
            }
            cmd(
                &mut a,
                "BITMAP header",
                format!(
                    "BITMAP {},{},{},{},0,",
                    o.offset_x, o.offset_y, r.width_bytes, r.height
                )
                .into_bytes(),
            );
            raster(&mut a, printer, r.data.iter().map(|x| x ^ 0xff).collect());
            cmd(&mut a, "bitmap terminator", b"\r\n".to_vec());
            delay(&mut a, 50);
            cmd(&mut a, "TSPL", tspl(&format!("PRINT {}", o.copies)));
            delay(&mut a, 50);
            cmd(&mut a, "TSPL", tspl("END"))
        }
        Protocol::Brother => {
            let media = o
                .brother_media
                .as_ref()
                .ok_or(PlanError::Range("brother media"))?;
            let compress = o.compress && printer.compression;
            if printer.min_rows > 0 && media.continuous && r.height < printer.min_rows {
                return Err(PlanError::Range("brother minimum rows"));
            }
            if printer.max_rows > 0 && r.height > printer.max_rows {
                return Err(PlanError::Range("brother maximum rows"));
            }
            cmd(&mut a, "switch to raster mode", vec![0x1b, 0x69, 0x61, 1]);
            cmd(
                &mut a,
                "invalidate",
                vec![0; printer.invalidate_bytes as usize],
            );
            cmd(&mut a, "ESC @ init", vec![0x1b, 0x40]);
            cmd(&mut a, "switch to raster mode", vec![0x1b, 0x69, 0x61, 1]);
            cmd(&mut a, "ESC i S status request", vec![0x1b, 0x69, 0x53]);
            a.push(Action::WaitForResponse {
                timeout_ms: 3000,
                fallback_delay_ms: 0,
                validation: ResponseValidation::BrotherStatus32,
            });
            let flags = 0x80 | 0x02 | 0x04 | 0x08 | if o.high_quality { 0x40 } else { 0 };
            let mut info = vec![
                0x1b,
                0x69,
                0x7a,
                flags,
                if media.continuous { 0x0a } else { 0x0b },
                media.width_mm,
                if media.continuous { 0 } else { media.length_mm },
            ];
            info.extend(r.height.to_le_bytes());
            info.extend([0, 0]);
            cmd(&mut a, "ESC i z print information", info);
            if o.cut {
                cmd(&mut a, "ESC i M autocut", vec![0x1b, 0x69, 0x4d, 0x40]);
                cmd(
                    &mut a,
                    "ESC i A cut every",
                    vec![0x1b, 0x69, 0x41, o.cut_every],
                );
            }
            cmd(
                &mut a,
                "ESC i K expanded mode",
                vec![0x1b, 0x69, 0x4b, if o.cut { 8 } else { 0 }],
            );
            let margin = media.feed_margin.to_le_bytes();
            cmd(
                &mut a,
                "ESC i d margins",
                vec![0x1b, 0x69, 0x64, margin[0], margin[1]],
            );
            if compress {
                cmd(&mut a, "M compression", vec![0x4d, 2]);
            }
            raster(&mut a, printer, brother_raster_lines(r, compress));
            cmd(&mut a, "print", vec![0x1a])
        }
    }
    a.push(Action::JobBoundary {
        kind: Boundary::End,
    });
    Ok(Plan {
        protocol: printer.protocol,
        source_commit: SOURCE_COMMIT.into(),
        actions: a,
    })
}
fn tspl_tenths_mm(dots: u64, dpi: u16) -> u16 {
    // Preserve the Python/reference 8 dpmm at 203 DPI without target float math.
    let numerator = dots.saturating_mul(2_030);
    let denominator = u64::from(dpi).saturating_mul(8);
    u16::try_from((numerator + denominator / 2) / denominator).unwrap_or(u16::MAX)
}
pub fn packbits(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return vec![];
    }
    if data.len() == 1 {
        return vec![0, data[0]];
    }
    let mut result = Vec::new();
    let mut literal = Vec::new();
    let mut repeat = 0usize;
    let mut pos = 0usize;
    let mut in_repeat = false;
    fn flush_literal(result: &mut Vec<u8>, literal: &mut Vec<u8>) {
        if !literal.is_empty() {
            result.push((literal.len() - 1) as u8);
            result.append(literal)
        }
    }
    while pos < data.len() - 1 {
        if data[pos] == data[pos + 1] {
            if !in_repeat {
                flush_literal(&mut result, &mut literal);
                in_repeat = true;
                repeat = 1
            } else {
                if repeat == 127 {
                    result.push((257 - repeat) as u8);
                    result.push(data[pos]);
                    repeat = 0
                }
                repeat += 1
            }
        } else if in_repeat {
            repeat += 1;
            result.push((257 - repeat) as u8);
            result.push(data[pos]);
            in_repeat = false;
            repeat = 0
        } else {
            if literal.len() == 127 {
                flush_literal(&mut result, &mut literal)
            }
            literal.push(data[pos])
        }
        pos += 1
    }
    if in_repeat {
        repeat += 1;
        result.push((257 - repeat) as u8);
        result.push(data[pos])
    } else {
        literal.push(data[pos]);
        flush_literal(&mut result, &mut literal)
    }
    result
}
fn brother_raster_lines(r: &Raster, compress: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for row in r.data.chunks(r.width_bytes as usize) {
        let mut mirrored = vec![0; row.len()];
        for bit in 0..row.len() * 8 {
            if row[bit / 8] & (0x80 >> (bit % 8)) != 0 {
                let dst = row.len() * 8 - 1 - bit;
                mirrored[dst / 8] |= 0x80 >> (dst % 8)
            }
        }
        let bytes = if compress {
            packbits(&mirrored)
        } else {
            mirrored
        };
        out.extend([0x67, 0, bytes.len() as u8]);
        out.extend(bytes)
    }
    out
}
/// Phomemo status queries, `1f 11 <code>`. Replies arrive as notifications
/// whose type byte does not repeat the query code.
pub const PHOMEMO_QUERIES: &[(&str, u8)] = &[
    ("battery query", 0x08),
    ("paper query", 0x11),
    ("cover query", 0x12),
    ("firmware query", 0x07),
    ("serial query", 0x09),
    ("label query", 0x19),
];
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhomemoStatus {
    /// Remaining charge in the printer's own coarse steps: 0, 3, 5, 10, or a raw level.
    pub battery: Option<u8>,
    pub paper: Option<&'static str>,
    pub cover: Option<&'static str>,
    pub label: Option<&'static str>,
    pub heating: Option<&'static str>,
    pub firmware: Option<String>,
    pub version: Option<String>,
    pub serial: Option<String>,
    pub errors: Vec<&'static str>,
}
/// Decodes `1a <type> <payload>` notification frames. Unknown frames are ignored
/// so one unrecognised reply cannot hide the rest.
pub fn phomemo_parse_status(frames: &[Vec<u8>]) -> PhomemoStatus {
    let mut status = PhomemoStatus::default();
    for frame in frames {
        if frame.len() < 3 || frame[0] != 0x1a {
            continue;
        }
        let value = frame[2];
        match frame[1] {
            0x03 => {
                status.heating = Some(match value {
                    0xa9 => "over temperature",
                    0xa8 => "ready",
                    _ => "heating",
                })
            }
            0x04 => {
                status.battery = Some(match value {
                    0xa4 => 0,
                    0xa3 => 3,
                    0xa2 => 5,
                    0xa1 => 10,
                    other => other,
                })
            }
            0x05 => {
                status.cover = Some(match value {
                    0x98 => "open",
                    0x99 => "closed",
                    _ => "unknown",
                })
            }
            0x06 => status.paper = Some(if value == 0x88 { "out" } else { "ok" }),
            0x07 => status.firmware = Some(dotted(&frame[2..])),
            0x08 => status.serial = Some(ascii(&frame[2..])),
            0x0c => {
                status.label = Some(match value {
                    0x0b => "continuous",
                    0x26 => "gap",
                    _ => "unknown",
                })
            }
            0x11 => status.version = Some(dotted(&frame[2..])),
            _ => {}
        }
    }
    if status.paper == Some("out") {
        status.errors.push("no media")
    }
    if status.cover == Some("open") {
        status.errors.push("cover open")
    }
    if status.heating == Some("over temperature") {
        status.errors.push("print head over temperature")
    }
    status
}
fn dotted(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join(".")
}
fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .filter(|byte| byte.is_ascii_graphic())
        .map(|byte| *byte as char)
        .collect()
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
pub fn brother_parse_status(data: &[u8]) -> Result<BrotherStatus, &'static str> {
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
fn cmd(a: &mut Vec<Action>, name: &str, bytes: Vec<u8>) {
    a.push(Action::CommandWrite {
        name: name.into(),
        bytes,
        atomic: true,
    })
}
fn delay(a: &mut Vec<Action>, milliseconds: u64) {
    a.push(Action::Delay { milliseconds })
}
fn raster(a: &mut Vec<Action>, p: &PrinterDefinition, bytes: Vec<u8>) {
    a.push(Action::RasterWrite {
        bytes,
        logical_chunk: p.chunk_size(),
        delay_after_each_physical_write_ms: p.chunk_delay_ms(),
    })
}
fn heat(d: u8) -> Vec<u8> {
    vec![
        0x1b,
        0x37,
        7,
        [40, 60, 80, 100, 120, 140, 160, 200][d as usize - 1],
        2,
    ]
}
fn header(r: &Raster) -> Vec<u8> {
    let w = r.width_bytes.to_le_bytes();
    let h = (r.height as u16).to_le_bytes();
    vec![0x1d, 0x76, 0x30, 0, w[0], w[1], h[0], h[1]]
}
fn round_div(a: u16, b: u16) -> u16 {
    (a + b / 2) / b
}
fn tspl(s: &str) -> Vec<u8> {
    format!("{s}\r\n").into_bytes()
}
fn decimal(v: i16) -> String {
    let sign = if v < 0 { "-" } else { "" };
    let magnitude = v.unsigned_abs();
    if magnitude.is_multiple_of(10) {
        format!("{sign}{}", magnitude / 10)
    } else {
        format!("{sign}{}.{:01}", magnitude / 10, magnitude % 10)
    }
}
fn fmt_mm(c: &str, a: u16, b: Option<u16>) -> String {
    match b {
        Some(b) => format!("{c} {} mm, {} mm", decimal(a as i16), decimal(b as i16)),
        None => format!("{c} {} mm", decimal(a as i16)),
    }
}
