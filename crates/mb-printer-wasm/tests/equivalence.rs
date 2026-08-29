// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{Document, render};
const DOC: &str = r#"{"version":4,"name":"eq","media":{"width":10000,"height":10000,"unit":"micrometre","dpi":203,"orientation":"portrait","printableBounds":{"x":0,"y":0,"width":10000,"height":10000},"shape":"rectangle"},"coordinateSystem":{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"},"elements":[{"type":"rectangle","id":"r","transform":{"x":1000,"y":1000,"width":8000,"height":8000},"zOrder":0,"strokeWidth":250,"fill":false}],"resources":[],"fields":[],"extensions":{}}"#;
#[test]
fn wasm_facade_and_native_core_are_identical() {
    let doc = Document::from_json(DOC).unwrap();
    let native = render::render(&doc, Default::default())
        .unwrap()
        .pack_msb()
        .unwrap();
    assert_eq!(mb_printer_wasm::render_packed(DOC).unwrap(), native);
    assert!(mb_printer_wasm::validate_document_json(DOC).starts_with('['))
}
#[test]
fn wasm_facade_builds_a_rendered_protocol_plan() {
    let plan = mb_printer_wasm::render_protocol_plan(DOC, "m03").unwrap();
    let value: serde_json::Value = serde_json::from_str(&plan).unwrap();
    assert_eq!(value["protocol"], "m-series");
    assert!(value["actions"].as_array().unwrap().len() > 5)
}
