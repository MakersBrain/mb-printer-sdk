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
    )
}
