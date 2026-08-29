// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{Document, render};
use sha2::{Digest, Sha256};

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

#[test]
fn native_matches_broad_cross_target_render_goldens() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/wasm/render-goldens.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let document: Document = serde_json::from_value(case["document"].clone()).unwrap();
        let packed = render::render(&document, Default::default())
            .unwrap()
            .pack_msb()
            .unwrap();
        assert_eq!(
            packed.len(),
            case["expectedPackedLength"].as_u64().unwrap() as usize
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&packed)),
            case["expectedPackedSha256"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}
