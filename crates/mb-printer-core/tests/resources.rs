// SPDX-License-Identifier: AGPL-3.0-or-later
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageEncoder;
use mb_printer_core::{document::Resource, resources};
fn resource(media: &str, bytes: &[u8]) -> Resource {
    Resource {
        id: "r".into(),
        media_type: media.into(),
        sha256: String::new(),
        data_base64: STANDARD.encode(bytes),
    }
}
#[test]
fn png_jpeg_and_svg_normalize_deterministically() {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[0, 64, 128, 255], 2, 2, image::ExtendedColorType::L8)
        .unwrap();
    let p = resources::normalize(&resource("image/png", &png), 100).unwrap();
    assert_eq!((p.width, p.height), (2, 2));
    let svg=br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="3"><rect width="2" height="3" fill="black"/></svg>"#;
    let a = resources::normalize(&resource("image/svg+xml", svg), 100).unwrap();
    let b = resources::normalize(&resource("image/svg+xml", svg), 100).unwrap();
    assert_eq!(a, b);
    assert_eq!((a.width, a.height), (4, 3));
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode(&[0, 128, 255], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
    assert_eq!(
        resources::normalize(&resource("image/jpeg", &jpeg), 100)
            .unwrap()
            .pixels
            .len(),
        1
    )
}
#[test]
fn svg_external_resources_are_rejected() {
    let svg=br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="3"><image href="https://example.test/a.png"/></svg>"#;
    assert!(resources::normalize(&resource("image/svg+xml", svg), 100).is_err())
}
