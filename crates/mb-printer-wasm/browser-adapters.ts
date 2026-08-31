// SPDX-License-Identifier: AGPL-3.0-or-later
/** Browser-side executor for plans emitted by mb-printer-core. */
export type PlanAction =
  | { action: "job-boundary"; kind: "start" | "end" }
  | { action: "subscribe-notifications" }
  | { action: "command-write"; name: string; bytes: number[]; atomic: boolean }
  | { action: "raster-write"; bytes: number[]; logical_chunk: number; delay_after_each_physical_write_ms: number }
  | { action: "delay"; milliseconds: number }
  | { action: "wait-for-response"; timeout_ms: number; fallback_delay_ms: number; validation: string };
export type ResponseWait = { kind: "response"; bytes: Uint8Array } | { kind: "timeout" } | { kind: "unavailable" };
export interface BrowserTransport {
  readonly payloadLimit: number;
  /** Atomic command limit; defaults to payloadLimit for BLE/stream transports. */
  readonly commandPayloadLimit?: number;
  subscribeNotifications(signal?: AbortSignal): Promise<boolean>;
  write(bytes: Uint8Array, signal?: AbortSignal, kind?: "command" | "raster"): Promise<void>;
  waitForResponse(timeoutMs: number, signal?: AbortSignal): Promise<ResponseWait>;
}
export interface BluetoothWritableCharacteristic extends EventTarget {
  startNotifications(): Promise<unknown>;
  writeValueWithoutResponse?(bytes: BufferSource): Promise<void>;
  writeValue?(bytes: BufferSource): Promise<void>;
  value?: DataView | null;
}
const abortError = () => new DOMException("Operation aborted", "AbortError");
const checkAbort = (signal?: AbortSignal) => { if (signal?.aborted) throw abortError(); };
const isAbort = (error: unknown) => error instanceof DOMException && error.name === "AbortError";
const raceAbort = <T>(operation: Promise<T>, signal?: AbortSignal): Promise<T> => {
  if (!signal) return operation;
  checkAbort(signal);
  return new Promise((resolve, reject) => {
    const abort = () => reject(abortError());
    signal.addEventListener("abort", abort, { once: true });
    operation.then(
      value => { signal.removeEventListener("abort", abort); resolve(value); },
      error => { signal.removeEventListener("abort", abort); reject(error); },
    );
  });
};

export interface PortableIppCodec {
  encodeIpp(messageJson: string, maximumMessageBytes: number): Uint8Array;
  decodeIpp(message: Uint8Array, maximumMessageBytes: number): string;
}

export interface BrowserIppLimits {
  maximumRequestBytes: number;
  maximumResponseBytes: number;
}

export type BrowserProtocol = "ipp" | "webusb" | "webbluetooth" | "webserial" | "snmp";
export interface BrowserProtocolAvailability { available: boolean; reason?: string }

/** Browser capability reporting is explicit; browsers do not expose UDP for SNMP. */
export function browserProtocolAvailability(protocol: BrowserProtocol): BrowserProtocolAvailability {
  if (protocol === "snmp") return {
    available: false,
    reason: "SNMP requires UDP and is unavailable in browser runtimes",
  };
  return { available: true };
}

/**
 * Promise/AbortSignal IPP adapter for browser-permitted HTTP endpoints. CORS,
 * mixed-content, and local-network permission failures are surfaced to the
 * caller; this function never falls back to a raw socket or proxy.
 */
export async function inspectIppOverFetch(endpoint: string, requestJson: string,
  codec: PortableIppCodec, limits: BrowserIppLimits, signal?: AbortSignal,
  fetchImpl: typeof globalThis.fetch = globalThis.fetch.bind(globalThis)): Promise<string> {
  checkAbort(signal);
  if (![limits.maximumRequestBytes, limits.maximumResponseBytes]
    .every(value => Number.isSafeInteger(value) && value >= 8)) throw new Error("invalid IPP limits");
  const url = new URL(endpoint);
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("browser IPP requires HTTP or HTTPS");
  const request = codec.encodeIpp(requestJson, limits.maximumRequestBytes);
  if (request.byteLength > limits.maximumRequestBytes) throw new Error("IPP request exceeds configured limit");
  const response = await raceAbort(fetchImpl(url, {
    method: "POST",
    headers: { "Accept": "application/ipp", "Content-Type": "application/ipp" },
    body: request.slice().buffer,
    signal,
    cache: "no-store",
  }), signal);
  if (!response.ok) throw new Error(`IPP HTTP response status ${response.status}`);
  const declared = response.headers.get("content-length");
  if (declared !== null && Number(declared) > limits.maximumResponseBytes) throw new Error("IPP response exceeds configured limit");
  if (!response.body) {
    const bytes = new Uint8Array(await raceAbort(response.arrayBuffer(), signal));
    if (bytes.byteLength > limits.maximumResponseBytes) throw new Error("IPP response exceeds configured limit");
    return codec.decodeIpp(bytes, limits.maximumResponseBytes);
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const item = await raceAbort(reader.read(), signal);
      if (item.done) break;
      length += item.value.byteLength;
      if (length > limits.maximumResponseBytes) {
        await reader.cancel("IPP response limit exceeded");
        throw new Error("IPP response exceeds configured limit");
      }
      chunks.push(item.value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return codec.decodeIpp(bytes, limits.maximumResponseBytes);
}

/** Thin WebBluetooth adapter; discovery and permission remain with the app. */
export class WebBluetoothTransport implements BrowserTransport {
  private readonly replies: Uint8Array[] = [];
  private readonly listeners: Array<(value: Uint8Array) => void> = [];
  private subscribed = false;
  constructor(private readonly writable: BluetoothWritableCharacteristic,
    private readonly notifications: BluetoothWritableCharacteristic, public readonly payloadLimit = 512) {}
  async subscribeNotifications(signal?: AbortSignal): Promise<boolean> {
    checkAbort(signal);
    if (!this.subscribed) {
      this.notifications.addEventListener("characteristicvaluechanged", () => {
        const view = this.notifications.value;
        if (!view) return;
        const bytes = new Uint8Array(view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength));
        const listener = this.listeners.shift();
        if (listener) listener(bytes); else this.replies.push(bytes);
      });
      await raceAbort(this.notifications.startNotifications(), signal);
      this.subscribed = true;
    }
    return true;
  }
  async write(bytes: Uint8Array, signal?: AbortSignal): Promise<void> {
    checkAbort(signal);
    if (bytes.length > this.payloadLimit) throw new Error("WebBluetooth payload exceeds negotiated limit");
    const payload = Uint8Array.from(bytes).buffer;
    const operation = this.writable.writeValueWithoutResponse ? this.writable.writeValueWithoutResponse(payload)
      : this.writable.writeValue ? this.writable.writeValue(payload)
      : Promise.reject(new Error("Bluetooth characteristic is not writable"));
    await raceAbort(operation, signal);
  }
  async waitForResponse(timeoutMs: number, signal?: AbortSignal): Promise<ResponseWait> {
    checkAbort(signal);
    if (!this.subscribed) return { kind: "unavailable" };
    const queued = this.replies.shift();
    if (queued) return { kind: "response", bytes: queued };
    return new Promise((resolve, reject) => {
      let settled = false;
      const receive = (bytes: Uint8Array) => { if (!settled) { settled = true; cleanup(); resolve({ kind: "response", bytes }); } };
      const abort = () => { if (!settled) { settled = true; remove(); reject(abortError()); } };
      const remove = () => { const i = this.listeners.indexOf(receive); if (i >= 0) this.listeners.splice(i, 1); };
      const cleanup = () => { remove(); signal?.removeEventListener("abort", abort); };
      this.listeners.push(receive);
      signal?.addEventListener("abort", abort, { once: true });
      globalThis.setTimeout(() => { if (!settled) { settled = true; cleanup(); resolve({ kind: "timeout" }); } }, timeoutMs);
    });
  }
}

export interface UsbTransferResult { data?: DataView | null }
export interface WebUsbDeviceLike {
  transferOut(endpointNumber: number, data: BufferSource): Promise<unknown>;
  transferIn(endpointNumber: number, length: number): Promise<UsbTransferResult>;
}
/** Thin WebUSB adapter; opening and interface selection remain with the app. */
export class WebUsbTransport implements BrowserTransport {
  constructor(private readonly device: WebUsbDeviceLike, private readonly outEndpoint: number,
    private readonly inEndpoint: number | undefined, public readonly payloadLimit = 512,
    private readonly responseLength = 64, public readonly commandPayloadLimit = payloadLimit) {}
  async subscribeNotifications(signal?: AbortSignal): Promise<boolean> { checkAbort(signal); return this.inEndpoint !== undefined; }
  async write(bytes: Uint8Array, signal?: AbortSignal, kind: "command" | "raster" = "raster"): Promise<void> {
    checkAbort(signal);
    const limit = kind === "command" ? this.commandPayloadLimit : this.payloadLimit;
    if (bytes.length > limit) throw new Error(`WebUSB ${kind} payload exceeds configured limit`);
    await raceAbort(this.device.transferOut(this.outEndpoint, Uint8Array.from(bytes).buffer), signal);
  }
  async waitForResponse(timeoutMs: number, signal?: AbortSignal): Promise<ResponseWait> {
    checkAbort(signal);
    if (this.inEndpoint === undefined) return { kind: "unavailable" };
    let timer: ReturnType<typeof globalThis.setTimeout> | undefined;
    const timeout = new Promise<ResponseWait>(resolve => { timer = globalThis.setTimeout(() => resolve({ kind: "timeout" }), timeoutMs); });
    const transfer = this.device.transferIn(this.inEndpoint, this.responseLength).then((result): ResponseWait => {
      const view = result.data;
      return { kind: "response", bytes: view ? new Uint8Array(view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength)) : new Uint8Array() };
    });
    try { return await raceAbort(Promise.race([transfer, timeout]), signal); }
    finally { if (timer !== undefined) globalThis.clearTimeout(timer); }
  }
}

export interface WebSerialPortLike {
  readonly readable: ReadableStream<Uint8Array> | null;
  readonly writable: WritableStream<Uint8Array> | null;
}

/**
 * Thin Web Serial adapter. Port selection, permission, opening, baud rate,
 * and closing remain with the application. A single background reader avoids
 * abandoning pending reads when an individual response wait times out.
 */
export class WebSerialTransport implements BrowserTransport {
  private readonly replies: Uint8Array[] = [];
  private readonly listeners: Array<(value: Uint8Array) => void> = [];
  private readerStarted = false;
  private readerAvailable = true;

  constructor(private readonly port: WebSerialPortLike, public readonly payloadLimit = 1024,
    public readonly commandPayloadLimit = payloadLimit) {
    if (!Number.isSafeInteger(payloadLimit) || payloadLimit <= 0
      || !Number.isSafeInteger(commandPayloadLimit) || commandPayloadLimit <= 0) {
      throw new Error("invalid Web Serial payload limit");
    }
  }

  async subscribeNotifications(signal?: AbortSignal): Promise<boolean> {
    checkAbort(signal);
    if (!this.port.readable) return false;
    if (!this.readerStarted) {
      this.readerStarted = true;
      const reader = this.port.readable.getReader();
      void (async () => {
        try {
          while (true) {
            const item = await reader.read();
            if (item.done) break;
            if (item.value.byteLength === 0) continue;
            const value = Uint8Array.from(item.value);
            const listener = this.listeners.shift();
            if (listener) listener(value); else this.replies.push(value);
          }
        } catch {
          // Transport failures are exposed as unavailable to pending/future waits.
        } finally {
          this.readerAvailable = false;
          reader.releaseLock();
          for (const listener of this.listeners.splice(0)) listener(new Uint8Array());
        }
      })();
    }
    return this.readerAvailable;
  }

  async write(bytes: Uint8Array, signal?: AbortSignal, kind: "command" | "raster" = "raster"): Promise<void> {
    checkAbort(signal);
    const limit = kind === "command" ? this.commandPayloadLimit : this.payloadLimit;
    if (bytes.length > limit) throw new Error(`Web Serial ${kind} payload exceeds configured limit`);
    if (!this.port.writable) throw new Error("Web Serial port is not writable");
    const writer = this.port.writable.getWriter();
    try { await raceAbort(writer.write(Uint8Array.from(bytes)), signal); }
    finally { writer.releaseLock(); }
  }

  async waitForResponse(timeoutMs: number, signal?: AbortSignal): Promise<ResponseWait> {
    checkAbort(signal);
    if (!this.readerStarted || !this.readerAvailable) return { kind: "unavailable" };
    const queued = this.replies.shift();
    if (queued) return { kind: "response", bytes: queued };
    return new Promise((resolve, reject) => {
      let settled = false;
      const receive = (bytes: Uint8Array) => {
        if (settled) return;
        settled = true;
        cleanup();
        resolve(bytes.byteLength === 0 && !this.readerAvailable
          ? { kind: "unavailable" }
          : { kind: "response", bytes });
      };
      const abort = () => {
        if (settled) return;
        settled = true;
        remove();
        reject(abortError());
      };
      const remove = () => {
        const index = this.listeners.indexOf(receive);
        if (index >= 0) this.listeners.splice(index, 1);
      };
      const cleanup = () => {
        globalThis.clearTimeout(timer);
        remove();
        signal?.removeEventListener("abort", abort);
      };
      const timer = globalThis.setTimeout(() => {
        if (settled) return;
        settled = true;
        cleanup();
        resolve({ kind: "timeout" });
      }, timeoutMs);
      this.listeners.push(receive);
      signal?.addEventListener("abort", abort, { once: true });
    });
  }
}

export interface ExecutionProgress { lastCompletedAction: number; bytesWritten: number; potentiallyAcceptedWrite: boolean }
export type ExecutionStatus = "completed" | "cancelled-before-send" | "cancelled-partial" | "outcome-unknown";
export interface ExecutionResult extends ExecutionProgress { status: ExecutionStatus; error?: string }
export interface ReferenceTiming {
  /** Safe additive pacing applied to every reference delay. */
  additionalDelayMs?: number;
  /** Diagnostic-only reduction; callers must never persist this as a normal default. */
  unsafeDiagnosticReductionMs?: number;
}
const delay = (milliseconds: number, signal?: AbortSignal): Promise<void> => {
  checkAbort(signal);
  if (milliseconds <= 0) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timer = globalThis.setTimeout(() => { signal?.removeEventListener("abort", abort); resolve(); }, milliseconds);
    const abort = () => { globalThis.clearTimeout(timer); reject(abortError()); };
    signal?.addEventListener("abort", abort, { once: true });
  });
};
const preflight = (actions: PlanAction[], limit: number, commandLimit: number) => {
  if (!Number.isInteger(limit) || limit <= 0) throw new Error("invalid transport payload limit");
  if (!Number.isInteger(commandLimit) || commandLimit <= 0) throw new Error("invalid transport command limit");
  for (const [index, action] of actions.entries()) {
    if ("bytes" in action && action.bytes.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) throw new Error(`invalid byte in action ${index}`);
    if (action.action === "command-write" && action.atomic && action.bytes.length > commandLimit) throw new Error(`atomic command ${index} exceeds command limit before job start`);
    if (action.action === "raster-write" && (!Number.isInteger(action.logical_chunk) || action.logical_chunk <= 0)) throw new Error(`invalid raster chunk in action ${index}`);
    if (action.action === "wait-for-response" && !["any-notification", "phomemo-notification", "brother-status32"].includes(action.validation)) throw new Error(`unsupported response validation in action ${index}`);
  }
};
const validResponse = (validation: string, bytes: Uint8Array) => validation === "any-notification"
  ? bytes.length > 0
  : validation === "phomemo-notification"
    ? bytes.length >= 3 && bytes[0] === 0x1a
    : bytes.length === 32 && bytes[0] === 0x80 && bytes[1] === 0x20 && bytes[2] === 0x42;
const phomemoFrame = (bytes: number[]): Uint8Array | undefined => {
  const start = bytes.indexOf(0x1a);
  return start >= 0 && bytes.length - start >= 3 ? Uint8Array.from(bytes.slice(start)) : undefined;
};

/** Executes exactly once. Runtime ambiguity is returned and never retried automatically. */
export async function executePlan(actions: PlanAction[], transport: BrowserTransport,
  progress?: (state: ExecutionProgress) => void, signal?: AbortSignal,
  timing: ReferenceTiming = {}): Promise<ExecutionResult> {
  preflight(actions, transport.payloadLimit, transport.commandPayloadLimit ?? transport.payloadLimit);
  const increase = timing.additionalDelayMs ?? 0, reduction = timing.unsafeDiagnosticReductionMs ?? 0;
  if (![increase, reduction].every(value => Number.isSafeInteger(value) && value >= 0)) throw new Error("invalid timing override");
  if (increase > 0 && reduction > 0) throw new Error("timing increase and unsafe reduction are mutually exclusive");
  const paced = (reference: number) => Math.max(0, reference + increase - reduction);
  const state: ExecutionProgress = { lastCompletedAction: -1, bytesWritten: 0, potentiallyAcceptedWrite: false };
  const done = (status: ExecutionStatus, error?: unknown): ExecutionResult => ({ ...state, status,
    ...(error === undefined ? {} : { error: error instanceof Error ? error.message : String(error) }) });
  if (signal?.aborted) return done("cancelled-before-send");
  const write = async (bytes: Uint8Array, kind: "command" | "raster"): Promise<ExecutionResult | undefined> => {
    state.potentiallyAcceptedWrite = true;
    try { await raceAbort(transport.write(bytes, signal, kind), signal); } catch (error) { return done("outcome-unknown", error); }
    state.bytesWritten += bytes.length;
  };
  for (const [index, action] of actions.entries()) {
    try {
      checkAbort(signal);
      if (action.action === "subscribe-notifications") await transport.subscribeNotifications(signal);
      else if (action.action === "delay") await delay(paced(action.milliseconds), signal);
      else if (action.action === "command-write") { const failed = await write(Uint8Array.from(action.bytes), "command"); if (failed) return failed; }
      else if (action.action === "raster-write") {
        const bytes = Uint8Array.from(action.bytes);
        for (let logicalOffset = 0; logicalOffset < bytes.length; logicalOffset += action.logical_chunk) {
          const logical = bytes.slice(logicalOffset, logicalOffset + action.logical_chunk);
          for (let offset = 0; offset < logical.length; offset += transport.payloadLimit) {
            const failed = await write(logical.slice(offset, offset + transport.payloadLimit), "raster"); if (failed) return failed;
            await delay(paced(action.delay_after_each_physical_write_ms), signal);
          }
        }
      } else if (action.action === "wait-for-response") {
        let reply: ResponseWait;
        if (action.validation === "phomemo-notification") {
          const deadline = Date.now() + action.timeout_ms, collected: number[] = [];
          reply = { kind: "timeout" };
          while (Date.now() < deadline) {
            const next = await transport.waitForResponse(Math.max(1, deadline - Date.now()), signal);
            if (next.kind !== "response") { reply = next; break; }
            collected.push(...next.bytes);
            const frame = phomemoFrame(collected);
            if (frame) { reply = { kind: "response", bytes: frame }; break; }
          }
        } else reply = await transport.waitForResponse(action.timeout_ms, signal);
        if ((reply.kind === "unavailable" || reply.kind === "timeout") && action.fallback_delay_ms > 0) await delay(paced(action.fallback_delay_ms), signal);
        else if ((reply.kind === "unavailable" || reply.kind === "timeout") && action.validation === "brother-status32") { /* best-effort frozen Brother preflight */ }
        else if (reply.kind === "unavailable") return done("outcome-unknown", `notifications unavailable after action ${index}`);
        else if (reply.kind === "timeout") return done("outcome-unknown", `response timeout after action ${index}`);
        else if (!validResponse(action.validation, reply.bytes)) return done("outcome-unknown", `invalid ${action.validation} response after action ${index}`);
      }
    } catch (error) {
      if (isAbort(error)) return done(state.bytesWritten > 0 ? "cancelled-partial" : "cancelled-before-send");
      return done(state.potentiallyAcceptedWrite ? "outcome-unknown" : "cancelled-before-send", error);
    }
    state.lastCompletedAction = index;
    progress?.({ ...state });
  }
  return done("completed");
}
