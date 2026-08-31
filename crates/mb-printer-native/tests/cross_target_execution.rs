// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    capabilities,
    protocol::{self, Action},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RasterFixture {
    width_bytes: u16,
    height: u32,
    pattern: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    raster: RasterFixture,
    payloads: Vec<usize>,
    models: Vec<String>,
    expected_sha256: BTreeMap<String, String>,
}

fn capture(actions: &[Action], payload: usize) -> Vec<String> {
    let mut events = vec![];
    for action in actions {
        match action {
            Action::JobBoundary { kind } => events.push(format!("b:{kind:?}")),
            Action::SubscribeNotifications => events.push("s".into()),
            Action::Delay { milliseconds } => events.push(format!("d:{milliseconds}")),
            Action::CommandWrite { bytes, .. } => events.push(format!("w:{}", hex(bytes))),
            Action::RasterWrite {
                bytes,
                logical_chunk,
                delay_after_each_physical_write_ms,
            } => {
                for logical in bytes.chunks(*logical_chunk) {
                    for physical in logical.chunks(payload) {
                        events.push(format!("w:{}", hex(physical)));
                        events.push(format!("d:{delay_after_each_physical_write_ms}"));
                    }
                }
            }
            Action::WaitForResponse {
                timeout_ms,
                fallback_delay_ms,
                validation,
            } => events.push(format!("q:{timeout_ms}:{fallback_delay_ms}:{validation:?}")),
            Action::CollectResponse {
                timeout_ms,
                idle_timeout_ms,
                maximum_bytes,
                validation,
            } => events.push(format!(
                "c:{timeout_ms}:{idle_timeout_ms}:{maximum_bytes}:{validation:?}"
            )),
        }
    }
    events
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn all_eight_families_match_the_shared_physical_matrix() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("fixtures/execution-contract.json")).unwrap();
    assert_eq!(fixture.raster.pattern, "incrementing-mod-256");
    let length = usize::from(fixture.raster.width_bytes) * fixture.raster.height as usize;
    let raster = protocol::Raster {
        width_bytes: fixture.raster.width_bytes,
        height: fixture.raster.height,
        data: (0..length).map(|value| value as u8).collect(),
    };
    let mut actual = BTreeMap::new();
    for model in &fixture.models {
        let printer = capabilities::by_id(model).unwrap();
        let options = protocol::Options {
            brother_media: (printer.protocol == capabilities::Protocol::Brother).then_some(
                protocol::BrotherMedia {
                    width_mm: 62,
                    length_mm: 29,
                    continuous: false,
                    feed_margin: 0,
                },
            ),
            ..Default::default()
        };
        let plan = protocol::plan(&printer, &raster, &options).unwrap();
        for payload in &fixture.payloads {
            let events = capture(&plan.actions, *payload);
            let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&events).unwrap()));
            actual.insert(format!("{model}@{payload}"), digest);
        }
    }
    if fixture.expected_sha256.is_empty() {
        panic!("{}", serde_json::to_string_pretty(&actual).unwrap());
    }
    assert_eq!(actual, fixture.expected_sha256);
}
