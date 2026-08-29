#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
sdk_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$sdk_root/Cargo.toml")
npm_version=$(node -p "require('$sdk_root/crates/mb-printer-wasm/package.json').version")
file_version=$(tr -d '\r\n' <"$sdk_root/VERSION")
test -n "$version"
test "$version" = "$npm_version"
test "$version" = "$file_version"
if [ -n "${GITHUB_REF_NAME:-}" ]; then
  test "${GITHUB_REF_NAME#v}" = "$version"
fi
echo "release version $version is consistent"
