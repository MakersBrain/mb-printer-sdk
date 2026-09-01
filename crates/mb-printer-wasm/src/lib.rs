// SPDX-License-Identifier: AGPL-3.0-or-later
#![forbid(unsafe_code)]
use mb_printer_core::{
    Document, capabilities, export, importer, ipp, materialize, media, pdf_import, protocol,
    render, template,
};
use std::collections::BTreeMap;
use std::num::NonZeroU16;

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetApiError {
    pub code: &'static str,
    pub message: String,
    pub details: serde_json::Value,
}

impl SheetApiError {
    fn request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: serde_json::json!({}),
        }
    }

    fn sheet(error: mb_printer_core::sheet::SheetError) -> Self {
        let details = match &error {
            mb_printer_core::sheet::SheetError::LabelSizeMismatch {
                item,
                actual_width_um,
                actual_height_um,
                expected_width_um,
                expected_height_um,
            } => serde_json::json!({
                "item": item,
                "actualWidthUm": actual_width_um,
                "actualHeightUm": actual_height_um,
                "expectedWidthUm": expected_width_um,
                "expectedHeightUm": expected_height_um,
            }),
            mb_printer_core::sheet::SheetError::SlotOutsidePaper { index } => {
                serde_json::json!({ "index": index })
            }
            mb_printer_core::sheet::SheetError::OverlappingSlots { left, right } => {
                serde_json::json!({ "left": left, "right": right })
            }
            mb_printer_core::sheet::SheetError::InvalidFirstSlot { first_slot } => {
                serde_json::json!({ "firstSlot": first_slot })
            }
            mb_printer_core::sheet::SheetError::InvalidDocument { item, .. }
            | mb_printer_core::sheet::SheetError::Render { item, .. } => {
                serde_json::json!({ "item": item })
            }
            mb_printer_core::sheet::SheetError::OutputTooLarge { limit } => {
                serde_json::json!({ "limit": limit })
            }
            _ => serde_json::json!({}),
        };
        Self {
            code: error.code(),
            message: error.to_string(),
            details,
        }
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializeApiError {
    pub version: u8,
    pub code: &'static str,
    pub message: String,
    pub details: serde_json::Value,
}

impl MaterializeApiError {
    fn request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            version: 1,
            code,
            message: message.into(),
            details: serde_json::json!({}),
        }
    }

    fn materialize(error: materialize::MaterializeError) -> Self {
        let details = match &error {
            materialize::MaterializeError::InvalidDocument { count } => {
                serde_json::json!({ "count": count })
            }
            materialize::MaterializeError::Template { element, .. } => {
                serde_json::json!({ "element": element })
            }
            materialize::MaterializeError::DuplicateZone { index }
            | materialize::MaterializeError::UnknownZone { index } => {
                serde_json::json!({ "index": index })
            }
            _ => serde_json::json!({}),
        };
        Self {
            version: 1,
            code: error.code(),
            message: error.to_string(),
            details,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct MaterializeOptionsWire {
    locale: String,
    current_date: String,
}

impl Default for MaterializeOptionsWire {
    fn default() -> Self {
        Self {
            locale: "en".into(),
            current_date: "1970-01-01".into(),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ZoneBatchInputWire {
    record_count: u32,
    zone_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct ZoneBatchOptionsWire {
    zone_ids: Vec<String>,
    locale: String,
    current_date: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ContinuousJobOptionsWire {
    cut_mode: capabilities::ContinuousCutMode,
    extra_feed_before_um: i64,
    extra_feed_after_um: i64,
    chain_copies: bool,
}

impl Default for ZoneBatchOptionsWire {
    fn default() -> Self {
        Self {
            zone_ids: Vec::new(),
            locale: "en".into(),
            current_date: "1970-01-01".into(),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SheetOptionsWire {
    first_slot: usize,
    dpi: u16,
}

impl TryFrom<SheetOptionsWire> for mb_printer_core::sheet::SheetOptions {
    type Error = SheetApiError;

    fn try_from(value: SheetOptionsWire) -> Result<Self, Self::Error> {
        let dpi = NonZeroU16::new(value.dpi).ok_or_else(|| {
            SheetApiError::request("sheet.invalid_dpi", "sheet DPI must be positive")
        })?;
        Ok(Self {
            first_slot: value.first_slot,
            dpi,
        })
    }
}

fn enforce_wire(
    inputs: &[&str],
    limits: mb_printer_core::limits::WireLimits,
) -> Result<(), SheetApiError> {
    let total = inputs
        .iter()
        .try_fold(0usize, |total, input| total.checked_add(input.len()));
    if total.is_none_or(|total| total > limits.max_input_bytes) {
        Err(SheetApiError::request(
            "request.too_large",
            "sheet request exceeds the encoded input limit",
        ))
    } else {
        Ok(())
    }
}

fn document_render_options(document: &Document) -> render::RenderOptions {
    render::options_for_document(document)
}

fn processing_limits() -> mb_printer_core::limits::ProcessingLimits {
    let mut limits = mb_printer_core::limits::ProcessingLimits::default();
    // JSON byte arrays can expand each binary byte to four encoded bytes.
    limits.max_plan_bytes = limits.max_plan_bytes.min(limits.max_output_bytes / 4);
    limits
}

fn enforce_json_wire(inputs: &[&str]) -> Result<(), String> {
    let limit = mb_printer_core::limits::WireLimits::default().max_input_bytes;
    let total = inputs
        .iter()
        .try_fold(0usize, |total, input| total.checked_add(input.len()));
    if total.is_none_or(|bytes| bytes > limit) {
        Err("request exceeds encoded input limit".into())
    } else {
        Ok(())
    }
}

fn bounded_json<T: serde::Serialize>(
    value: &T,
    limits: &mb_printer_core::limits::ProcessingLimits,
) -> Result<String, String> {
    let encoded = serde_json::to_string(value).map_err(|error| error.to_string())?;
    if encoded.len() > limits.max_output_bytes {
        Err("encoded output exceeds processing limit".into())
    } else {
        Ok(encoded)
    }
}

fn parse_document(
    input: &str,
    limits: &mb_printer_core::limits::ProcessingLimits,
) -> Result<Document, String> {
    enforce_json_wire(&[input])?;
    let document = Document::from_json(input).map_err(|error| error.to_string())?;
    document.validate_with_limits(limits).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    Ok(document)
}

pub fn validate_document_json(input: &str) -> String {
    if let Err(error) = enforce_json_wire(&[input]) {
        return serde_json::to_string(&vec![error]).unwrap();
    }
    let limits = processing_limits();
    match Document::from_json(input) {
        Ok(d) => match d.validate_with_limits(&limits) {
            Ok(()) => "[]".into(),
            Err(e) => serde_json::to_string(&e.iter().map(ToString::to_string).collect::<Vec<_>>())
                .unwrap(),
        },
        Err(e) => serde_json::to_string(&vec![e.to_string()]).unwrap(),
    }
}

pub fn capabilities_json() -> String {
    serde_json::to_string(&capabilities::bundled()).expect("definitions serialize")
}
pub fn import_v3_json(input: &str) -> Result<String, String> {
    enforce_json_wire(&[input])?;
    let limits = processing_limits();
    let value = importer::import_v3(input).map_err(|error| error.to_string())?;
    bounded_json(&value, &limits)
}
pub fn evaluate_template_json(input: &str, fields_json: &str) -> Result<String, String> {
    enforce_json_wire(&[input, fields_json])?;
    let fields: BTreeMap<String, String> =
        serde_json::from_str(fields_json).map_err(|e| e.to_string())?;
    template::evaluate(input, &fields).map_err(|e| e.to_string())
}
pub fn evaluate_template_context_json(
    input: &str,
    fields_json: &str,
    locale: &str,
    current_date: &str,
) -> Result<String, String> {
    enforce_json_wire(&[input, fields_json, locale, current_date])?;
    let fields: BTreeMap<String, String> =
        serde_json::from_str(fields_json).map_err(|e| e.to_string())?;
    template::evaluate_with_context(
        input,
        template::Context {
            fields: &fields,
            locale,
            current_date,
        },
    )
    .map_err(|e| e.to_string())
}

pub fn materialize_record_json(
    document_json: &str,
    record_json: &str,
    options_json: &str,
) -> Result<String, MaterializeApiError> {
    enforce_materialize_wire(&[document_json, record_json, options_json])?;
    let document: Document = serde_json::from_str(document_json)
        .map_err(|error| MaterializeApiError::request("request.invalid_json", error.to_string()))?;
    let record: BTreeMap<String, String> = serde_json::from_str(record_json)
        .map_err(|error| MaterializeApiError::request("request.invalid_json", error.to_string()))?;
    let options: MaterializeOptionsWire = serde_json::from_str(options_json)
        .map_err(|error| MaterializeApiError::request("request.invalid_json", error.to_string()))?;
    let output = materialize::materialize_record(
        &document,
        &record,
        materialize::MaterializeOptions {
            locale: &options.locale,
            current_date: &options.current_date,
        },
    )
    .map_err(MaterializeApiError::materialize)?;
    serde_json::to_string(&output)
        .map_err(|error| MaterializeApiError::request("request.encode_failed", error.to_string()))
}

pub fn plan_zone_batch_json(
    document_json: &str,
    input_json: &str,
) -> Result<String, MaterializeApiError> {
    enforce_materialize_wire(&[document_json, input_json])?;
    let document: Document = serde_json::from_str(document_json)
        .map_err(|error| MaterializeApiError::request("request.invalid_json", error.to_string()))?;
    let input: ZoneBatchInputWire = serde_json::from_str(input_json)
        .map_err(|error| MaterializeApiError::request("request.invalid_json", error.to_string()))?;
    if input.record_count > mb_printer_core::limits::WireLimits::default().max_request_documents {
        return Err(MaterializeApiError::request(
            "request.too_many_documents",
            "zone batch request contains too many records",
        ));
    }
    let output = materialize::plan_zone_batch(&document, input.record_count, &input.zone_ids)
        .map_err(MaterializeApiError::materialize)?;
    serde_json::to_string(&output)
        .map_err(|error| MaterializeApiError::request("request.encode_failed", error.to_string()))
}

pub fn materialize_zone_batch_json(
    document_json: &str,
    records_json: &str,
    options_json: &str,
) -> Result<String, MaterializeApiError> {
    enforce_materialize_wire(&[document_json, records_json, options_json])?;
    let document: Document = serde_json::from_str(document_json)
        .map_err(|error| MaterializeApiError::request("request.invalid_json", error.to_string()))?;
    let records: Vec<BTreeMap<String, String>> = serde_json::from_str(records_json)
        .map_err(|error| MaterializeApiError::request("request.invalid_json", error.to_string()))?;
    let options: ZoneBatchOptionsWire = serde_json::from_str(options_json)
        .map_err(|error| MaterializeApiError::request("request.invalid_json", error.to_string()))?;
    if records.len() > mb_printer_core::limits::WireLimits::default().max_request_documents as usize
    {
        return Err(MaterializeApiError::request(
            "request.too_many_documents",
            "materialization request contains too many records",
        ));
    }
    let output = materialize::materialize_zone_batch(
        &document,
        &records,
        &options.zone_ids,
        materialize::MaterializeOptions {
            locale: &options.locale,
            current_date: &options.current_date,
        },
    )
    .map_err(MaterializeApiError::materialize)?;
    serde_json::to_string(&output)
        .map_err(|error| MaterializeApiError::request("request.encode_failed", error.to_string()))
}

fn enforce_materialize_wire(inputs: &[&str]) -> Result<(), MaterializeApiError> {
    let limit = mb_printer_core::limits::WireLimits::default().max_input_bytes;
    let total = inputs
        .iter()
        .try_fold(0usize, |total, input| total.checked_add(input.len()));
    if total.is_none_or(|bytes| bytes > limit) {
        Err(MaterializeApiError::request(
            "request.too_large",
            "materialization request exceeds the encoded input limit",
        ))
    } else {
        Ok(())
    }
}
pub fn extract_laposte_json(
    code: &str,
    page: u32,
    width_um: i64,
    height_um: i64,
    raster_width: u32,
    raster_height: u32,
    pixels: Vec<u8>,
) -> Result<String, String> {
    let limits = processing_limits();
    let pixel_count = u64::from(raster_width)
        .checked_mul(u64::from(raster_height))
        .ok_or_else(|| "raster dimensions exceed processing limit".to_owned())?;
    if pixel_count > limits.max_canvas_pixels
        || usize::try_from(pixel_count).ok() != Some(pixels.len())
    {
        return Err("raster dimensions exceed processing limit".into());
    }
    let stamps = mb_printer_core::laposte::extract(
        &[mb_printer_core::laposte::NormalizedPage {
            page,
            width_um,
            height_um,
            raster: mb_printer_core::raster::GrayRaster {
                width: raster_width,
                height: raster_height,
                pixels,
            },
        }],
        code,
    )
    .map_err(|e| e.to_string())?;
    let values:Vec<_>=stamps.into_iter().map(|s|serde_json::json!({"page":s.page,"sourcePage":s.page,"slot":s.slot,"widthUm":s.width_um,"heightUm":s.height_um,"rasterWidth":s.raster.width,"rasterHeight":s.raster.height,"pixels":s.raster.pixels})).collect();
    bounded_json(&values, &limits)
}
pub fn protocol_plan_json(
    model: &str,
    width_bytes: u16,
    height: u32,
    bytes_json: &str,
) -> Result<String, String> {
    enforce_json_wire(&[bytes_json])?;
    let limits = processing_limits();
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    let data: Vec<u8> = serde_json::from_str(bytes_json).map_err(|e| e.to_string())?;
    let pixels = u64::from(width_bytes)
        .checked_mul(8)
        .and_then(|width| width.checked_mul(u64::from(height)))
        .ok_or_else(|| "raster dimensions exceed processing limit".to_owned())?;
    if pixels > limits.max_canvas_pixels || data.len() > limits.max_plan_bytes {
        return Err("raster dimensions exceed processing limit".into());
    }
    let raster = protocol::Raster {
        width_bytes,
        height,
        data,
    };
    let options = protocol::Options {
        brother_media: (printer.protocol == capabilities::Protocol::Brother).then_some(
            protocol::BrotherMedia {
                width_mm: 62,
                length_mm: 29,
                continuous: false,
                feed_margin: 0,
            },
        ),
        ..Default::default()
    };
    let plan = protocol::plan_with_limits(&printer, &raster, &options, &limits)
        .map_err(|e| e.to_string())?;
    bounded_json(&plan, &limits)
}
pub fn render_packed(input: &str) -> Result<Vec<u8>, String> {
    let limits = processing_limits();
    let doc = parse_document(input, &limits)?;
    let output = render::render_with_limits(&doc, document_render_options(&doc), &limits)
        .map_err(|e| e.to_string())?
        .pack_msb()
        .map_err(|e| e.to_string())?;
    if output.len() > limits.max_output_bytes {
        Err("packed raster exceeds processing limit".into())
    } else {
        Ok(output)
    }
}
pub fn render_png(input: &str) -> Result<Vec<u8>, String> {
    let limits = processing_limits();
    let doc = parse_document(input, &limits)?;
    let raster = render::render_with_limits(&doc, document_render_options(&doc), &limits)
        .map_err(|e| e.to_string())?;
    export::png_with_limits(&raster, doc.media.dpi, &limits).map_err(|e| e.to_string())
}
pub fn measure_document_json(input: &str) -> Result<String, String> {
    let limits = processing_limits();
    let doc = parse_document(input, &limits)?;
    let measurement = render::measure_with_limits(&doc, &limits).map_err(|e| e.to_string())?;
    bounded_json(&measurement, &limits)
}
pub fn render_pdf(input: &str) -> Result<Vec<u8>, String> {
    let limits = processing_limits();
    let doc = parse_document(input, &limits)?;
    let raster = render::render_with_limits(&doc, document_render_options(&doc), &limits)
        .map_err(|e| e.to_string())?;
    export::pdf_physical_with_limits(&raster, doc.media.width, doc.media.height, &limits)
        .map_err(|e| e.to_string())
}
pub fn render_batch_pdf(input: &str) -> Result<Vec<u8>, String> {
    enforce_json_wire(&[input])?;
    let limits = processing_limits();
    let documents: Vec<Document> = serde_json::from_str(input).map_err(|e| e.to_string())?;
    if documents.is_empty()
        || documents.len() > usize::try_from(limits.max_pages).unwrap_or(usize::MAX)
        || documents.len()
            > usize::try_from(mb_printer_core::limits::WireLimits::default().max_request_documents)
                .unwrap_or(usize::MAX)
    {
        return Err("batch document count exceeds processing limit".into());
    }
    let mut rasters = Vec::with_capacity(documents.len());
    let mut total_pixels = 0u64;
    for document in &documents {
        document.validate_with_limits(&limits).map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let raster =
            render::render_with_limits(document, document_render_options(document), &limits)
                .map_err(|e| e.to_string())?;
        total_pixels = total_pixels
            .checked_add(u64::from(raster.width) * u64::from(raster.height))
            .ok_or_else(|| "batch pixels exceed processing limit".to_owned())?;
        if total_pixels > limits.max_total_pixels {
            return Err("batch pixels exceed processing limit".into());
        }
        rasters.push(raster);
    }
    let pages = rasters
        .iter()
        .zip(&documents)
        .map(|(raster, document)| export::PdfPage {
            raster,
            width_um: document.media.width,
            height_um: document.media.height,
        })
        .collect::<Vec<_>>();
    export::pdf_pages_physical_with_limits(&pages, &limits).map_err(|e| e.to_string())
}

pub fn plan_sheet_json(
    plan_input_json: &str,
    layout_json: &str,
    options_json: &str,
) -> Result<String, SheetApiError> {
    let wire_limits = mb_printer_core::limits::WireLimits::default();
    enforce_wire(&[plan_input_json, layout_json, options_json], wire_limits)?;
    let input: mb_printer_core::sheet::SheetPlanInput = serde_json::from_str(plan_input_json)
        .map_err(|error| SheetApiError::request("request.invalid_json", error.to_string()))?;
    if input.item_count > wire_limits.max_request_documents {
        return Err(SheetApiError::request(
            "request.too_many_documents",
            "sheet request contains too many items",
        ));
    }
    let definition: mb_printer_core::sheet::SheetDefinition = serde_json::from_str(layout_json)
        .map_err(|error| SheetApiError::request("request.invalid_json", error.to_string()))?;
    let options: SheetOptionsWire = serde_json::from_str(options_json)
        .map_err(|error| SheetApiError::request("request.invalid_json", error.to_string()))?;
    let plan = mb_printer_core::sheet::plan(
        input,
        &definition,
        options.try_into()?,
        &mb_printer_core::limits::ProcessingLimits::default(),
    )
    .map_err(SheetApiError::sheet)?;
    serde_json::to_string(&plan)
        .map_err(|error| SheetApiError::request("internal.serialization", error.to_string()))
}

pub fn build_sheet_pdf_json(
    documents_json: &str,
    layout_json: &str,
    options_json: &str,
) -> Result<Vec<u8>, SheetApiError> {
    let wire_limits = mb_printer_core::limits::WireLimits::default();
    enforce_wire(&[documents_json, layout_json, options_json], wire_limits)?;
    let documents: Vec<Document> = serde_json::from_str(documents_json)
        .map_err(|error| SheetApiError::request("request.invalid_json", error.to_string()))?;
    if documents.len()
        > usize::try_from(wire_limits.max_request_documents).map_err(|_| {
            SheetApiError::request("request.too_many_documents", "invalid document limit")
        })?
    {
        return Err(SheetApiError::request(
            "request.too_many_documents",
            "sheet request contains too many documents",
        ));
    }
    let definition: mb_printer_core::sheet::SheetDefinition = serde_json::from_str(layout_json)
        .map_err(|error| SheetApiError::request("request.invalid_json", error.to_string()))?;
    let options: SheetOptionsWire = serde_json::from_str(options_json)
        .map_err(|error| SheetApiError::request("request.invalid_json", error.to_string()))?;
    mb_printer_core::sheet::pdf(
        &documents,
        &definition,
        options.try_into()?,
        &mb_printer_core::limits::ProcessingLimits::default(),
    )
    .map_err(SheetApiError::sheet)
}
pub fn normalize_pdf_json(
    bytes: Vec<u8>,
    dpi: u16,
    first_page_only: bool,
) -> Result<String, String> {
    if bytes.len() > mb_printer_core::limits::WireLimits::default().max_input_bytes {
        return Err("PDF input exceeds encoded input limit".into());
    }
    let mut limits = processing_limits();
    let json_pixels = u64::try_from(limits.max_output_bytes / 4).unwrap_or(u64::MAX);
    limits.max_canvas_pixels = limits.max_canvas_pixels.min(json_pixels);
    limits.max_total_pixels = limits.max_total_pixels.min(json_pixels);
    let pages = pdf_import::normalize_with_limits(bytes, dpi, first_page_only, &limits)
        .map_err(|e| e.to_string())?;
    let values: Vec<_> = pages
        .into_iter()
        .map(|page| {
            serde_json::json!({
                "page":page.page,"sourcePage":page.page,"widthUm":page.width_um,"heightUm":page.height_um,
                "rasterWidth":page.raster.width,"rasterHeight":page.raster.height,
                "pixels":page.raster.pixels
            })
        })
        .collect();
    bounded_json(&values, &limits)
}
pub fn extract_laposte_pdf_json(code: &str, bytes: Vec<u8>, dpi: u16) -> Result<String, String> {
    if bytes.len() > mb_printer_core::limits::WireLimits::default().max_input_bytes {
        return Err("PDF input exceeds encoded input limit".into());
    }
    let mut limits = processing_limits();
    let json_pixels = u64::try_from(limits.max_output_bytes / 4).unwrap_or(u64::MAX);
    limits.max_canvas_pixels = limits.max_canvas_pixels.min(json_pixels);
    limits.max_total_pixels = limits.max_total_pixels.min(json_pixels);
    let pages =
        pdf_import::normalize_with_limits(bytes, dpi, false, &limits).map_err(|e| e.to_string())?;
    let stamps = mb_printer_core::laposte::extract(&pages, code).map_err(|e| e.to_string())?;
    let values:Vec<_>=stamps.into_iter().map(|s|serde_json::json!({"page":s.page,"sourcePage":s.page,"slot":s.slot,"widthUm":s.width_um,"heightUm":s.height_um,"rasterWidth":s.raster.width,"rasterHeight":s.raster.height,"pixels":s.raster.pixels})).collect();
    bounded_json(&values, &limits)
}
/// Every media a model can carry, already filtered by head width and tape width.
pub fn media_presets_json(model: &str) -> Result<String, String> {
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    serde_json::to_string(&media::presets_for(&printer)).map_err(|e| e.to_string())
}
/// Names the media a printer reported, or `null` when nothing matches.
pub fn match_media_json(model: &str, width_mm: f64, height_mm: f64) -> Result<String, String> {
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    serde_json::to_string(&media::match_media(&printer, width_mm, height_mm))
        .map_err(|e| e.to_string())
}
/// Document-free plan that only asks the printer for its status.
pub fn status_plan_json(model: &str) -> Result<String, String> {
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    let plan = protocol::status_plan(&printer).map_err(|e| e.to_string())?;
    serde_json::to_string(&serde_json::json!({"protocol":plan.protocol,"actions":plan.actions}))
        .map_err(|e| e.to_string())
}
/// Decodes the `1a <type> <payload>` notification frames a Phomemo printer
/// returns to the `1f 11` queries. Input is a JSON array of byte arrays.
pub fn parse_phomemo_status_json(frames_json: &str) -> Result<String, String> {
    enforce_json_wire(&[frames_json])?;
    let frames: Vec<Vec<u8>> = serde_json::from_str(frames_json).map_err(|e| e.to_string())?;
    let limits = processing_limits();
    let bytes = frames
        .iter()
        .try_fold(0usize, |total, frame| total.checked_add(frame.len()));
    if frames.len() > limits.max_plan_actions
        || bytes.is_none_or(|bytes| bytes > limits.max_resource_bytes)
    {
        return Err("status frames exceed processing limit".into());
    }
    serde_json::to_string(&protocol::phomemo_parse_status(&frames)).map_err(|e| e.to_string())
}
/// Decodes the 32-byte reply a Brother printer returns to `ESC i S`.
pub fn parse_brother_status_json(data: &[u8]) -> Result<String, String> {
    if data.len() > processing_limits().max_resource_bytes {
        return Err("status frame exceeds processing limit".into());
    }
    let status = protocol::brother_parse_status(data).map_err(|e| e.to_owned())?;
    serde_json::to_string(&status).map_err(|e| e.to_string())
}

/// Decode bounded IPP bytes using the same portable codec as native clients.
pub fn decode_ipp_json(data: &[u8], maximum_message_bytes: usize) -> Result<String, String> {
    let limits = ipp::Limits {
        max_message_bytes: maximum_message_bytes,
        ..ipp::Limits::default()
    };
    let message = ipp::decode(data, limits).map_err(|error| error.to_string())?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}

/// Encode a typed IPP message without accessing HTTP, TLS, Tokio, or browser
/// globals. Promise/AbortSignal transport adapters remain in TypeScript.
pub fn encode_ipp_json(input: &str, maximum_message_bytes: usize) -> Result<Vec<u8>, String> {
    let message: ipp::Message = serde_json::from_str(input).map_err(|error| error.to_string())?;
    message
        .encode(ipp::Limits {
            max_message_bytes: maximum_message_bytes,
            ..ipp::Limits::default()
        })
        .map_err(|error| error.to_string())
}
pub fn render_protocol_plan(input: &str, model: &str) -> Result<String, String> {
    render_protocol_plan_with_options(input, model, "{}")
}
pub fn render_protocol_plan_with_options(
    input: &str,
    model: &str,
    options_json: &str,
) -> Result<String, String> {
    enforce_json_wire(&[input, options_json])?;
    let limits = processing_limits();
    let doc = parse_document(input, &limits)?;
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    let raster = render::render_for_printer_with_limits(
        &doc,
        &printer,
        document_render_options(&doc),
        &limits,
    )
    .map_err(|e| e.to_string())?;
    let mut options = protocol_options(options_json, &doc, &printer)?;
    if printer.protocol == capabilities::Protocol::Tspl {
        let tenths_mm = |value: i64| {
            u16::try_from(value.saturating_add(50) / 100)
                .map_err(|_| "TSPL media dimension is outside range".to_owned())
        };
        if options.label_width_tenths_mm.is_none() {
            options.label_width_tenths_mm = Some(tenths_mm(doc.media.width)?);
        }
        if options.label_height_tenths_mm.is_none() {
            options.label_height_tenths_mm = Some(tenths_mm(doc.media.height)?);
        }
    }
    if printer.protocol == capabilities::Protocol::Brother {
        let brother_62x29 = (doc.media.width.abs_diff(62_000) <= 1_500
            && doc.media.height.abs_diff(29_000) <= 1_500)
            || (doc.media.width.abs_diff(29_000) <= 1_500
                && doc.media.height.abs_diff(62_000) <= 1_500);
        let millimetres = |value: i64| {
            u8::try_from(value.saturating_add(500) / 1000)
                .map_err(|_| "Brother media dimension is outside range".to_owned())
        };
        options.continuous = doc.media.continuous;
        if options.brother_media.is_none() {
            options.brother_media = Some(protocol::BrotherMedia {
                width_mm: if brother_62x29 {
                    62
                } else {
                    millimetres(doc.media.width)?
                },
                length_mm: if doc.media.continuous {
                    0
                } else if brother_62x29 {
                    29
                } else {
                    millimetres(doc.media.height)?
                },
                continuous: doc.media.continuous,
                feed_margin: 0,
            });
        }
    }
    let plan = protocol::plan_with_limits(&printer, &raster, &options, &limits)
        .map_err(|e| e.to_string())?;
    bounded_json(&plan, &limits)
}

pub fn render_protocol_batch_plan_with_options(
    input: &str,
    model: &str,
    options_json: &str,
) -> Result<String, String> {
    enforce_json_wire(&[input, options_json])?;
    let limits = processing_limits();
    let documents: Vec<Document> = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let maximum_documents =
        usize::try_from(mb_printer_core::limits::WireLimits::default().max_request_documents)
            .unwrap_or(usize::MAX);
    if documents.is_empty() || documents.len() > maximum_documents {
        return Err("batch document count exceeds processing limit".into());
    }
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    if printer.protocol != capabilities::Protocol::Brother {
        return Err("printer protocol does not support native variable-raster batches".into());
    }
    let first = &documents[0];
    if documents.iter().any(|document| {
        document.media.continuous != first.media.continuous
            || document.media.width != first.media.width
            || document.media.dpi != first.media.dpi
    }) {
        return Err("batch documents must use the same roll width, DPI, and media mode".into());
    }
    for document in &documents {
        validate_continuous_document(document, &printer)?;
    }

    let mut options_value: serde_json::Value =
        serde_json::from_str(options_json).map_err(|error| error.to_string())?;
    let copies = options_value
        .get("copies")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let labels = copies
        .checked_mul(documents.len() as u64)
        .ok_or_else(|| "batch label count exceeds range".to_owned())?;
    if labels == 0 || labels > u64::from(u8::MAX) {
        return Err("batch label count exceeds cut-every range".into());
    }
    let batch_continuous = options_value
        .get("continuous")
        .cloned()
        .map(serde_json::from_value::<ContinuousJobOptionsWire>)
        .transpose()
        .map_err(|error| error.to_string())?;
    options_value["copies"] = serde_json::Value::from(labels);
    let options_text = serde_json::to_string(&options_value).map_err(|e| e.to_string())?;
    let mut options = protocol_options(&options_text, first, &printer)?;
    if batch_continuous.as_ref().is_some_and(|job| {
        job.chain_copies && matches!(job.cut_mode, capabilities::ContinuousCutMode::AfterEach)
    }) {
        options.cut_every =
            u8::try_from(copies).map_err(|_| "copy count exceeds cut-every range".to_owned())?;
    }
    options.copies = 1;

    if printer.protocol == capabilities::Protocol::Brother {
        let millimetres = |value: i64| {
            u8::try_from(value.saturating_add(500) / 1000)
                .map_err(|_| "Brother media dimension is outside range".to_owned())
        };
        options.continuous = first.media.continuous;
        options.brother_media = Some(protocol::BrotherMedia {
            width_mm: millimetres(first.media.width)?,
            length_mm: if first.media.continuous {
                0
            } else {
                millimetres(first.media.height)?
            },
            continuous: first.media.continuous,
            feed_margin: 0,
        });
    }

    let mut rasters = Vec::with_capacity(usize::try_from(labels).unwrap_or(0));
    for document in &documents {
        let raster = render::render_for_printer_with_limits(
            document,
            &printer,
            document_render_options(document),
            &limits,
        )
        .map_err(|e| e.to_string())?;
        for _ in 0..copies {
            rasters.push(protocol::Raster {
                width_bytes: raster.width_bytes,
                height: raster.height,
                data: raster.data.clone(),
            });
        }
    }
    let plan = protocol::plan_batch_with_limits(&printer, &rasters, &options, &limits)
        .map_err(|e| e.to_string())?;
    bounded_json(&plan, &limits)
}

fn protocol_options(
    options_json: &str,
    document: &Document,
    printer: &capabilities::PrinterDefinition,
) -> Result<protocol::Options, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(options_json).map_err(|error| error.to_string())?;
    let continuous = value
        .as_object_mut()
        .and_then(|options| options.get("continuous"))
        .filter(|value| value.is_object())
        .cloned();
    if continuous.is_some() {
        value
            .as_object_mut()
            .expect("continuous options require an object")
            .remove("continuous");
    }
    let mut options: protocol::Options =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    let capabilities = validate_continuous_document(document, printer)?;
    let Some(value) = continuous else {
        return Ok(options);
    };
    let job: ContinuousJobOptionsWire =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    let capabilities = capabilities
        .ok_or_else(|| "continuous print options require continuous media".to_owned())?;
    if !capabilities.cut_modes.contains(&job.cut_mode) {
        return Err("requested continuous cut mode is unsupported by this printer".into());
    }
    if job.chain_copies && !capabilities.supports_chained_raster {
        return Err("printer does not support chained continuous rasters".into());
    }
    let feeds = [job.extra_feed_before_um, job.extra_feed_after_um];
    if feeds.iter().any(|feed| *feed < 0)
        || feeds.iter().any(|feed| {
            (*feed as f64) < capabilities.minimum_extra_feed_mm * 1_000.0
                || (*feed as f64) > capabilities.maximum_extra_feed_mm * 1_000.0
        })
    {
        return Err("continuous extra feed is outside the printer capability range".into());
    }
    options.cut = !matches!(job.cut_mode, capabilities::ContinuousCutMode::None);
    options.cut_every = match job.cut_mode {
        capabilities::ContinuousCutMode::AfterEach if job.chain_copies => {
            u8::try_from(options.copies)
                .map_err(|_| "copy count exceeds cut-every range".to_owned())?
        }
        capabilities::ContinuousCutMode::AfterEach => 1,
        capabilities::ContinuousCutMode::AfterJob => u8::try_from(options.copies)
            .map_err(|_| "copy count exceeds cut-every range".to_owned())?,
        capabilities::ContinuousCutMode::None => 1,
    };
    options.continuous = true;
    Ok(options)
}

fn validate_continuous_document<'a>(
    document: &Document,
    printer: &'a capabilities::PrinterDefinition,
) -> Result<Option<&'a capabilities::ContinuousMediaCapabilities>, String> {
    if !document.media.continuous {
        return Ok(None);
    }
    let capabilities = printer
        .continuous_media
        .as_ref()
        .filter(|capabilities| capabilities.supported)
        .ok_or_else(|| "printer does not support qualified continuous-media jobs".to_owned())?;
    let length_mm = document.media.height as f64 / 1_000.0;
    if length_mm < capabilities.minimum_length_mm || length_mm > capabilities.maximum_length_mm {
        return Err("continuous document length is outside the printer capability range".into());
    }
    Ok(Some(capabilities))
}
#[cfg(target_arch = "wasm32")]
mod bindings {
    use wasm_bindgen::prelude::*;

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
    pub fn normalize_pdf(
        bytes: Vec<u8>,
        dpi: u16,
        first_page_only: bool,
    ) -> Result<String, JsValue> {
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
        super::decode_ipp_json(data, maximum_message_bytes)
            .map_err(|error| JsValue::from_str(&error))
    }

    #[wasm_bindgen(js_name=encodeIpp)]
    pub fn encode_ipp(input: &str, maximum_message_bytes: usize) -> Result<Vec<u8>, JsValue> {
        super::encode_ipp_json(input, maximum_message_bytes)
            .map_err(|error| JsValue::from_str(&error))
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
}

#[cfg(test)]
mod render_option_tests {
    use super::*;

    #[test]
    fn reads_non_destructive_dither_profile_from_document_extension() {
        let input = r#"{"version":4,"name":"dither","media":{"width":1000,"height":1000,"unit":"micrometre","dpi":203,"orientation":"portrait","printableBounds":{"x":0,"y":0,"width":1000,"height":1000},"shape":"rectangle"},"coordinateSystem":{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"},"elements":[],"resources":[],"fields":[],"extensions":{"makersbrain.render:dither":{"algorithm":"atkinson","threshold":140}}}"#;
        let document = Document::from_json(input).unwrap();
        assert_eq!(
            document_render_options(&document).dither,
            mb_printer_core::raster::Dither::Atkinson
        );
    }
}
