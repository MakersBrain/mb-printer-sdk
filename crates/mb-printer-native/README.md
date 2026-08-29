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

Licensed under AGPL-3.0-or-later; the complete text is included as `LICENSE`.
