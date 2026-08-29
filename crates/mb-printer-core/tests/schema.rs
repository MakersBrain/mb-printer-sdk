// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::Document;
#[test]
fn shared_corpus_matches_json_schema_and_serde() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schema/mb-label-v4.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let valid = include_str!("../fixtures/v4/valid/all-elements.json");
    let value: serde_json::Value = serde_json::from_str(valid).unwrap();
    assert!(validator.is_valid(&value));
    Document::from_json(valid).unwrap().validate().unwrap();
    let invalid = include_str!("../fixtures/v4/invalid/unknown-element-field.json");
    let value: serde_json::Value = serde_json::from_str(invalid).unwrap();
    assert!(!validator.is_valid(&value));
    assert!(Document::from_json(invalid).is_err());
    for (name, semantic_invalid) in [
        (
            "zone-cycle",
            include_str!("../fixtures/v4/invalid-semantic/zone-cycle.json"),
        ),
        (
            "group-cycle",
            include_str!("../fixtures/v4/invalid-semantic/group-cycle.json"),
        ),
        (
            "constraint-zone",
            include_str!("../fixtures/v4/invalid-semantic/constraint-zone.json"),
        ),
        (
            "missing-resource",
            include_str!("../fixtures/v4/invalid-semantic/missing-resource.json"),
        ),
        (
            "resource-hash",
            include_str!("../fixtures/v4/invalid-semantic/resource-hash.json"),
        ),
        (
            "zone-missing-clone",
            include_str!("../fixtures/v4/invalid-semantic/zone-missing-clone.json"),
        ),
        (
            "group-missing-parent",
            include_str!("../fixtures/v4/invalid-semantic/group-missing-parent.json"),
        ),
        (
            "resource-type",
            include_str!("../fixtures/v4/invalid-semantic/resource-type.json"),
        ),
    ] {
        let value: serde_json::Value = serde_json::from_str(semantic_invalid).unwrap();
        assert!(
            validator.is_valid(&value),
            "{name} must remain schema-valid"
        );
        let document = Document::from_json(semantic_invalid).unwrap();
        assert!(document.validate().is_err(), "{name} must fail semantics");
    }
}
