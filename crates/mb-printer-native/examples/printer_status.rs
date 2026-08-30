// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reads live status from a USB-attached Brother printer and prints it as JSON.
//!
//! cargo run -p mb-printer-native --features usb --example printer_status -- [model-id]
//!
//! Requires permission on the USB device (a udev rule, or run with sudo).

#[cfg(feature = "usb")]
fn main() -> Result<(), String> {
    use mb_printer_core::{capabilities, protocol};
    use mb_printer_native::{Transport, WaitOutcome, execute, transports::usb};

    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ql-1110nwb".into());
    let printer = capabilities::by_id(&model).ok_or_else(|| format!("unknown model: {model}"))?;
    let plan = protocol::status_plan(&printer).map_err(|error| error.to_string())?;

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
    // Write the request, then read the reply directly so a malformed frame can be shown.
    let request = protocol::Plan {
        actions: plan
            .actions
            .iter()
            .filter(|action| !matches!(action, protocol::Action::WaitForResponse { .. }))
            .cloned()
            .collect(),
        ..plan.clone()
    };
    if std::env::var("MB_STATUS_EXECUTE").is_ok() {
        // Exercise the executor's own capture path, which the browser route mirrors.
        let progress = execute(&plan, &mut transport).map_err(|error| error.to_string())?;
        let captured = progress
            .responses
            .last()
            .ok_or("the printer did not return a status reply")?;
        println!(
            "{}",
            serde_json::to_string_pretty(&protocol::brother_parse_status(captured)?)
                .map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    execute(&request, &mut transport).map_err(|error| error.to_string())?;
    let mut reply = Vec::new();
    for _ in 0..8 {
        match transport.wait_response(3_000)? {
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
        "reply {} bytes: {}",
        reply.len(),
        reply
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let status = protocol::brother_parse_status(&reply)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&status).map_err(|error| error.to_string())?
    );
    Ok(())
}

#[cfg(not(feature = "usb"))]
fn main() {
    eprintln!("rebuild with --features usb");
}
