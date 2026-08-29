// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::raster::{MonoRaster, RasterError};
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
}
pub fn png(r: &MonoRaster, dpi: u16) -> Result<Vec<u8>, ExportError> {
    r.validate()?;
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
    Ok(out)
}
/// Minimal deterministic PDF 1.4 containing one bilevel image page.
pub fn pdf(r: &MonoRaster, dpi: u16) -> Result<Vec<u8>, ExportError> {
    pdf_pages(&[(r, dpi)])
}

/// A raster page paired with its authoritative physical media dimensions.
#[derive(Debug, Clone, Copy)]
pub struct PdfPage<'a> {
    pub raster: &'a MonoRaster,
    pub width_um: i64,
    pub height_um: i64,
}

/// Export one page using authoritative media geometry rather than rounded dots/DPI.
pub fn pdf_physical(r: &MonoRaster, width_um: i64, height_um: i64) -> Result<Vec<u8>, ExportError> {
    pdf_pages_physical(&[PdfPage {
        raster: r,
        width_um,
        height_um,
    }])
}

/// Deterministic multi-page PDF with exact per-page physical media geometry.
pub fn pdf_pages_physical(pages: &[PdfPage<'_>]) -> Result<Vec<u8>, ExportError> {
    let mut points = Vec::with_capacity(pages.len());
    for page in pages {
        if page.width_um <= 0 || page.height_um <= 0 {
            return Err(ExportError::InvalidPdfMedia);
        }
        points.push((
            page.raster,
            page.width_um as f64 * 72.0 / 25_400.0,
            page.height_um as f64 * 72.0 / 25_400.0,
        ));
    }
    pdf_pages_points(&points)
}

/// Deterministic multi-page PDF. Each page may use its own raster dimensions and DPI.
pub fn pdf_pages(pages: &[(&MonoRaster, u16)]) -> Result<Vec<u8>, ExportError> {
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
    pdf_pages_points(&points)
}
fn pdf_pages_points(pages: &[(&MonoRaster, f64, f64)]) -> Result<Vec<u8>, ExportError> {
    if pages.is_empty() {
        return Err(ExportError::EmptyPdf);
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
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = vec![0];
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(obj);
        out.extend(b"\nendobj\n")
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets.iter().skip(1) {
        out.extend(format!("{offset:010} 00000 n \n").as_bytes())
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Ok(out)
}
fn stream(dict: String, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend(data);
    v.extend(b"\nendstream");
    v
}
