<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Versioning and release policy

The three Cargo crates and `@makersbrain/printer-sdk` use one lockstep SemVer
version recorded in the workspace manifest, npm manifest, and `VERSION`. Release
tags are exactly `v<version>`; CI rejects mismatches before packaging or publishing.

Breaking Rust APIs, JavaScript exports, v4 schema semantics, or protocol-plan
contracts require a major version. Backward-compatible additions require a minor
version. Fixes that preserve public contracts use a patch version. Prereleases
use standard SemVer suffixes and are published under a matching npm dist-tag.

Cargo crates publish in dependency order: core, native, then WASM. The npm package
is built from the same commit and release WASM. Every release attaches checksums,
an SPDX SBOM, and GitHub artifact provenance.
