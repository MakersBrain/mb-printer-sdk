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

#[test]
fn wasm_protocol_options_control_copies_density_and_reject_unknown_fields() {
    let encoded = mb_printer_wasm::render_protocol_plan_with_options(
        DOC,
        "m03",
        r#"{"copies":2,"density":8}"#,
    )
    .unwrap();
    let plan: mb_printer_core::protocol::Plan = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| matches!(action, mb_printer_core::protocol::Action::CommandWrite { name, .. } if name == "ESC @ init"))
            .count(),
        2
    );
    assert!(plan.actions.iter().any(|action| matches!(
        action,
        mb_printer_core::protocol::Action::CommandWrite { name, bytes, .. }
            if name == "GS | density" && bytes == &[0x1d, 0x7c, 8]
    )));
    assert!(
        mb_printer_wasm::render_protocol_plan_with_options(
            DOC,
            "m03",
            r#"{"copies":1,"retry":true}"#,
        )
        .is_err()
    );
}

#[test]
fn wasm_facade_infers_brother_62x29_media_and_printable_rows() {
    let input = DOC
        .replace("10000,\"height\":10000", "62000,\"height\":29000")
        .replace("\"dpi\":203", "\"dpi\":300")
        .replace(
            "\"width\":10000,\"height\":10000},\"shape\"",
            "\"width\":62000,\"height\":29000},\"shape\"",
        );
    let encoded = mb_printer_wasm::render_protocol_plan(&input, "ql-1110nwb").unwrap();
    let plan: mb_printer_core::protocol::Plan = serde_json::from_str(&encoded).unwrap();
    assert!(plan.actions.iter().any(|action| matches!(
        action,
        mb_printer_core::protocol::Action::CommandWrite { bytes, .. }
            if bytes == &[0x1b, 0x69, 0x7a, 0xce, 0x0b, 62, 29, 0x0f, 0x01, 0, 0, 0, 0]
    )));
}
