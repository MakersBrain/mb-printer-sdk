// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    Document, capabilities, export,
    raster::{Dither, Fit, GrayRaster, Rotation},
    render,
};
const DOC: &str = r#"{
 "version":4,"name":"render fixture",
 "media":{"width":50000,"height":30000,"unit":"micrometre","dpi":203,"orientation":"landscape","printableBounds":{"x":0,"y":0,"width":50000,"height":30000},"shape":"rectangle"},
 "coordinateSystem":{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"},
 "elements":[
  {"type":"rectangle","id":"border","transform":{"x":1000,"y":1000,"width":48000,"height":28000},"zOrder":0,"strokeWidth":250,"fill":false},
  {"type":"text","id":"text","transform":{"x":3000,"y":3000,"width":18000,"height":6000},"zOrder":1,"text":"MB 42","fontSize":3000,"horizontalAlign":"left","verticalAlign":"top","overflow":"clip"},
  {"type":"barcode","id":"code39","transform":{"x":3000,"y":12000,"width":24000,"height":12000},"zOrder":2,"data":"MB-42","symbology":"code39","humanReadable":false},
  {"type":"qr-code","id":"qr","transform":{"x":31000,"y":4000,"width":15000,"height":15000},"zOrder":3,"data":"makersbrain:42","errorCorrection":"M"}
 ],"resources":[],"fields":[],"extensions":{}
}"#;
#[test]
fn fixed_point_rounding_is_explicit() {
    assert_eq!(render::micrometres_to_dots(12_700, 203), 102);
    assert_eq!(render::micrometres_to_dots(-12_700, 203), -102)
}
#[test]
fn dithers_rotation_fitting_and_packing_are_stable() {
    let g = GrayRaster {
        width: 3,
        height: 2,
        pixels: vec![0, 64, 255, 127, 128, 200],
    };
    for d in [
        Dither::Threshold(128),
        Dither::Auto,
        Dither::Bayer2,
        Dither::Bayer4,
        Dither::FloydSteinberg,
        Dither::Atkinson,
    ] {
        let m = g.dither(d).unwrap();
        assert_eq!(m.pack_msb().unwrap().len(), 2);
        let r = m.rotate(Rotation::Clockwise90);
        assert_eq!((r.width, r.height), (2, 3));
        assert_eq!(r.fit_head(8, Fit::Center).unwrap().width, 8)
    }
}
#[test]
fn mirror_padding_and_offsets_are_deterministic() {
    let m = mb_printer_core::raster::MonoRaster {
        width: 3,
        height: 1,
        pixels: vec![1, 0, 0],
    };
    assert_eq!(m.mirror_horizontal().pixels, vec![0, 0, 1]);
    assert_eq!(
        m.pad_rows(1, 2).pixels,
        vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    let placed = m.place_on_head(5, Fit::Center, 1, 0).unwrap();
    assert_eq!(placed.pixels, vec![0, 0, 1, 0, 0])
}
#[test]
fn document_render_and_exports_are_byte_deterministic() {
    let d = Document::from_json(DOC).unwrap();
    let a = render::render(&d, Default::default()).unwrap();
    let b = render::render(&d, Default::default()).unwrap();
    assert_eq!(a, b);
    assert!(a.pixels.contains(&1));
    let png1 = export::png(&a, d.media.dpi).unwrap();
    let png2 = export::png(&a, d.media.dpi).unwrap();
    assert_eq!(png1, png2);
    assert!(png1.starts_with(b"\x89PNG\r\n\x1a\n"));
    let pdf1 = export::pdf(&a, d.media.dpi).unwrap();
    let pdf2 = export::pdf(&a, d.media.dpi).unwrap();
    assert_eq!(pdf1, pdf2);
    assert!(pdf1.starts_with(b"%PDF-1.4"))
}
#[test]
fn multi_page_pdf_is_deterministic() {
    let d = Document::from_json(DOC).unwrap();
    let raster = render::render(&d, Default::default()).unwrap();
    let pages = [(&raster, d.media.dpi), (&raster, 300)];
    let first = export::pdf_pages(&pages).unwrap();
    let second = export::pdf_pages(&pages).unwrap();
    assert_eq!(first, second);
    assert!(String::from_utf8_lossy(&first).contains("/Count 2"));
    assert!(export::pdf_pages(&[]).is_err());
}
#[test]
fn physical_pdf_uses_authoritative_media_geometry() {
    let raster = mb_printer_core::raster::MonoRaster {
        width: 64,
        height: 8,
        pixels: vec![0; 512],
    };
    let exact = export::pdf_physical(&raster, 8_000, 1_000).unwrap();
    let text = String::from_utf8_lossy(&exact);
    assert!(text.contains("/MediaBox [0 0 22.677165 2.834646]"));
    assert!(text.contains("22.677165 0 0 2.834646 0 0 cm"));
    let stamp = export::pdf_physical(&raster, 63_500, 33_900).unwrap();
    let text = String::from_utf8_lossy(&stamp);
    assert!(text.contains("/MediaBox [0 0 180.000000 96.094488]"));
    assert!(export::pdf_physical(&raster, 0, 1_000).is_err());
}
#[test]
fn embedded_ttf_is_shaped_and_rasterized_deterministically() {
    // ttf-parser's AGPL-compatible 400-byte demo font, embedded to keep the test hermetic.
    let data = "AAEAAAAHAEAAAgAwY21hcAAJAHYAAAEAAAAALGdseWbxy2aYAAABNAAAAFxoZWFk8jXd+AAAAHwAAAA2aGhlYQZhAMoAAAC0AAAAJGhtdHgEdABqAAAA+AAAAAhsb2NhAC4AFAAAASwAAAAGbWF4cAAFAAsAAADYAAAAIAABAAAAAQAA9ZwpRF8PPPUAAgPoAAAAALSS9AAAAAAA3C+mXAAGAAACWAK8AAAAAwACAAAAAAAAAAEAAAQA/nAAAAJYAAb//wJYAAEAAAAAAAAAAAAAAAAAAAACAAEAAAACAAsAAgAAAAAAAAAAAAAAAAAAAAAAAAAAAAACWABkAhwABgAAAAEAAAADAAAADAAEACAAAAAEAAQAAQAAAEH//wAAAEH////AAAEAAAAAAAAAFAAuAAAAAgBkAAACWAK8AAMABwAAMxEhESUhESFkAfT+NAGk/lwCvP1EKAJsAAIABgAAAh0CkAACAAoAABMzAwETMxMjJyMHrcRj/vjaYN1ZPu9CAQsBQP21ApD9cMjIAA==";
    let source = DOC
        .replace("\"text\":\"MB 42\",", "\"text\":\"A\",\"fontResource\":\"demo-font\",")
        .replace(
            "\"resources\":[]",
            &format!("\"resources\":[{{\"id\":\"demo-font\",\"mediaType\":\"font/ttf\",\"sha256\":\"cb40a3b0aed56dbd2465355ff5ac53ea5e6b567877132844d8f780fd600bdade\",\"dataBase64\":\"{data}\"}}]"),
        );
    let document = Document::from_json(&source).unwrap();
    let first = render::render(&document, Default::default()).unwrap();
    let second = render::render(&document, Default::default()).unwrap();
    assert_eq!(first, second);
}
#[test]
fn cloned_zones_repeat_source_zone_elements() {
    let source = r#"{
      "version":4,"name":"clone zones",
      "media":{"width":20000,"height":10000,"unit":"micrometre","dpi":254,"orientation":"landscape","printableBounds":{"x":0,"y":0,"width":20000,"height":10000},"shape":"rectangle","zones":[{"id":"source","bounds":{"x":0,"y":0,"width":10000,"height":10000}},{"id":"copy","bounds":{"x":10000,"y":0,"width":10000,"height":10000},"cloneOf":"source"}]},
      "coordinateSystem":{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"},
      "elements":[{"type":"rectangle","id":"mark","transform":{"x":1000,"y":1000,"width":3000,"height":3000},"zOrder":0,"constraints":{"zone":"source"},"strokeWidth":100,"fill":true}],
      "resources":[],"fields":[],"extensions":{}
    }"#;
    let document = Document::from_json(source).unwrap();
    let raster = render::render(&document, Default::default()).unwrap();
    assert_eq!(raster.pixels[20 * raster.width as usize + 20], 1);
    assert_eq!(raster.pixels[20 * raster.width as usize + 120], 1);
}
fn rotation_document(elements: &str) -> Document {
    Document::from_json(&format!(
        r#"{{"version":4,"name":"rotation","media":{{"width":10000,"height":10000,"unit":"micrometre","dpi":254,"orientation":"portrait","printableBounds":{{"x":0,"y":0,"width":10000,"height":10000}},"shape":"rectangle"}},"coordinateSystem":{{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"}},"elements":[{elements}],"resources":[],"fields":[],"extensions":{{}}}}"#
    ))
    .unwrap()
}
#[test]
fn arbitrary_element_rotation_is_deterministic() {
    let document = rotation_document(
        r#"{"type":"rectangle","id":"mark","transform":{"x":2000,"y":2000,"width":4000,"height":1000,"rotationMillidegrees":45000},"zOrder":0,"strokeWidth":100,"fill":true}"#,
    );
    let first = render::render(&document, Default::default()).unwrap();
    let second = render::render(&document, Default::default()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.pixels[25 * 100 + 40], 1);
    assert_eq!(first.pixels[20 * 100 + 20], 0);
}
#[test]
fn affine_group_rotation_matches_flattened_transform() {
    let grouped = rotation_document(
        r#"{"type":"group","id":"group","transform":{"x":5000,"y":2000,"width":4000,"height":4000,"rotationMillidegrees":90000},"zOrder":0,"children":["mark"]},{"type":"rectangle","id":"mark","transform":{"x":0,"y":1000,"width":2000,"height":1000},"zOrder":1,"groupId":"group","strokeWidth":100,"fill":true}"#,
    );
    let flattened = rotation_document(
        r#"{"type":"rectangle","id":"mark","transform":{"x":6500,"y":2500,"width":2000,"height":1000,"rotationMillidegrees":90000},"zOrder":0,"strokeWidth":100,"fill":true}"#,
    );
    assert_eq!(
        render::render(&grouped, Default::default()).unwrap(),
        render::render(&flattened, Default::default()).unwrap()
    );
}
#[test]
fn render_to_printer_raster_is_plan_ready() {
    let d = Document::from_json(DOC).unwrap();
    let p = capabilities::by_id("m03").unwrap();
    let r = render::render_for_printer(&d, &p, Default::default()).unwrap();
    assert_eq!(r.width_bytes, 54);
    assert_eq!(r.data.len(), 54 * r.height as usize);
    mb_printer_core::protocol::plan(&p, &r, &Default::default()).unwrap();
}
#[test]
fn code128_is_supported() {
    let source = DOC.replace("\"code39\"", "\"code128\"");
    let d = Document::from_json(&source).unwrap();
    assert!(render::render(&d, Default::default()).is_ok())
}
#[test]
fn retail_barcode_check_digits_are_enforced() {
    for (kind, data) in [("ean13", "4006381333931"), ("upc-a", "036000291452")] {
        let source = DOC
            .replace("\"code39\"", &format!("\"{kind}\""))
            .replace("MB-42", data);
        let d = Document::from_json(&source).unwrap();
        assert!(render::render(&d, Default::default()).is_ok());
    }
    let bad = DOC
        .replace("\"code39\"", "\"ean13\"")
        .replace("MB-42", "4006381333930");
    assert!(render::render(&Document::from_json(&bad).unwrap(), Default::default()).is_err())
}
