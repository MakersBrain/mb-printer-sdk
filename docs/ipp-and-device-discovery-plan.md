<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# IPP and device discovery architecture plan

## Objective and scope

Build standards-compliant printer discovery, inspection, and guarded
administration for restricted browser/WASM and full native execution. The
shared foundation is an in-house, deterministic, bounded IPP wire codec in
`mb-printer-core`. The product scope covers the IPP operations needed for
discovery and printer administration; it does not attempt to implement every
IPP operation, extension, print-job workflow, or server behavior.

Discovery is always read-only. Configuration changes are separate operations
with explicit authorization, confirmation, stale-plan protection, and
read-back verification. Vendor operations begin with registered, qualified,
read-only probes and may be carried over IPP extensions, PJL, SNMP, USB,
serial, Bluetooth, or another known management protocol.

An IPP response is not a complete inventory of firmware settings.
`Get-Printer-Attributes` with `requested-attributes=all` returns only the
attributes the printer chooses and is authorized to expose to that caller.
No client library can recover settings the firmware does not return. Hidden
sleep, Wi-Fi, power, maintenance, security, or other vendor settings require a
known read-only vendor query through an appropriate management protocol. Model
catalogues may guide research, but absence from IPP is not proof of absence and
catalogue presence is not proof of runtime support.

## Guiding decisions

1. Treat a live, qualified device observation as authoritative for the device,
   firmware, endpoint generation, principal, and time at which it was made.
2. Keep transport, printer protocol, normalized semantics, and evidence
   separate. A protocol can use several transports without changing its
   decoder.
3. Preserve the original bounded IPP message bytes and decoded unknown values
   so later releases can reinterpret data without pretending it was understood
   earlier.
4. Run only registered and proven read-only probes automatically. Never use
   blind byte probing or arbitrary byte tunneling.
5. Fail closed on writability. Defaults and inferred capabilities are not write
   authorization.
6. Bound message bytes, attributes, values, nesting, time, concurrent workers,
   and probe counts before performing I/O or allocating unbounded memory.
7. Make redaction and output policy explicit. Sensitive data never enters
   ordinary logs, telemetry, or cloud results.
8. Keep `mb-printer-core` synchronous and independent of Tokio, HTTP, TLS,
   browser globals, and permission APIs.
9. Merge device observations only with strong identity evidence or an explicit
   user association.
10. Treat an agent as a policy-enforcing executor near a printer, not as a
    general network tunnel.

## Runtime and crate boundaries

```text
mb-printer-core
  Synchronous portable values, limits, IPP codec, protocol codecs,
  normalization, identity reconciliation, and probe identifiers

mb-printer-native
  Async discovery and clients on a caller-owned Tokio runtime, HTTP/TLS,
  DNS-SD, native sockets, and bounded hardware workers

mb-printer-wasm
  Promise/AbortSignal adapters for fetch, WebUSB, Web Serial, and Web Bluetooth
  using the same portable codecs

mb-printer API service
  Optional native bridge for a local restricted browser

mb-printer agent
  Native executor, published-device binding, policy enforcement, and redaction

cloud control plane
  Authenticated routing over the existing protobuf/tonic agent session,
  authorization, deadlines, capability negotiation, and audit metadata
```

Core accepts and returns bounded bytes and typed values. Its encoder and
decoder are deterministic and synchronous. It must not create a Tokio runtime,
perform HTTP or TLS, call browser APIs, or prompt for permissions.

Native discovery exposes async APIs and uses the Tokio runtime supplied by the
application. Library constructors and synchronous APIs must never create or
enter a hidden runtime. USB, serial, and other blocking hardware calls execute
on bounded workers. Each operation also has a device- or OS-level timeout,
because cancelling an async task does not necessarily interrupt a blocking
system call.

WASM wraps the same core codec in JavaScript promises and supports
`AbortSignal`. Direct access is limited to interfaces the browser and user
grant: WebUSB, Web Serial, known Web Bluetooth characteristics, and HTTP IPP
through `fetch` when local-network policy, mixed-content rules, and CORS permit
it. Raw TCP, UDP, DNS-SD, SNMP, and Bluetooth RFCOMM are normally unavailable
in a browser and must be reported as unavailable rather than attempted.

## IPP codec architecture

Keep the IPP codec in `mb-printer-core`. It is complete for the bounded IPP
wire format it accepts, independently of which product operations are
implemented. It supports:

- IPP versions, operation/status codes, request IDs, delimiter groups, and the
  end-of-attributes marker.
- Every IPP value tag and every out-of-band tag, including integer, enum,
  boolean, octet string, date-time, resolution, range-of-integer, collection,
  text/name with and without language, keyword, URI, URI scheme, charset,
  natural-language, MIME type, unsupported, unknown, no-value, not-settable,
  delete-attribute, and admin-define.
- Nested `begCollection`, `memberAttrName`, and `endCollection` values.
- Repeated attributes and repeated values encoded with zero-length names.
- Unknown or future tags and vendor-defined attributes without data loss.
- The original bounded message bytes in addition to the decoded representation.
- Explicit configurable limits for total message bytes, group and attribute
  counts, values per attribute, individual name/value bytes, total decoded
  bytes, and collection depth/member count.
- Precise errors for truncation, malformed lengths, illegal group structure,
  limit violations, and incomplete collections, without panics.

Encoding is canonical and deterministic for newly constructed messages. A
decoded message retains its original bytes for exact forwarding, comparison,
or local diagnostics; re-encoding is not falsely presented as byte-identical
when multiple legal encodings exist. Transport clients separately enforce HTTP
body and decompression limits before handing bytes to core.

The native HTTP boundary supports `ipp://` and `ipps://`, content-length and
chunked bodies, connection/read/write deadlines, certificate validation, and
optional explicitly supplied trust material. None of these concerns enter the
codec.

Use `ipp.rs` only as a development reference and differential-test oracle.
Representative requests and responses are generated or decoded by both
implementations and compared through fixtures. `ipp.rs` is not a production,
runtime, public, or target dependency of any shipped crate, including WASM.

## Protocol-neutral snapshot and evidence

Normalized data must not encode IPP as the universal shape. IPP attributes,
PJL variables, SNMP objects, and vendor replies feed protocol-neutral types:

```rust
pub struct DeviceSnapshot {
    pub identity: PrinterIdentity,
    pub state: DeviceState,
    pub supplies: Vec<Supply>,
    pub job_capabilities: Vec<JobCapability>,
    pub device_settings: Vec<DeviceSetting>,
    pub mutation_support: Vec<MutationSupport>,
    pub operations: Vec<OperationCapability>,
    pub observations: Vec<ProtocolObservation>,
}

pub struct JobCapability {
    pub id: CapabilityId,
    pub current_default: Option<SettingValue>,
    pub supported_values: Option<ValueConstraint>,
    pub format_scope: Option<DocumentFormat>,
    pub evidence: Vec<Evidence>,
}

pub struct DeviceSetting {
    pub id: SettingId,
    pub current_value: Option<SettingValue>,
    pub sensitive: bool,
    pub evidence: Vec<Evidence>,
}

pub struct MutationSupport {
    pub setting: SettingId,
    pub access: MutationAccess,
    pub constraints: Option<ValueConstraint>,
    pub evidence: Vec<Evidence>,
}

pub struct OperationCapability {
    pub operation: OperationId,
    pub availability: CapabilityAvailability,
    pub evidence: Vec<Evidence>,
}

pub struct ProtocolObservation {
    pub source: ProtocolSource,
    pub values: Vec<RawProtocolValue>,
    pub original_bytes: Option<BoundedBytes>,
    pub evidence: Evidence,
}
```

Job submission choices are `JobCapability`; persistent firmware configuration
is `DeviceSetting`; permission to change it is separate `MutationSupport`.
Advertised operations use `OperationCapability`. Raw IPP attributes remain in
an IPP `ProtocolObservation` rather than leaking IPP-specific fields into every
snapshot.

Evidence is split along three independent axes:

```rust
pub struct Evidence {
    pub source: ProtocolSource,
    pub kind: EvidenceKind,
    pub origin: ObservationOrigin,
}

pub enum ProtocolSource {
    IppAttribute { name: String },
    DnsSdTxt { key: String },
    Ieee1284DeviceId,
    PjlVariable { name: String },
    SnmpObject { oid: String },
    RegisteredProbe { probe_id: ProbeId },
    ModelCatalogue { catalogue: String },
}

pub enum EvidenceKind {
    Advertised,
    Observed,
    Inferred,
    HardwareQualified { qualification_id: String },
}

pub struct ObservationOrigin {
    pub agent_id: Option<AgentId>,
    pub printer_id: PrinterId,
    pub endpoint: EndpointIdentity,
    pub endpoint_generation: u64,
    pub transport: TransportKind,
    pub protocol: ProtocolFamily,
    pub request_id: RequestId,
    pub probe_id: Option<ProbeId>,
    pub observed_at: Timestamp,
    pub qualification: Option<QualificationMetadata>,
}
```

An inferred model-catalogue value never becomes advertised, observed, or
hardware-qualified without new evidence. Evidence remains attached when values
are normalized or merged; the cloud route is never recorded as the printer
transport.

## Device identity and observation merging

Use `printer-uuid` as the preferred network identity when it is valid and
stable. Supplement it with compatible serial numbers, IEEE 1284 device IDs,
USB descriptors, and other protocol-specific identifiers.

Merge observations only when one of these conditions holds:

1. The same validated device UUID is present.
2. Serial number and compatible device identity agree without contradictory
   UUID, vendor, or product evidence.
3. A user explicitly associates the endpoints, with the association retained
   as evidence and reversible.

An IP address, host name, DNS-SD service instance, USB path, Bluetooth address,
or model name alone is never sufficient. DHCP reuse, service renaming,
multi-function services, proxying, and endpoint rebinding must not collapse
different printers. Conflicting strong identifiers create an identity conflict
for user resolution rather than an automatic merge.

Each registered endpoint has a monotonically changing generation. Plans and
forwarded requests bind to that generation so replacing or rebinding an
endpoint invalidates prior authority.

## Discovery and visibility rules

Local IPP inspection starts with `Get-Printer-Attributes` and
`requested-attributes=all`, then performs focused group or attribute queries
and document-format-qualified queries when advertised data indicates they are
needed. The normalized result includes identity, device state, alerts,
supplies, trays, media, finishing, job capabilities, document formats,
security requirements, operation capabilities, and uninterpreted raw
attributes.

“All” is server-scoped, authorization-scoped, and firmware-scoped. The SDK must
describe an absent attribute as unobserved, not unsupported, unless protocol
evidence establishes lack of support. It must not claim that IPP can enumerate
hidden firmware settings. Known hidden-feature research proceeds only through
registered read-only probes with protocol, transport, device, and firmware
qualification.

Start without `document-format`, then query formats from
`document-format-supported` only where capabilities may vary. Preserve each
format-specific observation instead of overwriting the base snapshot.

Common semantic mappings include copies, sides, orientation, quality,
resolution, media, color mode, print scaling, formats, label darkness/speed,
and finishings. A mapping is emitted only from actual corresponding evidence.
Brother toner-save or eco mode must not be mapped to
`print-content-optimize`, whose semantics differ.

Sleep and power timers, quiet/eco modes, Wi-Fi and IP configuration, mail,
SNMP, certificates, calibration, maintenance, firmware administration,
sensors, alignment, physical controls, templates, and access restrictions
remain vendor-specific unless a device actually exposes a registered semantic
equivalent.

## Writability and confirmed changes

IPP discovery never writes. Discover persistent IPP writability only when
`Set-Printer-Attributes` is advertised and
`printer-settable-attributes-supported` is present and well formed. Missing,
malformed, contradictory, or unrecognized settable metadata means read-only.
Never infer persistent writability from a `*-default` attribute, from a value's
presence, from model data, or by sending a trial write.

When `Get-Printer-Supported-Values` is advertised, implement its request
and response semantics according to RFC 3380. It is an administrator operation
for obtaining the values accepted when setting settable `xxx-supported`
Printer attributes; it is not a general constraint query for arbitrary
settings. Preserve its one-set-of result semantics and the `admin-define`
out-of-band value. Do not invent a `*-supported` naming transformation or
treat the operation result as independent authorization for
`Set-Printer-Attributes`. Validate each requested change under the applicable
RFC 3380 rules, advertised syntax, returned constraints where relevant, and
local limits.

A confirmed change is a short-lived authorization artifact bound to:

```rust
pub struct ConfirmedChangePlan {
    pub printer_id: PrinterId,
    pub endpoint_generation: u64,
    pub setting: SettingId,
    pub expected_old_value_hash: ValueHash,
    pub requested_value: SettingValue,
    pub principal: PrincipalId,
    pub protocol: ProtocolFamily,
    pub expires_at: Timestamp,
}
```

Planning reads the current value, validates support and constraints, and shows
a dry-run diff. Applying a plan re-resolves printer ID, verifies endpoint
generation, principal, protocol, and expiry, then immediately re-reads the
value. A mismatched old-value hash or any changed binding rejects the plan as
stale. Only then may it write. The implementation reads back afterward and
reports verified success, verified failure, or ambiguous outcome.

Writes require explicit authorization at every boundary and never occur from
discover, inspect, list, or dry-run paths. A timeout, disconnect, cancellation,
or transport error after write transmission is an ambiguous write: never retry
it automatically. Authentication material and sensitive values are redacted
from traces, plans, errors, and ordinary serialized output.

## Probe registry

Probe descriptions contain data, limits, and a concrete kind, not serialized
decoder objects or code pointers:

```rust
pub struct ProbeDefinition {
    pub id: ProbeId,
    pub kind: ProbeKind,
    pub protocols: Vec<ProtocolFamily>,
    pub transports: Vec<TransportKind>,
    pub risk: ProbeRisk,
    pub limits: ProbeLimits,
    pub qualification: ProbeQualification,
}

pub enum ProbeKind {
    Ieee1284DeviceId,
    BrotherRasterStatus,
    BrotherSystemReport,
    BrotherWirelessStatus,
    PjlInfo(PjlInfoKind),
    PjlDinquire { variable: RegisteredPjlVariable },
    // New variants require review and fixtures.
}

pub enum ProbeRisk {
    ReadOnly,
    BenignStateChange,
    ConfigurationWrite,
    Destructive,
}
```

A registry keyed by `ProbeId` resolves immutable `ProbeDefinition` values.
Protocol modules own typed request encoding and response decoding selected by
`ProbeKind`; callers cannot inject a decoder or arbitrary payload. Automatic
execution is restricted to qualified `ReadOnly` probes whose identity,
protocol, transport, and firmware predicates match. Failures remain isolated
per probe and do not erase successful observations.

Initial Brother providers may include bounded IPP vendor attributes, DNS-SD
TXT, IEEE 1284 identity, USB descriptors and port status, Brother raster
status, Brother system/configuration reports, `OBJBRNET` wireless status, WLAN
scan results, and known PJL reads (`INFO ID`, `INFO STATUS`, `INFO CONFIG`,
`INFO VARIABLES`, and selected registered `DINQUIRE` variables). Discovery
must never issue USB soft reset, PJL `SET`/`DEFAULT`, reset, storage,
filesystem, firmware commands, or writes to unknown BLE characteristics.

The same protocol codec may run over USB bulk, raw TCP, IPP-over-USB, RFCOMM,
serial, or a known BLE profile when bidirectional behavior is qualified. SSIDs,
network addresses, usernames, certificate information, serial numbers, and
similar identifiers are sensitive. Passwords, credentials, and private keys
must never be returned or persisted.

## Agent/cloud protocol

Extend the existing protobuf/tonic agent session; a Rust enum is an internal
implementation detail, not the wire contract. Add versioned protobuf messages
for:

- Agent/session capability negotiation, including contract versions, protocol
  operations, transports, limits, and redaction features.
- A short-lived protocol request containing request ID, authenticated
  principal, published printer ID, endpoint generation, operation kind,
  deadline/timeout, and maximum response bytes.
- Explicit request acceptance or rejection before execution.
- Cancellation keyed by request ID.
- A terminal result with outcome, bounded response, evidence, redaction mode,
  and structured failure/ambiguity status.
- Timeout and response-limit enforcement reported distinctly from printer
  protocol errors.

Concrete messages can be named `AgentCapabilityAdvertisement`,
`ProtocolRequest`, `ProtocolRequestAccepted`, `ProtocolRequestRejected`,
`CancelProtocolRequest`, `ProtocolOperationLimits`, and `ProtocolResult`.
Names and fields evolve through protobuf compatibility rules and negotiated
versions rather than by serializing an internal Rust operation enum.

The printer field is always a published `PrinterId`, never an arbitrary host,
port, USB path, or Bluetooth address. The agent resolves it locally and checks
identity, endpoint generation, operation, protocol, advertised support, risk,
authorization, and limits. Cancellation is best effort and cannot convert an
ambiguous write into a safe retry.

The first cloud release ships only read-only `IppInspect`; it does not expose
generic read-setting, probe, plan-change, or apply-change operations. A
normalized `ReadSetting` operation is introduced with the later cloud settings
workflow, registered `RunProbe` only after the probe registry exists, and
confirmed writes only after local writes are qualified. No initial message
accepts arbitrary protocol bytes. The agent advertises capabilities when
connecting and when publishing a printer so unsupported agent versions are
rejected before work is accepted.

Forwarded evidence preserves this path:

```text
principal -> cloud request ID -> agent ID -> published printer ID
  -> endpoint generation -> transport -> protocol -> probe/request ID
  -> qualification -> observation
```

Read requests are short-lived and expire rather than being replayed as durable
jobs. The cloud does not impersonate the printer, weaken local authorization,
or become part of printer identity.

## Output, sensitivity, and retention

Define four deliberate output modes:

1. Normalized and redacted is the default for SDK, CLI, API, agent, and cloud.
2. Raw local protocol output requires an explicit opt-in and remains subject to
   byte/count limits and safe terminal handling.
3. Sensitive values require a separate, stronger opt-in in addition to any raw
   output opt-in, with local authorization and audit metadata.
4. Raw cloud responses require dedicated authorization, strict size and field
   limits, explicit audit events, and agent-side policy approval. They are
   returned ephemerally and are never persisted, cached, indexed, included in
   telemetry, or written to application/proxy logs.

Credentials, authentication headers, passwords, private keys, and unrestricted
captures are excluded in every mode. Redaction occurs before serialization or
upload, and tests cover both decoded fields and raw-response paths.

## Delivery phases

### Phase 1: bounded IPP codec

Implement the portable codec and limits in `mb-printer-core`, preserve original
messages and unknown data, and build differential fixtures against `ipp.rs`
without shipping it.

Acceptance:

- Decode the captured HL-L2375DW response without losing unknown attributes,
  repeated values, value tags, collections, or original bytes.
- Differential-test representative requests and responses against `ipp.rs`.
- Reject malformed, truncated, oversized, and over-nested messages without
  panics or unbounded allocation.
- Deterministic constructed-message encoding works on native and WASM targets.

### Phase 2: local IPP inspection

Add native HTTP/IPPS transport and local `IppInspect` SDK/CLI operations. Return
bounded decoded observations and apply output modes. This is the first
shippable milestone.

Acceptance:

- `requested-attributes=all` is described and tested as server-selected rather
  than exhaustive firmware discovery.
- The captured response and a fake IPP server produce deterministic local
  inspection output.
- Inspection performs no write and creates no hidden runtime.

### Phase 3: normalized model and evidence

Implement `JobCapability`, `DeviceSetting`, `MutationSupport`,
`OperationCapability`, `ProtocolObservation`, the three-part evidence model,
and identity reconciliation.

Acceptance:

- Model inference cannot be promoted without a protocol observation.
- Strong-identity matches merge; IP-, service-, or model-only matches do not.
- Identity conflicts are retained and surfaced.
- Normalized output keeps its raw protocol observation and full origin path.

### Phase 4: format-aware discovery

Add focused group queries, format qualification, and semantic normalization.
Represent base and format-specific capabilities separately.

Acceptance:

- Per-format differences never overwrite the base observation.
- Missing attributes remain unobserved rather than being presented as hidden
  settings or definite lack of support.
- Format-varying response/status cases and unsupported attributes have
  fixtures.

### Phase 5: cloud IPP inspection

Extend the protobuf/tonic session with capability negotiation, short-lived
request, acceptance, cancellation, result, deadline, and response-limit
messages. Enable only read-only `IppInspect`.

Acceptance:

- Arbitrary endpoints, unsupported agent versions, expired requests, stale
  endpoint generations, and oversized responses are rejected.
- Disconnect and cancellation produce explicit terminal state with no replay.
- Redaction occurs before cloud serialization and raw cloud data is neither
  persisted nor logged.

### Phase 6: guarded local IPP writes

Implement planning and application using
`printer-settable-attributes-supported`, RFC 3380 set-operation rules,
`Get-Printer-Supported-Values` where applicable, confirmed-plan bindings,
immediate pre-write reads, and post-write verification.

Acceptance:

- Missing or malformed writability metadata fails closed.
- RFC 3380 supported-value, `admin-define`, response-status, and unsupported
  attribute cases have fixtures.
- Fake-server scenarios cover success, stale old values, endpoint rebinding,
  authentication failure, unsupported values, partial failure, read-back
  mismatch, timeout, and an ambiguous write.
- No ambiguous write is automatically retried.

### Phase 7: guarded cloud IPP writes

Add separately negotiated protobuf messages for normalized `ReadSetting`,
planning, and applying confirmed IPP changes, plus dual cloud/agent
authorization, only after the local workflow is qualified.

Acceptance:

- Printer ID, endpoint generation, old-value hash, requested value, principal,
  protocol, and expiry survive the wire contract and are revalidated locally.
- Cancellation, disconnect, deadline, and lost-result scenarios cannot cause a
  write retry.
- Audit output is useful without exposing sensitive values.

### Phase 8: probe registry

Implement `ProbeKind`, the `ProbeId` registry, risk classification, protocol-
owned codecs, qualification predicates, and per-probe isolation. Initially
execute locally; after that path is qualified, add a negotiated `RunProbe`
message that accepts only a registered `ProbeId`, never arbitrary bytes.

Acceptance:

- Definitions cannot carry arbitrary request bytes or decoder objects.
- Every automatic probe has a documented read-only justification and frozen
  request/response fixtures.
- Insufficient identity, protocol, transport, or qualification skips the probe.

### Phase 9: Brother providers

Register the bounded Brother, IEEE 1284, report, wireless-status, and known PJL
read providers. Add normalized mappings only where semantics are established.

Acceptance:

- Hardware reports record identity, firmware, endpoint, probe IDs, bounded raw
  response hashes, redacted results, timing, limits, and confirmation that no
  configuration changed.
- Sensitive fields are redacted from default output and tracing.
- Probe failure does not discard other evidence.

### Phase 10: additional transports

Add IPP-over-USB, USB bulk, raw TCP, serial, RFCOMM, and known BLE profile
adapters. Expose Promise/`AbortSignal` wrappers for permitted WASM transports
and bounded native async adapters on caller-owned Tokio.

Acceptance:

- Blocking hardware work uses bounded workers plus device-level timeouts.
- Cancellation and idle completion are deterministic and bounded.
- Transport identity is not mistaken for printer identity or protocol.

### Phase 11: SNMP

Add SNMP as a native/agent-executed provider with allowlisted objects, bounded
walks, credential redaction, and protocol observations. Browser direct mode
reports it unavailable.

Acceptance:

- Only registered read objects are automatic.
- Walk, response, retry, and timeout limits are enforced.
- SNMP evidence participates in the same identity-conflict and redaction rules.

### Phase 12: qualified vendor writes

Consider vendor writes only as a separately reviewed project after read-only
providers are hardware-qualified. Each operation needs a known command, exact
model/firmware allowlist, explicit authorization and confirmation, stale-plan
protection, a reversible value where possible, read-back verification, and a
hardware qualification record. Blind hidden-feature probing remains out of
scope.

## Validation strategy

### Codec, fuzz, and differential tests

- Decode the captured HL-L2375DW response while preserving every unknown value
  and the original bounded bytes.
- Compare representative constructed and captured requests/responses with
  `ipp.rs` as a development-only oracle.
- Fuzz truncation at every boundary, unknown tags, repeated values, malformed
  lengths, oversized names/values/messages, and deeply nested collections.
- Property-test limit enforcement, deterministic encoding, and parser panic
  freedom.

### Integration and acceptance scenarios

- Fake HTTP/IPP endpoints cover chunked/content-length responses, IPPS trust,
  authorization, protocol errors, and format-dependent discovery.
- Identity tests cover UUID matches, compatible serial/device IDs, conflicts,
  DHCP/IP reuse, service-name collisions, explicit association, and endpoint
  rebinding.
- Change tests cover expired and stale plans, changed principals/protocols,
  ambiguous writes, post-write mismatch, and prevention of automatic retries.
- Agent-session tests cover unsupported versions, capability negotiation,
  acceptance/rejection, cancellation, deadline expiry, oversized responses,
  disconnects before and after acceptance, redaction, and raw-cloud retention
  prohibitions.
- Native/WASM tests demonstrate the shared codec, caller-owned runtimes,
  Promise/`AbortSignal` cancellation, bounded workers, and unavailable-capability
  reporting.

### Final document and dependency checks

- Confirm no unsuffixed settable-attribute name is used and only
  `printer-settable-attributes-supported` identifies writable attributes.
- Confirm the document makes no claim to implement all IPP operations and no
  claim that IPP can enumerate firmware settings it does not expose.
- Confirm `ipp.rs` appears only as a development reference and differential
  oracle, never as a production dependency.
- Inspect phase references and dependencies against the required delivery
  order: codec, local inspection, normalized model/evidence, format-aware
  discovery, cloud inspection, guarded local writes, guarded cloud writes,
  probe registry, Brother providers, additional transports, SNMP, then
  qualified vendor writes.
- Run `git diff --check`.

## Research baseline

The Brother Mass Deployment Tool 2.10.0.100 contains three shared schema
families: `pns_firmware_A` (approximately 1,485 leaf settings),
`pns_firmware_B` (approximately 133, including HL-L2375DW), and `lm_firmware`
(approximately 324). They are vocabulary and research inputs, not live
capability evidence, and must not be bundled without confirming licence and
redistribution terms.

A captured `Get-Printer-Attributes` response from a Brother HL-L2375DW is 8,421
bytes and includes identity, state, supply, media, resolution, duplex, format,
tray, firmware, and job-template data. That printer advertises
`Get-Printer-Attributes` but not `Set-Printer-Attributes`. The capture is a
valuable codec and normalization fixture; it does not establish visibility of
every firmware setting.
