// SPDX-License-Identifier: AGPL-3.0-or-later
/** Browser I/O contract consumed by the Rust/Wasm plan executor. */
export type ResponseWait = { kind: "response"; bytes: Uint8Array } | { kind: "timeout" } | { kind: "unavailable" };
export interface BrowserTransport {
  readonly payloadLimit: number;
  /** Atomic command limit; defaults to payloadLimit for BLE/stream transports. */
  readonly commandPayloadLimit?: number;
  subscribeNotifications(signal?: AbortSignal): Promise<boolean>;
  write(bytes: Uint8Array, signal?: AbortSignal, kind?: "command" | "raster"): Promise<void>;
  waitForResponse(timeoutMs: number, signal?: AbortSignal): Promise<ResponseWait>;
  /** Releases resources owned by the adapter. App-owned devices remain open. */
  disconnect(signal?: AbortSignal): Promise<void>;
}
export interface BluetoothCharacteristic extends EventTarget {
  startNotifications(): Promise<unknown>;
  value?: DataView | null;
}
export interface BluetoothWritableCharacteristic extends BluetoothCharacteristic {
  writeValueWithoutResponse?(bytes: BufferSource): Promise<void>;
}
export type BluetoothFlowControl = "none" | "phomemo-credit";
const abortError = () => new DOMException("Operation aborted", "AbortError");
const checkAbort = (signal?: AbortSignal) => { if (signal?.aborted) throw abortError(); };
const unsupportedProfile = (message: string): Error => {
  const error = new Error(message);
  Object.assign(error, { code: "unsupported-profile" });
  return error;
};
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
  private readonly creditWaiters: Array<() => void> = [];
  private credits = 0;
  private negotiatedPayloadLimit?: number;
  private subscribed = false;
  constructor(private readonly writable: BluetoothWritableCharacteristic,
    private readonly notifications?: BluetoothCharacteristic,
    private readonly configuredPayloadLimit = 512,
    private readonly flowControl: BluetoothFlowControl = "none") {
    if (!Number.isSafeInteger(configuredPayloadLimit) || configuredPayloadLimit <= 0) {
      throw new Error("invalid WebBluetooth payload limit");
    }
  }
  get payloadLimit(): number {
    return Math.min(this.configuredPayloadLimit, this.negotiatedPayloadLimit ?? this.configuredPayloadLimit);
  }
  private grantCredits(count: number): void {
    while (count > 0 && this.creditWaiters.length > 0) {
      count--;
      this.creditWaiters.shift()?.();
    }
    this.credits = Math.min(Number.MAX_SAFE_INTEGER, this.credits + count);
  }
  private receiveNotification(bytes: Uint8Array): void {
    if (this.flowControl === "phomemo-credit") {
      if (bytes.length === 2 && bytes[0] === 0x01) {
        this.grantCredits(bytes[1]);
        return;
      }
      if (bytes.length === 3 && bytes[0] === 0x02) {
        const limit = bytes[1] | (bytes[2] << 8);
        if (limit > 0) this.negotiatedPayloadLimit = limit;
        return;
      }
    }
    const listener = this.listeners.shift();
    if (listener) listener(bytes); else this.replies.push(bytes);
  }
  private takeCredit(signal?: AbortSignal): Promise<void> {
    checkAbort(signal);
    if (this.credits > 0) {
      this.credits--;
      return Promise.resolve();
    }
    return new Promise((resolve, reject) => {
      const grant = () => { cleanup(); resolve(); };
      const abort = () => { remove(); reject(abortError()); };
      const remove = () => {
        const index = this.creditWaiters.indexOf(grant);
        if (index >= 0) this.creditWaiters.splice(index, 1);
        signal?.removeEventListener("abort", abort);
      };
      const cleanup = remove;
      this.creditWaiters.push(grant);
      signal?.addEventListener("abort", abort, { once: true });
    });
  }
  async subscribeNotifications(signal?: AbortSignal): Promise<boolean> {
    checkAbort(signal);
    const notifications = this.notifications;
    if (!notifications) return false;
    if (!this.subscribed) {
      notifications.addEventListener("characteristicvaluechanged", () => {
        const view = notifications.value;
        if (!view) return;
        const bytes = new Uint8Array(view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength));
        this.receiveNotification(bytes);
      });
      await raceAbort(notifications.startNotifications(), signal);
      this.subscribed = true;
    }
    return true;
  }
  async write(bytes: Uint8Array, signal?: AbortSignal): Promise<void> {
    checkAbort(signal);
    if (bytes.length > this.payloadLimit) throw new Error("WebBluetooth payload exceeds negotiated limit");
    if (this.flowControl === "phomemo-credit") {
      if (!this.subscribed) throw new Error("WebBluetooth credit flow requires notification subscription");
      await this.takeCredit(signal);
      if (bytes.length > this.payloadLimit) throw new Error("WebBluetooth payload exceeds printer flow limit");
    }
    const payload = Uint8Array.from(bytes).buffer;
    const operation = this.writable.writeValueWithoutResponse?.(payload)
      ?? Promise.reject(unsupportedProfile("Bluetooth profile requires write-without-response"));
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
  async disconnect(signal?: AbortSignal): Promise<void> {
    checkAbort(signal);
    this.credits = 0;
    this.negotiatedPayloadLimit = undefined;
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
  async disconnect(signal?: AbortSignal): Promise<void> { checkAbort(signal); }
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
  async disconnect(signal?: AbortSignal): Promise<void> { checkAbort(signal); }
}
