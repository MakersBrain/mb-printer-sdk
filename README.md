<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Makers' Brain printer SDK

Deterministic Rust foundations for `.mb-label.json` v4 documents and thermal
printer protocol execution. The workspace separates portable core logic,
native action execution, and browser/WASM bindings.

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
