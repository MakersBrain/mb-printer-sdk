// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reads live status from a USB-attached or raw-TCP Brother printer and prints it as JSON.
//!
//! cargo run -p mb-printer-native --features usb --example printer_status -- [model-id]
//! MB_STATUS_TCP=192.0.2.10:9100 cargo run -p mb-printer-native --features usb --example printer_status -- [model-id]
//! MB_STATUS_ATTEMPTS=5 cargo run -p mb-printer-native --features usb --example printer_status -- [model-id]
//!
//! USB access requires permission on the device (a udev rule, or run with sudo).

#[cfg(feature = "usb")]
#[tokio::main]
async fn main() -> Result<(), String> {
    use mb_printer_core::{capabilities, protocol};
    use mb_printer_native::transports::{TcpTransport, usb};

    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ql-1110nwb".into());
    let printer = capabilities::by_id(&model).ok_or_else(|| format!("unknown model: {model}"))?;
    let plan = protocol::status_plan(&printer).map_err(|error| error.to_string())?;

    if let Ok(value) = std::env::var("MB_STATUS_TCP") {
        let address = value
            .parse()
            .map_err(|error| format!("invalid MB_STATUS_TCP address: {error}"))?;
        eprintln!("using Brother raw TCP status endpoint {address}");
        let mut transport = TcpTransport::connect(address, 16_384, 16_384)
            .await
            .map_err(|error| error.to_string())?;
        return read_status(&plan, &mut transport).await;
    }

    let candidates = usb::discover_rusb_bulk()?;
    let candidate = candidates
        .iter()
        .find(|item| item.identity.vendor_id == 0x04f9 && item.in_endpoint.is_some())
        .or_else(|| candidates.iter().find(|item| item.in_endpoint.is_some()))
        .ok_or("no USB printer with a bulk IN endpoint was found")?;
    eprintln!(
        "using {:04x}:{:04x} {} interface {} in-endpoint {:?}",
        candidate.identity.vendor_id,
        candidate.identity.product_id,
        candidate.product.clone().unwrap_or_default(),
        candidate.interface,
        candidate.in_endpoint
    );

    let mut transport = usb::open_rusb_with_limits(candidate, 16_384, 16_384, 64, 3_000)?;
    read_status(&plan, &mut transport).await
}

#[cfg(feature = "usb")]
async fn read_status<T: mb_printer_executor::Transport>(
    plan: &mb_printer_core::protocol::Plan,
    transport: &mut T,
) -> Result<(), String> {
    use mb_printer_core::protocol;
    use mb_printer_executor::{ExecuteError, WaitOutcome, execute};
    use std::time::Duration;

    let attempts = std::env::var("MB_STATUS_ATTEMPTS")
        .map(|value| {
            value
                .parse::<u8>()
                .map_err(|error| format!("invalid MB_STATUS_ATTEMPTS: {error}"))
                .and_then(|value| {
                    if (1..=10).contains(&value) {
                        Ok(value)
                    } else {
                        Err("MB_STATUS_ATTEMPTS must be between 1 and 10".into())
                    }
                })
        })
        .unwrap_or(Ok(3))?;

    if std::env::var("MB_STATUS_EXECUTE").is_ok() {
        for attempt in 1..=attempts {
            match execute(plan, &mut *transport).await {
                Ok(progress) => {
                    let captured = progress
                        .responses
                        .last()
                        .ok_or("the printer did not return a status reply")?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&protocol::brother_parse_status(captured)?)
                            .map_err(|error| error.to_string())?
                    );
                    transport
                        .disconnect()
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(());
                }
                Err(error @ ExecuteError::Timeout { .. }) if attempt < attempts => {
                    eprintln!("status attempt {attempt}/{attempts} failed: {error}; retrying");
                    transport.delay(Duration::from_millis(250)).await;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    // Write the request, then read the reply directly so malformed frames can be shown.
    let request = protocol::Plan {
        actions: plan
            .actions
            .iter()
            .filter(|action| !matches!(action, protocol::Action::WaitForResponse { .. }))
            .cloned()
            .collect(),
        ..plan.clone()
    };
    for attempt in 1..=attempts {
        execute(&request, &mut *transport)
            .await
            .map_err(|error| error.to_string())?;
        let mut reply = Vec::new();
        for _ in 0..8 {
            match transport
                .wait_response(Duration::from_secs(3))
                .await
                .map_err(|error| error.to_string())?
            {
                WaitOutcome::Response(bytes) => {
                    reply.extend(bytes);
                    if reply.len() >= 32 {
                        break;
                    }
                }
                WaitOutcome::Timeout | WaitOutcome::Unavailable => break,
            }
        }
        eprintln!(
            "status attempt {attempt}/{attempts}: reply {} bytes: {}",
            reply.len(),
            reply
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if !reply.is_empty() || attempt == attempts {
            println!(
                "{}",
                serde_json::to_string_pretty(&protocol::brother_parse_status(&reply)?)
                    .map_err(|error| error.to_string())?
            );
            transport
                .disconnect()
                .await
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
        transport.delay(Duration::from_millis(250)).await;
    }

    unreachable!("attempt count is validated as positive")
}

#[cfg(not(feature = "usb"))]
fn main() {
    eprintln!("rebuild with --features usb");
}
