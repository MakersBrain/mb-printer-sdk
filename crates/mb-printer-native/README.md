<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# mb-printer-native

Native transports for typed `mb-printer-core` action plans. Plan preflight,
physical chunking, pacing, response validation, progress, and cancellation live
in the shared `mb-printer-executor` crate.

Platform integrations are opt-in Cargo features so headless consumers retain a
portable dependency graph:

- `serial`: configured 8-N-1 `SerialTransport`, `SerialConfig`, and serial-port discovery.
- `bluetooth-rfcomm`: direct Linux RFCOMM socket discovery/connect without privileged TTY binding.
- `usb`: panic-free explicit-context identity/bulk-interface discovery, automatic
  endpoint selection, descriptor identity, and independent command/raster limits.
- `ble`: Tokio-native discovery and model-profiled `BtleplugTransport` APIs.
- `blocking`: native-only `BlockingPrinterClient` backed by one dedicated
  worker thread and current-thread Tokio runtime (currently for BLE connection).
- `wifi`: Brother PJL configuration plus reusable bounded IPP status/media
  queries and candidate probing.
- `native-input`: bounded PDF/PNG/JPEG/SVG filesystem ingestion.

The backend traits remain public for deterministic tests and alternative
platform integrations. No hardware feature is enabled by default.

## Execution

Async is the canonical API. The application owns the Tokio runtime:

```rust,no_run
use std::{num::NonZeroUsize, time::Duration};
use mb_printer_core::capabilities;
use mb_printer_executor::Transport;
use mb_printer_native::transports::ble::{BtleplugConnectOptions, BtleplugTransport};

# async fn print(address: &str, plan: &mb_printer_core::protocol::Plan)
#     -> Result<(), Box<dyn std::error::Error>> {
let printer = capabilities::by_id("m02").ok_or("unknown printer")?;
let ble = printer.ble_gatt().ok_or("model does not support BLE")?;
let options = BtleplugConnectOptions {
    scan_timeout: Duration::from_secs(5),
    payload_limit: NonZeroUsize::new(512).unwrap(),
};
let mut transport = BtleplugTransport::connect(address, ble, options).await?;
let progress = mb_printer_executor::execute(plan, &mut transport).await?;
transport.disconnect().await?;
println!("wrote {} bytes", progress.bytes_written);
# Ok(())
# }
```

Synchronous applications can opt into `blocking`. This facade owns one worker
runtime rather than nesting a runtime on the caller's thread:

```rust,no_run
use mb_printer_native::blocking::BlockingPrinterClient;
# fn print(
#     address: &str,
#     ble: mb_printer_core::capabilities::BleGattCapabilities,
#     options: mb_printer_native::transports::ble::BtleplugConnectOptions,
#     plan: mb_printer_core::protocol::Plan,
# ) -> Result<(), Box<dyn std::error::Error>> {
let mut client = BlockingPrinterClient::connect_btleplug(
    address.to_owned(),
    ble,
    options,
)?;
let progress = client.execute(plan)?;
client.disconnect()?;
println!("wrote {} bytes", progress.bytes_written);
# Ok(())
# }
```

Catalogue models own their GATT profiles; callers never provide FF02 or FF03
UUIDs independently. FF02 is accepted only with write-without-response. FF03
may be absent when the model declares it optional. Cancellation never triggers
an automatic retry, including when a write's outcome is unknown; reconnect and
retry policy belongs to the caller.

## Read-only Brother settings retrieval

`brother-settings-retrieve` reproduces the retrieval side of Brother's Printer
Setting Tool for the fixed OBJBRNET wireless-field allowlist recovered from
`brwfcfg.dll`. It never sends settings, credentials, or caller-supplied OIDs.
Network identifiers are redacted by default:

```console
cargo run -p mb-printer-native --features brother-tools --bin brother-settings-retrieve -- tcp 192.168.1.25
cargo run -p mb-printer-native --features brother-tools,usb --bin brother-settings-retrieve -- usb
```

Use `--show-sensitive` to include SSID and IP address. Raw local response bytes
require both `--raw` and `--show-sensitive`. The tool does not attempt to read a
Wi-Fi password: the inspected Brother software provides no device-password
retrieval operation.

`brother-device-settings-retrieve` implements the separate Printer Setting Tool
device-settings session recovered by comparing the native QL-800/QL-1100 and
PT-P710BT `brdvset.exe` builds. It validates the selected model against the
printer's 32-byte status response, then reads only the confirmed common
settings (command mode, auto-cut, print density, and serialization mode).
QL-800, QL-810W, and QL-820NWB additionally expose their confirmed second-color
density query.

The separately compared PJ-700-series build is deliberately not included: its
native executable does not contain the same common query sequences and needs a
separate, model-qualified command table.

```console
cargo run -p mb-printer-native --features brother-tools --bin brother-device-settings-retrieve -- list-models
cargo run -p mb-printer-native --features brother-tools --bin brother-device-settings-retrieve -- tcp ql-1110nwb 192.168.1.25
cargo run -p mb-printer-native --features brother-tools,usb --bin brother-device-settings-retrieve -- usb pt-p710bt
```

Raw responses are local-only and require `--raw`. The retriever always attempts
to leave device-settings mode after entering it and stops after an uncorrelated
response instead of sending later queries blindly.

Licensed under AGPL-3.0-or-later; the complete text is included as `LICENSE`.
