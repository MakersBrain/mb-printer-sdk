<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# @makersbrain/printer-sdk

Deterministic browser and Node bindings for Makers' Brain v4 label validation,
rendering, PDF import/export, La Poste sheet extraction, printer capabilities,
and typed protocol plans.

Browser applications use the web export and initialize its WASM module:

```js
import init, { validateDocument, renderPacked } from "@makersbrain/printer-sdk/web";
await init();
const errors = JSON.parse(validateDocument(documentJson));
const bytes = renderPacked(documentJson);
```

CommonJS Node applications use the synchronous Node export:

```js
const sdk = require("@makersbrain/printer-sdk/node");
const errors = JSON.parse(sdk.validateDocument(documentJson));
```

`@makersbrain/printer-sdk/adapters` exports thin `WebBluetoothTransport` and
`WebUsbTransport` wrappers plus the transport-independent plan executor. Device
discovery, permission prompts, opening, and interface claiming remain application
responsibilities.

Within this repository, editor integration tests may use the stable generated
entrypoint `mb-printer-sdk/crates/mb-printer-wasm/pkg/web/mb_printer_wasm.js`
after running `npm run build` in this package.

## PDF limitations

PDF decoding uses the pinned memory-safe pure-Rust Hayro backend. Embedded PDF
fonts are supported. To keep the browser module bounded and reproducible, Hayro's
large built-in fallback-font bundle is disabled; PDFs that reference missing,
non-embedded standard or CJK fonts can render incomplete text. Encrypted or
password-protected PDFs are unsupported and are rejected. Malformed PDFs and
pages over the configured raster limit are also rejected.

Generated bindings use `wasm-bindgen-cli 0.2.127`. Package metadata records the
generator and license, and the tarball includes the complete AGPL-3.0-or-later
license text.
