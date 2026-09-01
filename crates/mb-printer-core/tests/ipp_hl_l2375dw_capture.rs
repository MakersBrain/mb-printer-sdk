// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sanitized, length-preserving hardware capture regression.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ipp::{
    model::{DelimiterTag, IppVersion},
    parser::IppParser,
    reader::IppReader,
};
use mb_printer_core::{
    discovery::{MutationAccess, ObservationOrigin, ProtocolFamily, TransportKind, normalize_ipp},
    ipp::{self as core_ipp, Limits, ValueData, ValueTag},
};
use sha2::{Digest, Sha256};
use std::io::Cursor;

const CAPTURE_BASE64: &str = include_str!("fixtures/hl-l2375dw-get-printer-attributes.ipp.b64");
const CAPTURE_MANIFEST: &str =
    include_str!("fixtures/hl-l2375dw-get-printer-attributes.capture.json");
const QUALIFICATION_REPORT: &str = include_str!("fixtures/hl-l2375dw-ipp-inspect-2026-08-31.json");

fn fixture() -> Vec<u8> {
    STANDARD
        .decode(CAPTURE_BASE64.split_whitespace().collect::<String>())
        .expect("checked-in capture fixture is valid base64")
}

fn attribute_count(message: &core_ipp::Message) -> usize {
    message
        .groups
        .iter()
        .map(|group| group.attributes.len())
        .sum()
}

#[test]
fn real_hl_l2375dw_capture_is_lossless_interoperable_and_normalized() {
    let bytes = fixture();
    assert_eq!(bytes.len(), 8_421);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "826e107984e84bb4bf497aad81f53bdce309834e8abeb134686f8768df2aa8b8"
    );
    for placeholder in [
        "203.0.113.15",
        "lab-printer01",
        "REDACTED-ROOM-01",
        "urn:uuid:00000000-0000-4000-8000-000000000000",
    ] {
        assert!(
            bytes
                .windows(placeholder.len())
                .any(|window| window == placeholder.as_bytes())
        );
    }
    assert!(!bytes.windows(6).any(|window| window == b"10.83."));

    let decoded = core_ipp::decode(&bytes, Limits::default()).unwrap();
    assert_eq!(decoded.version, core_ipp::Version::V1_1);
    assert_eq!(decoded.code, 0);
    assert_eq!(decoded.request_id, 104_395);
    assert_eq!(decoded.groups.len(), 2);
    assert_eq!(attribute_count(&decoded), 111);
    assert_eq!(decoded.original_bytes, bytes);
    assert_eq!(decoded.encode(Limits::default()).unwrap(), bytes);
    let geo = decoded
        .groups
        .iter()
        .flat_map(|group| &group.attributes)
        .find(|attribute| attribute.name == b"printer-geo-location")
        .unwrap();
    assert!(matches!(
        geo.values.as_slice(),
        [core_ipp::Value {
            tag: ValueTag::Unknown,
            data: ValueData::OutOfBand,
        }]
    ));
    assert!(
        decoded
            .groups
            .iter()
            .flat_map(|group| &group.attributes)
            .any(|attribute| attribute.name == b"media-size-supported"
                && attribute
                    .values
                    .iter()
                    .all(|value| matches!(value.data, ValueData::Collection(_))))
    );

    let oracle = IppParser::new(IppReader::new(Cursor::new(bytes.clone())))
        .parse()
        .unwrap();
    assert_eq!(oracle.header().version, IppVersion::v1_1());
    assert_eq!(oracle.header().operation_or_status, 0);
    assert_eq!(oracle.header().request_id, 104_395);
    let printer = oracle
        .attributes()
        .first_of(DelimiterTag::PrinterAttributes)
        .unwrap();
    assert_eq!(printer.attributes().len(), 109);
    assert!(printer.get("printer-firmware-string-version").is_some());
    assert!(printer.get("printer-geo-location").is_some());

    let snapshot = normalize_ipp(
        &decoded,
        &ObservationOrigin {
            agent_id: Some("capture-test".into()),
            printer_id: "hl-l2375dw-sanitized".into(),
            endpoint: "ipp://203.0.113.15:631/ipp/print".into(),
            endpoint_generation: 1,
            transport: TransportKind::Ipp,
            protocol: ProtocolFamily::Ipp,
            request_id: "capture-request-104395".into(),
            probe_id: None,
            observed_at: "2026-08-31T18:39:03.500581522+02:00".into(),
            qualification: None,
        },
        None,
    );
    assert_eq!(
        snapshot.identity.model.as_deref(),
        Some("Brother HL-L2375DW series")
    );
    assert_eq!(snapshot.state.state.as_deref(), Some("idle"));
    assert!(
        snapshot
            .supplies
            .iter()
            .any(|supply| { supply.id == "BK" && supply.level_percent == Some(90) })
    );
    assert!(
        snapshot.job_capabilities.iter().any(|capability| {
            capability.id == "sides" && capability.supported_values.is_some()
        })
    );
    assert!(
        snapshot
            .device_settings
            .iter()
            .any(|setting| { setting.id == "printer-firmware-string-version" })
    );
    assert!(
        snapshot
            .mutation_support
            .iter()
            .all(|support| support.access != MutationAccess::ConfirmedWrite)
    );
    assert_eq!(
        snapshot
            .observations
            .iter()
            .filter(|observation| observation.original_bytes.is_some())
            .count(),
        1
    );
}

#[test]
fn capture_provenance_is_pseudonymous_and_cannot_claim_release_qualification() {
    let manifest: serde_json::Value = serde_json::from_str(CAPTURE_MANIFEST).unwrap();
    let report: serde_json::Value = serde_json::from_str(QUALIFICATION_REPORT).unwrap();
    assert_eq!(manifest["rawCapture"]["responseBytes"], 8_421);
    assert_eq!(
        manifest["sanitizedFixture"]["decodedSha256"],
        format!("{:x}", Sha256::digest(fixture()))
    );
    assert_eq!(report["status"], "unsigned");
    assert_eq!(report["hardwareClaim"], false);
    assert_eq!(report["review"]["releaseQualification"], false);
    assert_eq!(report["result"]["configurationChanged"], false);
    assert_eq!(
        report["evidence"]["rawResponseSha256"],
        manifest["rawCapture"]["responseSha256"]
    );
    assert!(!CAPTURE_MANIFEST.contains("10.83."));
    assert!(!QUALIFICATION_REPORT.contains("10.83."));
}
