#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
sdk_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
crate="$sdk_root/crates/mb-printer-wasm"
tool="$sdk_root/.tools/wasm-bindgen/bin/wasm-bindgen"
if [ ! -x "$tool" ]; then
  cargo install wasm-bindgen-cli --version 0.2.127 --locked --root "$sdk_root/.tools/wasm-bindgen"
fi
cargo build --manifest-path "$sdk_root/Cargo.toml" -p mb-printer-wasm \
  --target wasm32-unknown-unknown --release --locked
wasm="$sdk_root/target/wasm32-unknown-unknown/release/mb_printer_wasm.wasm"
"$tool" "$wasm" --target web --out-dir "$crate/pkg/web"
"$tool" "$wasm" --target nodejs --out-dir "$crate/pkg/node"
mv "$crate/pkg/node/mb_printer_wasm.js" "$crate/pkg/node/mb_printer_wasm.cjs"
perl -pi -e 's#/mb_printer_wasm_bg\.wasm#/../web/mb_printer_wasm_bg.wasm#' \
  "$crate/pkg/node/mb_printer_wasm.cjs"
rm -f "$crate/pkg/node/mb_printer_wasm_bg.wasm" \
  "$crate/pkg/node/mb_printer_wasm_bg.wasm.d.ts"
python3 "$sdk_root/scripts/stamp_wasm_bindings.py" "$crate/pkg"
cp "$sdk_root/LICENSE" "$crate/LICENSE"
(cd "$crate" && npm run build:adapters && npm run check:types)
cp "$sdk_root/schema/mb-label-v4.schema.json" "$crate/dist/mb-label-v4.schema.json"
