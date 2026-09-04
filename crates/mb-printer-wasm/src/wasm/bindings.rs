// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::protocol::Plan;
use mb_printer_executor::{
    Cancellation, ExecuteError, ExecutionOptions, NeverCancelled, Progress, ReferenceTiming,
    execute_with_options,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

fn execution_request_error(code: &'static str, message: &str) -> JsValue {
    let error = js_sys::Error::new(message);
    let _ = js_sys::Reflect::set(
        error.as_ref(),
        &JsValue::from_str("code"),
        &JsValue::from_str(code),
    );
    error.into()
}

fn set(object: &js_sys::Object, name: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(object, &JsValue::from_str(name), value);
}

fn progress_value(progress: &Progress) -> JsValue {
    let value = js_sys::Object::new();
    set(
        &value,
        "lastCompletedAction",
        &JsValue::from_f64(
            progress
                .last_completed_action
                .map_or(-1.0, |action| action as f64),
        ),
    );
    set(
        &value,
        "bytesWritten",
        &JsValue::from_f64(progress.bytes_written as f64),
    );
    set(
        &value,
        "potentiallyAcceptedWrite",
        &JsValue::from_bool(progress.potentially_accepted_write),
    );
    let responses = js_sys::Array::new();
    for response in &progress.responses {
        responses.push(&js_sys::Uint8Array::from(response.as_slice()));
    }
    set(&value, "responses", &responses);
    value.into()
}

fn execution_value(
    progress: &Progress,
    status: &str,
    error_code: Option<&str>,
    error: Option<&str>,
) -> JsValue {
    let value = progress_value(progress);
    let object: &js_sys::Object = value.unchecked_ref();
    set(object, "status", &JsValue::from_str(status));
    if let Some(code) = error_code {
        set(object, "errorCode", &JsValue::from_str(code));
    }
    if let Some(message) = error {
        set(object, "error", &JsValue::from_str(message));
    }
    value
}

fn map_execution_result(result: Result<Progress, ExecuteError>) -> JsValue {
    match result {
        Ok(progress) => execution_value(&progress, "completed", None, None),
        Err(ExecuteError::Cancelled { progress }) => {
            let status = if progress.bytes_written == 0 {
                "cancelled-before-send"
            } else {
                "cancelled-partial"
            };
            execution_value(&progress, status, Some("cancelled"), None)
        }
        Err(ExecuteError::WriteOutcomeUnknown { progress, source }) => execution_value(
            &progress,
            "outcome-unknown",
            Some("write-outcome-unknown"),
            source.as_ref().map(|error| error.message.as_str()),
        ),
        Err(error) => {
            let (progress, code): (Progress, &'static str) = match &error {
                ExecuteError::AtomicTooLarge { .. } => (Progress::default(), "atomic-too-large"),
                ExecuteError::InvalidPlan { .. } => (Progress::default(), "invalid-plan"),
                ExecuteError::Replay(_) => (Progress::default(), "replay"),
                ExecuteError::ReplayStore { .. } => (Progress::default(), "replay-store"),
                ExecuteError::Transport { progress, .. } => (progress.clone(), "transport"),
                ExecuteError::Timeout { progress } => (progress.clone(), "timeout"),
                ExecuteError::Response { progress, .. } => (progress.clone(), "response"),
                ExecuteError::Cancelled { progress } => (progress.clone(), "cancelled"),
                ExecuteError::WriteOutcomeUnknown { progress, .. } => {
                    (progress.clone(), "write-outcome-unknown")
                }
            };
            execution_value(&progress, "failed", Some(code), Some(&error.to_string()))
        }
    }
}

fn timing_value(value: &JsValue, name: &str) -> Result<u64, JsValue> {
    let value = js_sys::Reflect::get(value, &JsValue::from_str(name))
        .map_err(|_| execution_request_error("invalid-options", "invalid timing override"))?;
    if value.is_null() || value.is_undefined() {
        return Ok(0);
    }
    let number = value.as_f64().filter(|number| {
        number.is_finite()
            && *number >= 0.0
            && number.fract() == 0.0
            && *number <= js_sys::Number::MAX_SAFE_INTEGER
    });
    number
        .map(|number| number as u64)
        .ok_or_else(|| execution_request_error("invalid-options", "invalid timing override"))
}

fn parse_timing(value: &JsValue) -> Result<ReferenceTiming, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(ReferenceTiming::Preserve);
    }
    if !value.is_object() {
        return Err(execution_request_error(
            "invalid-options",
            "timing override must be an object",
        ));
    }
    let increase = timing_value(value, "additionalDelayMs")?;
    let reduction = timing_value(value, "unsafeDiagnosticReductionMs")?;
    match (increase, reduction) {
        (0, 0) => Ok(ReferenceTiming::Preserve),
        (increase, 0) => Ok(ReferenceTiming::IncreaseBy(increase)),
        (0, reduction) => Ok(ReferenceTiming::UnsafeDiagnosticReduceBy(reduction)),
        _ => Err(execution_request_error(
            "invalid-options",
            "timing increase and unsafe reduction are mutually exclusive",
        )),
    }
}

fn sheet_error(error: super::SheetApiError) -> JsValue {
    let value = js_sys::Error::new(&error.message);
    let _ = js_sys::Reflect::set(
        value.as_ref(),
        &JsValue::from_str("code"),
        &JsValue::from_str(error.code),
    );
    if let Ok(json) = serde_json::to_string(&error.details)
        && let Ok(details) = js_sys::JSON::parse(&json)
    {
        let _ = js_sys::Reflect::set(value.as_ref(), &JsValue::from_str("details"), &details);
    }
    value.into()
}
fn materialize_error(error: super::MaterializeApiError) -> JsValue {
    let value = js_sys::Error::new(&error.message);
    let _ = js_sys::Reflect::set(
        value.as_ref(),
        &JsValue::from_str("version"),
        &JsValue::from_f64(f64::from(error.version)),
    );
    let _ = js_sys::Reflect::set(
        value.as_ref(),
        &JsValue::from_str("code"),
        &JsValue::from_str(error.code),
    );
    if let Ok(json) = serde_json::to_string(&error.details)
        && let Ok(details) = js_sys::JSON::parse(&json)
    {
        let _ = js_sys::Reflect::set(value.as_ref(), &JsValue::from_str("details"), &details);
    }
    value.into()
}
#[wasm_bindgen(js_name=validateDocument)]
pub fn validate_document(input: &str) -> String {
    super::validate_document_json(input)
}
#[wasm_bindgen(js_name=printerCapabilities)]
pub fn printer_capabilities() -> String {
    super::capabilities_json()
}
/// JSON `BuildInfo`: package name and version plus the git commit the
/// WebAssembly module was compiled from.
#[wasm_bindgen(js_name=buildInfo)]
pub fn build_info() -> String {
    super::build_info_json()
}
#[wasm_bindgen(js_name=importV3)]
pub fn import_v3(input: &str) -> Result<String, JsValue> {
    super::import_v3_json(input).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=evaluateTemplate)]
pub fn evaluate_template(input: &str, fields_json: &str) -> Result<String, JsValue> {
    super::evaluate_template_json(input, fields_json).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=evaluateTemplateContext)]
pub fn evaluate_template_context(
    input: &str,
    fields_json: &str,
    locale: &str,
    current_date: &str,
) -> Result<String, JsValue> {
    super::evaluate_template_context_json(input, fields_json, locale, current_date)
        .map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=materializeRecord)]
pub fn materialize_record(
    document_json: &str,
    record_json: &str,
    options_json: &str,
) -> Result<String, JsValue> {
    super::materialize_record_json(document_json, record_json, options_json)
        .map_err(materialize_error)
}
#[wasm_bindgen(js_name=planZoneBatch)]
pub fn plan_zone_batch(document_json: &str, input_json: &str) -> Result<String, JsValue> {
    super::plan_zone_batch_json(document_json, input_json).map_err(materialize_error)
}
#[wasm_bindgen(js_name=materializeZoneBatch)]
pub fn materialize_zone_batch(
    document_json: &str,
    records_json: &str,
    options_json: &str,
) -> Result<String, JsValue> {
    super::materialize_zone_batch_json(document_json, records_json, options_json)
        .map_err(materialize_error)
}
#[wasm_bindgen(js_name=extractLaPoste)]
pub fn extract_laposte(
    code: &str,
    page: u32,
    width_um: i64,
    height_um: i64,
    raster_width: u32,
    raster_height: u32,
    pixels: Vec<u8>,
) -> Result<String, JsValue> {
    super::extract_laposte_json(
        code,
        page,
        width_um,
        height_um,
        raster_width,
        raster_height,
        pixels,
    )
    .map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=protocolPlan)]
pub fn protocol_plan(
    model: &str,
    width_bytes: u16,
    height: u32,
    bytes_json: &str,
) -> Result<String, JsValue> {
    super::protocol_plan_json(model, width_bytes, height, bytes_json)
        .map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=renderPacked)]
pub fn render_packed(input: &str) -> Result<Vec<u8>, JsValue> {
    super::render_packed(input).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=renderPng)]
pub fn render_png(input: &str) -> Result<Vec<u8>, JsValue> {
    super::render_png(input).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=measureDocument)]
pub fn measure_document(input: &str) -> Result<String, JsValue> {
    super::measure_document_json(input).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=renderPdf)]
pub fn render_pdf(input: &str) -> Result<Vec<u8>, JsValue> {
    super::render_pdf(input).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=renderBatchPdf)]
pub fn render_batch_pdf(input: &str) -> Result<Vec<u8>, JsValue> {
    super::render_batch_pdf(input).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=planSheet)]
pub fn plan_sheet(
    plan_input_json: &str,
    layout_json: &str,
    options_json: &str,
) -> Result<String, JsValue> {
    super::plan_sheet_json(plan_input_json, layout_json, options_json).map_err(sheet_error)
}
#[wasm_bindgen(js_name=buildSheetPdf)]
pub fn build_sheet_pdf(
    documents_json: &str,
    layout_json: &str,
    options_json: &str,
) -> Result<Vec<u8>, JsValue> {
    super::build_sheet_pdf_json(documents_json, layout_json, options_json).map_err(sheet_error)
}
#[wasm_bindgen(js_name=normalizePdf)]
pub fn normalize_pdf(bytes: Vec<u8>, dpi: u16, first_page_only: bool) -> Result<String, JsValue> {
    super::normalize_pdf_json(bytes, dpi, first_page_only).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=extractLaPostePdf)]
pub fn extract_laposte_pdf(code: &str, bytes: Vec<u8>, dpi: u16) -> Result<String, JsValue> {
    super::extract_laposte_pdf_json(code, bytes, dpi).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=mediaPresets)]
pub fn media_presets(model: &str) -> Result<String, JsValue> {
    super::media_presets_json(model).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=matchMedia)]
pub fn match_media(model: &str, width_mm: f64, height_mm: f64) -> Result<String, JsValue> {
    super::match_media_json(model, width_mm, height_mm).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=statusPlan)]
pub fn status_plan(model: &str) -> Result<String, JsValue> {
    super::status_plan_json(model).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=parsePhomemoStatus)]
pub fn parse_phomemo_status(frames_json: &str) -> Result<String, JsValue> {
    super::parse_phomemo_status_json(frames_json).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=parseBrotherStatus)]
pub fn parse_brother_status(data: &[u8]) -> Result<String, JsValue> {
    super::parse_brother_status_json(data).map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen(js_name=decodeIpp)]
pub fn decode_ipp(data: &[u8], maximum_message_bytes: usize) -> Result<String, JsValue> {
    super::decode_ipp_json(data, maximum_message_bytes).map_err(|error| JsValue::from_str(&error))
}

#[wasm_bindgen(js_name=encodeIpp)]
pub fn encode_ipp(input: &str, maximum_message_bytes: usize) -> Result<Vec<u8>, JsValue> {
    super::encode_ipp_json(input, maximum_message_bytes).map_err(|error| JsValue::from_str(&error))
}
#[wasm_bindgen(js_name=renderProtocolPlan)]
pub fn render_protocol_plan(input: &str, model: &str) -> Result<String, JsValue> {
    super::render_protocol_plan(input, model).map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=renderProtocolPlanWithOptions)]
pub fn render_protocol_plan_with_options(
    input: &str,
    model: &str,
    options_json: &str,
) -> Result<String, JsValue> {
    super::render_protocol_plan_with_options(input, model, options_json)
        .map_err(|e| JsValue::from_str(&e))
}
#[wasm_bindgen(js_name=renderProtocolBatchPlanWithOptions)]
pub fn render_protocol_batch_plan_with_options(
    input: &str,
    model: &str,
    options_json: &str,
) -> Result<String, JsValue> {
    super::render_protocol_batch_plan_with_options(input, model, options_json)
        .map_err(|e| JsValue::from_str(&e))
}

/// Executes a serialized core plan through a structural browser transport.
/// All execution policy remains in `mb-printer-executor`; JavaScript only
/// supplies the platform I/O operations.
#[wasm_bindgen(js_name = executePlan, unchecked_return_type = "ExecutionResult")]
pub async fn execute_plan(
    plan_json: &str,
    transport: super::transport::BrowserTransport,
    #[wasm_bindgen(unchecked_param_type = "ReferenceTiming")] timing: JsValue,
    signal: Option<web_sys::AbortSignal>,
    #[wasm_bindgen(unchecked_optional_param_type = "(progress: ExecutionProgress) => void")]
    on_progress: Option<js_sys::Function>,
) -> Result<JsValue, JsValue> {
    let plan: Plan = serde_json::from_str(plan_json)
        .map_err(|_| execution_request_error("invalid-plan-json", "plan JSON is malformed"))?;
    let timing = parse_timing(&timing)?;
    let cancellation = signal
        .clone()
        .map(super::transport::AbortSignalCancellation::new);
    let never = NeverCancelled;
    let cancellation: &dyn Cancellation = cancellation
        .as_ref()
        .map_or(&never, |cancellation| cancellation);
    let mut transport = super::transport::JsTransport::new(transport, signal);
    transport.validate_limits().map_err(|_| {
        execution_request_error(
            "invalid-transport",
            "browser transport has invalid payload limits",
        )
    })?;
    let result = execute_with_options(
        &plan,
        &mut transport,
        ExecutionOptions {
            timing,
            cancellation,
        },
        |progress| {
            if let Some(callback) = &on_progress {
                let _ = callback.call1(&JsValue::UNDEFINED, &progress_value(progress));
            }
        },
    )
    .await;
    Ok(map_execution_result(result))
}
