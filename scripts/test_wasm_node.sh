#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
sdk_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tool="$sdk_root/.tools/wasm-bindgen/bin/wasm-bindgen"
if [ ! -x "$tool" ]; then
  cargo install wasm-bindgen-cli --version 0.2.127 --locked --root "$sdk_root/.tools/wasm-bindgen"
fi
cargo build --manifest-path "$sdk_root/Cargo.toml" -p mb-printer-wasm \
  --target wasm32-unknown-unknown --release --locked
out="$sdk_root/target/wasm-node-pkg"
"$tool" "$sdk_root/target/wasm32-unknown-unknown/release/mb_printer_wasm.wasm" \
  --target nodejs --out-dir "$out"
node "$sdk_root/crates/mb-printer-wasm/wasm-node-equivalence.cjs" \
  "$out/mb_printer_wasm.js"
(cd "$sdk_root/crates/mb-printer-wasm" && npm run build:adapters >/dev/null && node browser-adapters-node.mjs)
