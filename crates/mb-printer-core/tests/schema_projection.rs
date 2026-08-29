// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::Document;
use mb_printer_core::schema_types_generated::{ELEMENT_DISCRIMINATORS, SchemaElementKind};
use std::collections::BTreeSet;

#[test]
fn generated_schema_projection_conforms_to_rust_model() {
    let projection: serde_json::Value =
        serde_json::from_str(include_str!("../schema/mb-label-v4.projection.json")).unwrap();
    let document =
        Document::from_json(include_str!("../fixtures/v4/valid/all-elements.json")).unwrap();
    let projected = projection["elements"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let serialized = serde_json::to_value(document).unwrap();
    let represented = serialized["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["type"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        represented, projected,
        "the Rust conformance document must represent every schema discriminator"
    );
    let generated = ELEMENT_DISCRIMINATORS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(generated, projected.iter().map(String::as_str).collect());
    assert_eq!(
        serde_json::to_string(&SchemaElementKind::QrCode).unwrap(),
        "\"qr-code\""
    );
}
