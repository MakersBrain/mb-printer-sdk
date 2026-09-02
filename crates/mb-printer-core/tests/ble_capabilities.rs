// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::capabilities::{
    self, BleSupport, BleWriteType, NotificationRequirement, PrinterDefinition,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;

const FF02: &str = "0000ff02-0000-1000-8000-00805f9b34fb";
const FF03: &str = "0000ff03-0000-1000-8000-00805f9b34fb";

// Reviewed against fixtures/hardware/matrix.json's explicit Web Bluetooth
// qualification entries. Keep this model list explicit: a shared print
// protocol is not evidence that a future model has this GATT profile.
const REVIEWED_FF02_FF03_MODELS: &[&str] = &[
    "p12", "a30", "m02", "m02-pro", "m03", "t02", "m04s-53", "m04s-80", "m04s-110", "m110",
    "m110s", "m200", "m250", "m220", "m221", "m260", "pm241", "d-series",
];

const REVIEWED_UNSUPPORTED_MODELS: &[&str] = &["ql-1110nwb", "ql-1115nwb", "ql-1100"];

fn raw_catalogue() -> Value {
    serde_json::from_str(include_str!("../data/printers.json")).unwrap()
}

#[test]
fn every_raw_catalogue_entry_explicitly_declares_ble_support() {
    for printer in raw_catalogue()["printers"].as_array().unwrap() {
        assert!(
            printer.get("ble").is_some(),
            "{} is missing an explicit ble field",
            printer["id"]
        );
    }
}

#[test]
fn omitted_ble_support_is_rejected() {
    let mut printer = raw_catalogue()["printers"][0].clone();
    printer.as_object_mut().unwrap().remove("ble");
    let error = serde_json::from_value::<PrinterDefinition>(printer).unwrap_err();
    assert!(error.to_string().contains("missing field `ble`"));
}

#[test]
fn uuid_wire_format_serializes_canonically() {
    let support: BleSupport = serde_json::from_value(json!({
        "kind": "gatt",
        "capabilities": {
            "writeCharacteristic": "0000FF02-0000-1000-8000-00805F9B34FB",
            "writeType": "without-response",
            "notification": {
                "characteristic": "0000FF03-0000-1000-8000-00805F9B34FB",
                "requirement": "optional"
            }
        }
    }))
    .unwrap();

    let encoded = serde_json::to_value(support).unwrap();
    assert_eq!(encoded["capabilities"]["writeCharacteristic"], FF02);
    assert_eq!(
        encoded["capabilities"]["notification"]["characteristic"],
        FF03
    );
}

#[test]
fn reviewed_models_have_exact_ff02_ff03_profile() {
    let reviewed: BTreeSet<String> = REVIEWED_FF02_FF03_MODELS
        .iter()
        .map(|id| (*id).to_owned())
        .collect();
    let actual: BTreeSet<_> = capabilities::bundled()
        .into_iter()
        .filter(|printer| printer.ble_gatt().is_some())
        .map(|printer| printer.id)
        .collect();
    assert_eq!(actual, reviewed);

    for id in REVIEWED_FF02_FF03_MODELS {
        let printer = capabilities::by_id(id).unwrap();
        let gatt = printer.ble_gatt().unwrap();
        assert_eq!(gatt.write_characteristic, Uuid::parse_str(FF02).unwrap());
        assert_eq!(gatt.write_type, BleWriteType::WithoutResponse);
        let notification = gatt.notification.as_ref().unwrap();
        assert_eq!(notification.characteristic, Uuid::parse_str(FF03).unwrap());
        assert_eq!(notification.requirement, NotificationRequirement::Optional);
    }
}

#[test]
fn reviewed_unsupported_models_remain_unsupported() {
    for id in REVIEWED_UNSUPPORTED_MODELS {
        let printer = capabilities::by_id(id).unwrap();
        assert_eq!(printer.ble, BleSupport::Unsupported);
        assert!(printer.ble_gatt().is_none());
    }
}
