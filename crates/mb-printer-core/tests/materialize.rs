// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    Document,
    limits::ProcessingLimits,
    materialize::{self, MaterializeError, MaterializeOptions},
};
use std::collections::BTreeMap;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../fixtures/materialize/parity.json")).unwrap()
}

fn values(document: &Document) -> Vec<&str> {
    document
        .elements
        .iter()
        .filter_map(|element| match element {
            mb_printer_core::document::Element::Text { text, .. } => Some(text.as_str()),
            mb_printer_core::document::Element::Barcode { data, .. }
            | mb_printer_core::document::Element::QrCode { data, .. } => Some(data.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn materialization_and_zone_batch_match_the_shared_fixture() {
    let fixture = fixture();
    let document: Document = serde_json::from_value(fixture["document"].clone()).unwrap();
    let records: Vec<BTreeMap<String, String>> =
        serde_json::from_value(fixture["records"].clone()).unwrap();
    let zone_ids: Vec<String> = serde_json::from_value(fixture["zoneIds"].clone()).unwrap();
    let options = MaterializeOptions {
        locale: fixture["options"]["locale"].as_str().unwrap(),
        current_date: fixture["options"]["currentDate"].as_str().unwrap(),
    };
    let record = materialize::materialize_record(&document, &records[0], options).unwrap();
    assert_eq!(values(&record), ["ALICE", "ID-001", "30/08/2026"]);

    let plan = materialize::plan_zone_batch(&document, records.len() as u32, &zone_ids).unwrap();
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        fixture["expected"]["plan"]
    );

    let pages =
        materialize::materialize_zone_batch(&document, &records, &zone_ids, options).unwrap();
    assert_eq!(
        pages
            .iter()
            .map(|page| page.name.as_str())
            .collect::<Vec<_>>(),
        ["zone batch page 1", "zone batch page 2"]
    );
    assert_eq!(
        pages[0]
            .elements
            .iter()
            .map(|element| serde_json::to_value(element).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned())
            .collect::<Vec<_>>(),
        serde_json::from_value::<Vec<String>>(fixture["expected"]["firstPageElementIds"].clone())
            .unwrap()
    );
    for page in &pages {
        page.validate().unwrap();
    }
}

#[test]
fn batch_errors_are_typed_and_limits_are_checked_before_output_growth() {
    let fixture = fixture();
    let document: Document = serde_json::from_value(fixture["document"].clone()).unwrap();
    let zones = vec!["left".to_owned(), "right".to_owned()];
    let limits = ProcessingLimits {
        max_pages: 1,
        ..ProcessingLimits::default()
    };
    assert!(matches!(
        materialize::plan_zone_batch_with_limits(&document, 3, &zones, &limits),
        Err(MaterializeError::LimitExceeded)
    ));
    let records = vec![BTreeMap::new(), BTreeMap::new()];
    let limits = ProcessingLimits {
        max_elements: 7,
        ..ProcessingLimits::default()
    };
    assert!(matches!(
        materialize::materialize_zone_batch_with_limits(
            &document,
            &records,
            &zones,
            Default::default(),
            &limits,
        ),
        Err(MaterializeError::LimitExceeded)
    ));
    assert_eq!(
        materialize::plan_zone_batch(&document, 1, &[])
            .unwrap_err()
            .code(),
        "batch.no_zones"
    );
    assert!(matches!(
        materialize::plan_zone_batch(&document, 1, &["left".into(), "left".into()]),
        Err(MaterializeError::DuplicateZone { index: 1 })
    ));
    assert!(matches!(
        materialize::plan_zone_batch(&document, 1, &["missing".into()]),
        Err(MaterializeError::UnknownZone { index: 0 })
    ));
}

#[test]
fn template_errors_report_only_the_bounded_element_index() {
    let fixture = fixture();
    let mut document: Document = serde_json::from_value(fixture["document"].clone()).unwrap();
    if let mb_printer_core::document::Element::Text { text, .. } = &mut document.elements[1] {
        *text = "{{secret}}".into();
    }
    let error = materialize::materialize_record(&document, &BTreeMap::new(), Default::default())
        .unwrap_err();
    assert!(matches!(
        error,
        MaterializeError::Template { element: 1, .. }
    ));
    assert_eq!(error.to_string(), "template evaluation failed at element 1");
}
