#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
sdk_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
port=18765
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$sdk_root" >/tmp/mb-printer-wasm-http.log 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' EXIT INT TERM
chromium_bin=${CHROMIUM_BIN:-chromium}
output=$("$chromium_bin" --headless=new --no-sandbox --disable-gpu --disable-dev-shm-usage \
  --virtual-time-budget=30000 --dump-dom \
  "http://127.0.0.1:$port/crates/mb-printer-wasm/browser-equivalence.html" 2>/dev/null)
case "$output" in
  *MB_WASM_BROWSER_PASS*) echo "Chromium/WASM shared fixture equivalence passed" ;;
  *) echo "$output"; exit 1 ;;
esac
