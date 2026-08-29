// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{document::Resource, raster::GrayRaster};
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
    let bytes = resource.decoded_bytes().ok_or(ResourceError::Base64)?;
    match resource.media_type.as_str() {
        "image/png" | "image/jpeg" => decode_image(&bytes, max_pixels),
        "image/svg+xml" => decode_svg(&bytes, max_pixels),
        x => Err(ResourceError::MediaType(x.into())),
    }
}
fn decode_image(bytes: &[u8], max_pixels: u64) -> Result<GrayRaster, ResourceError> {
    let img = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?
        .decode()?
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
    let pixels = pixmap
        .data()
        .chunks_exact(4)
        .map(|p| ((p[0] as u16 * 54 + p[1] as u16 * 183 + p[2] as u16 * 19 + 128) / 256) as u8)
        .collect();
    Ok(GrayRaster {
        width: size.width(),
        height: size.height(),
        pixels,
    })
}
