# Async-First Transport Refactoring Plan

## Goal

Create one Rust implementation of plan execution that works with native and browser transports. Native callers get an async API and an optional blocking facade. Browser callers get a Promise-based API backed by the same Rust executor.

Backward compatibility is not required. The final architecture is:

```text
mb-printer-core       Printer models, BLE profiles, plans, protocol generation
mb-printer-executor   Cross-platform async transport contract and executor
mb-printer-native     btleplug/TCP/USB/serial transports and blocking facade
mb-printer-wasm       JavaScript transport bridge and thin browser adapters
```

The finished SDK has:

- one plan executor in Rust;
- one async transport contract;
- platform-specific concrete transports;
- no hidden or nested Tokio runtime;
- no duplicated TypeScript execution policy;
- model-owned FF02/FF03 configuration;
- optional FF03 subscription;
- write-without-response enforced for BLE.

## 1. Add a cross-platform executor crate

Create:

```text
crates/mb-printer-executor/
|-- Cargo.toml
|-- LICENSE
|-- README.md
`-- src/
    |-- lib.rs
    |-- error.rs
    |-- executor.rs
    |-- replay.rs
    `-- transport.rs
```

Add it to the workspace. It depends on:

- `mb-printer-core`;
- `thiserror`;
- `tracing`;
- `futures-util` for cancellation races;
- `web-time` for a monotonic `Instant` that works on native and Wasm.

It must compile on native and `wasm32-unknown-unknown`. It must not depend on Tokio, wasm-bindgen, btleplug, rusb, or serialport.

### 1.1 Future type

Use an object-safe boxed future because the Wasm bridge needs dynamic dispatch. Native futures are `Send`; browser futures may be `!Send`.

```rust
#[cfg(not(target_arch = "wasm32"))]
pub type TransportFuture<'a, T> =
    Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(target_arch = "wasm32")]
pub type TransportFuture<'a, T> =
    Pin<Box<dyn Future<Output = T> + 'a>>;
```

Define the trait with the same methods on both targets. The native definition additionally requires `Send`:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub trait Transport: Send {
    // Methods below.
}

#[cfg(target_arch = "wasm32")]
pub trait Transport {
    // Identical methods, without the Send supertrait.
}
```

Keep the duplicated declarations next to each other, or generate them with an internal macro, so their method signatures cannot drift.

### 1.2 Transport contract

```rust
pub trait Transport {
    fn payload_limit(&self) -> usize;

    fn command_limit(&self) -> usize {
        self.payload_limit()
    }

    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>>;

    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        kind: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>>;

    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>>;

    fn delay(
        &mut self,
        duration: Duration,
    ) -> TransportFuture<'_, ()>;

    fn disconnect(
        &mut self,
    ) -> TransportFuture<'_, Result<(), TransportError>>;
}
```

Supporting types:

```rust
pub enum WriteKind {
    Command,
    Raster,
}

pub enum NotificationSupport {
    Subscribed,
    Unavailable,
}

pub enum WaitOutcome {
    Response(Vec<u8>),
    Timeout,
    Unavailable,
}
```

`WriteKind` preserves the independent USB command and raster limits.

`delay` remains on the transport deliberately. It lets Tokio, browser, and deterministic fake transports provide their own non-blocking timer without adding a runtime dependency to the executor crate. The executor uses `web_time::Instant` only for monotonic response deadlines.

### 1.3 Structured transport errors

Replace `Result<_, String>` with:

```rust
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
}

pub enum TransportErrorKind {
    Connection,
    Disconnected,
    Unsupported,
    InvalidConfiguration,
    PermissionDenied,
    Timeout,
    Cancelled,
    Io,
}
```

Transport errors must not contain credentials, printer payload bytes, or unfiltered platform debug data.

### 1.4 Awaitable cancellation

A boolean alone cannot interrupt an in-flight operation. Define the same API on both targets; require `Send + Sync` on native and no thread-safety bound on Wasm:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub trait Cancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
    fn cancelled(&self) -> TransportFuture<'_, ()>;
}

#[cfg(target_arch = "wasm32")]
pub trait Cancellation {
    fn is_cancelled(&self) -> bool;
    fn cancelled(&self) -> TransportFuture<'_, ()>;
}

pub struct NeverCancelled;
```

`NeverCancelled::cancelled()` remains pending forever. Provide an atomic/waker-based native token in `mb-printer-executor`; the Wasm crate supplies an `AbortSignal` implementation.

The executor races every subscription, write, response wait, and delay against `cancelled()`. If cancellation wins, it drops the operation future and stops the plan.

Dropping a write future cannot prove that the device accepted nothing, so cancellation while a write is in flight is always outcome-unknown.

### Phase 1 acceptance

- The crate builds for native and Wasm.
- A native fake transport is statically asserted to be `Send`.
- A Wasm fake containing `JsValue` can implement `Transport` without `Send`.
- `Transport` is usable through `dyn Transport`.
- Cancellation can wake an executor blocked in a fake response wait.

## 2. Move execution into the shared crate

Move these types and helpers from `mb-printer-native/src/lib.rs`:

- `Progress`;
- `ReferenceTiming`;
- `ExecuteError`;
- response collection;
- response validation;
- execution tracing;
- `ReplayGuard`, `ReplayStore`, and `MemoryReplayStore`.

Keep `Plan`, `Action`, and `ResponseValidation` in `mb-printer-core`.

### 2.1 Canonical API

Retain the familiar Rust result shape:

```rust
pub async fn execute<T>(
    plan: &Plan,
    transport: &mut T,
) -> Result<Progress, ExecuteError>
where
    T: Transport + ?Sized;
```

`execute` uses `ReferenceTiming::Preserve` and `NeverCancelled`.

Provide:

```rust
pub async fn execute_with_options<T, F>(
    plan: &Plan,
    transport: &mut T,
    options: ExecutionOptions<'_>,
    progress: F,
) -> Result<Progress, ExecuteError>
where
    T: Transport + ?Sized,
    F: FnMut(&Progress);
```

On native, additionally require `F: Send`; Wasm does not require it. This keeps native execution futures spawnable without excluding JavaScript callbacks.

```rust
pub struct ExecutionOptions<'a> {
    pub timing: ReferenceTiming,
    pub cancellation: &'a dyn Cancellation,
}
```

Do not add a second `ExecutionOutcome` or `ExecutionReport` hierarchy. `Progress` plus `ExecuteError` remains the single Rust contract.

### 2.2 Error semantics

Add precise cancellation and uncertain-write variants:

```rust
pub enum ExecuteError {
    AtomicTooLarge { /* existing fields */ },
    InvalidPlan { /* existing fields */ },
    Replay(String),
    ReplayStore(String),
    Cancelled { progress: Progress },
    WriteOutcomeUnknown {
        progress: Progress,
        source: Option<TransportError>,
    },
    Transport { progress: Progress, source: TransportError },
    Timeout { progress: Progress },
    Response { progress: Progress, message: String },
}
```

Rules:

- cancellation before or between transport effects returns `Cancelled`;
- callers inspect `progress.bytes_written` to distinguish before-send from partial cancellation;
- cancellation or failure while a write future is pending returns `WriteOutcomeUnknown`;
- `TransportErrorKind::Cancelled` maps to `Cancelled` for non-write effects and `WriteOutcomeUnknown` for writes;
- a completed write increments `bytes_written`;
- the executor never retries automatically.

The Wasm boundary maps this canonical result into its JavaScript execution status. It does not invent separate execution semantics.

### 2.3 Whole-plan preflight

Before the first transport operation, validate:

- positive payload and command limits;
- every atomic command fits `command_limit`;
- every logical raster chunk is positive;
- response collection timeouts and maximum sizes are positive;
- variable-length validators use `CollectResponse`;
- plan action count and retained response bytes are bounded.

No subscribe, delay, or write may occur until the complete plan passes preflight.

### 2.4 Async action loop

Await each physical effect. Preserve logical raster boundaries:

```text
Action::RasterWrite bytes
  -> logical_chunk slices
    -> payload_limit slices
      -> write(WriteKind::Raster)
      -> delay(reference pacing)
```

Before polling a write future, set:

```rust
progress.potentially_accepted_write = true;
```

Increment `bytes_written` only after the write returns success.

Check cancellation before every action and every physical write. Race cancellation against all awaited transport effects.

### 2.5 Response collection

Convert the existing helpers to async functions while preserving:

- the 16-read fixed-response ceiling;
- the 4096-read multipart ceiling;
- total and idle timeouts;
- maximum retained bytes;
- Phomemo `0x1a` frame alignment;
- exact Brother 32-byte status validation;
- Brother best-effort status behavior;
- fallback delays.

Use `web_time::Instant` for total deadlines and `Transport::delay` for pacing. Fake transports record requested delays; response tests use zero or minimal timeouts rather than wall-clock sleeps.

### 2.6 Replay storage

Keep `MemoryReplayStore` in the shared crate. Keep `FileReplayStore` in `mb-printer-native`, because durable filesystem claims are native behavior.

Claim a replay key before execution. A claimed key remains claimed after cancellation, timeout, disconnect, or an uncertain write.

### Phase 2 acceptance

Port the behavioral tests from:

- `mb-printer-native/tests/execution.rs`;
- `mb-printer-native/tests/executor_contract.rs`.

The async executor must produce identical write boundaries, delay requests, responses, progress, and validation decisions.

## 3. Add explicit BLE model capabilities

Add `uuid` with serde support to `mb-printer-core`.

Use a required enum instead of `Option`. This makes an omitted `ble` field a deserialization error and distinguishes unsupported models from configured GATT models.

```rust
pub struct PrinterDefinition {
    // Existing required fields.
    pub ble: BleSupport,
}

#[serde(tag = "kind", content = "capabilities", rename_all = "kebab-case")]
pub enum BleSupport {
    Unsupported,
    Gatt(BleGattCapabilities),
}

pub struct BleGattCapabilities {
    pub write_characteristic: Uuid,
    pub write_type: BleWriteType,
    pub notification: Option<BleNotification>,
}

pub enum BleWriteType {
    WithoutResponse,
}

pub struct BleNotification {
    pub characteristic: Uuid,
    pub requirement: NotificationRequirement,
}

pub enum NotificationRequirement {
    Optional,
    Required,
}
```

Use `#[serde(deny_unknown_fields, rename_all = "camelCase")]` on structs and the existing kebab-case convention on enums.

Unsupported model JSON:

```json
"ble": {
  "kind": "unsupported"
}
```

FF02/FF03 model JSON:

```json
"ble": {
  "kind": "gatt",
  "capabilities": {
    "writeCharacteristic": "0000ff02-0000-1000-8000-00805f9b34fb",
    "writeType": "without-response",
    "notification": {
      "characteristic": "0000ff03-0000-1000-8000-00805f9b34fb",
      "requirement": "optional"
    }
  }
}
```

Add:

```rust
impl PrinterDefinition {
    pub fn ble_gatt(&self) -> Option<&BleGattCapabilities>;
}
```

Populate profiles from an explicit reviewed list of compatible model IDs. Do not infer BLE support solely from the print protocol.

Tests must verify:

- every catalogue entry contains `ble` in the raw JSON;
- missing `ble` is rejected;
- UUIDs deserialize and serialize canonically;
- reviewed FF02/FF03 models have the exact profile;
- unsupported models remain unsupported;
- Wasm `capabilities_json()` exposes the same data.

## 4. Replace the native BLE hierarchy

Remove after the new implementation passes its tests:

- `BleGattBackend`;
- synchronous `BleTransport`;
- `BtleplugBackend`;
- the old `BtleplugTransport` alias;
- `connect_btleplug`;
- synchronous compatibility errors.

Rename `AsyncBtleplugTransport` to `BtleplugTransport`.

### 4.1 Connection API

```rust
pub struct BtleplugConnectOptions {
    pub scan_timeout: Duration,
    pub payload_limit: NonZeroUsize,
}

impl BtleplugTransport {
    pub async fn connect(
        address: &str,
        capabilities: &BleGattCapabilities,
        options: BtleplugConnectOptions,
    ) -> Result<Self, TransportError>;
}
```

Catalogue callers obtain `BleGattCapabilities` from `PrinterDefinition::ble_gatt()`. Non-catalogue callers can construct the same type explicitly. Do not expose a second constructor with unrelated UUID arguments.

### 4.2 Characteristic selection

The write characteristic must match both:

```text
UUID == write_characteristic
AND properties contain WRITE_WITHOUT_RESPONSE
```

Never fall back to `WRITE` or an arbitrary writable characteristic.

For notifications:

- no configured notification means unavailable;
- an optional characteristic missing at runtime means unavailable;
- a required characteristic missing at runtime is a connection error;
- a matching characteristic with incompatible properties is a profile error;
- subscription is attempted only when the plan requests it.

### 4.3 Notification lifecycle

Track:

```rust
enum NotificationState {
    Unsupported,
    Available,
    Subscribed,
    Disconnected,
}
```

Requirements:

- subscription is idempotent;
- waiting before subscription returns `Unavailable`;
- waiting without FF03 returns `Unavailable`;
- timeout and unavailable remain distinct;
- queued notifications are bounded;
- disconnect wakes pending waits;
- the forwarding task is stored and aborted during disconnect/drop;
- disconnect is idempotent.

### 4.4 Implement the shared trait

Implement `mb_printer_executor::Transport` directly:

- write using `WriteType::WithoutResponse` only;
- validate payload length before btleplug is called;
- use `tokio::time::sleep` for `delay`;
- use `tokio::time::timeout` for response waits;
- return structured errors.

Tests use the injectable async backend and cover:

- exact FF02 selection;
- rejection of arbitrary writable characteristics;
- rejection of write-with-response-only FF02;
- optional FF03 present and absent;
- required FF03 absent;
- invalid notification properties;
- wait before subscription;
- notification, timeout, and unavailable outcomes;
- oversized writes;
- disconnect during a wait;
- bounded notification queues.

## 5. Convert the remaining native transports

Implement the shared async trait for all transports before deleting the old executor.

### 5.1 Naturally async transports

- Replace `std::net::TcpStream` with Tokio TCP.
- Use existing async network implementations where available.
- Keep btleplug on the caller-owned Tokio runtime.

### 5.2 Blocking USB and serial transports

Do not issue blocking hardware calls directly from async futures. Use one persistent worker thread per connected device:

```text
async Transport
    -> bounded command channel
    -> device worker thread
    -> one-shot response channel
```

This keeps device ownership and write order stable and avoids one `spawn_blocking` task per raster chunk. Reuse the limits and error concepts in `src/workers.rs`.

### 5.3 File transport

Use Tokio file I/O or a small persistent worker. Do not block an executor worker thread.

Run the same executor contract suite against every native transport fake.

## 6. Replace the TypeScript executor

`crates/mb-printer-wasm/browser-adapters.ts` currently duplicates preflight, chunking, validation, timing, and progress policy. Move those semantics to Rust.

Keep TypeScript responsible only for browser I/O:

- `WebBluetoothTransport`;
- `WebUsbTransport`;
- `WebSerialTransport`;
- browser IPP fetch support.

Remove TypeScript definitions and code for:

- plan execution;
- plan preflight;
- raster splitting;
- response validation;
- Phomemo frame collection;
- executor progress state.

### 6.1 JavaScript transport bridge

Add `crates/mb-printer-wasm/src/transport.rs`. Declare the structural `BrowserTransport` with wasm-bindgen and wrap returned promises using `wasm_bindgen_futures::JsFuture`.

Add target-specific dependencies:

- `wasm-bindgen-futures`;
- `web-sys` features for `AbortSignal` and required DOM types;
- relevant `js-sys` types.

`JsTransport` holds the JavaScript transport and optional `AbortSignal`, implements the shared `Transport` trait, and passes the signal to every browser operation.

Add an `AbortSignalCancellation` implementation that resolves `cancelled()` from the signal's abort event. The Rust executor both observes the signal and races it against each pending operation.

### 6.2 Wasm executor API

```rust
#[wasm_bindgen(js_name = executePlan)]
pub async fn execute_plan(
    plan_json: &str,
    transport: BrowserTransport,
    timing: JsValue,
    signal: Option<web_sys::AbortSignal>,
    on_progress: Option<js_sys::Function>,
) -> Result<JsValue, JsValue>;
```

The function maps the canonical Rust result as follows:

- success -> `completed`;
- `Cancelled` with zero bytes -> `cancelled-before-send`;
- `Cancelled` with written bytes -> `cancelled-partial`;
- `WriteOutcomeUnknown` -> `outcome-unknown`;
- other execution errors -> a failed result containing progress and a stable error code;
- malformed input or invalid options -> rejected Promise.

Invoke `on_progress` after the same action boundaries as native execution.

### 6.3 Web Bluetooth rules

Make the notification characteristic optional. `subscribeNotifications()` returns `false` when it is absent.

Remove the current fallback from `writeValueWithoutResponse` to `writeValue`. If the former is unavailable, fail with a stable unsupported-profile error.

Run the existing Node and browser fixtures against the Rust/Wasm executor before deleting the TypeScript executor.

## 7. Add a native blocking facade

Add an optional `blocking` feature to `mb-printer-native`:

```toml
blocking = ["dep:tokio"]
```

`mb-printer-executor` is a normal dependency, not part of the feature list.

Add `src/blocking.rs`. Use a dedicated worker thread that owns:

- one current-thread Tokio runtime;
- the async transport;
- the shared async executor;
- an async command loop.

The caller communicates through bounded standard channels. Never call `Handle::block_on` on the caller's runtime and never create a runtime per method.

```rust
pub struct BlockingPrinterClient {
    commands: SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl BlockingPrinterClient {
    pub fn connect_btleplug(
        address: String,
        capabilities: BleGattCapabilities,
        options: BtleplugConnectOptions,
    ) -> Result<Self, TransportError>;

    pub fn execute(&mut self, plan: Plan) -> Result<Progress, ExecuteError>;

    pub fn execute_with_progress<F>(
        &mut self,
        plan: Plan,
        progress: F,
    ) -> Result<Progress, ExecuteError>
    where
        F: FnMut(&Progress);

    pub fn disconnect(mut self) -> Result<(), TransportError>;
}
```

Progress events travel back over a channel; callbacks execute on the caller thread.

Explicit `disconnect` performs protocol shutdown and joins the worker. `Drop` sends a best-effort shutdown command and waits for an acknowledgement with `recv_timeout`; if it expires, it detaches the worker rather than attempting an unbounded join. Store the handle in `Option` so explicit shutdown can take ownership safely.

Test the blocking facade:

- from ordinary synchronous code;
- while a Tokio runtime already exists on the caller thread;
- with progress callbacks;
- during response timeout and cancellation;
- after worker termination;
- during explicit and best-effort shutdown.

## 8. Buildable migration sequence

Do not move or delete the current executor at the start. Run the old and new implementations in parallel until every transport is converted.

Use this sequence:

1. Add required BLE capability types and catalogue data.
2. Add `mb-printer-executor` with a temporary `AsyncTransport` trait name.
3. Copy execution behavior into the async executor; leave the old native executor intact.
4. Port contract tests and run them against both executors.
5. Add the new btleplug implementation and model-aware connection API.
6. Convert TCP, USB, serial, and file transports to `AsyncTransport`.
7. Add the Wasm JavaScript bridge and run browser parity tests.
8. Switch native async and Wasm public entry points to the shared executor.
9. Add and test the blocking facade.
10. In one cutover commit, remove the old Rust and TypeScript executors and rename `AsyncTransport` to `Transport`.
11. Remove obsolete aliases, tests, imports, and feature paths.
12. Update examples and documentation.

Every commit must build. No deprecated compatibility layer remains after the cutover.

## 9. Verification

Target test layout:

```text
crates/mb-printer-executor/tests/
|-- execution.rs
|-- executor_contract.rs
|-- response_collection.rs
|-- cancellation.rs
`-- fixtures/execution-contract.json

crates/mb-printer-native/tests/
|-- btleplug_transport.rs
|-- blocking_client.rs
|-- usb_transport.rs
|-- serial_transport.rs
`-- tcp_transport.rs

crates/mb-printer-wasm/tests/
|-- executor_bridge.rs
`-- equivalence.rs
```

Required commands:

```console
cargo fmt --all --check
cargo test --workspace
cargo test -p mb-printer-native --features ble
cargo test -p mb-printer-native --features ble,blocking
cargo test -p mb-printer-native --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check -p mb-printer-executor --target wasm32-unknown-unknown
cargo check -p mb-printer-wasm --target wasm32-unknown-unknown
npm run check:types --prefix crates/mb-printer-wasm
npm run test:schema --prefix crates/mb-printer-wasm
npm run test:wasm-node --prefix crates/mb-printer-wasm
npm run test:wasm-browser --prefix crates/mb-printer-wasm
npm run build --prefix crates/mb-printer-wasm
```

If an aggregate `npm test` command is desired, add it explicitly to `package.json`; do not reference a nonexistent script.

## 10. Documentation examples

Native async:

```rust
let printer = capabilities::by_id("m02").unwrap();
let ble = printer.ble_gatt().ok_or("model does not support BLE")?;

let mut transport =
    BtleplugTransport::connect(address, ble, options).await?;

let progress = mb_printer_executor::execute(&plan, &mut transport).await?;
transport.disconnect().await?;
```

Native blocking:

```rust
let mut client =
    BlockingPrinterClient::connect_btleplug(address, ble.clone(), options)?;

let progress = client.execute(plan)?;
client.disconnect()?;
```

Document these guarantees:

- async is the canonical API;
- blocking is native-only and uses a dedicated worker runtime;
- catalogue models own their GATT profile;
- FF02 is always used with write-without-response;
- FF03 subscription is optional when declared optional by the model;
- cancellation never causes an automatic retry;
- reconnect and retry decisions belong to the caller.

## Definition of done

The refactor is complete when:

- one Rust implementation owns preflight, chunking, pacing, validation, progress, response collection, and cancellation;
- native async, native blocking, and browser execution pass the same contract fixtures;
- `BtleplugTransport` executes plans through the shared executor;
- catalogue callers never supply FF02 or FF03 manually;
- FF02 requires and uses write-without-response;
- optional FF03 absence produces `Unavailable`, not a connection failure;
- cancellation interrupts pending waits and delays;
- cancellation during a write produces `WriteOutcomeUnknown`;
- native transport and executor futures satisfy their documented `Send` bounds;
- no hidden or nested Tokio runtime exists;
- the TypeScript executor is removed;
- all native and Wasm verification commands pass;
- no legacy synchronous transport or executor API remains.
