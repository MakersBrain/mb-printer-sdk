// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{Document, render};

#[test]
fn native_matches_shared_wasm_fixture_bytes() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/wasm/equivalence.json")).unwrap();
    let document: Document = serde_json::from_value(fixture["document"].clone()).unwrap();
    document.validate().unwrap();
    let packed = render::render(&document, Default::default())
        .unwrap()
        .pack_msb()
        .unwrap();
    let actual: String = packed.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(actual, fixture["expectedPackedHex"]);
}
