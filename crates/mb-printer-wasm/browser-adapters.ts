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
  subscribeNotifications(signal?: AbortSignal): Promise<boolean>;
  write(bytes: Uint8Array, signal?: AbortSignal): Promise<void>;
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
    private readonly responseLength = 64) {}
  async subscribeNotifications(signal?: AbortSignal): Promise<boolean> { checkAbort(signal); return this.inEndpoint !== undefined; }
  async write(bytes: Uint8Array, signal?: AbortSignal): Promise<void> {
    checkAbort(signal);
    if (bytes.length > this.payloadLimit) throw new Error("WebUSB payload exceeds endpoint limit");
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

export interface ExecutionProgress { lastCompletedAction: number; bytesWritten: number; potentiallyAcceptedWrite: boolean }
export type ExecutionStatus = "completed" | "cancelled-before-send" | "cancelled-partial" | "outcome-unknown";
export interface ExecutionResult extends ExecutionProgress { status: ExecutionStatus; error?: string }
const delay = (milliseconds: number, signal?: AbortSignal): Promise<void> => {
  checkAbort(signal);
  if (milliseconds <= 0) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timer = globalThis.setTimeout(() => { signal?.removeEventListener("abort", abort); resolve(); }, milliseconds);
    const abort = () => { globalThis.clearTimeout(timer); reject(abortError()); };
    signal?.addEventListener("abort", abort, { once: true });
  });
};
const preflight = (actions: PlanAction[], limit: number) => {
  if (!Number.isInteger(limit) || limit <= 0) throw new Error("invalid transport payload limit");
  for (const [index, action] of actions.entries()) {
    if ("bytes" in action && action.bytes.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) throw new Error(`invalid byte in action ${index}`);
    if (action.action === "command-write" && action.atomic && action.bytes.length > limit) throw new Error(`atomic command ${index} exceeds payload limit before job start`);
    if (action.action === "raster-write" && (!Number.isInteger(action.logical_chunk) || action.logical_chunk <= 0)) throw new Error(`invalid raster chunk in action ${index}`);
    if (action.action === "wait-for-response" && !["any-notification", "brother-status32"].includes(action.validation)) throw new Error(`unsupported response validation in action ${index}`);
  }
};
const validResponse = (validation: string, bytes: Uint8Array) => validation === "any-notification"
  ? bytes.length > 0
  : bytes.length === 32 && bytes[0] === 0x80 && bytes[1] === 0x20 && bytes[2] === 0x42;

/** Executes exactly once. Runtime ambiguity is returned and never retried automatically. */
export async function executePlan(actions: PlanAction[], transport: BrowserTransport,
  progress?: (state: ExecutionProgress) => void, signal?: AbortSignal): Promise<ExecutionResult> {
  preflight(actions, transport.payloadLimit);
  const state: ExecutionProgress = { lastCompletedAction: -1, bytesWritten: 0, potentiallyAcceptedWrite: false };
  const done = (status: ExecutionStatus, error?: unknown): ExecutionResult => ({ ...state, status,
    ...(error === undefined ? {} : { error: error instanceof Error ? error.message : String(error) }) });
  if (signal?.aborted) return done("cancelled-before-send");
  const write = async (bytes: Uint8Array): Promise<ExecutionResult | undefined> => {
    state.potentiallyAcceptedWrite = true;
    try { await raceAbort(transport.write(bytes, signal), signal); } catch (error) { return done("outcome-unknown", error); }
    state.bytesWritten += bytes.length;
  };
  for (const [index, action] of actions.entries()) {
    try {
      checkAbort(signal);
      if (action.action === "subscribe-notifications") await transport.subscribeNotifications(signal);
      else if (action.action === "delay") await delay(action.milliseconds, signal);
      else if (action.action === "command-write") { const failed = await write(Uint8Array.from(action.bytes)); if (failed) return failed; }
      else if (action.action === "raster-write") {
        const bytes = Uint8Array.from(action.bytes), size = Math.min(action.logical_chunk, transport.payloadLimit);
        for (let offset = 0; offset < bytes.length; offset += size) {
          const failed = await write(bytes.slice(offset, offset + size)); if (failed) return failed;
          await delay(action.delay_after_each_physical_write_ms, signal);
        }
      } else if (action.action === "wait-for-response") {
        const reply = await transport.waitForResponse(action.timeout_ms, signal);
        if (reply.kind === "unavailable" && action.fallback_delay_ms > 0) await delay(action.fallback_delay_ms, signal);
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
