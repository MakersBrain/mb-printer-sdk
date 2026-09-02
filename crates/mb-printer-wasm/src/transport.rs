// SPDX-License-Identifier: AGPL-3.0-or-later
//! Structural JavaScript transport bridge used by the shared Rust executor.

use std::{future::Future, time::Duration};

use js_sys::{Function, Promise, Reflect, Uint8Array};
use mb_printer_executor::{
    Cancellation, NotificationSupport, Transport, TransportError, TransportErrorKind,
    TransportFuture, WaitOutcome, WriteKind,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortSignal, AddEventListenerOptions};

#[wasm_bindgen(typescript_custom_section)]
const TYPESCRIPT_TYPES: &str = r#"
export type ResponseWait =
    | { kind: "response"; bytes: Uint8Array }
    | { kind: "timeout" }
    | { kind: "unavailable" };

export interface BrowserTransport {
    readonly payloadLimit: number;
    readonly commandPayloadLimit?: number;
    subscribeNotifications(signal?: AbortSignal): Promise<boolean>;
    write(bytes: Uint8Array, signal?: AbortSignal, kind?: "command" | "raster"): Promise<void>;
    waitForResponse(timeoutMs: number, signal?: AbortSignal): Promise<ResponseWait>;
    disconnect(signal?: AbortSignal): Promise<void>;
}

export interface ReferenceTiming {
    additionalDelayMs?: number;
    unsafeDiagnosticReductionMs?: number;
}

export interface ExecutionProgress {
    lastCompletedAction: number;
    bytesWritten: number;
    potentiallyAcceptedWrite: boolean;
    responses: Uint8Array[];
}

export type ExecutionStatus =
    | "completed"
    | "cancelled-before-send"
    | "cancelled-partial"
    | "outcome-unknown"
    | "failed";

export interface ExecutionResult extends ExecutionProgress {
    status: ExecutionStatus;
    errorCode?: string;
    error?: string;
}
"#;

#[wasm_bindgen]
extern "C" {
    /// A structural JavaScript object implementing the browser transport API.
    #[wasm_bindgen(typescript_type = "BrowserTransport")]
    pub type BrowserTransport;

    #[wasm_bindgen(method, getter, structural, js_name = payloadLimit)]
    fn payload_limit_value(this: &BrowserTransport) -> JsValue;

    #[wasm_bindgen(method, getter, structural, js_name = commandPayloadLimit)]
    fn command_limit_value(this: &BrowserTransport) -> JsValue;

    #[wasm_bindgen(method, structural, catch, js_name = subscribeNotifications)]
    fn subscribe_notifications_js(
        this: &BrowserTransport,
        signal: JsValue,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(method, structural, catch, js_name = write)]
    fn write_js(
        this: &BrowserTransport,
        bytes: Uint8Array,
        signal: JsValue,
        kind: &str,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(method, structural, catch, js_name = waitForResponse)]
    fn wait_response_js(
        this: &BrowserTransport,
        timeout_ms: f64,
        signal: JsValue,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(method, structural, catch)]
    fn disconnect_js(this: &BrowserTransport, signal: JsValue) -> Result<Promise, JsValue>;

    #[wasm_bindgen(js_namespace = globalThis, js_name = setTimeout)]
    fn set_timeout(callback: &Function, milliseconds: f64) -> JsValue;
}

fn transport_error(kind: TransportErrorKind, value: JsValue) -> TransportError {
    let code = Reflect::get(&value, &JsValue::from_str("code"))
        .ok()
        .and_then(|code| code.as_string());
    let name = Reflect::get(&value, &JsValue::from_str("name"))
        .ok()
        .and_then(|name| name.as_string());
    let kind = match (code.as_deref(), name.as_deref()) {
        (Some("unsupported-profile"), _) | (_, Some("NotSupportedError")) => {
            TransportErrorKind::Unsupported
        }
        (_, Some("NotAllowedError" | "SecurityError")) => TransportErrorKind::PermissionDenied,
        (_, Some("AbortError")) => TransportErrorKind::Cancelled,
        _ => kind,
    };
    // Do not propagate arbitrary platform messages: they may include endpoint
    // URLs, credentials, payloads, or vendor-specific debug data.
    let message = match kind {
        TransportErrorKind::Unsupported => "browser transport operation is unsupported",
        TransportErrorKind::PermissionDenied => "browser transport permission was denied",
        TransportErrorKind::Cancelled => "browser transport operation was cancelled",
        _ => "browser transport operation failed",
    };
    TransportError::new(kind, message)
}

fn invalid_configuration(message: impl Into<String>) -> TransportError {
    TransportError {
        kind: TransportErrorKind::InvalidConfiguration,
        message: message.into(),
    }
}

fn signal_value(signal: Option<&AbortSignal>) -> JsValue {
    signal.map_or(JsValue::UNDEFINED, |signal| signal.clone().into())
}

fn promise_result(
    promise: Result<Promise, JsValue>,
    signal: Option<&AbortSignal>,
) -> impl Future<Output = Result<JsValue, TransportError>> {
    let signal = signal.cloned();
    async move {
        let promise = promise.map_err(|error| {
            transport_error(
                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    TransportErrorKind::Cancelled
                } else {
                    TransportErrorKind::Io
                },
                error,
            )
        })?;
        JsFuture::from(promise).await.map_err(|error| {
            transport_error(
                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    TransportErrorKind::Cancelled
                } else {
                    TransportErrorKind::Io
                },
                error,
            )
        })
    }
}

/// Adapter from a structural JavaScript object to the shared transport trait.
pub struct JsTransport {
    inner: BrowserTransport,
    signal: Option<AbortSignal>,
}

impl JsTransport {
    pub fn new(inner: BrowserTransport, signal: Option<AbortSignal>) -> Self {
        Self { inner, signal }
    }

    fn limit(value: JsValue, name: &str) -> Result<usize, TransportError> {
        let number = value
            .as_f64()
            .filter(|number| number.is_finite() && number.fract() == 0.0 && *number > 0.0)
            .ok_or_else(|| invalid_configuration(format!("invalid browser transport {name}")))?;
        if number > usize::MAX as f64 {
            return Err(invalid_configuration(format!(
                "browser transport {name} exceeds range"
            )));
        }
        Ok(number as usize)
    }

    pub fn validate_limits(&self) -> Result<(), TransportError> {
        Self::limit(self.inner.payload_limit_value(), "payload limit")?;
        let command = self.inner.command_limit_value();
        if !command.is_null() && !command.is_undefined() {
            Self::limit(command, "command limit")?;
        }
        Ok(())
    }
}

impl Transport for JsTransport {
    fn payload_limit(&self) -> usize {
        Self::limit(self.inner.payload_limit_value(), "payload limit").unwrap_or(0)
    }

    fn command_limit(&self) -> usize {
        let value = self.inner.command_limit_value();
        if value.is_null() || value.is_undefined() {
            self.payload_limit()
        } else {
            Self::limit(value, "command limit").unwrap_or(0)
        }
    }

    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        let promise = self
            .inner
            .subscribe_notifications_js(signal_value(self.signal.as_ref()));
        let signal = self.signal.clone();
        Box::pin(async move {
            let value = promise_result(promise, signal.as_ref()).await?;
            match value.as_bool() {
                Some(true) => Ok(NotificationSupport::Subscribed),
                Some(false) => Ok(NotificationSupport::Unavailable),
                None => Err(invalid_configuration(
                    "subscribeNotifications must resolve to a boolean",
                )),
            }
        })
    }

    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        kind: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        let bytes = Uint8Array::from(bytes);
        let kind = match kind {
            WriteKind::Command => "command",
            WriteKind::Raster => "raster",
        };
        let promise = self
            .inner
            .write_js(bytes, signal_value(self.signal.as_ref()), kind);
        let signal = self.signal.clone();
        Box::pin(async move {
            promise_result(promise, signal.as_ref()).await?;
            Ok(())
        })
    }

    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        let promise = self.inner.wait_response_js(
            timeout.as_secs_f64() * 1_000.0,
            signal_value(self.signal.as_ref()),
        );
        let signal = self.signal.clone();
        Box::pin(async move {
            let value = promise_result(promise, signal.as_ref()).await?;
            let kind = Reflect::get(&value, &JsValue::from_str("kind"))
                .ok()
                .and_then(|kind| kind.as_string())
                .ok_or_else(|| invalid_configuration("response wait has no valid kind"))?;
            match kind.as_str() {
                "timeout" => Ok(WaitOutcome::Timeout),
                "unavailable" => Ok(WaitOutcome::Unavailable),
                "response" => {
                    let bytes = Reflect::get(&value, &JsValue::from_str("bytes"))
                        .map_err(|_| invalid_configuration("response wait has no bytes"))?;
                    if !bytes.is_instance_of::<Uint8Array>() {
                        return Err(invalid_configuration(
                            "response wait bytes must be a Uint8Array",
                        ));
                    }
                    Ok(WaitOutcome::Response(Uint8Array::new(&bytes).to_vec()))
                }
                _ => Err(invalid_configuration("response wait has an unknown kind")),
            }
        })
    }

    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
        let promise = Promise::new(&mut |resolve, _reject| {
            let callback = Closure::once_into_js(move || {
                let _ = resolve.call0(&JsValue::UNDEFINED);
            });
            let _ = set_timeout(callback.unchecked_ref(), duration.as_secs_f64() * 1_000.0);
        });
        Box::pin(async move {
            let _ = JsFuture::from(promise).await;
        })
    }

    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        let promise = self.inner.disconnect_js(signal_value(self.signal.as_ref()));
        let signal = self.signal.clone();
        Box::pin(async move {
            promise_result(promise, signal.as_ref()).await?;
            Ok(())
        })
    }
}

/// Awaitable cancellation backed by a browser `AbortSignal`.
pub struct AbortSignalCancellation {
    signal: AbortSignal,
    aborted: Promise,
}

impl AbortSignalCancellation {
    pub fn new(signal: AbortSignal) -> Self {
        let target = signal.clone();
        let already_aborted = signal.aborted();
        let aborted = Promise::new(&mut move |resolve, _reject| {
            if already_aborted {
                let _ = resolve.call0(&JsValue::UNDEFINED);
                return;
            }
            let callback = Closure::once_into_js(move |_event: web_sys::Event| {
                let _ = resolve.call0(&JsValue::UNDEFINED);
            });
            let options = AddEventListenerOptions::new();
            options.set_once(true);
            let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
                "abort",
                callback.unchecked_ref(),
                &options,
            );
        });
        Self { signal, aborted }
    }
}

impl Cancellation for AbortSignalCancellation {
    fn is_cancelled(&self) -> bool {
        self.signal.aborted()
    }

    fn cancelled(&self) -> TransportFuture<'_, ()> {
        let promise = self.aborted.clone();
        Box::pin(async move {
            let _ = JsFuture::from(promise).await;
        })
    }
}
