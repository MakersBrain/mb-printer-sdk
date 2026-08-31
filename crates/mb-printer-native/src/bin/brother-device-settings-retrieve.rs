// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only Brother Printer Setting Tool device-settings retrieval.

use std::{
    env,
    net::{SocketAddr, ToSocketAddrs},
};

use mb_printer_native::{
    brother_device_settings::{
        BrotherModelProfile, DeviceSettingsInspection, MODEL_PROFILES, model_profile,
        retrieve_device_settings,
    },
    transports::TcpTransport,
};
use serde_json::{Value, json};

const COMMAND_LIMIT: usize = 4 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024;
const USB_VENDOR_BROTHER: u16 = 0x04f9;

#[derive(Debug)]
struct Options {
    target: Target,
    raw: bool,
}

#[derive(Debug)]
enum Target {
    ListModels,
    Tcp {
        profile: &'static BrotherModelProfile,
        host: String,
    },
    Usb {
        profile: &'static BrotherModelProfile,
        selector: Option<(u8, u8)>,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let options = parse_args(env::args().skip(1))?;
    if matches!(options.target, Target::ListModels) {
        for profile in MODEL_PROFILES {
            println!(
                "{}\t{}\t{:02x}/{:02x}",
                profile.id, profile.display_name, profile.series_code, profile.model_code
            );
        }
        return Ok(());
    }

    let (transport, endpoint, profile, inspection) = match options.target {
        Target::Tcp { profile, host } => retrieve_tcp(profile, &host)?,
        Target::Usb { profile, selector } => retrieve_usb(profile, selector)?,
        Target::ListModels => unreachable!(),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&render(
            &transport,
            &endpoint,
            profile,
            &inspection,
            options.raw,
        ))
        .map_err(|error| error.to_string())?
    );
    Ok(())
}

fn retrieve_tcp(
    profile: &'static BrotherModelProfile,
    host: &str,
) -> Result<
    (
        String,
        String,
        &'static BrotherModelProfile,
        DeviceSettingsInspection,
    ),
    String,
> {
    let target = if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:9100")
    };
    let address = resolve_one(&target)?;
    let mut transport = TcpTransport::connect(address, COMMAND_LIMIT, RESPONSE_LIMIT)?;
    let inspection = retrieve_device_settings(&mut transport, profile);
    Ok(("raw-tcp".into(), address.to_string(), profile, inspection))
}

fn resolve_one(target: &str) -> Result<SocketAddr, String> {
    target
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve {target}: {error}"))?
        .next()
        .ok_or_else(|| format!("{target} resolved to no address"))
}

#[cfg(feature = "usb")]
fn retrieve_usb(
    profile: &'static BrotherModelProfile,
    selector: Option<(u8, u8)>,
) -> Result<
    (
        String,
        String,
        &'static BrotherModelProfile,
        DeviceSettingsInspection,
    ),
    String,
> {
    use mb_printer_native::transports::usb;

    let candidates = usb::discover_rusb_bulk()?
        .into_iter()
        .filter(|candidate| {
            candidate.identity.vendor_id == USB_VENDOR_BROTHER
                && candidate.in_endpoint.is_some()
                && selector.is_none_or(|(bus, address)| {
                    candidate.identity.bus == bus && candidate.identity.address == address
                })
        })
        .collect::<Vec<_>>();
    let mut identities = candidates
        .iter()
        .map(|candidate| candidate.identity)
        .collect::<Vec<_>>();
    identities.sort_by_key(|identity| (identity.bus, identity.address));
    identities.dedup();
    if identities.is_empty() {
        return Err("no matching Brother USB device with bulk IN/OUT endpoints was found".into());
    }
    if identities.len() != 1 {
        let choices = identities
            .iter()
            .map(|identity| format!("{}:{}", identity.bus, identity.address))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "multiple Brother USB devices found; select one: {choices}"
        ));
    }
    let identity = identities[0];
    let candidate = usb::select_bulk_candidate(&candidates, identity)
        .ok_or("selected USB device has no usable bulk interface")?;
    let endpoint = format!(
        "{:04x}:{:04x}@{}:{}",
        identity.vendor_id, identity.product_id, identity.bus, identity.address
    );
    let mut transport = usb::open_rusb_with_limits(
        candidate,
        usize::from(candidate.max_packet_size),
        COMMAND_LIMIT,
        RESPONSE_LIMIT,
        3_000,
    )?;
    let inspection = retrieve_device_settings(&mut transport, profile);
    Ok(("usb".into(), endpoint, profile, inspection))
}

#[cfg(not(feature = "usb"))]
fn retrieve_usb(
    _: &'static BrotherModelProfile,
    _: Option<(u8, u8)>,
) -> Result<
    (
        String,
        String,
        &'static BrotherModelProfile,
        DeviceSettingsInspection,
    ),
    String,
> {
    let _ = USB_VENDOR_BROTHER;
    Err("USB support is disabled; rebuild with --features brother-tools,usb".into())
}

fn render(
    transport: &str,
    endpoint: &str,
    profile: &BrotherModelProfile,
    inspection: &DeviceSettingsInspection,
    raw: bool,
) -> Value {
    let settings = inspection
        .observations
        .iter()
        .map(|observation| {
            let mut value = json!({
                "id": observation.id,
                "value": observation.value,
                "error": observation.error,
            });
            if raw {
                value["rawResponseHex"] = observation
                    .raw_response
                    .as_deref()
                    .map(hex)
                    .map(Value::String)
                    .unwrap_or(Value::Null);
            }
            value
        })
        .collect::<Vec<_>>();
    let mut output = json!({
        "schemaVersion": 1,
        "operation": "brother-device-settings-retrieve",
        "readOnly": true,
        "transport": transport,
        "endpoint": endpoint,
        "model": {
            "id": profile.id,
            "name": profile.display_name,
            "expectedSeriesCode": profile.series_code,
            "expectedModelCode": profile.model_code,
        },
        "settings": settings,
        "error": inspection.error,
        "notes": [
            "The status identity must match the selected model before settings mode is entered.",
            "Only read commands confirmed in the compared Brother native executables were sent.",
            "No setting mutation or credential retrieval was attempted."
        ]
    });
    if raw {
        output["identityResponseHex"] = inspection
            .identity_response
            .as_deref()
            .map(hex)
            .map(Value::String)
            .unwrap_or(Value::Null);
    }
    output
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_args(arguments: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    for argument in &arguments {
        if argument.starts_with("--") && argument != "--raw" && argument != "--help" {
            return Err(format!("unknown option {argument}\n{}", usage()));
        }
    }
    let positional = arguments
        .iter()
        .filter(|argument| !argument.starts_with("--"))
        .collect::<Vec<_>>();
    let target = match positional.as_slice() {
        [command] if command.as_str() == "list-models" => Target::ListModels,
        [kind, model, host] if kind.as_str() == "tcp" => Target::Tcp {
            profile: parse_model(model)?,
            host: host.to_string(),
        },
        [kind, model] if kind.as_str() == "usb" => Target::Usb {
            profile: parse_model(model)?,
            selector: None,
        },
        [kind, model, selector] if kind.as_str() == "usb" => Target::Usb {
            profile: parse_model(model)?,
            selector: Some(parse_usb_selector(selector)?),
        },
        _ => return Err(usage()),
    };
    Ok(Options {
        target,
        raw: arguments.iter().any(|value| value == "--raw"),
    })
}

fn parse_model(value: &str) -> Result<&'static BrotherModelProfile, String> {
    model_profile(value).ok_or_else(|| {
        format!("unsupported model {value}; run brother-device-settings-retrieve list-models")
    })
}

fn parse_usb_selector(value: &str) -> Result<(u8, u8), String> {
    let (bus, address) = value
        .split_once(':')
        .ok_or("USB selector must be BUS:ADDRESS")?;
    Ok((
        bus.parse().map_err(|_| "USB bus must be 0..255")?,
        address.parse().map_err(|_| "USB address must be 0..255")?,
    ))
}

fn usage() -> String {
    "usage:\n  brother-device-settings-retrieve list-models\n  brother-device-settings-retrieve tcp MODEL HOST[:PORT] [--raw]\n  brother-device-settings-retrieve usb MODEL [BUS:ADDRESS] [--raw]".into()
}
