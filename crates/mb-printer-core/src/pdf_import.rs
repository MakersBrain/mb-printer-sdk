// SPDX-License-Identifier: AGPL-3.0-or-later
//! Memory-safe, pure-Rust PDF normalization for native and browser/WASM callers.
use crate::{laposte::NormalizedPage, limits::ProcessingLimits, raster::GrayRaster};
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
    #[error("PDF exceeds normalization limits")]
    TooLarge,
}

/// Normalize PDF pages to opaque grayscale at an injected DPI.
pub fn normalize(
    bytes: Vec<u8>,
    dpi: u16,
    first_page_only: bool,
    max_pixels_per_page: u64,
) -> Result<Vec<NormalizedPage>, PdfImportError> {
    let limits = ProcessingLimits {
        max_canvas_pixels: max_pixels_per_page,
        max_total_pixels: max_pixels_per_page
            .saturating_mul(u64::from(ProcessingLimits::default().max_pages)),
        ..ProcessingLimits::default()
    };
    normalize_with_limits(bytes, dpi, first_page_only, &limits)
}

/// Normalize PDF pages with explicit bounds for untrusted encoded input,
/// parsed page count, raster work, and retained grayscale output.
pub fn normalize_with_limits(
    bytes: Vec<u8>,
    dpi: u16,
    first_page_only: bool,
    limits: &ProcessingLimits,
) -> Result<Vec<NormalizedPage>, PdfImportError> {
    if dpi == 0
        || limits.max_resource_bytes == 0
        || limits.max_canvas_pixels == 0
        || limits.max_total_pixels == 0
        || limits.max_pages == 0
        || limits.max_output_bytes == 0
        || bytes.len() > limits.max_resource_bytes
    {
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
    let max_pages = usize::try_from(limits.max_pages).unwrap_or(usize::MAX);
    if pdf.pages().len() > max_pages {
        return Err(PdfImportError::TooLarge);
    }
    let cache = RenderCache::new();
    let interpreter = InterpreterSettings::default();
    let mut pages = Vec::new();
    let mut total_pixels = 0_u64;
    let mut grayscale_bytes = 0_usize;
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
        {
            return Err(PdfImportError::TooLarge);
        }
        let width = width as u16;
        let height = height as u16;
        let page_pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(PdfImportError::TooLarge)?;
        if page_pixels > limits.max_canvas_pixels {
            return Err(PdfImportError::TooLarge);
        }
        total_pixels = total_pixels
            .checked_add(page_pixels)
            .filter(|pixels| *pixels <= limits.max_total_pixels)
            .ok_or(PdfImportError::TooLarge)?;
        let page_grayscale_bytes =
            usize::try_from(page_pixels).map_err(|_| PdfImportError::TooLarge)?;
        grayscale_bytes = grayscale_bytes
            .checked_add(page_grayscale_bytes)
            .filter(|bytes| *bytes <= limits.max_output_bytes)
            .ok_or(PdfImportError::TooLarge)?;
        let pixmap = hayro::render(
            page,
            &cache,
            &interpreter,
            &RenderSettings {
                x_scale: dpi as f32 / 72.0,
                y_scale: dpi as f32 / 72.0,
                width: Some(width),
                height: Some(height),
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
