// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{document::Resource, limits::ProcessingLimits, raster::GrayRaster};
use image::ImageReader;
use resvg::{tiny_skia, usvg};
use std::io::Cursor;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum ResourceError {
    #[error("invalid base64 resource")]
    Base64,
    #[error("unsupported media type: {0}")]
    MediaType(String),
    #[error("image decode failed: {0}")]
    Image(#[from] image::ImageError),
    #[error("SVG parse failed: {0}")]
    Svg(String),
    #[error("resource raster is too large")]
    TooLarge,
}
pub fn normalize(resource: &Resource, max_pixels: u64) -> Result<GrayRaster, ResourceError> {
    let limits = ProcessingLimits {
        max_resource_pixels: max_pixels,
        ..ProcessingLimits::default()
    };
    normalize_with_limits(resource, &limits)
}

pub fn normalize_with_limits(
    resource: &Resource,
    limits: &ProcessingLimits,
) -> Result<GrayRaster, ResourceError> {
    let _span = tracing::debug_span!(
        "resource.decode",
        encoded_bytes = resource.data_base64.len(),
        max_pixels = limits.max_resource_pixels
    )
    .entered();
    let bytes = resource
        .decoded_bytes_with_limits(limits)
        .map_err(|error| match error {
            crate::document::ResourceDecodeError::Invalid => ResourceError::Base64,
            crate::document::ResourceDecodeError::EncodedTooLarge
            | crate::document::ResourceDecodeError::DecodedTooLarge => ResourceError::TooLarge,
        })?;
    match resource.media_type.as_str() {
        "image/png" | "image/jpeg" => decode_image(&bytes, limits.max_resource_pixels),
        "image/svg+xml" => decode_svg(&bytes, limits.max_resource_pixels),
        x => Err(ResourceError::MediaType(x.into())),
    }
}
fn decode_image(bytes: &[u8], max_pixels: u64) -> Result<GrayRaster, ResourceError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?;
    let maximum_dimension = u32::try_from(max_pixels).unwrap_or(u32::MAX);
    let mut decode_limits = image::Limits::default();
    decode_limits.max_image_width = Some(maximum_dimension);
    decode_limits.max_image_height = Some(maximum_dimension);
    decode_limits.max_alloc = max_pixels.checked_mul(8);
    reader.limits(decode_limits);
    let img = reader
        .decode()
        .map_err(|error| {
            if matches!(error, image::ImageError::Limits(_)) {
                ResourceError::TooLarge
            } else {
                ResourceError::Image(error)
            }
        })?
        .to_luma8();
    if img.width() as u64 * img.height() as u64 > max_pixels {
        return Err(ResourceError::TooLarge);
    }
    Ok(GrayRaster {
        width: img.width(),
        height: img.height(),
        pixels: img.into_raw(),
    })
}
/// SVG encoded bytes and output pixels are bounded before allocation. `usvg`
/// does not expose a node-count/depth limiter, so parser complexity inside an
/// otherwise byte-bounded SVG remains a documented residual risk.
fn decode_svg(bytes: &[u8], max_pixels: u64) -> Result<GrayRaster, ResourceError> {
    let text = std::str::from_utf8(bytes).map_err(|e| ResourceError::Svg(e.to_string()))?;
    if [
        "href=\"http://",
        "href=\"https://",
        "href='http://",
        "href='https://",
        "url(http://",
        "url(https://",
        "href=\"file:",
        "href='file:",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        return Err(ResourceError::Svg(
            "external resources are forbidden".into(),
        ));
    }
    let tree = usvg::Tree::from_str(text, &usvg::Options::default())
        .map_err(|e| ResourceError::Svg(e.to_string()))?;
    let size = tree.size().to_int_size();
    if size.width() as u64 * size.height() as u64 > max_pixels {
        return Err(ResourceError::TooLarge);
    }
    let mut pixmap =
        tiny_skia::Pixmap::new(size.width(), size.height()).ok_or(ResourceError::TooLarge)?;
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let (rgba, remainder) = pixmap.data().as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    let pixels = rgba
        .iter()
        .map(|p| ((p[0] as u16 * 54 + p[1] as u16 * 183 + p[2] as u16 * 19 + 128) / 256) as u8)
        .collect();
    Ok(GrayRaster {
        width: size.width(),
        height: size.height(),
        pixels,
    })
}
