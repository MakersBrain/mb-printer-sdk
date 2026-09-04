# SNMP property access implementation plan

## Outcome

Add typed, bounded SNMP property reads to the SDK, starting with the Brother
inventory required to identify firmware updates. Add SNMP property writes only
through a separately qualified plan/confirm/apply workflow. The SDK must never
expose an agent or cloud operation that accepts an arbitrary OID and value.

SNMP is used here to inspect a printer and, for explicitly supported
properties, configure it. Firmware selection may consume SNMP inventory, but
firmware download and installation are separate protocols and are not part of
an SNMP `SET` implementation.

## Existing foundation

The repository already has useful parts of the read path:

- `mb-printer-core::snmp` is a synchronous, runtime-independent SNMPv2c BER
  codec for one-object `GET`, `GETNEXT`, and `RESPONSE` messages.
- `ObjectRegistry` allowlists readable OIDs and maps them to semantic setting
  IDs.
- `normalize_snmp` records protocol-neutral values, evidence, bounded original
  bytes, sensitivity, and read-only mutation support. Its current retention of
  complete SNMPv2c response bytes is not credential-safe because the response
  repeats the community; migration must close that gap before raw SNMP evidence
  is exposed or persisted.
- `mb-printer-native::transports::snmp::SnmpClient` provides bounded UDP reads
  and walks on the caller's Tokio runtime, with credential redaction, timeout,
  retry, response-size, and walk-step limits.

The existing implementation proves the required boundaries and supplies useful
fixtures, but it should not grow into a second general SNMP stack. The preferred
implementation is to adopt `async-snmp` behind the native `snmp` feature and
retain this repository's property catalogue, normalization, evidence, policy,
and confirmed-write layers.

## Crate decision

Use [`async-snmp`](https://crates.io/crates/async-snmp) as the native protocol
and transport implementation, subject to the Phase 0 adapter spike below. As of
2026-08-31, release `0.18.1` provides Tokio-based SNMPv1/v2c/v3 USM, `GET`,
`GETNEXT`, `GETBULK`, `SET`, walks, multiple varbinds, response-shape
diagnostics, configurable retries and limits, unknown-value preservation, and
redacted community handling. Its MSRV is 1.88, below this workspace's 1.98.
The license is `MIT OR Apache-2.0`.

It is a strong functional fit, but it is pre-1.0 and has changed rapidly.
Therefore:

- pin an exact reviewed release such as `=0.18.1` in the workspace lockstep;
- review its changelog and rerun protocol fixtures before every upgrade;
- add it only to `mb-printer-native`, never `mb-printer-core` or WASM;
- start with `default-features = false` for v2c reads, avoiding unused crypto,
  agent, CLI, and MIB dependencies;
- add a separate `snmp-v3` feature enabling the reviewed RustCrypto backend;
- do not enable its MIB parser initially: the product catalogue uses reviewed,
  checked-in numeric OIDs and semantic metadata rather than runtime MIB files;
- configure strict response-shape and source correlation for product reads;
- construct write clients with `Retry::none()`, because the crate's general
  retry configuration otherwise also applies to `SET` requests;
- keep crate types private to the native adapter so a future replacement does
  not break the SDK API.

Proposed dependency shape:

```toml
# workspace dependency
async-snmp = { version = "=0.18.1", default-features = false }

# mb-printer-native
[features]
snmp = ["dep:async-snmp", "dep:tokio"]
snmp-v3 = ["snmp", "async-snmp/crypto-rustcrypto"]
```

The crate's normal client response exposes decoded varbinds and decode/shape
diagnostics, but not the complete original datagram. A complete SNMPv1/v2c
datagram is not an acceptable evidence object because it contains the community
credential. Phase 0 must replace the current raw-byte behavior with a
credential-safe evidence representation. Preferred options, in order, are:

1. parse the accepted message, replace the v1/v2c community and other
   credential-bearing fields with fixed markers, re-encode a canonical bounded
   message, and store its domain-separated hash together with the original
   length, response source, request ID, and decode/shape diagnostics;
2. if byte-level diagnostics are required, retain that sanitized canonical
   message rather than the accepted datagram;
3. retain unsanitized bytes only in an ephemeral, explicitly sensitive local
   diagnostic sink that cannot serialize, persist, log, or cross the agent
   boundary. A private recording wrapper around the crate's public `Transport`
   trait may feed that sink, but must record only the candidate accepted by the
   client's validator;
4. if none is safe and maintainable, retain the current minimal codec only for
   evidence decoding/differential tests, not as a second network client.

Do not choose `snmp-parser` as the client: it is an analysis/parser library, not
a manager implementation. `snmp2` is a plausible fallback and supports the
required operations and versions, but `async-snmp` has a more complete async
API, response-shape diagnostics, configurable correlation/limits, and no
native `libnetsnmp` requirement when MIB support is disabled.

## Design rules

1. Keep the property domain, normalization, and policy in `mb-printer-core`;
   keep `async-snmp`, BER/PDU adaptation, UDP, clocks, retries, and credentials
   in `mb-printer-native`.
2. Represent public operations by semantic property IDs such as
   `firmware.components`, not user-supplied OID strings.
3. Separate protocol access from product authorization. Decoding an SNMP
   `SET` packet does not make an object writable by the SDK.
4. Reads and writes must both use a registered object definition. Write
   registration additionally requires value constraints, device
   qualification, risk, and verification metadata.
5. Treat manufacturer, model, serial, firmware, and endpoint generation as a
   binding. An IP address alone is not printer identity.
6. Bound message bytes, varbind count, value bytes, table rows, requests,
   retries, elapsed time, and concurrent device operations.
7. Do not log communities, SNMPv3 keys, administrator passwords, sensitive
   property values, or unredacted packets.
8. Never automatically retry a `SET` after a send when the result is unknown.
9. Preserve unknown response values and credential-safe evidence for local
   diagnostics. Never put an unsanitized v1/v2c datagram in a serializable
   observation; export only registered, normalized values by default.
10. Keep browser/WASM behavior explicit: direct SNMP is unavailable; a browser
    can use an authorized local agent.

## Domain model

Evolve `RegisteredObject` into a definition that describes protocol syntax and
product behavior rather than storing only an OID and label. Prefer enums and
plain data over trait objects so catalogues remain deterministic, testable,
serializable where needed, and easy to inspect.

```rust
pub struct ObjectDefinition {
    pub key: ObjectKey,
    pub oid: ObjectId,
    pub syntax: ObjectSyntax,
    pub sensitivity: Sensitivity,
    pub access: ObjectAccess,
    pub qualification: DeviceQualification,
}

pub enum ObjectAccess {
    ReadOnly,
    ConfirmedWrite(WriteDefinition),
}

pub struct WriteDefinition {
    pub constraint: ValueConstraint,
    pub risk: WriteRisk,
    pub verification: Verification,
}

pub enum Verification {
    ReadBackSameObject,
    ReadBack { oid: ObjectId, expected: ExpectedValue },
}
```

`ObjectKey` is a validated semantic newtype, not an arbitrary `String` at the
API boundary. `ObjectSyntax` covers the wire type and an explicit conversion
policy, for example integer, octets, UTF-8 with trailing NUL removal, IPv4, or
a registered vendor record. Invalid vendor data remains an observation and
does not silently become a semantic value.

Use separate catalogue construction paths:

- a read catalogue may include standard Printer-MIB objects and qualified
  vendor inventory objects;
- a write catalogue is compiled from reviewed definitions and cannot be
  expanded from a request, configuration file, or cloud payload;
- the effective catalogue is selected only after printer identity and profile
  qualification.

Do not make transport a trait in core. Core owns typed property definitions and
protocol-neutral observations. Native adapts `async_snmp::{Oid, Value,
VarBind}` to those core types. A recording transport wrapper remains a private
native implementation detail if it is needed to hash, sanitize, or feed an
ephemeral sensitive sink with the accepted response.

## Brother firmware inventory provider

Implement a Brother provider under a vendor-owned module, for example
`mb_printer_core::providers::brother::snmp`. It should return a typed result:

```rust
pub struct Observed<T> {
    pub value: T,
    pub evidence: Vec<Evidence>,
}

pub enum FieldResult<T> {
    Observed(Observed<T>),
    Unsupported,
    Missing,
    Malformed { evidence: Vec<Evidence> },
    Conflict { observations: Vec<Observed<T>> },
}

pub struct FirmwareInventory {
    pub update_model: FieldResult<String>,
    pub specification: FieldResult<String>,
    pub schema_version: FieldResult<String>,
    pub components: FieldResult<Vec<FirmwareComponent>>,
}

pub struct FirmwareComponent {
    pub id: String,
    pub version: String,
    pub compatibility_key: FieldResult<String>,
    pub evidence: Vec<Evidence>,
}
```

The initial object catalogue should be derived from the checked-in Brother
research and then frozen in fixtures. The already identified scalar objects
include Brother serial, main/sub/security firmware, settings schema version,
Phoenix capability vector, firmware-update support, keyword count, and indexed
firmware-update keywords. The keyword count and indexed firmware-update
keywords are compatibility checks for firmware jobs; they are not the update
inventory row count.

For the recovered network MFP inventory, register exactly the 16 instances
`1.3.6.1.4.1.2435.2.4.3.99.3.1.6.1.2.1` through
`1.3.6.1.4.1.2435.2.4.3.99.3.1.6.1.2.16`. With SNMPv2c, fetch those registered
instances in bounded `GET` batches. Parse consecutive `KEY="value"` records and
stop at the first returned value without `=`. Recognize `MODEL`, `SPEC`,
`FIRMID`, and `FIRMVER`; preserve unknown keys as diagnostic observations.
Pair `FIRMID` and `FIRMVER` in encounter order and return `Malformed` if their
counts differ. Do not infer a row count, walk the Brother enterprise subtree,
or treat missing MODEL/SPEC as empty strings.

Label-printer inventory is a separate qualified profile. Do not assume the MFP
objects exist on a QL device merely because the update service accepts similar
fields. Until a label inventory source is confirmed by frozen capture or
hardware qualification, return typed `Unsupported` or partial results and use
IPP only as independent firmware evidence, not as a guessed replacement for
server-facing model, `SPEC`, or component records.

Normalization should expose stable semantic IDs and retain per-field SNMP
evidence. UUID, serial, device ID, and manufacturer feed the existing identity
reconciliation logic. Model and firmware require a separate evidence-consistency
check because the current identity reconciler intentionally does not use them.
That check must normalize known manufacturer prefixes and model aliases, compare
version fields only within the same qualified component namespace, and preserve
all observations. A strong-identifier mismatch is an identity conflict; an
incompatible model or firmware observation is an evidence conflict. Neither is
resolved by silently choosing one source.

## Native crate adapter

Before adding providers, build a narrow adapter around `async-snmp`:

- convert OIDs and every supported/unknown value variant without lossy string
  round trips;
- configure strict response shape, exact community/source policy, bounded
  receive/send sizes, maximum OIDs, walks, retries, and total deadlines;
- map crate errors and anomalies to stable SDK errors without exposing
  credentials or dependency-specific types;
- use `get_many` for registered batches and reject duplicate or unexpected
  response OIDs;
- use bounded `GETNEXT` walks initially; enable `GETBULK` only when a Brother
  profile needs it and its fallback behavior is covered by fixtures;
- separate read-client construction from write-client construction so a write
  always uses `Retry::none()`;
- produce the credential-safe evidence record selected in Phase 0. Sanitize and
  canonically re-encode before computing a stable hash or creating any
  serializable byte representation. If exact-datagram correlation is required,
  use a locally keyed HMAC whose key and input never cross the native boundary;
  never pass an unsanitized v1/v2c datagram into core;
- run the existing in-house codec and the crate against the same frozen corpus
  until parity is established, then remove the in-house codec from production
  network paths.

Keep SNMPv2c as the first read-only interoperability target because the current
code and Brother evidence use it. Model credentials as a non-serializable,
redacted enum so SNMPv3 can be added without changing every client call:

```rust
pub enum Credentials {
    V2c(Community),
    V3(UserSecurityParameters),
}
```

SNMPv3 authentication/privacy is its own security milestone. Generic remote
writes should require SNMPv3 `authPriv` unless a specific Brother operation has
a separately reviewed and hardware-qualified password-proof flow. SNMPv2c
communities are cleartext bearer credentials and must not be presented as a
secure administration mechanism.

## Public read API

Expose semantic operations rather than a generic public `get(oid)`:

```rust
impl PrinterClient {
    pub async fn read_property(
        &self,
        printer: &QualifiedPrinter,
        property: PropertyId,
    ) -> Result<PropertyObservation, ReadPropertyError>;

    pub async fn inspect_firmware(
        &self,
        printer: &QualifiedPrinter,
    ) -> Result<FirmwareInventory, FirmwareInspectionError>;
}
```

A lower-level allowlisted SNMP client may remain available in
`mb-printer-native` for local integrations, but it must accept registry keys,
not use arbitrary OIDs as authorization. Raw diagnostic output should require
an explicit local-only output mode and apply existing redaction policy.

Provider selection is data driven: reconcile identity, select a qualified
printer profile, obtain its registered property definition, then execute the
protocol operation. Avoid a large `match model_name` in the network client.

## Confirmed write workflow

Implement `SET` only after Brother inventory reads are working and qualified.
The safe product API is `plan_change` plus `apply_change`, not `set`:

1. Resolve a semantic property through the qualified write catalogue.
2. Read its current value and the verification object.
3. Validate type and constraints locally.
4. Create a short-lived plan bound to printer ID, endpoint generation,
   principal, protocol, property definition/version, old-value hash, requested
   value hash, and exact device qualification. Store it inside the enforcing
   local boundary under a cryptographically random opaque plan ID.
5. Return only the opaque plan ID plus safe confirmation metadata. Require an
   authenticated `apply_change(plan_id)` call as explicit confirmation and
   reapply local agent policy authorization.
6. Under the per-printer operation lock, resolve the stored plan, require the
   same principal, reject expired/unknown/consumed plans, and re-read and
   validate all bindings immediately before the write. Caller-supplied hashes
   or protocol fields are not authority.
7. Atomically mark the plan `Sending` immediately before the first possible
   transmission, encode one registered `SET` request, and send it once. A plan
   in `Sending` or a terminal state cannot be replayed. Pre-send validation
   failures may leave the plan pending when retry remains safe; stale, expired,
   or policy-denied plans become terminal.
8. Classify timeout/disconnect after send as `Ambiguous`; never retry it. Mark
   every post-send outcome terminal even if read-back fails.
9. Read back through the registered verification rule and return `Verified`,
   `Rejected`, `ReadBackMismatch`, or `Ambiguous`.
10. Emit an audit record containing hashes and safe metadata, not credentials
    or sensitive values.

The current `ConfirmedChangePlan` is IPP-shaped because it embeds an IPP
`Value`. Refactor it without weakening existing IPP checks:

```rust
pub enum ProtocolChangeValue {
    Ipp(ipp::Value),
    Snmp(snmp::SetValue),
}
```

Alternatively keep protocol-specific plan payloads inside a shared envelope.
The envelope approach is preferable if it prevents accidental cross-protocol
validation. In either design, the internal plan representation is stored only
by the enforcing boundary: a private in-memory store for direct local SDK use,
or the local agent store for agent-mediated use. The versioned wire contract
uses a typed `oneof` for a plan request and returns an opaque plan ID; apply
carries that ID, not a caller-reconstructed plan. Opaque value `bytes`,
caller-supplied hashes, and a caller-supplied protocol string must not be an
authorization boundary. If a deployment cannot keep server-side plan state,
use an authenticated and encrypted, expiring, nonce-bearing token plus a replay
store; a merely encoded or signed-but-replayable structure is insufficient.

The native adapter may call `async-snmp::Client::set`, but the agent must expose
only a registered `PlanChange`/`ApplyChange` operation. Brother password proof,
reset/reboot, commit OIDs, and multi-step changes are vendor operations with
their own state machines. They must not be modeled as ordinary scalar property
writes.

## Agent and capability integration

Extend the agent only after the local SDK path is stable:

- advertise SNMP read support separately from confirmed SNMP write support;
- publish support per printer/profile, not merely per agent binary;
- route `ReadSetting` by semantic ID through the selected provider;
- version `PlanChange` payloads with typed protocol values, return an opaque
  agent-issued plan ID, and make `ApplyChange` accept only that plan ID;
- enforce endpoint generation, deadline, principal, local property allowlist,
  output mode, and response limits before I/O;
- use a per-printer operation lock for writes and for reads that participate in
  plan/apply verification;
- store plans with bounded count and lifetime, consume them atomically at the
  send boundary, and reject missing, forged, cross-principal, or replayed IDs;
- redact sensitive observations before crossing the agent boundary;
- report SNMP as unavailable in direct WASM mode.

## Delivery sequence

### Phase 0: dependency and adapter spike

Pin `async-snmp`, implement private OID/value/error conversion, configure strict
correlation and limits, disable default features, and run it against the
existing fake agent and frozen codec corpus. Implement hashes of canonical
credential-elided responses first; prototype sanitized or ephemeral
accepted-datagram diagnostics only if hashes and structured diagnostics are
insufficient. Record the dependency and license inventory.

Acceptance: all Brother-required value types round-trip; malformed or unrelated
packets cannot be accepted as a requested property; credential-safe hashed or
sanitized response evidence is available; neither normalized nor redacted raw
output contains the v1/v2c community; no crate type leaks through the public
SDK; and the dependency builds with the workspace MSRV and selected features.

### Phase 1: Brother firmware inventory, local read only

Add the reviewed Brother read catalogue and typed firmware provider. Validate
the recovered 16-instance MFP record sequence first against frozen captures,
then inspect the HL-L2375DW and QL-1110NWB hardware used by the existing
research. Treat each device as a separately qualified profile; do not promote
an MFP inventory assumption to the QL profile without supporting evidence.

Acceptance: for each profile, the provider returns observed update-service
model, `SPEC`, component IDs, installed versions, and compatibility data with
per-field bounded evidence, or a typed partial/unsupported result. Unequal
`FIRMID`/`FIRMVER` counts and malformed records fail closed, and inspection
makes no configuration change.

### Phase 2: normalized SDK property reads

Add `read_property`/`inspect_firmware`, profile selection, identity conflict
handling, redaction, and merge behavior with IPP and Brother report evidence.

Acceptance: callers do not need to know OIDs, and unsupported properties have
a typed `Unsupported` result rather than an empty or guessed value.

### Phase 3: local-agent reads

Route registered SNMP reads through the agent contract with per-printer policy,
capability negotiation, deadlines, and safe output modes.

Acceptance: cloud requests cannot supply OIDs, communities, or raw SNMP bytes.

### Phase 4: dormant `SET` adapter and plan model

Implement native `Value` conversion into the crate's `SET`, protocol-specific
confirmed plans, opaque enforcing-boundary-issued plan IDs with bounded private
storage, a dedicated no-retry write client, and fake-agent tests. Keep all
production write catalogues empty.

Acceptance: arbitrary OIDs cannot reach the encoder through the public SDK or
agent; apply without a live agent-issued plan, cross-principal apply, concurrent
apply, and replay are rejected before network I/O; and ambiguous outcomes are
never retried.

### Phase 5: first qualified write

Choose one reversible, non-connectivity-breaking property with known syntax
and read-back behavior, such as a supported contact/location field. Qualify it
on an exact model and firmware pair before enabling it in that profile.

Acceptance: plan expiry, stale identity/value, wrong syntax, policy denial,
timeout after send, read-back mismatch, and rollback guidance are covered by
tests and a hardware report.

### Phase 6: SNMPv3 and additional writes

Enable the crate's RustCrypto-backed SNMPv3 USM path and validate engine
discovery, boots/time handling, authentication, privacy, key localization,
replay checks, and secret-zeroization behavior through the SDK adapter. Expand
write catalogues one property and device qualification at a time.

Acceptance: security-level downgrade is rejected, credentials never serialize
or log, and interoperability fixtures cover supported auth/privacy suites.

## Test strategy

- Adapter tests for OID boundaries, every supported and unknown value tag,
  multi-varbind ordering, response correlation, and all configured limits.
- Retain a focused differential corpus for the dependency's BER behavior rather
  than duplicating its complete unit-test suite.
- Property tests and fuzzing for adapter conversions, duplicate OIDs, invalid
  indexed-object suffixes, limit mapping, and panic freedom. Keep malformed BER
  corpus tests as dependency acceptance tests.
- UDP fake-agent tests for loss, duplicates, reordering, spoofed request IDs,
  wrong OIDs, error-status/index, exact-limit datagrams, retries, and deadlines.
- Golden Brother fixtures for scalar and indexed firmware inventory, missing
  optional components, early non-record sentinels, malformed records, unequal
  `FIRMID`/`FIRMVER` counts, unknown keys, and sensitive serial redaction.
- Cross-protocol tests for agreeing and conflicting SNMP/IPP/PJL identity and
  firmware observations, including benign model aliases and component-scoped
  firmware comparisons.
- Write state-machine tests for every point before send, after send, and during
  read-back. Assert that only pre-send failures can be retried.
- Plan-authority tests for unissued, expired, forged, cross-principal,
  concurrent, and replayed plan IDs. Assert rejection before network I/O and
  atomic transition to `Sending` at the first possible transmission.
- Agent contract tests proving that arbitrary OIDs/bytes and unregistered
  properties are rejected before network I/O.
- Evidence-output tests using a distinctive community value. Assert that it is
  absent from normalized output, redacted raw output, serialized observations,
  logs, errors, and persisted evidence.
- Hardware qualification reports containing model, serial hash, firmware,
  endpoint generation, exact catalogue version, credential-elided canonical
  request/response hashes, timings, limits, observed result, and confirmation
  that read-only phases made no configuration change.

## Definition of done for the firmware use case

The first useful release is complete when a caller can select a reconciled
Brother printer and receive a typed, redacted `FirmwareInventory` with clear
per-field provenance. For a supported, qualified profile, the successful result
contains enough device-specific information to perform an update lookup. Other
profiles and incomplete devices return typed partial/unsupported results rather
than guessed lookup inputs. It does not require SNMP `SET`, firmware upload, or
a generic MIB browser. Those remain explicitly separate, qualified
capabilities.
