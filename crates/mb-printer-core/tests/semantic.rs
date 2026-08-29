// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{Document, document::ValidationError};
fn validate(mutator: impl FnOnce(&mut serde_json::Value)) -> Vec<ValidationError> {
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/v4/valid/all-elements.json")).unwrap();
    mutator(&mut v);
    Document::from_json(&v.to_string())
        .unwrap()
        .validate()
        .unwrap_err()
}
#[test]
fn zones_require_bounds_references_and_acyclic_clones() {
    let e = validate(|v| v["media"]["zones"][0]["bounds"]["width"] = 0.into());
    assert!(e.iter().any(|e| matches!(e, ValidationError::Zone(_))));
    let e = validate(|v| v["media"]["zones"][0]["cloneOf"] = "missing".into());
    assert!(e.iter().any(|e| matches!(e, ValidationError::Reference(_))));
    let e = validate(|v| v["media"]["zones"][0]["cloneOf"] = "z2".into());
    assert!(e.iter().any(|e| matches!(e, ValidationError::Cycle(_))))
}
#[test]
fn groups_constraints_and_resource_types_are_checked() {
    let e = validate(|v| v["elements"][1]["groupId"] = "missing".into());
    assert!(e.iter().any(|e| matches!(e, ValidationError::Reference(_))));
    let e = validate(|v| v["elements"][0]["groupId"] = "g".into());
    assert!(e.iter().any(|e| matches!(e, ValidationError::Cycle(_))));
    let e = validate(|v| v["elements"][9]["constraints"]["zone"] = "missing".into());
    assert!(e.iter().any(|e| matches!(e, ValidationError::Reference(_))));
    let e = validate(|v| v["resources"][0]["mediaType"] = "text/plain".into());
    assert!(
        e.iter()
            .any(|e| matches!(e, ValidationError::ResourceMedia(_)))
    );

    let e = validate(|v| v["elements"][0]["children"] = serde_json::json!([]));
    assert!(e.iter().any(|e| matches!(e, ValidationError::Reference(_))));
}
#[test]
fn schema_value_constraints_are_enforced_by_runtime_validation() {
    let e = validate(|v| v["name"] = "".into());
    assert!(e.iter().any(|e| matches!(e, ValidationError::Name)));
    let e = validate(|v| v["elements"][1]["transform"]["rotationMillidegrees"] = 360_001.into());
    assert!(e.iter().any(|e| matches!(e, ValidationError::Element(_))));
    let e = validate(|v| {
        let hash = v["resources"][0]["sha256"].as_str().unwrap().to_uppercase();
        v["resources"][0]["sha256"] = hash.into();
    });
    assert!(
        e.iter()
            .any(|e| matches!(e, ValidationError::ResourceHash(_)))
    );
    let e = validate(|v| v["extensions"] = serde_json::json!({":": {}}));
    assert!(
        e.iter()
            .any(|e| matches!(e, ValidationError::ExtensionNamespace(_)))
    );
}
