<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Changelog

All notable changes follow Keep a Changelog; versions follow Semantic Versioning.

## Unreleased

- Preserve Brother status identity, raw error bytes, model-dependent byte 14,
  phase number, notification, extension, and tape fields, including turned-off
  status.
- Add explicit gap, continuous, and black-mark Phomemo media selection while
  retaining the legacy `continuous` option as the default selector.
- Reject unqualified LZO print plans; physical M110s evidence showed that the
  device misinterprets the compressed stream.
- Gate M110s WebBluetooth and native GATT writes on the printer's `01 <credits>`
  notifications, apply its `02 <limit-le16>` payload limit, and keep those
  control frames out of status responses.

## 0.1.0 - 2026-08-29

- Initial v4 document/schema, validation, v3 import, deterministic rendering,
  embedded resources/fonts, PNG/PDF, La Poste extraction, and template engine.
- Data-driven capabilities and typed plans for eight printer protocol families.
- Native executor plus browser/Node WASM packages and WebBluetooth/WebUSB adapters.
- Shared schema, semantic, Python protocol, native/WASM, Node, and Chromium fixtures.
- Release and browser CI use only organization-approved actions plus the Chrome
  installation already supplied by GitHub-hosted runners.
