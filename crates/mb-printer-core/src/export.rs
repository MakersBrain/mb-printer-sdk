// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    limits::ProcessingLimits,
    raster::{MonoRaster, PackedMonoRaster, RasterError},
};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum ExportError {
    #[error(transparent)]
    Raster(#[from] RasterError),
    #[error("PNG encoding failed: {0}")]
    Png(#[from] png::EncodingError),
    #[error("PDF export requires at least one page")]
    EmptyPdf,
    #[error("PDF media dimensions must be positive micrometres")]
    InvalidPdfMedia,
    #[error("PDF DPI must be positive")]
    InvalidDpi,
    #[error("PDF output exceeds the configured {limit}-byte limit")]
    OutputTooLarge { limit: usize },
}
pub fn png(r: &MonoRaster, dpi: u16) -> Result<Vec<u8>, ExportError> {
    png_with_limits(r, dpi, &ProcessingLimits::default())
}

pub fn png_with_limits(
    r: &MonoRaster,
    dpi: u16,
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, ExportError> {
    r.validate()?;
    enforce_raster_pixels(r, limits.max_canvas_pixels)?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, r.width, r.height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::One);
        encoder.set_compression(png::Compression::High);
        encoder.set_filter(png::Filter::NoFilter);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let ppm = (dpi as u32 * 10_000 + 127) / 254;
        encoder.set_pixel_dims(Some(png::PixelDimensions {
            xppu: ppm,
            yppu: ppm,
            unit: png::Unit::Meter,
        }));
        let mut writer = encoder.write_header()?;
        let packed = r.pack_msb()?.into_iter().map(|b| !b).collect::<Vec<_>>();
        writer.write_image_data(&packed)?;
    }
    if out.len() > limits.max_output_bytes {
        Err(ExportError::OutputTooLarge {
            limit: limits.max_output_bytes,
        })
    } else {
        Ok(out)
    }
}
/// Minimal deterministic PDF 1.4 containing one bilevel image page.
pub fn pdf(r: &MonoRaster, dpi: u16) -> Result<Vec<u8>, ExportError> {
    pdf_with_limits(r, dpi, &ProcessingLimits::default())
}

pub fn pdf_with_limits(
    r: &MonoRaster,
    dpi: u16,
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, ExportError> {
    pdf_pages_with_limits(&[(r, dpi)], limits)
}

/// A raster page paired with its authoritative physical media dimensions.
#[derive(Debug, Clone, Copy)]
pub struct PdfPage<'a> {
    pub raster: &'a MonoRaster,
    pub width_um: i64,
    pub height_um: i64,
}

/// An owned packed raster page paired with authoritative physical dimensions.
#[derive(Debug, Clone)]
pub struct PackedPdfPage {
    pub raster: PackedMonoRaster,
    pub width_um: i64,
    pub height_um: i64,
}

/// Export one page using authoritative media geometry rather than rounded dots/DPI.
pub fn pdf_physical(r: &MonoRaster, width_um: i64, height_um: i64) -> Result<Vec<u8>, ExportError> {
    pdf_physical_with_limits(r, width_um, height_um, &ProcessingLimits::default())
}

pub fn pdf_physical_with_limits(
    r: &MonoRaster,
    width_um: i64,
    height_um: i64,
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, ExportError> {
    pdf_pages_physical_with_limits(
        &[PdfPage {
            raster: r,
            width_um,
            height_um,
        }],
        limits,
    )
}

/// Deterministic multi-page PDF with exact per-page physical media geometry.
pub fn pdf_pages_physical(pages: &[PdfPage<'_>]) -> Result<Vec<u8>, ExportError> {
    pdf_pages_physical_with_limits(pages, &ProcessingLimits::default())
}

pub fn pdf_pages_physical_with_limits(
    pages: &[PdfPage<'_>],
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, ExportError> {
    enforce_page_count(pages.len(), limits)?;
    let mut total_pixels = 0u64;
    let mut packed = Vec::with_capacity(pages.len());
    for page in pages {
        if page.width_um <= 0 || page.height_um <= 0 {
            return Err(ExportError::InvalidPdfMedia);
        }
        let pixels = raster_pixels(page.raster)?;
        if pixels > limits.max_canvas_pixels {
            return Err(ExportError::Raster(RasterError::Dimensions));
        }
        total_pixels = total_pixels
            .checked_add(pixels)
            .ok_or(ExportError::Raster(RasterError::Dimensions))?;
        if total_pixels > limits.max_total_pixels {
            return Err(ExportError::Raster(RasterError::Dimensions));
        }
        packed.push(PackedPdfPage {
            raster: PackedMonoRaster::from_mono(page.raster, limits.max_canvas_pixels)?,
            width_um: page.width_um,
            height_um: page.height_um,
        });
    }
    pdf_packed_pages_physical_with_limits(&packed, limits)
}

/// Deterministic multi-page PDF. Each page may use its own raster dimensions and DPI.
pub fn pdf_pages(pages: &[(&MonoRaster, u16)]) -> Result<Vec<u8>, ExportError> {
    pdf_pages_with_limits(pages, &ProcessingLimits::default())
}

pub fn pdf_pages_with_limits(
    pages: &[(&MonoRaster, u16)],
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, ExportError> {
    enforce_page_count(pages.len(), limits)?;
    if pages.iter().any(|(_, dpi)| *dpi == 0) {
        return Err(ExportError::InvalidDpi);
    }
    let points = pages
        .iter()
        .map(|(raster, dpi)| {
            (
                *raster,
                raster.width as f64 * 72.0 / f64::from(*dpi),
                raster.height as f64 * 72.0 / f64::from(*dpi),
            )
        })
        .collect::<Vec<_>>();
    pdf_pages_points(&points, limits)
}

/// Deterministic multi-page PDF from bounded, owned one-bit page rasters.
pub fn pdf_packed_pages_physical(
    pages: &[PackedPdfPage],
    max_output_bytes: usize,
) -> Result<Vec<u8>, ExportError> {
    if pages.is_empty() {
        return Err(ExportError::EmptyPdf);
    }
    let mut estimated = 4_096usize;
    for page in pages {
        page.raster.validate()?;
        if page.width_um <= 0 || page.height_um <= 0 {
            return Err(ExportError::InvalidPdfMedia);
        }
        estimated = estimated
            .checked_add(page.raster.bytes().len())
            .and_then(|value| value.checked_add(2_048))
            .ok_or(ExportError::OutputTooLarge {
                limit: max_output_bytes,
            })?;
    }
    if estimated > max_output_bytes {
        return Err(ExportError::OutputTooLarge {
            limit: max_output_bytes,
        });
    }

    let kids = (0..pages.len())
        .map(|index| format!("{} 0 R", 3 + index * 3))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).into_bytes(),
    ];
    for (index, page) in pages.iter().enumerate() {
        let width_pt = page.width_um as f64 * 72.0 / 25_400.0;
        let height_pt = page.height_um as f64 * 72.0 / 25_400.0;
        let image_id = 4 + index * 3;
        let content_id = 5 + index * 3;
        let content = format!("q\n{width_pt:.6} 0 0 {height_pt:.6} 0 0 cm\n/Im0 Do\nQ\n");
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width_pt:.6} {height_pt:.6}] /Resources << /XObject << /Im0 {image_id} 0 R >> >> /Contents {content_id} 0 R >>").into_bytes());
        objects.push(stream(
            format!(
                "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]",
                page.raster.width(), page.raster.height()
            ),
            page.raster.bytes(),
        ));
        objects.push(stream(String::new(), content.as_bytes()));
    }
    finish_pdf(objects, max_output_bytes)
}

pub fn pdf_packed_pages_physical_with_limits(
    pages: &[PackedPdfPage],
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, ExportError> {
    enforce_page_count(pages.len(), limits)?;
    let total_pixels = pages.iter().try_fold(0u64, |total, page| {
        let pixels = u64::from(page.raster.width())
            .checked_mul(u64::from(page.raster.height()))
            .ok_or(ExportError::Raster(RasterError::Dimensions))?;
        if pixels > limits.max_canvas_pixels {
            return Err(ExportError::Raster(RasterError::Dimensions));
        }
        total
            .checked_add(pixels)
            .ok_or(ExportError::Raster(RasterError::Dimensions))
    })?;
    if total_pixels > limits.max_total_pixels {
        return Err(ExportError::Raster(RasterError::Dimensions));
    }
    pdf_packed_pages_physical(pages, limits.max_output_bytes)
}

fn bounded_extend(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<(), ExportError> {
    let next = output
        .len()
        .checked_add(bytes.len())
        .ok_or(ExportError::OutputTooLarge { limit })?;
    if next > limit {
        return Err(ExportError::OutputTooLarge { limit });
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn finish_pdf(objects: Vec<Vec<u8>>, limit: usize) -> Result<Vec<u8>, ExportError> {
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = vec![0];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        bounded_extend(&mut out, format!("{} 0 obj\n", i + 1).as_bytes(), limit)?;
        bounded_extend(&mut out, object, limit)?;
        bounded_extend(&mut out, b"\nendobj\n", limit)?;
    }
    let xref = out.len();
    bounded_extend(
        &mut out,
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        limit,
    )?;
    for offset in offsets.iter().skip(1) {
        bounded_extend(
            &mut out,
            format!("{offset:010} 00000 n \n").as_bytes(),
            limit,
        )?;
    }
    bounded_extend(
        &mut out,
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
        limit,
    )?;
    Ok(out)
}
fn pdf_pages_points(
    pages: &[(&MonoRaster, f64, f64)],
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, ExportError> {
    if pages.is_empty() {
        return Err(ExportError::EmptyPdf);
    }
    let mut total_pixels = 0u64;
    let mut estimated = 4_096usize;
    for (raster, _, _) in pages {
        let pixels = raster_pixels(raster)?;
        if pixels > limits.max_canvas_pixels {
            return Err(ExportError::Raster(RasterError::Dimensions));
        }
        total_pixels = total_pixels
            .checked_add(pixels)
            .ok_or(ExportError::Raster(RasterError::Dimensions))?;
        estimated = estimated
            .checked_add(raster.pack_msb()?.len())
            .and_then(|value| value.checked_add(2_048))
            .ok_or(ExportError::OutputTooLarge {
                limit: limits.max_output_bytes,
            })?;
    }
    if total_pixels > limits.max_total_pixels || estimated > limits.max_output_bytes {
        return Err(ExportError::OutputTooLarge {
            limit: limits.max_output_bytes,
        });
    }
    let kids = (0..pages.len())
        .map(|index| format!("{} 0 R", 3 + index * 3))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).into_bytes(),
    ];
    for (index, (r, width_pt, height_pt)) in pages.iter().enumerate() {
        r.validate()?;
        let image = r.pack_msb()?;
        let image_id = 4 + index * 3;
        let content_id = 5 + index * 3;
        let content = format!("q\n{width_pt:.6} 0 0 {height_pt:.6} 0 0 cm\n/Im0 Do\nQ\n");
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width_pt:.6} {height_pt:.6}] /Resources << /XObject << /Im0 {image_id} 0 R >> >> /Contents {content_id} 0 R >>").into_bytes());
        objects.push(stream(
            format!(
                "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]",
                r.width, r.height
            ),
            &image,
        ));
        objects.push(stream(String::new(), content.as_bytes()));
    }
    finish_pdf(objects, limits.max_output_bytes)
}

fn raster_pixels(raster: &MonoRaster) -> Result<u64, ExportError> {
    raster.validate()?;
    u64::from(raster.width)
        .checked_mul(u64::from(raster.height))
        .ok_or(ExportError::Raster(RasterError::Dimensions))
}

fn enforce_raster_pixels(raster: &MonoRaster, max_pixels: u64) -> Result<(), ExportError> {
    if raster_pixels(raster)? > max_pixels {
        Err(ExportError::Raster(RasterError::Dimensions))
    } else {
        Ok(())
    }
}

fn enforce_page_count(count: usize, limits: &ProcessingLimits) -> Result<(), ExportError> {
    if count == 0 {
        return Err(ExportError::EmptyPdf);
    }
    if count > usize::try_from(limits.max_pages).unwrap_or(usize::MAX) {
        return Err(ExportError::OutputTooLarge {
            limit: limits.max_output_bytes,
        });
    }
    Ok(())
}
fn stream(dict: String, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend(data);
    v.extend(b"\nendstream");
    v
}
