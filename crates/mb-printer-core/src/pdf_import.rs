// SPDX-License-Identifier: AGPL-3.0-or-later
//! Memory-safe, pure-Rust PDF normalization for native and browser/WASM callers.
use crate::{laposte::NormalizedPage, raster::GrayRaster};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PdfImportError {
    #[error("encrypted/password-protected PDFs are unsupported")]
    Encrypted,
    #[error("PDF is malformed or unsupported")]
    Invalid,
    #[error("PDF contains no pages")]
    Empty,
    #[error("PDF page dimensions exceed limits")]
    TooLarge,
}

/// Normalize PDF pages to opaque grayscale at an injected DPI.
pub fn normalize(
    bytes: Vec<u8>,
    dpi: u16,
    first_page_only: bool,
    max_pixels_per_page: u64,
) -> Result<Vec<NormalizedPage>, PdfImportError> {
    if dpi == 0 || max_pixels_per_page == 0 {
        return Err(PdfImportError::TooLarge);
    }
    if bytes
        .windows(b"/Encrypt".len())
        .any(|window| window == b"/Encrypt")
    {
        return Err(PdfImportError::Encrypted);
    }
    let pdf = Pdf::new(bytes).map_err(|_| PdfImportError::Invalid)?;
    if pdf.pages().is_empty() {
        return Err(PdfImportError::Empty);
    }
    let cache = RenderCache::new();
    let interpreter = InterpreterSettings::default();
    let mut pages = Vec::new();
    for (index, page) in pdf.pages().iter().enumerate() {
        if first_page_only && index > 0 {
            break;
        }
        let (points_w, points_h) = page.render_dimensions();
        if !points_w.is_finite() || !points_h.is_finite() || points_w <= 0.0 || points_h <= 0.0 {
            return Err(PdfImportError::Invalid);
        }
        let width = (f64::from(points_w) * f64::from(dpi) / 72.0).round();
        let height = (f64::from(points_h) * f64::from(dpi) / 72.0).round();
        if width < 1.0
            || height < 1.0
            || width > f64::from(u16::MAX)
            || height > f64::from(u16::MAX)
            || width * height > max_pixels_per_page as f64
        {
            return Err(PdfImportError::TooLarge);
        }
        let pixmap = hayro::render(
            page,
            &cache,
            &interpreter,
            &RenderSettings {
                x_scale: dpi as f32 / 72.0,
                y_scale: dpi as f32 / 72.0,
                width: Some(width as u16),
                height: Some(height as u16),
                bg_color: WHITE,
            },
        );
        let pixels = pixmap
            .data()
            .iter()
            .map(|pixel| {
                ((77 * u32::from(pixel.r)
                    + 150 * u32::from(pixel.g)
                    + 29 * u32::from(pixel.b)
                    + 128)
                    / 256) as u8
            })
            .collect();
        pages.push(NormalizedPage {
            // Public provenance is one-based, matching PDF viewers and the CLI.
            page: index as u32 + 1,
            width_um: (f64::from(points_w) * 25_400.0 / 72.0).round() as i64,
            height_um: (f64::from(points_h) * 25_400.0 / 72.0).round() as i64,
            raster: GrayRaster {
                width: u32::from(pixmap.width()),
                height: u32::from(pixmap.height()),
                pixels,
            },
        });
    }
    Ok(pages)
}
