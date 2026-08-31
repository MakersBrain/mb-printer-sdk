// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::protocol::{
    Action, ResponseValidation,
    brother::{report, wifi},
};

#[test]
fn typed_wireless_command_matches_the_python_reference() {
    let settings = wifi::WirelessSettings {
        ssid: "Café".into(),
        password: "secret".into(),
        encryption: wifi::WirelessEncryption::TkipAes,
        authentication: wifi::WirelessAuthentication::WpaPsk,
        infrastructure: true,
        wireless_direct: false,
        reboot: false,
    };
    let command = settings.command().unwrap();
    assert!(command.starts_with(wifi::PJL_HEADER));
    assert!(command.ends_with(wifi::PJL_FOOTER));
    assert!(contains(&command, b"458877:-43-61-66-c3-a9"));
    assert!(contains(&command, b"458880:8"));
    assert!(contains(&command, b"458881:3"));
    assert!(!contains(&command, b"secret"));
    assert!(!format!("{settings:?}").contains("secret"));
    assert_eq!(
        wifi::xor_password(&wifi::xor_password(b"correct horse")),
        b"correct horse"
    );
}

#[test]
fn typed_wireless_security_rejects_invalid_combinations() {
    let open = wifi::WirelessSettings {
        ssid: "Guest".into(),
        password: String::new(),
        encryption: wifi::WirelessEncryption::None,
        authentication: wifi::WirelessAuthentication::Open,
        infrastructure: true,
        wireless_direct: false,
        reboot: true,
    };
    let command = open.command().unwrap();
    assert!(!contains(&command, b"99458890"));
    assert!(!contains(&command, b"99458889.1"));
    assert!(command.ends_with(wifi::REBOOT_COMMAND));

    let invalid = wifi::WirelessSettings {
        encryption: wifi::WirelessEncryption::Aes,
        ..open.clone()
    };
    assert_eq!(
        invalid.command().unwrap_err(),
        wifi::WirelessError::InvalidOpenSecurity
    );
    let missing_password = wifi::WirelessSettings {
        authentication: wifi::WirelessAuthentication::Wpa2Only,
        encryption: wifi::WirelessEncryption::TkipAes,
        ..open
    };
    assert_eq!(
        missing_password.command().unwrap_err(),
        wifi::WirelessError::MissingPassword
    );
}

#[test]
fn wireless_read_commands_and_typed_parsers_match_captures() {
    assert_eq!(
        wifi::wifi_scan_start_command(),
        [
            wifi::PJL_HEADER,
            b"@PJL DEFAULT OBJBRNET=\"458845:31-3a\"\r\n",
            wifi::PJL_FOOTER,
        ]
        .concat()
    );
    assert_eq!(
        wifi::wifi_scan_result_command(),
        [
            wifi::PJL_HEADER,
            b"@PJL INFO AVAILABLEWLAN\r\n",
            wifi::PJL_FOOTER,
        ]
        .concat()
    );
    assert!(wifi::inquire_command("458967.2").is_ok());
    for oid in ["", ".", "1.", ".1", "1..2", "1\"\r\n@PJL RESET"] {
        assert_eq!(
            wifi::inquire_command(oid).unwrap_err(),
            wifi::WirelessError::InvalidOid
        );
    }

    let reply = b"@PJL INFO OBJBRNET\r\n\"458867 : 1\"\r\n\"458967.2:-c0-a8-01-32\"\r\n\"458877:-4d-61-6b-65-72\"\r\n\"458880:8\"\r\n\"458881:3\"\r\n\"459138.2:1\"\r\n\"459138.3:0\"\r\n";
    assert_eq!(wifi::parse_wifi_status(reply), Some(true));
    assert_eq!(
        wifi::parse_ip_address(reply).as_deref(),
        Some("192.168.1.50")
    );
    assert_eq!(
        wifi::parse_oid_value(reply, wifi::WirelessField::Ssid.oid()).as_deref(),
        Some("Maker")
    );
    assert_eq!(
        wifi::parse_encryption(reply),
        Some(wifi::WirelessEncryption::TkipAes)
    );
    assert_eq!(
        wifi::parse_authentication(reply),
        Some(wifi::WirelessAuthentication::WpaPsk)
    );
    assert_eq!(
        wifi::parse_boolean_field(reply, wifi::WirelessField::Infrastructure),
        Some(true)
    );
    assert_eq!(
        wifi::parse_boolean_field(reply, wifi::WirelessField::WirelessDirect),
        Some(false)
    );
    assert_eq!(wifi::parse_oid_value(b"\"1458867:1\"", "458867"), None);
}

#[test]
fn access_point_parser_handles_quoted_csv_and_bad_rows() {
    let points = wifi::parse_access_points(
        b"header\r\nVAP,\"-43-61-66-c3-a9\",x,x,6,-42,3,2\r\nVAP,\"Cafe, west\",x,x,11,87,0,2\r\nbad,row\r\n",
    );
    assert_eq!(points.len(), 2);
    assert_eq!(points[0].ssid, "Café");
    assert_eq!(points[0].channel, 6);
    assert!(points[0].enterprise && points[0].encrypted);
    assert_eq!(points[1].ssid, "Cafe, west");
}

#[test]
fn wireless_plans_use_bounded_collection() {
    let status = wifi::wireless_status_plan();
    assert_eq!(status.actions.len(), wifi::WirelessField::ALL.len() * 2);
    assert_eq!(
        status
            .actions
            .iter()
            .filter(|action| matches!(action, Action::CollectResponse { .. }))
            .count(),
        7
    );
    assert!(status.actions.iter().all(|action| match action {
        Action::CollectResponse {
            timeout_ms,
            idle_timeout_ms,
            maximum_bytes,
            validation,
        } =>
            *timeout_ms == 2000
                && *idle_timeout_ms == 200
                && *maximum_bytes == 4096
                && *validation == ResponseValidation::BrotherObjbrnet,
        _ => true,
    }));

    let scan = wifi::wireless_scan_plan();
    assert!(matches!(
        scan.actions.last(),
        Some(Action::CollectResponse {
            timeout_ms: 8000,
            idle_timeout_ms: 300,
            maximum_bytes: 16384,
            validation: ResponseValidation::BrotherWifiScan,
        })
    ));
}

#[test]
fn system_report_command_parser_and_redaction_are_bounded() {
    let fixture = include_bytes!("fixtures/brother-system-report.txt");
    let mut response = b"\x00\x12".to_vec();
    response.extend_from_slice(fixture);
    let parsed = report::parse_system_report(&response).unwrap();
    assert_eq!(parsed.sections["Printer"]["Printer"], "QL-1110NWB");
    assert_eq!(parsed.sections["Printer"]["ProgVer"], "V2.13");
    assert_eq!(parsed.sections["WLAN"]["IP Address"], "192.0.2.7");

    let redacted = parsed.redacted();
    let json = serde_json::to_string(&redacted).unwrap();
    let debug = format!("{parsed:?}");
    for secret in [
        "QL-SECRET",
        "private-net",
        "192.0.2.7",
        "192.0.2.1",
        "00:11:22:33:44:55",
    ] {
        assert!(!json.contains(secret));
        assert!(!debug.contains(secret));
    }
    assert_eq!(redacted.sections["Printer"]["Serial No."], report::REDACTED);
    assert_eq!(redacted.sections["Printer"]["ProgVer"], "V2.13");
}

#[test]
fn system_report_rejects_unrelated_and_oversized_responses() {
    assert_eq!(
        report::parse_system_report(b"ordinary printer reply").unwrap_err(),
        report::SystemReportError::MissingMarker
    );
    assert_eq!(
        report::parse_system_report(&vec![0; report::MAX_SYSTEM_REPORT_BYTES + 1]).unwrap_err(),
        report::SystemReportError::TooLarge
    );

    let plan = report::system_report_plan();
    assert!(matches!(
        &plan.actions[0],
        Action::CommandWrite { bytes, .. } if bytes == report::SYSTEM_REPORT_COMMAND
    ));
    assert!(matches!(
        &plan.actions[1],
        Action::CollectResponse {
            timeout_ms: 5000,
            idle_timeout_ms: 300,
            maximum_bytes: report::MAX_SYSTEM_REPORT_BYTES,
            validation: ResponseValidation::BrotherSystemReport,
        }
    ));
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
