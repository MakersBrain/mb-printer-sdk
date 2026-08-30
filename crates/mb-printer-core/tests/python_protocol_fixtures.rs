// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    capabilities::{self, Protocol},
    protocol,
    raster::{Fit, MonoRaster, Rotation},
};
use serde_json::{Value, json};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn unpack(width_bytes: u16, height: u32, bytes: &[u8]) -> MonoRaster {
    let width = u32::from(width_bytes) * 8;
    let mut pixels = vec![0; width as usize * height as usize];
    for y in 0..height {
        for x in 0..width {
            pixels[(y * width + x) as usize] =
                (bytes[(y * u32::from(width_bytes) + x / 8) as usize] >> (7 - x % 8)) & 1;
        }
    }
    MonoRaster {
        width,
        height,
        pixels,
    }
}

fn observable(actions: &[protocol::Action], include_waits: bool) -> Vec<Value> {
    let mut result = Vec::new();
    for action in actions {
        match action {
            protocol::Action::CommandWrite { bytes, .. } => {
                result.push(json!({"action":"write", "hex":hex(bytes)}));
            }
            protocol::Action::RasterWrite {
                bytes,
                logical_chunk,
                delay_after_each_physical_write_ms,
            } => {
                for chunk in bytes.chunks(*logical_chunk) {
                    result.push(json!({"action":"write", "hex":hex(chunk)}));
                    result.push(json!({
                        "action":"delay",
                        "milliseconds":delay_after_each_physical_write_ms
                    }));
                }
            }
            protocol::Action::Delay { milliseconds } => {
                result.push(json!({"action":"delay", "milliseconds":milliseconds}));
            }
            protocol::Action::WaitForResponse { timeout_ms, .. } => {
                if include_waits {
                    result.push(json!({"action":"wait", "timeoutMs":timeout_ms}));
                }
            }
            protocol::Action::JobBoundary { .. } | protocol::Action::SubscribeNotifications => {}
        }
    }
    result
}

#[test]
fn every_plan_matches_executed_python_actions() {
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/protocol/python-actions.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let model = case["model"].as_str().unwrap();
        let printer = capabilities::by_id(model).unwrap();
        let values = &case["options"];
        let options = protocol::Options {
            density: values["density"].as_u64().unwrap() as u8,
            feed: values["feed"].as_u64().unwrap() as u8,
            continuous: values["continuous"].as_bool().unwrap(),
            speed: values["speed"].as_u64().unwrap() as u8,
            copies: values["copies"].as_u64().unwrap() as u16,
            gap_tenths_mm: values["gapTenthsMm"].as_i64().unwrap() as i16,
            offset_tenths_mm: values["offsetTenthsMm"].as_i64().unwrap() as i16,
            label_width_tenths_mm: Some(values["labelWidthTenthsMm"].as_u64().unwrap() as u16),
            label_height_tenths_mm: Some(values["labelHeightTenthsMm"].as_u64().unwrap() as u16),
            offset_x: values["offsetX"].as_u64().unwrap() as u16,
            offset_y: values["offsetY"].as_u64().unwrap() as u16,
            brother_media: values["brotherMedia"]
                .as_object()
                .map(|media| protocol::BrotherMedia {
                    width_mm: media["widthMm"].as_u64().unwrap() as u8,
                    length_mm: media["lengthMm"].as_u64().unwrap() as u8,
                    continuous: media["continuous"].as_bool().unwrap(),
                    feed_margin: media["feedMargin"].as_u64().unwrap() as u16,
                }),
            cut: values["cut"].as_bool().unwrap(),
            cut_every: values["cutEvery"].as_u64().unwrap() as u8,
            compress: values["compress"].as_bool().unwrap(),
            high_quality: values["highQuality"].as_bool().unwrap(),
            // The reference fixtures predate both and describe a paced, uncompressed job.
            streaming: false,
            lzo: false,
        };
        let input = &case["inputRaster"];
        let input_bytes = decode_hex(input["hex"].as_str().unwrap());
        let mut prepared = unpack(
            input["widthBytes"].as_u64().unwrap() as u16,
            input["height"].as_u64().unwrap() as u32,
            &input_bytes,
        );
        if printer.rotated {
            prepared = prepared.rotate(Rotation::Clockwise90);
        }
        if printer.protocol == Protocol::Brother {
            let head = printer.width_px().unwrap();
            let right_margin = values["brotherRightMarginDots"].as_u64().unwrap() as u32;
            let left = head - prepared.width - right_margin;
            prepared = prepared
                .place_on_head(head, Fit::Left, left as i32, 0)
                .unwrap();
        } else if printer.protocol != Protocol::Tspl {
            let alignment = match values["alignment"].as_str().unwrap() {
                "left" => Fit::Left,
                "center" => Fit::Center,
                "right" => Fit::Right,
                other => panic!("invalid fixture alignment: {other}"),
            };
            let head = printer.width_px().unwrap_or(prepared.width);
            prepared = prepared
                .place_on_head_byte_aligned(
                    head,
                    alignment,
                    options.offset_x.into(),
                    options.offset_y.into(),
                )
                .unwrap();
        }
        let prepared_fixture = &case["preparedRaster"];
        let prepared_bytes = prepared.pack_msb().unwrap();
        assert_eq!(
            (
                prepared.width.div_ceil(8),
                prepared.height,
                hex(&prepared_bytes)
            ),
            (
                prepared_fixture["widthBytes"].as_u64().unwrap() as u32,
                prepared_fixture["height"].as_u64().unwrap() as u32,
                prepared_fixture["hex"].as_str().unwrap().to_owned()
            ),
            "Python placement/rotation divergence for {model}/{}",
            case["profile"].as_str().unwrap_or("legacy")
        );
        let raster = protocol::Raster {
            width_bytes: prepared.width.div_ceil(8) as u16,
            height: prepared.height,
            data: prepared_bytes,
        };
        let plan = protocol::plan(&printer, &raster, &options).unwrap();
        assert_eq!(
            observable(&plan.actions, printer.protocol != Protocol::Brother),
            *case["actions"].as_array().unwrap(),
            "Python action divergence for {model}/{}",
            case["profile"].as_str().unwrap_or("legacy")
        );
    }
}

#[test]
fn brother_status_wait_is_an_explicit_stricter_rust_policy() {
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/protocol/python-actions.json")).unwrap();
    let divergences = fixture["provenance"]["knownDivergences"]
        .as_array()
        .unwrap();
    assert_eq!(divergences.len(), 1);
    assert!(
        divergences[0]
            .as_str()
            .unwrap()
            .contains("Rust waits for and validates")
    );
    let brother = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["protocol"] == "brother")
        .unwrap();
    assert!(
        brother["actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| action["action"] != "wait")
    );
    let printer = capabilities::by_id(brother["model"].as_str().unwrap()).unwrap();
    let prepared = &brother["preparedRaster"];
    let raster = protocol::Raster {
        width_bytes: prepared["widthBytes"].as_u64().unwrap() as u16,
        height: prepared["height"].as_u64().unwrap() as u32,
        data: decode_hex(prepared["hex"].as_str().unwrap()),
    };
    let options = protocol::Options {
        brother_media: Some(protocol::BrotherMedia {
            width_mm: 29,
            length_mm: 42,
            continuous: false,
            feed_margin: 0,
        }),
        ..Default::default()
    };
    assert!(
        protocol::plan(&printer, &raster, &options)
            .unwrap()
            .actions
            .iter()
            .any(|action| matches!(
                action,
                protocol::Action::WaitForResponse {
                    validation: protocol::ResponseValidation::BrotherStatus32,
                    ..
                }
            ))
    );
}
