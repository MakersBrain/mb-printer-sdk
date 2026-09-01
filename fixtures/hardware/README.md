<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Hardware acceptance contract

`matrix.json` is an optional hardware-evidence inventory. It does not block
builds, releases, or deployment in this single-developer project. A cell is
complete only when every
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

If formal evidence is useful later, copy `report-template.json`, replace every
`REQUIRED` value, attach the trace artifact by its SHA-256, and record the
result. This optional process is informational and is not part of the normal
development or release workflow. Synthetic and loopback results must still not
be represented as physical hardware observations.

