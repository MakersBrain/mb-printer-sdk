<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Protocol authority index

Production behavior is frozen by the conformance fixtures under
`fixtures/protocol/`. Research material is maintained outside this repository
and may become authoritative only after it reaches `final` maturity and is
published at an immutable research commit or tag.

The current Phomemo/AIMO research specification is pinned for review at
[`mb-printer-research` commit `d59178b785fa6cf470391299e4b1852d2968b811`](https://github.com/MakersBrain/mb-printer-research/blob/d59178b785fa6cf470391299e4b1852d2968b811/vendors/phomemo/protocols/phomemo-protocol-spec.md).
It is version `0.1.0`, maturity `candidate`, and therefore is explicitly not
the SDK authority yet. The checked-in Python traces remain authoritative for
compatibility, pinned to `mb-cli-printer` commit
`96e2e622cea6f57018a0b8d488e746a39b10da54`; Phomemo-derived behavior also
retains the upstream `transcriptionstream/phomymo` commit
`1f58d3f0e7f941b9143277cda828380149e56855` recorded in the typed contract.

| SDK protocol | Research specification | Current authority | Known limitations / deviations |
|---|---|---|---|
| `m-series`, `m02`, `m04`, `m110`, `d-series`, `p12` | Phomemo/AIMO 0.1.0 (`candidate`) | Frozen Python action traces | Research is only hardware-qualified on one M110s; per-model conclusions remain unqualified. |
| `tspl` | Phomemo/AIMO 0.1.0 (`candidate`) | Frozen Python action traces | The SDK implements its supported print subset, not the full TSPL command language. |
| `brother` | No final research specification | Frozen Python action traces plus SDK tests | Rust deliberately waits for and validates the Brother status response that the Python print flow only requests. |

Machine-readable fixture IDs, specification versions, pinned commits,
maturity, and intentional deviations are in
[`fixtures/protocol/specifications.json`](../../fixtures/protocol/specifications.json).
