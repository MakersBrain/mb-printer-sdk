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
tag=${1:-}
if [ -n "$tag" ]; then
  test "$tag" = "v$version" || {
    echo "tag $tag does not match package version v$version" >&2
    exit 1
  }
fi
echo "release version $version is consistent"
