// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local semantic SNMP inspection CLI. Credentials are read from an
//! environment variable and are never accepted as command-line arguments.

use mb_printer_core::snmp::{DeviceQualification, ObjectKey};
use mb_printer_native::{
    snmp_properties::{PrinterClient, QualifiedPrinter},
    transports::snmp::{ClientLimits, Community, Credentials},
};
use std::{collections::BTreeMap, env, net::SocketAddr, process::ExitCode};

const HELP: &str = "\
Semantic, allowlisted SNMP access for qualified Brother printers

Usage:
  mb-printer-snmp inspect-firmware --endpoint IP:PORT --model MODEL --qualification-id ID [options]
  mb-printer-snmp read-property --endpoint IP:PORT --model MODEL --qualification-id ID --property ID [options]

Options:
  --manufacturer NAME       Device manufacturer (default: Brother)
  --printer-id ID           Stable local printer identity (default: endpoint)
  --endpoint-generation N   Endpoint generation (default: 0)
  --firmware VERSION        Firmware bound to the qualification
  --community-env NAME      Credential environment variable (default: MB_PRINTER_SNMP_COMMUNITY)
  -h, --help                Show this help

Only semantic properties from the compiled catalogue are accepted. There is
no arbitrary OID or SET command. The community is never included in output.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mb-printer-snmp: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let Some(command) = arguments.next() else {
        print!("{HELP}");
        return Ok(());
    };
    if command == "--help" || command == "-h" {
        print!("{HELP}");
        return Ok(());
    }
    if command != "inspect-firmware" && command != "read-property" {
        return Err(format!("unknown command {command:?}; use --help"));
    }
    let options = parse_options(arguments)?;
    let endpoint = required(&options, "endpoint")?
        .parse::<SocketAddr>()
        .map_err(|_| {
            "--endpoint must be an IP address and port, for example 192.0.2.10:161".to_owned()
        })?;
    let model = required(&options, "model")?.to_owned();
    let qualification_id = required(&options, "qualification-id")?.to_owned();
    let manufacturer = options
        .get("manufacturer")
        .cloned()
        .unwrap_or_else(|| "Brother".into());
    let printer_id = options
        .get("printer-id")
        .cloned()
        .unwrap_or_else(|| endpoint.to_string());
    let endpoint_generation = options.get("endpoint-generation").map_or(Ok(0), |value| {
        value
            .parse::<u64>()
            .map_err(|_| "--endpoint-generation must be an unsigned integer".to_owned())
    })?;
    let firmware = options.get("firmware").cloned();
    let community_variable = options
        .get("community-env")
        .map(String::as_str)
        .unwrap_or("MB_PRINTER_SNMP_COMMUNITY");
    let community = env::var_os(community_variable).ok_or_else(|| {
        format!("credential environment variable {community_variable} is not set")
    })?;
    let community =
        Community::new(community.as_encoded_bytes().to_vec()).map_err(|error| error.to_string())?;
    let qualification = DeviceQualification {
        manufacturer: manufacturer.clone(),
        models: vec![model.clone()],
        firmware: firmware.clone(),
        qualification_id,
    };
    let client = PrinterClient::brother_mfp(qualification.clone(), ClientLimits::default())
        .map_err(|error| error.to_string())?;
    let printer = QualifiedPrinter {
        printer_id,
        endpoint,
        endpoint_generation,
        manufacturer,
        model,
        firmware,
        qualification,
        credentials: Credentials::V2c(community),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let output = if command == "inspect-firmware" {
        let inventory = runtime
            .block_on(client.inspect_firmware(&printer))
            .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&inventory).map_err(|error| error.to_string())?
    } else {
        let property = ObjectKey::new(required(&options, "property")?.to_owned())
            .map_err(|error| error.to_string())?;
        let observation = runtime
            .block_on(client.read_property(&printer, &property))
            .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&observation).map_err(|error| error.to_string())?
    };
    println!("{output}");
    Ok(())
}

fn parse_options(
    arguments: impl Iterator<Item = String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut arguments = arguments.peekable();
    let mut options = BTreeMap::new();
    while let Some(argument) = arguments.next() {
        let name = argument
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected positional argument {argument:?}"))?;
        if !matches!(
            name,
            "endpoint"
                | "model"
                | "qualification-id"
                | "manufacturer"
                | "printer-id"
                | "endpoint-generation"
                | "firmware"
                | "community-env"
                | "property"
        ) {
            return Err(format!("unknown option --{name}"));
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("--{name} requires a value"))?;
        if value.starts_with("--") {
            return Err(format!("--{name} requires a value"));
        }
        if options.insert(name.to_owned(), value).is_some() {
            return Err(format!("--{name} was supplied more than once"));
        }
    }
    Ok(options)
}

fn required<'a>(options: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, String> {
    options
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("--{name} is required"))
}
