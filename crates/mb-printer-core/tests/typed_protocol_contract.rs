// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{capabilities, protocol};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    protocols: Vec<Contract>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Contract {
    protocol: String,
    model: String,
    raster_chunk: usize,
    raster_delay_ms: u64,
    subscribe_count: usize,
    wait_count: usize,
    wait_timeout_ms: u64,
    fallback_delay_ms: u64,
}

#[test]
fn every_python_family_has_a_frozen_typed_action_contract() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("../fixtures/protocol/typed-contract.json")).unwrap();
    assert_eq!(fixture.protocols.len(), 8);
    for contract in fixture.protocols {
        let printer = capabilities::by_id(&contract.model).unwrap();
        assert_eq!(
            serde_json::to_value(printer.protocol).unwrap(),
            contract.protocol
        );
        let width = printer.width_bytes.unwrap_or(8);
        let raster = protocol::Raster {
            width_bytes: width,
            height: 2,
            data: vec![0x55; usize::from(width) * 2],
        };
        let mut options = protocol::Options::default();
        if printer.protocol == capabilities::Protocol::Brother {
            options.brother_media = Some(protocol::BrotherMedia {
                width_mm: 62,
                length_mm: 29,
                continuous: false,
                feed_margin: 0,
            });
        }
        let plan = protocol::plan(&printer, &raster, &options).unwrap();
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
        ));
        assert!(plan.actions.iter().all(|action| !matches!(
            action,
            protocol::Action::CommandWrite {
                name,
                atomic: false,
                ..
            } if !name.is_empty()
        )));
        let raster_action = plan.actions.iter().find_map(|action| match action {
            protocol::Action::RasterWrite {
                logical_chunk,
                delay_after_each_physical_write_ms,
                ..
            } => Some((*logical_chunk, *delay_after_each_physical_write_ms)),
            _ => None,
        });
        assert_eq!(
            raster_action,
            Some((contract.raster_chunk, contract.raster_delay_ms)),
            "{}",
            contract.protocol
        );
        assert_eq!(
            plan.actions
                .iter()
                .filter(|action| matches!(action, protocol::Action::SubscribeNotifications))
                .count(),
            contract.subscribe_count
        );
        let waits = plan
            .actions
            .iter()
            .filter_map(|action| match action {
                protocol::Action::WaitForResponse {
                    timeout_ms,
                    fallback_delay_ms,
                    ..
                } => Some((*timeout_ms, *fallback_delay_ms)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(waits.len(), contract.wait_count);
        assert!(
            waits
                .iter()
                .all(|wait| { *wait == (contract.wait_timeout_ms, contract.fallback_delay_ms) })
        );
        if contract.subscribe_count > 0 {
            let subscribe = plan
                .actions
                .iter()
                .position(|action| matches!(action, protocol::Action::SubscribeNotifications))
                .unwrap();
            let first_write = plan
                .actions
                .iter()
                .position(|action| matches!(action, protocol::Action::CommandWrite { .. }))
                .unwrap();
            assert!(subscribe < first_write);
        }
    }
}
