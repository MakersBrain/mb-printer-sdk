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

`renderProtocolPlanWithOptions(documentJson, modelId, optionsJson)` accepts the
strict camel-case protocol option object, including `copies`, `density`, feed,
speed, offsets, TSPL media dimensions, and Brother cut/compression settings.
Unknown option fields and unsafe zero copy/cut cadence values are rejected.
`renderProtocolPlan` remains the deterministic default-options shorthand.
The authoritative Draft 2020-12 document schema is published at
`@makersbrain/printer-sdk/schema.json`; generated structural declarations remain
available from `@makersbrain/printer-sdk/schema`.

CommonJS Node applications use the synchronous Node export:

```js
const sdk = require("@makersbrain/printer-sdk/node");
const errors = JSON.parse(sdk.validateDocument(documentJson));
```

`@makersbrain/printer-sdk/adapters` exports thin `WebBluetoothTransport`,
`WebUsbTransport`, and `WebSerialTransport` I/O wrappers. Plan execution is the
Promise-based `executePlan(planJson, transport, timing, signal, onProgress)`
export from the WebAssembly package and uses the same Rust executor as native
clients. Device discovery, permission prompts, opening, and interface claiming
remain application responsibilities. WebUSB exposes independent atomic-command
and physical-raster limits so a bulk endpoint can retain complete commands while
chunking raster data to its qualified packet size. Web Bluetooth requires
write-without-response and treats the notification characteristic as optional.
For a capability whose `flowControl` is `phomemo-credit`, construct
`WebBluetoothTransport` with `"phomemo-credit"` as its fourth argument. The
adapter then consumes limit/credit frames internally and gates every write;
the M110s capability marks its FF03 notification characteristic as required.

Within this repository, editor integration tests may use the stable generated
entrypoint `mb-printer-sdk/crates/mb-printer-wasm/pkg/web/mb_printer_wasm.js`
after running `npm run build` in this package.

## Build identity

`buildInfo()` returns JSON describing the embedded module: `name`, `version`,
the full git `commit` it was compiled from, whether the tree was `dirty`, and
the `protocolSourceCommit` stamped into plans. `build.rs` reads the commit from
git; set `MB_SDK_GIT_COMMIT` (and optionally `MB_SDK_GIT_DIRTY=1`) when building
from an archive or container without a repository.

## PDF limitations

PDF decoding uses the pinned memory-safe pure-Rust Hayro backend. Embedded PDF
fonts and Hayro's permissively licensed substitutes for the 14 standard PDF
fonts are supported. CJK fonts must be embedded by the PDF producer; there is no
portable implicit system-font lookup. Encrypted or password-protected PDFs are
unsupported and return a distinct rejection. Malformed PDFs and pages over the
configured raster limit are also rejected.

Generated bindings use `wasm-bindgen-cli 0.2.127`. Package metadata records the
generator and license, and the tarball includes the complete AGPL-3.0-or-later
license text.
