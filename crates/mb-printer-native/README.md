<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# mb-printer-native

Native asynchronous execution boundary for typed `mb-printer-core` action
plans, including atomic-write preflight, physical chunking, pacing, and response
validation.

Platform integrations are opt-in Cargo features so headless consumers retain a
portable dependency graph:

- `serial`: configured 8-N-1 `SerialTransport`, `SerialConfig`, and serial-port discovery.
- `bluetooth-rfcomm`: direct Linux RFCOMM socket discovery/connect without privileged TTY binding.
- `usb`: panic-free explicit-context identity/bulk-interface discovery, automatic
  endpoint selection, descriptor identity, and independent command/raster limits.
- `ble`: blocking compatibility transports plus Tokio-native discovery and
  `AsyncBtleplugTransport` connect/write/notification APIs.
- `wifi`: Brother PJL configuration plus reusable bounded IPP status/media
  queries and candidate probing.
- `native-input`: bounded PDF/PNG/JPEG/SVG filesystem ingestion.

The backend traits remain public for deterministic tests and alternative
platform integrations. No hardware feature is enabled by default.

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

Licensed under AGPL-3.0-or-later; the complete text is included as `LICENSE`.
