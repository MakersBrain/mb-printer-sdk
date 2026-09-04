// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only Brother Printer Setting Tool-style wireless retrieval.

use std::{
    env,
    net::{SocketAddr, ToSocketAddrs},
};

use mb_printer_native::{
    brother_settings::{
        BrotherSettingsInspection, retrieve_wireless_setting, retrieve_wireless_settings_with,
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
    show_sensitive: bool,
    raw: bool,
}

#[derive(Debug)]
enum Target {
    Tcp(String),
    Usb(Option<(u8, u8)>),
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), String> {
    let options = parse_args(env::args().skip(1))?;
    if options.raw && !options.show_sensitive {
        return Err("--raw requires the separate --show-sensitive opt-in".into());
    }

    let (transport_name, endpoint, inspection) = match &options.target {
        Target::Tcp(target) => retrieve_tcp(target).await?,
        Target::Usb(selector) => retrieve_usb(*selector).await?,
    };
    let output = render(
        &transport_name,
        &endpoint,
        &inspection,
        options.show_sensitive,
        options.raw,
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())?
    );
    Ok(())
}

async fn retrieve_tcp(target: &str) -> Result<(String, String, BrotherSettingsInspection), String> {
    let target = if target.contains(':') {
        target.to_owned()
    } else {
        format!("{target}:9100")
    };
    let address = resolve_one(&target)?;
    let mut observations = Vec::new();
    for field in mb_printer_core::protocol::brother::wifi::WirelessField::ALL {
        let mut transport = TcpTransport::connect(address, COMMAND_LIMIT, RESPONSE_LIMIT)
            .await
            .map_err(|error| error.to_string())?;
        observations.push(retrieve_wireless_setting(&mut transport, field).await);
    }
    let inspection = BrotherSettingsInspection { observations };
    Ok(("raw-tcp".into(), address.to_string(), inspection))
}

fn resolve_one(target: &str) -> Result<SocketAddr, String> {
    target
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve {target}: {error}"))?
        .next()
        .ok_or_else(|| format!("{target} resolved to no address"))
}

#[cfg(feature = "usb")]
async fn retrieve_usb(
    selector: Option<(u8, u8)>,
) -> Result<(String, String, BrotherSettingsInspection), String> {
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
    let inspection = retrieve_wireless_settings_with(|| {
        usb::open_rusb_with_limits(
            candidate,
            usize::from(candidate.max_packet_size),
            COMMAND_LIMIT,
            RESPONSE_LIMIT,
            3_000,
        )
    })
    .await;
    Ok(("usb".into(), endpoint, inspection))
}

#[cfg(not(feature = "usb"))]
async fn retrieve_usb(
    _: Option<(u8, u8)>,
) -> Result<(String, String, BrotherSettingsInspection), String> {
    let _ = USB_VENDOR_BROTHER;
    Err("USB support is disabled; rebuild with --features brother-tools,usb".into())
}

fn render(
    transport: &str,
    endpoint: &str,
    inspection: &BrotherSettingsInspection,
    show_sensitive: bool,
    raw: bool,
) -> Value {
    let settings = inspection
        .observations
        .iter()
        .map(|observation| {
            let value = if observation.sensitive && !show_sensitive {
                json!({ "redacted": true })
            } else {
                observation
                    .value
                    .as_ref()
                    .map(|value| serde_json::to_value(value).expect("SettingValue serializes"))
                    .unwrap_or(Value::Null)
            };
            let mut item = json!({
                "id": observation.id,
                "oid": observation.oid,
                "sensitive": observation.sensitive,
                "value": value,
                "error": observation.error,
            });
            if raw {
                item["rawResponseHex"] = observation
                    .raw_response
                    .as_deref()
                    .map(hex)
                    .map(Value::String)
                    .unwrap_or(Value::Null);
            }
            item
        })
        .collect::<Vec<_>>();
    json!({
        "schemaVersion": 1,
        "operation": "brother-wireless-settings-retrieve",
        "readOnly": true,
        "transport": transport,
        "endpoint": endpoint,
        "settings": settings,
        "notes": [
            "Only the fixed, qualified OBJBRNET allowlist was queried.",
            "wireless-state (OID 458867) is not claimed to mean live association status.",
            "No password, credential, mutation, or arbitrary OID query was attempted."
        ]
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_args(arguments: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    for argument in &arguments {
        if argument.starts_with("--")
            && argument != "--show-sensitive"
            && argument != "--raw"
            && argument != "--help"
        {
            return Err(format!("unknown option {argument}\n{}", usage()));
        }
    }
    let positional = arguments
        .iter()
        .filter(|argument| !argument.starts_with("--"))
        .collect::<Vec<_>>();
    let Some(kind) = positional.first() else {
        return Err(usage());
    };
    let target = match kind.as_str() {
        "tcp" if positional.len() == 2 => Target::Tcp(positional[1].to_string()),
        "tcp" => return Err(usage()),
        "usb" => {
            if positional.len() > 2 {
                return Err(usage());
            }
            let selector = positional
                .get(1)
                .map(|value| parse_usb_selector(value))
                .transpose()?;
            Target::Usb(selector)
        }
        "-h" | "--help" => return Err(usage()),
        _ => return Err(usage()),
    };
    Ok(Options {
        target,
        show_sensitive: arguments.iter().any(|value| value == "--show-sensitive"),
        raw: arguments.iter().any(|value| value == "--raw"),
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
    "usage:\n  brother-settings-retrieve tcp HOST[:PORT] [--show-sensitive] [--raw]\n  brother-settings-retrieve usb [BUS:ADDRESS] [--show-sensitive] [--raw]".into()
}
