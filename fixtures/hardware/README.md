<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Hardware acceptance contract

`matrix.json` is the release-gate inventory. A cell is complete only when every
catalogue ID in `requiredCatalogIds` is named by a valid signed report for that
exact transport and platform. Similar names, protocol families, and aliases do
not qualify one another automatically. A report may qualify multiple IDs only
when it records each ID and the tested device actually exposes identical
protocol, head, media, and transport behavior.

The three historical successes predate the report contract. They remain
`provisional`, not signed: their ledger entries lack at least the device serial,
firmware, complete platform version, operator identity/signature, and retained
trace artifact required by `report.schema.json`. Failed attempts remain
`unsigned`; a connection alone is never a print acceptance.

Copy `report-template.json`, replace every `REQUIRED` value, attach the trace
artifact by its SHA-256, sign the canonical JSON with the declared operator key,
and review it before changing a matrix cell to `signed`. Synthetic and loopback
tests set `hardwareClaim` to false and can never satisfy this gate.

