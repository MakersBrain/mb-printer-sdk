// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{Document, render};
use std::collections::BTreeMap;
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
fn wasm_capabilities_expose_core_ble_profiles() {
    let wasm: serde_json::Value =
        serde_json::from_str(&mb_printer_wasm::capabilities_json()).unwrap();
    let core = serde_json::to_value(mb_printer_core::capabilities::bundled()).unwrap();
    assert_eq!(wasm, core);
    let printers = wasm.as_array().unwrap();
    assert_eq!(
        printers
            .iter()
            .find(|printer| printer["id"] == "m110")
            .unwrap()["ble"]["capabilities"]["writeCharacteristic"],
        "0000ff02-0000-1000-8000-00805f9b34fb"
    );
    assert_eq!(
        printers
            .iter()
            .find(|printer| printer["id"] == "ql-1100")
            .unwrap()["ble"]["kind"],
        "unsupported"
    );
}

#[test]
fn measurement_reports_versioned_physical_ink_bounds() {
    let encoded = mb_printer_wasm::measure_document_json(DOC).unwrap();
    let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(value["layoutVersion"], "mb-printer-layout-v1");
    assert_eq!(value["elements"][0]["instanceId"], "r");
    assert_eq!(value["elements"][0]["sourceElementId"], "r");
    let bounds = &value["contentBounds"];
    assert!(bounds["x"].as_i64().unwrap() <= 1_000);
    assert!(bounds["y"].as_i64().unwrap() <= 1_000);
    assert!(bounds["width"].as_i64().unwrap() >= 8_000);
    assert!(bounds["height"].as_i64().unwrap() >= 8_000);
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
fn continuous_job_options_are_capability_checked_and_control_brother_cutting() {
    let input = DOC
        .replace("\"height\":10000", "\"height\":100000")
        .replace(
            "\"shape\":\"rectangle\"",
            "\"shape\":\"rectangle\",\"continuous\":true",
        );
    let encoded = mb_printer_wasm::render_protocol_plan_with_options(
        &input,
        "ql-1110nwb",
        r#"{"copies":1,"continuous":{"cutMode":"none","extraFeedBeforeUm":0,"extraFeedAfterUm":0,"chainCopies":false}}"#,
    )
    .unwrap();
    let plan: mb_printer_core::protocol::Plan = serde_json::from_str(&encoded).unwrap();
    assert!(!plan.actions.iter().any(|action| matches!(
        action,
        mb_printer_core::protocol::Action::CommandWrite { name, .. } if name == "ESC i M autocut"
    )));
    let after_job = mb_printer_wasm::render_protocol_plan_with_options(
        &input,
        "ql-1110nwb",
        r#"{"copies":1,"continuous":{"cutMode":"after-job","extraFeedBeforeUm":0,"extraFeedAfterUm":0,"chainCopies":false}}"#,
    )
    .unwrap();
    let after_job: mb_printer_core::protocol::Plan = serde_json::from_str(&after_job).unwrap();
    assert!(after_job.actions.iter().any(|action| matches!(
        action,
        mb_printer_core::protocol::Action::CommandWrite { name, bytes, .. }
            if name == "ESC i A cut every" && bytes == &[0x1b, 0x69, 0x41, 1]
    )));
    let chained = mb_printer_wasm::render_protocol_plan_with_options(
        &input,
        "ql-1110nwb",
        r#"{"copies":3,"continuous":{"cutMode":"after-each","extraFeedBeforeUm":0,"extraFeedAfterUm":0,"chainCopies":true}}"#,
    )
    .unwrap();
    let chained: mb_printer_core::protocol::Plan = serde_json::from_str(&chained).unwrap();
    assert!(chained.actions.iter().any(|action| matches!(
        action,
        mb_printer_core::protocol::Action::CommandWrite { name, bytes, .. }
            if name == "ESC i A cut every" && bytes == &[0x1b, 0x69, 0x41, 3]
    )));
}

#[test]
fn continuous_documents_are_capability_checked_without_job_options() {
    let input = DOC
        .replace("\"height\":10000", "\"height\":100000")
        .replace(
            "\"shape\":\"rectangle\"",
            "\"shape\":\"rectangle\",\"continuous\":true",
        );

    assert!(mb_printer_wasm::render_protocol_plan(&input, "ql-1110nwb").is_ok());
    let unsupported = mb_printer_wasm::render_protocol_plan(&input, "m03").unwrap_err();
    assert!(unsupported.contains("does not support qualified continuous-media jobs"));

    let too_short = input.replace("\"height\":100000", "\"height\":10000");
    let invalid_length =
        mb_printer_wasm::render_protocol_plan(&too_short, "ql-1110nwb").unwrap_err();
    assert!(invalid_length.contains("length is outside the printer capability range"));
}

#[test]
fn every_document_in_a_continuous_batch_is_length_checked() {
    let first = DOC
        .replace("\"height\":10000", "\"height\":100000")
        .replace("\"dpi\":203", "\"dpi\":300")
        .replace(
            "\"shape\":\"rectangle\"",
            "\"shape\":\"rectangle\",\"continuous\":true",
        );
    let too_short = first.replace("\"height\":100000", "\"height\":10000");
    let error = mb_printer_wasm::render_protocol_batch_plan_with_options(
        &format!("[{first},{too_short}]"),
        "ql-1110nwb",
        "{}",
    )
    .unwrap_err();
    assert!(error.contains("length is outside the printer capability range"));
}

#[test]
fn native_brother_batch_has_one_boundary_and_one_batch_cut_counter() {
    let first = DOC
        .replace("\"height\":10000", "\"height\":100000")
        .replace("\"dpi\":203", "\"dpi\":300")
        .replace(
            "\"shape\":\"rectangle\"",
            "\"shape\":\"rectangle\",\"continuous\":true",
        );
    let second = first.replace("\"height\":100000", "\"height\":120000");
    let encoded = mb_printer_wasm::render_protocol_batch_plan_with_options(
        &format!("[{first},{second}]"),
        "ql-1110nwb",
        r#"{"copies":2,"continuous":{"cutMode":"after-job","extraFeedBeforeUm":0,"extraFeedAfterUm":0,"chainCopies":true}}"#,
    )
    .unwrap();
    let plan: mb_printer_core::protocol::Plan = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| matches!(
                action,
                mb_printer_core::protocol::Action::JobBoundary {
                    kind: mb_printer_core::protocol::Boundary::Start
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| matches!(action,
                mb_printer_core::protocol::Action::CommandWrite { name, bytes, .. }
                    if name == "ESC i A cut every" && bytes == &[0x1b, 0x69, 0x41, 4]
            ))
            .count(),
        1
    );
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| matches!(action,
                mb_printer_core::protocol::Action::CommandWrite { name, .. } if name == "print"
            ))
            .count(),
        4
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

#[test]
fn sheet_facade_plans_and_exports_with_structured_errors() {
    let definition = r#"{"kind":"grid","id":"one","paperWidthUm":30000,"paperHeightUm":20000,"rows":1,"columns":1,"labelWidthUm":10000,"labelHeightUm":10000,"marginLeftUm":5000,"marginTopUm":5000,"gapXUm":0,"gapYUm":0,"fillOrder":"row-major"}"#;
    let options = r#"{"firstSlot":0,"dpi":100}"#;
    let plan = mb_printer_wasm::plan_sheet_json(
        r#"{"itemCount":1,"labelWidthUm":10000,"labelHeightUm":10000}"#,
        definition,
        options,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&plan).unwrap();
    assert_eq!(value["pageCount"], 1);
    assert_eq!(value["layout"]["slots"][0]["xUm"], 5_000);

    let pdf =
        mb_printer_wasm::build_sheet_pdf_json(&format!("[{DOC}]"), definition, options).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"));

    let error = mb_printer_wasm::plan_sheet_json(
        r#"{"itemCount":1,"labelWidthUm":10000,"labelHeightUm":10000}"#,
        definition,
        r#"{"firstSlot":0,"dpi":0}"#,
    )
    .unwrap_err();
    assert_eq!(error.code, "sheet.invalid_dpi");

    let unknown_layout =
        definition.replace("\"kind\":\"grid\"", "\"kind\":\"grid\",\"bogus\":true");
    let error = mb_printer_wasm::plan_sheet_json(
        r#"{"itemCount":1,"labelWidthUm":10000,"labelHeightUm":10000}"#,
        &unknown_layout,
        options,
    )
    .unwrap_err();
    assert_eq!(error.code, "request.invalid_json");
}

#[test]
fn materialization_facade_matches_core_and_preserves_structured_errors() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../mb-printer-core/fixtures/materialize/parity.json"
    ))
    .unwrap();
    let document_json = serde_json::to_string(&fixture["document"]).unwrap();
    let record_json = serde_json::to_string(&fixture["records"][0]).unwrap();
    let options_json = serde_json::to_string(&fixture["options"]).unwrap();
    let document: Document = serde_json::from_str(&document_json).unwrap();
    let record: BTreeMap<String, String> = serde_json::from_str(&record_json).unwrap();
    let native = mb_printer_core::materialize::materialize_record(
        &document,
        &record,
        mb_printer_core::materialize::MaterializeOptions {
            locale: fixture["options"]["locale"].as_str().unwrap(),
            current_date: fixture["options"]["currentDate"].as_str().unwrap(),
        },
    )
    .unwrap();
    let wasm: Document = serde_json::from_str(
        &mb_printer_wasm::materialize_record_json(&document_json, &record_json, &options_json)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(wasm).unwrap(),
        serde_json::to_value(native).unwrap()
    );

    let zone_ids = fixture["zoneIds"].clone();
    let input = serde_json::json!({ "recordCount": 3, "zoneIds": zone_ids });
    let plan: serde_json::Value = serde_json::from_str(
        &mb_printer_wasm::plan_zone_batch_json(&document_json, &input.to_string()).unwrap(),
    )
    .unwrap();
    assert_eq!(plan, fixture["expected"]["plan"]);

    let batch_options = serde_json::json!({
        "zoneIds": fixture["zoneIds"],
        "locale": fixture["options"]["locale"],
        "currentDate": fixture["options"]["currentDate"]
    });
    let pages: Vec<Document> = serde_json::from_str(
        &mb_printer_wasm::materialize_zone_batch_json(
            &document_json,
            &fixture["records"].to_string(),
            &batch_options.to_string(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(pages.len(), 2);
    pages.iter().for_each(|page| page.validate().unwrap());

    let error = mb_printer_wasm::plan_zone_batch_json(
        &document_json,
        r#"{"recordCount":1,"zoneIds":["missing"]}"#,
    )
    .unwrap_err();
    assert_eq!(error.version, 1);
    assert_eq!(error.code, "batch.unknown_zone");
    assert_eq!(error.details, serde_json::json!({ "index": 0 }));
    assert!(!error.message.contains("missing"));
}

#[test]
fn materialization_facade_rejects_record_counts_above_the_wire_limit() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../mb-printer-core/fixtures/materialize/parity.json"
    ))
    .unwrap();
    let document_json = fixture["document"].to_string();
    let records = vec![BTreeMap::<String, String>::new(); 1_001];
    let error = mb_printer_wasm::materialize_zone_batch_json(
        &document_json,
        &serde_json::to_string(&records).unwrap(),
        r#"{"zoneIds":["left"]}"#,
    )
    .unwrap_err();
    assert_eq!(error.code, "request.too_many_documents");
    let error = mb_printer_wasm::plan_zone_batch_json(
        &document_json,
        r#"{"recordCount":1001,"zoneIds":["left"]}"#,
    )
    .unwrap_err();
    assert_eq!(error.code, "request.too_many_documents");
}

#[test]
fn wasm_facade_rejects_oversized_wire_batch_and_protocol_requests() {
    let oversized = " ".repeat(mb_printer_core::limits::WireLimits::default().max_input_bytes + 1);
    let validation: Vec<String> =
        serde_json::from_str(&mb_printer_wasm::validate_document_json(&oversized)).unwrap();
    assert!(validation[0].contains("encoded input limit"));

    let documents = format!(
        "[{}]",
        std::iter::repeat_n(DOC, 101).collect::<Vec<_>>().join(",")
    );
    assert!(
        mb_printer_wasm::render_batch_pdf(&documents)
            .unwrap_err()
            .contains("document count")
    );

    assert!(
        mb_printer_wasm::render_protocol_plan_with_options(DOC, "m03", r#"{"copies":1001}"#,)
            .unwrap_err()
            .contains("copies")
    );
}

#[test]
fn wasm_facade_uses_the_portable_bounded_ipp_codec() {
    use mb_printer_core::ipp::{
        self, Attribute, AttributeGroup, Message, Value, ValueTag, Version,
    };
    let message = Message {
        version: Version::V2_0,
        code: 0,
        request_id: 7,
        groups: vec![AttributeGroup {
            tag: ipp::PRINTER_ATTRIBUTES_TAG,
            attributes: vec![Attribute::new(
                b"x-vendor".to_vec(),
                Value::raw(ValueTag::Extension(0x7f), [1, 2, 3]),
            )],
        }],
        original_bytes: Vec::new(),
    };
    let bytes = message.encode(ipp::Limits::default()).unwrap();
    let json = mb_printer_wasm::decode_ipp_json(&bytes, 1024).unwrap();
    let decoded: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.original_bytes, bytes);
    assert_eq!(decoded.groups, message.groups);
    assert_eq!(
        mb_printer_wasm::encode_ipp_json(&json, 1024).unwrap(),
        bytes
    );
    assert!(mb_printer_wasm::decode_ipp_json(&bytes, 8).is_err());
}
