// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    capabilities::{self, PrinterOperation},
    document::*,
    importer,
    limits::ProcessingLimits,
    protocol, template,
};
use std::collections::BTreeMap;
#[test]
fn printer_definitions_are_losslessly_loaded() {
    let p = capabilities::bundled();
    assert!(p.len() > 20);
    assert_eq!(capabilities::detect("M02 PRO-123").unwrap().id, "m02-pro");
    assert_eq!(capabilities::by_id("p12").unwrap().chunk_size(), 128)
}

#[test]
fn printer_operations_default_to_print_and_are_explicit_for_brother_models() {
    let legacy: capabilities::PrinterDefinition = serde_json::from_str(
        r#"{"id":"minimal","name":"Minimal","protocol":"m02","ble":{"kind":"unsupported"},"widthBytes":null}"#,
    )
    .unwrap();
    assert_eq!(legacy.operations, vec![PrinterOperation::Print]);
    assert!(legacy.supports(PrinterOperation::Print));
    assert!(!legacy.supports(PrinterOperation::Status));

    for id in ["ql-1110nwb", "ql-1115nwb", "ql-1100"] {
        let brother = capabilities::by_id(id).unwrap();
        assert!(brother.supports(PrinterOperation::Print));
        assert!(brother.supports(PrinterOperation::Status));
        assert!(brother.supports(PrinterOperation::SystemReport));
        let wireless = matches!(id, "ql-1110nwb" | "ql-1115nwb");
        assert_eq!(brother.supports(PrinterOperation::WifiStatus), wireless);
        assert_eq!(brother.supports(PrinterOperation::WifiScan), wireless);
        assert_eq!(brother.supports(PrinterOperation::WifiConfigure), wireless);
    }
}
#[test]
fn template_is_deterministic_and_allowlisted() {
    let f = BTreeMap::from([("name".into(), "  Été  ".into())]);
    assert_eq!(
        template::evaluate("{{name|trim|upper}}", &f).unwrap(),
        "ÉTÉ"
    );
    assert!(template::evaluate("{{name|eval}}", &f).is_err())
}
#[test]
fn template_numeric_conditional_date_and_locale_are_injected() {
    let f = BTreeMap::from([
        ("price".into(), "12.5".into()),
        ("state".into(), "ok".into()),
        ("empty".into(), String::new()),
    ]);
    let ctx = template::Context {
        fields: &f,
        locale: "fr-FR",
        current_date: "2026-08-29",
    };
    assert_eq!(template::evaluate_with_context("{{price|number:2}} {{state|if-eq:ok:YES:NO}} {{empty|if-empty:none:some}} {{@date|date:%d/%m/%Y}}",ctx).unwrap(),"12,50 YES none 29/08/2026")
}
#[test]
fn v3_import_normalizes_dimensions_and_alignment() {
    let v=importer::import_v3(r#"{"version":3,"name":"old","widthMm":30,"heightMm":20,"dotsPerMm":8,"elements":[{"id":"a","type":"text","x":0,"y":0,"width":80,"height":16,"fontSize":12,"text":"x","align":"centre","valign":"center"}]}"#).unwrap();
    assert_eq!(v["media"]["width"], 30000);
    assert_eq!(v["elements"][0]["horizontalAlign"], "center");
    assert_eq!(v["elements"][0]["verticalAlign"], "middle");
    let parsed: Document = serde_json::from_value(v).expect("importer must emit canonical v4");
    parsed.validate().expect("imported v4 must validate")
}
#[test]
fn v3_imports_resources_barcodes_groups_and_aliases() {
    let input = r#"{"version":3,"widthMm":50,"heightMm":30,"dotsPerMm":8,"elements":[{"id":"img","type":"img","x":0,"y":0,"width":8,"height":8,"imageData":"data:image/png;base64,"},{"id":"svg","type":"svg","x":8,"y":0,"width":8,"height":8,"svgData":"<svg xmlns='http://www.w3.org/2000/svg' width='1' height='1'/>"},{"id":"bar","type":"bar-code","x":0,"y":8,"width":40,"height":8,"data":"ABC","format":"Code 39"},{"id":"txt","type":"text","x":0,"y":16,"width":40,"height":8,"fontSize":8,"text":"x","fontData":"AA=="},{"id":"grp","type":"group","x":0,"y":0,"width":40,"height":24,"children":["img","svg"]}]}"#;
    let value = importer::import_v3(input).unwrap();
    assert_eq!(value["resources"].as_array().unwrap().len(), 3);
    assert_eq!(value["elements"][2]["symbology"], "code39");
    let doc: Document = serde_json::from_value(value).unwrap();
    doc.validate().unwrap()
}
#[test]
fn strict_json_rejects_unknown_fields() {
    let bad = r#"{"version":4,"name":"x","bogus":1,"media":{"width":1,"height":1,"unit":"micrometre","dpi":203,"orientation":"portrait","printableBounds":{"x":0,"y":0,"width":1,"height":1},"shape":"rectangle"},"coordinateSystem":{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"},"elements":[]}"#;
    assert!(Document::from_json(bad).is_err())
}
#[test]
fn all_protocols_emit_typed_boundaries_and_reference() {
    for p in capabilities::bundled() {
        let Some(w) = p.width_bytes else { continue };
        let r = protocol::Raster {
            width_bytes: w,
            height: 1,
            data: vec![0; w as usize],
        };
        let mut options = protocol::Options::default();
        if p.protocol == capabilities::Protocol::Brother {
            options.brother_media = Some(protocol::BrotherMedia {
                width_mm: 62,
                length_mm: 29,
                continuous: false,
                feed_margin: 0,
            })
        }
        let plan = protocol::plan(&p, &r, &options).unwrap();
        assert_eq!(plan.source_commit, protocol::SOURCE_COMMIT);
        assert!(matches!(
            plan.actions.first(),
            Some(protocol::Action::JobBoundary {
                kind: protocol::Boundary::Start
            })
        ));
        assert!(matches!(
            plan.actions.last(),
            Some(protocol::Action::JobBoundary {
                kind: protocol::Boundary::End
            })
        ))
    }
}
#[test]
fn m_series_timing_fixture() {
    let p = capabilities::by_id("m03").unwrap();
    let r = protocol::Raster {
        width_bytes: 1,
        height: 2,
        data: vec![0xaa, 0x55],
    };
    let plan = protocol::plan(&p, &r, &Default::default()).unwrap();
    let delays: Vec<_> = plan
        .actions
        .iter()
        .filter_map(|a| {
            if let protocol::Action::Delay { milliseconds } = a {
                Some(*milliseconds)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(delays, vec![100, 30, 50, 300, 800]);
    assert!(plan.actions.iter().any(|a| matches!(
        a,
        protocol::Action::RasterWrite {
            logical_chunk: 128,
            delay_after_each_physical_write_ms: 20,
            ..
        }
    )))
}
#[test]
fn all_python_protocol_delay_fixtures_match() {
    let cases = [
        ("m02", vec![50, 100, 30, 300, 500]),
        ("m04s-110", vec![30, 30, 30, 30, 300, 30, 30, 500]),
        ("m110", vec![30, 30, 30, 300, 500]),
        ("d-series", vec![30, 30, 100]),
        ("pm241", vec![50; 9]),
    ];
    for (model, want) in cases {
        let p = capabilities::by_id(model).unwrap();
        let w = p.width_bytes.unwrap_or(48);
        let r = protocol::Raster {
            width_bytes: w,
            height: 1,
            data: vec![0; w as usize],
        };
        let got = protocol::plan(&p, &r, &Default::default())
            .unwrap()
            .actions
            .into_iter()
            .filter_map(|a| {
                if let protocol::Action::Delay { milliseconds } = a {
                    Some(milliseconds)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(got, want, "{model}")
    }
    let p = capabilities::by_id("p12").unwrap();
    let r = protocol::Raster {
        width_bytes: 12,
        height: 1,
        data: vec![0; 12],
    };
    let plan = protocol::plan(&p, &r, &Default::default()).unwrap();
    assert_eq!(
        plan.actions
            .iter()
            .filter(|a| matches!(
                a,
                protocol::Action::WaitForResponse {
                    timeout_ms: 500,
                    fallback_delay_ms: 500,
                    ..
                }
            ))
            .count(),
        6
    );
    assert!(matches!(
        plan.actions[1],
        protocol::Action::SubscribeNotifications
    ));
}

#[test]
fn m110_density_matches_python_ties_to_even_rounding() {
    let p = capabilities::by_id("m110").unwrap();
    let r = protocol::Raster {
        width_bytes: 48,
        height: 1,
        data: vec![0; 48],
    };
    let expected = [6, 8, 9, 10, 11, 12, 14, 15];
    for (density, expected_byte) in (1..=8).zip(expected) {
        let options = protocol::Options {
            density,
            ..Default::default()
        };
        let plan = protocol::plan(&p, &r, &options).unwrap();
        assert!(plan.actions.iter().any(|action| matches!(
            action,
            protocol::Action::CommandWrite { name, bytes, .. }
                if name == "M110 density" && bytes == &[0x1b, 0x4e, 4, expected_byte]
        )));
    }
}
#[test]
fn non_tspl_copies_repeat_the_complete_flow() {
    let p = capabilities::by_id("m03").unwrap();
    let r = protocol::Raster {
        width_bytes: 1,
        height: 1,
        data: vec![0],
    };
    let o = protocol::Options {
        copies: 2,
        ..Default::default()
    };
    let plan = protocol::plan(&p, &r, &o).unwrap();
    assert_eq!(
        plan.actions
            .iter()
            .filter(|a| matches!(a,protocol::Action::CommandWrite{name,..}if name=="ESC @ init"))
            .count(),
        2
    )
}

#[test]
fn protocol_rejects_zero_copy_and_cut_cadence() {
    let printer = capabilities::by_id("m03").unwrap();
    let raster = protocol::Raster {
        width_bytes: 1,
        height: 1,
        data: vec![0],
    };
    assert_eq!(
        protocol::plan(
            &printer,
            &raster,
            &protocol::Options {
                copies: 0,
                ..Default::default()
            }
        ),
        Err(protocol::PlanError::Range("copies"))
    );
    assert_eq!(
        protocol::plan(
            &printer,
            &raster,
            &protocol::Options {
                cut: true,
                cut_every: 0,
                ..Default::default()
            }
        ),
        Err(protocol::PlanError::Range("cut every"))
    );
}

#[test]
fn protocol_rejects_copy_action_and_owned_byte_limits_before_expansion() {
    let printer = capabilities::by_id("m03").unwrap();
    let raster = protocol::Raster {
        width_bytes: 1,
        height: 1,
        data: vec![0],
    };
    let options = protocol::Options {
        copies: 2,
        ..Default::default()
    };

    let copy_limits = ProcessingLimits {
        max_copies: 1,
        ..ProcessingLimits::default()
    };
    assert_eq!(
        protocol::plan_with_limits(&printer, &raster, &options, &copy_limits),
        Err(protocol::PlanError::Limit("copies"))
    );

    let action_limits = ProcessingLimits {
        max_plan_actions: 2,
        ..ProcessingLimits::default()
    };
    assert_eq!(
        protocol::plan_with_limits(&printer, &raster, &options, &action_limits),
        Err(protocol::PlanError::Limit("actions"))
    );

    let byte_limits = ProcessingLimits {
        max_plan_bytes: 1,
        ..ProcessingLimits::default()
    };
    assert_eq!(
        protocol::plan_with_limits(&printer, &raster, &options, &byte_limits),
        Err(protocol::PlanError::Limit("owned bytes"))
    );
}
#[test]
fn qr_quiet_zone_is_configurable_and_defaults_to_four_modules() {
    fn dark_bounds(quiet_zone: Option<u8>) -> (u32, u32, u32, u32) {
        let zone = quiet_zone.map_or(String::new(), |z| format!(r#","quietZone":{z}"#));
        let json = format!(
            r#"{{"version":4,"name":"qr","media":{{"width":20000,"height":20000,"unit":"micrometre","dpi":254,"orientation":"portrait","printableBounds":{{"x":0,"y":0,"width":20000,"height":20000}},"shape":"rectangle"}},"coordinateSystem":{{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"}},"elements":[{{"type":"qr-code","id":"q","transform":{{"x":0,"y":0,"width":20000,"height":20000}},"zOrder":0,"data":"MB","errorCorrection":"L"{zone}}}]}}"#
        );
        let document = Document::from_json(&json).unwrap();
        document.validate().unwrap();
        let raster = mb_printer_core::render::render(&document, Default::default()).unwrap();
        let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..raster.height {
            for x in 0..raster.width {
                if raster.pixels[(y * raster.width + x) as usize] == 1 {
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x);
                    bottom = bottom.max(y);
                }
            }
        }
        (left, top, right, bottom)
    }
    let default = dark_bounds(None);
    let four = dark_bounds(Some(4));
    let zero = dark_bounds(Some(0));
    assert_eq!(default, four, "an absent quiet zone means four modules");
    // Without a quiet zone the symbol grows to the box; with one it sits inside a margin.
    assert!(zero.0 < four.0 && zero.1 < four.1 && zero.2 > four.2 && zero.3 > four.3);
    assert!(
        zero.0 <= 10,
        "no quiet zone leaves at most the centring remainder"
    );
}
