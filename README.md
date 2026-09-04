<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Makers' Brain printer SDK

Deterministic Rust foundations for `.mb-label.json` v4 documents and thermal
printer protocol execution. The workspace has four explicit layers:

- `mb-printer-core` owns printer models, BLE profiles, rendering, and protocol plans.
- `mb-printer-executor` owns the runtime-independent asynchronous transport
  contract and the single plan executor.
- `mb-printer-native` supplies Tokio-native BLE, TCP, USB, serial, and file
  transports plus an optional native-only blocking facade.
- `mb-printer-wasm` bridges JavaScript transports into that same Rust executor;
  its TypeScript adapters contain browser I/O only.

Async execution is canonical. Applications own the Tokio runtime and call
`mb_printer_executor::execute(&plan, &mut transport).await`; the SDK never
creates a hidden or nested runtime. The optional blocking facade instead owns
one dedicated worker runtime. Cancellation never retries a write or a plan:
reconnect and retry decisions always belong to the caller.

Catalogue models own their BLE GATT profile. BLE uses the declared FF02
characteristic with write-without-response only; an FF03 notification
characteristic declared optional may be absent without making connection fail.
The hardware-qualified M110s profile requires FF03 and gates every write on its
credit notifications; flow-control frames are never exposed as status replies.

The portable rendering slice uses integer micrometre-to-dot conversion,
deterministic bilevel dithering, raster rotation/head fitting, shapes, bitmap
text, Code 128, Code 39, EAN-13, UPC-A and QR encoding, and byte-stable
one-page PNG/PDF export.

Browser and Node packages are generated reproducibly with
`scripts/build_wasm_packages.sh`; headless Chromium and Node runtime equivalence
tests exercise the resulting bindings. PDF normalization uses pure-Rust Hayro.
Embedded fonts and deterministic permissively licensed substitutes for PDF's 14
required standard fonts are supported. Non-embedded CJK fonts remain unsupported
and must be embedded by the PDF producer. Encrypted or
password-protected PDFs are unsupported and return a distinct error.

```sh
cargo test --workspace
```

Licensed under AGPL-3.0-or-later. Protocol compatibility behavior is derived
from the frozen Python implementation and the documented Phomymo reference at
commit `1f58d3f0e7f941b9143277cda828380149e56855`.

Physical qualification is tracked by the machine-readable
[`fixtures/hardware/matrix.json`](fixtures/hardware/matrix.json) contract. The
three historical successful prints remain provisional until complete signed
reports satisfy the bundled schema; synthetic tests never claim hardware
acceptance.
