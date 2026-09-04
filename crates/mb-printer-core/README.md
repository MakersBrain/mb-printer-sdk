<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# mb-printer-core

Portable deterministic v4 label document, validation, rendering, PDF import and
export, La Poste extraction, printer capabilities, and protocol action plans.
The crate is filesystem- and transport-independent and builds for native Rust
and `wasm32-unknown-unknown`.

Printer catalogue entries also own their transport capabilities. Call
`PrinterDefinition::ble_gatt()` to obtain a model's reviewed GATT profile;
callers do not supply FF02 or FF03 UUIDs separately. A `None` result means the
model is explicitly unsupported for BLE.

Licensed under AGPL-3.0-or-later; the complete text is included as `LICENSE`.
