// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "snmp")]

use std::process::Command;

#[test]
fn cli_documents_semantic_only_and_environment_credential_boundary() {
    let output = Command::new(env!("CARGO_BIN_EXE_mb-printer-snmp"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("inspect-firmware"));
    assert!(stdout.contains("read-property"));
    assert!(stdout.contains("--community-env"));
    assert!(!stdout.contains("--community "));
    assert!(!stdout.contains("--oid"));
    assert!(!stdout.contains(" set "));
}

#[test]
fn cli_rejects_an_arbitrary_oid_option_before_network_io() {
    let output = Command::new(env!("CARGO_BIN_EXE_mb-printer-snmp"))
        .args(["read-property", "--oid", "1.3.6.1.2.1.1.1.0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unknown option --oid")
    );
}
