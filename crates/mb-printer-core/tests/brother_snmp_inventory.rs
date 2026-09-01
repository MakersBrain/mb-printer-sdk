// SPDX-License-Identifier: AGPL-3.0-or-later

use mb_printer_core::{
    providers::brother::snmp::{FieldResult, mfp_read_catalogue, parse_mfp_inventory},
    snmp::{DeviceQualification, ObjectAccess, ObjectId, ObjectValue, VarBind},
};

fn binding(index: usize, value: &str) -> VarBind {
    VarBind {
        oid: ObjectId::parse(&format!("1.3.6.1.4.1.2435.2.4.3.99.3.1.6.1.2.{index}")).unwrap(),
        value: ObjectValue::Bytes(value.as_bytes().to_vec()),
    }
}

#[test]
fn production_mfp_catalogue_is_frozen_and_read_only() {
    let registry = mfp_read_catalogue(DeviceQualification {
        manufacturer: "Brother".into(),
        models: vec!["HL-L2375DW".into()],
        firmware: None,
        qualification_id: "fixture".into(),
    })
    .unwrap();
    assert_eq!(registry.definitions().count(), 24);
    assert!(
        registry
            .definitions()
            .all(|definition| definition.access == ObjectAccess::ReadOnly)
    );
}

#[test]
fn parses_bounded_firmware_record_sequence() {
    let inventory = parse_mfp_inventory(
        &[
            binding(1, "MODEL=\"HL-L2375DW\""),
            binding(2, "SPEC=\"A\""),
            binding(3, "FIRMID=\"FIRM\""),
            binding(4, "FIRMVER=\"1.72\""),
            binding(5, ""),
            binding(6, "FIRMID=\"IGNORED\""),
        ],
        |_| Vec::new(),
    );
    assert!(matches!(inventory.update_model, FieldResult::Observed(_)));
    let FieldResult::Observed(components) = inventory.components else {
        panic!("components should be observed");
    };
    assert_eq!(components.value.len(), 1);
    assert_eq!(components.value[0].id, "FIRM");
    assert_eq!(components.value[0].version, "1.72");
}

#[test]
fn unequal_component_pairs_fail_closed() {
    let inventory = parse_mfp_inventory(&[binding(1, "FIRMID=\"FIRM\""), binding(2, "")], |_| {
        Vec::new()
    });
    assert!(matches!(
        inventory.components,
        FieldResult::Malformed { .. }
    ));
}

#[test]
fn out_of_order_instances_fail_closed() {
    let inventory = parse_mfp_inventory(&[binding(2, "MODEL=\"HL-L2375DW\"")], |_| Vec::new());
    assert!(matches!(
        inventory.components,
        FieldResult::Malformed { .. }
    ));
}

#[test]
fn conflicting_model_records_are_preserved() {
    let inventory = parse_mfp_inventory(
        &[
            binding(1, "MODEL=\"HL-L2375DW\""),
            binding(2, "MODEL=\"MFC-L3770CDW\""),
            binding(3, ""),
        ],
        |_| Vec::new(),
    );
    let FieldResult::Conflict { observations } = inventory.update_model else {
        panic!("conflicting models should not be silently selected");
    };
    assert_eq!(observations.len(), 2);
}

#[test]
fn unknown_records_are_diagnostic_and_missing_instances_are_partial() {
    let inventory = parse_mfp_inventory(
        &[
            binding(1, "VENDORKEY=\"retained\""),
            VarBind {
                oid: ObjectId::parse("1.3.6.1.4.1.2435.2.4.3.99.3.1.6.1.2.2").unwrap(),
                value: ObjectValue::NoSuchInstance,
            },
        ],
        |_| Vec::new(),
    );
    assert_eq!(inventory.diagnostic_records[0].value, "VENDORKEY=retained");
    assert!(matches!(inventory.components, FieldResult::Missing));
}
