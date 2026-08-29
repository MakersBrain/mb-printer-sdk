// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::raster::GrayRaster;
use thiserror::Error;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub columns: u8,
    pub rows: u8,
    pub left_um: i64,
    pub top_um: i64,
    pub column_pitch_um: i64,
    pub row_pitch_um: i64,
}
#[derive(Debug, Clone)]
pub struct NormalizedPage {
    pub page: u32,
    pub width_um: i64,
    pub height_um: i64,
    pub raster: GrayRaster,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub page: u32,
    pub slot: u16,
    pub width_um: i64,
    pub height_um: i64,
    pub raster: GrayRaster,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExtractError {
    #[error("unknown La Poste format: {0}")]
    Format(String),
    #[error("page {page} is not A4: {width_um}x{height_um} um")]
    NotA4 {
        page: u32,
        width_um: i64,
        height_um: i64,
    },
    #[error("source page provenance must be one-based")]
    PageNumber,
    #[error("normalized page raster is invalid")]
    Raster,
    #[error("no occupied stamps found")]
    Empty,
}
pub const STAMP_WIDTH_UM: i64 = 63_500;
pub const STAMP_HEIGHT_UM: i64 = 33_900;
pub fn format(code: &str) -> Option<Format> {
    Some(match code.to_ascii_uppercase().as_str() {
        "L24A" | "L24A_SHEET" | "SHEET" => f(3, 8, 7200, 13100, 66000, 33900),
        "L24B" => f(3, 8, 5000, 3500, 68250, 36700),
        "L21A" => f(3, 7, 7200, 17200, 66000, 38100),
        "L18A" => f(3, 6, 7200, 15100, 66000, 46600),
        "L16A" => f(2, 8, 22500, 13500, 101600, 33900),
        "L14A" => f(2, 7, 22500, 17200, 101600, 38100),
        "L12A" => f(2, 6, 22500, 25600, 101600, 42300),
        _ => return None,
    })
}
const fn f(
    columns: u8,
    rows: u8,
    left_um: i64,
    top_um: i64,
    column_pitch_um: i64,
    row_pitch_um: i64,
) -> Format {
    Format {
        columns,
        rows,
        left_um,
        top_um,
        column_pitch_um,
        row_pitch_um,
    }
}
pub fn extract(pages: &[NormalizedPage], code: &str) -> Result<Vec<Stamp>, ExtractError> {
    let fmt = format(code).ok_or_else(|| ExtractError::Format(code.into()))?;
    let mut out = Vec::new();
    for p in pages {
        if p.page == 0 {
            return Err(ExtractError::PageNumber);
        }
        p.raster.validate().map_err(|_| ExtractError::Raster)?;
        if (p.width_um - 210_000).abs() > 1500 || (p.height_um - 297_000).abs() > 1500 {
            return Err(ExtractError::NotA4 {
                page: p.page,
                width_um: p.width_um,
                height_um: p.height_um,
            });
        }
        for slot in 0..fmt.columns as u16 * fmt.rows as u16 {
            let col = slot % fmt.columns as u16;
            let row = slot / fmt.columns as u16;
            let left = fmt.left_um + col as i64 * fmt.column_pitch_um;
            let top = fmt.top_um + row as i64 * fmt.row_pitch_um;
            let x0 = scale(left, p.raster.width, p.width_um);
            let y0 = scale(top, p.raster.height, p.height_um);
            let x1 = scale(left + STAMP_WIDTH_UM, p.raster.width, p.width_um);
            let y1 = scale(top + STAMP_HEIGHT_UM, p.raster.height, p.height_um);
            let raster = crop(&p.raster, x0, y0, x1, y1);
            if has_ink(&raster) {
                out.push(Stamp {
                    page: p.page,
                    slot: slot + 1,
                    width_um: STAMP_WIDTH_UM,
                    height_um: STAMP_HEIGHT_UM,
                    raster,
                })
            }
        }
    }
    if out.is_empty() {
        Err(ExtractError::Empty)
    } else {
        Ok(out)
    }
}
fn scale(um: i64, pixels: u32, total: i64) -> u32 {
    ((um as i128 * pixels as i128 + total as i128 / 2) / total as i128).clamp(0, pixels as i128)
        as u32
}
fn crop(r: &GrayRaster, x0: u32, y0: u32, x1: u32, y1: u32) -> GrayRaster {
    let w = x1.saturating_sub(x0);
    let h = y1.saturating_sub(y0);
    let mut out = GrayRaster::new(w, h, 255);
    for y in 0..h {
        let src = ((y0 + y) * r.width + x0) as usize;
        let dst = (y * w) as usize;
        out.pixels[dst..dst + w as usize].copy_from_slice(&r.pixels[src..src + w as usize])
    }
    out
}
fn has_ink(r: &GrayRaster) -> bool {
    let inset = (r.width.min(r.height) as f64 * 0.02).round().max(1.0) as u32;
    if r.width <= 2 * inset || r.height <= 2 * inset {
        return false;
    }
    let mut n = 0usize;
    for y in inset..r.height - inset {
        for x in inset..r.width - inset {
            n += (r.pixels[(y * r.width + x) as usize] < 250) as usize
        }
    }
    let area = (r.width - 2 * inset) as usize * (r.height - 2 * inset) as usize;
    n >= 8.max((area as f64 * 0.001).round() as usize)
}
