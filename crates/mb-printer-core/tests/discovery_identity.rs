// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::discovery::{MergeDecision, MergeReason, PrinterIdentity, reconcile_identity};

fn identity(uuid: Option<&str>, serial: Option<&str>, device_id: Option<&str>) -> PrinterIdentity {
    PrinterIdentity {
        printer_id: "published-id-is-not-identity-evidence".into(),
        uuid: uuid.map(str::to_owned),
        serial_number: serial.map(str::to_owned),
        device_id: device_id.map(str::to_owned),
        manufacturer: Some("Brother".into()),
        model: Some("HL-L2375DW".into()),
    }
}

#[test]
fn matching_uuid_or_compatible_device_identity_can_merge() {
    assert_eq!(
        reconcile_identity(
            &identity(Some("urn:uuid:1"), None, None),
            &identity(Some("urn:uuid:1"), None, None),
            false
        ),
        MergeDecision::Merge(MergeReason::MatchingUuid)
    );
    assert_eq!(
        reconcile_identity(
            &identity(None, Some("serial-1"), Some("device-1")),
            &identity(None, Some("serial-1"), Some("device-1")),
            false
        ),
        MergeDecision::Merge(MergeReason::CompatibleSerialOrDeviceId)
    );
}

#[test]
fn strong_conflicts_never_merge_automatically() {
    assert_eq!(
        reconcile_identity(
            &identity(Some("urn:uuid:1"), Some("serial-1"), None),
            &identity(Some("urn:uuid:2"), Some("serial-1"), None),
            false
        ),
        MergeDecision::IdentityConflict
    );
    assert_eq!(
        reconcile_identity(
            &identity(None, Some("serial-1"), None),
            &identity(None, Some("serial-2"), None),
            false
        ),
        MergeDecision::IdentityConflict
    );
}

#[test]
fn model_only_observations_require_explicit_user_association() {
    let left = identity(None, None, None);
    let right = identity(None, None, None);
    assert_eq!(
        reconcile_identity(&left, &right, false),
        MergeDecision::RequiresUserAssociation
    );
    assert_eq!(
        reconcile_identity(&left, &right, true),
        MergeDecision::Merge(MergeReason::ExplicitUserAssociation)
    );
}
