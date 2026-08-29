// SPDX-License-Identifier: AGPL-3.0-or-later
#![forbid(unsafe_code)]
use mb_printer_core::{
    Document, capabilities, export, importer, pdf_import, protocol, render, template,
};
use std::collections::BTreeMap;

pub fn validate_document_json(input: &str) -> String {
    match Document::from_json(input) {
        Ok(d) => match d.validate() {
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
    importer::import_v3(input)
        .and_then(|v| serde_json::to_string(&v).map_err(importer::ImportError::from))
        .map_err(|e| e.to_string())
}
pub fn evaluate_template_json(input: &str, fields_json: &str) -> Result<String, String> {
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
pub fn extract_laposte_json(
    code: &str,
    page: u32,
    width_um: i64,
    height_um: i64,
    raster_width: u32,
    raster_height: u32,
    pixels: Vec<u8>,
) -> Result<String, String> {
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
    serde_json::to_string(&values).map_err(|e| e.to_string())
}
pub fn protocol_plan_json(
    model: &str,
    width_bytes: u16,
    height: u32,
    bytes_json: &str,
) -> Result<String, String> {
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    let data: Vec<u8> = serde_json::from_str(bytes_json).map_err(|e| e.to_string())?;
    let raster = protocol::Raster {
        width_bytes,
        height,
        data,
    };
    let plan = protocol::plan(&printer, &raster, &Default::default()).map_err(|e| e.to_string())?;
    serde_json::to_string(&plan).map_err(|e| e.to_string())
}
pub fn render_packed(input: &str) -> Result<Vec<u8>, String> {
    let doc = Document::from_json(input).map_err(|e| e.to_string())?;
    render::render(&doc, Default::default())
        .map_err(|e| e.to_string())?
        .pack_msb()
        .map_err(|e| e.to_string())
}
pub fn render_png(input: &str) -> Result<Vec<u8>, String> {
    let doc = Document::from_json(input).map_err(|e| e.to_string())?;
    let raster = render::render(&doc, Default::default()).map_err(|e| e.to_string())?;
    export::png(&raster, doc.media.dpi).map_err(|e| e.to_string())
}
pub fn render_pdf(input: &str) -> Result<Vec<u8>, String> {
    let doc = Document::from_json(input).map_err(|e| e.to_string())?;
    let raster = render::render(&doc, Default::default()).map_err(|e| e.to_string())?;
    export::pdf_physical(&raster, doc.media.width, doc.media.height).map_err(|e| e.to_string())
}
pub fn render_batch_pdf(input: &str) -> Result<Vec<u8>, String> {
    let documents: Vec<Document> = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let mut rasters = Vec::with_capacity(documents.len());
    for document in &documents {
        document.validate().map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        rasters.push(render::render(document, Default::default()).map_err(|e| e.to_string())?);
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
    export::pdf_pages_physical(&pages).map_err(|e| e.to_string())
}
pub fn normalize_pdf_json(
    bytes: Vec<u8>,
    dpi: u16,
    first_page_only: bool,
) -> Result<String, String> {
    let pages = pdf_import::normalize(bytes, dpi, first_page_only, 100_000_000)
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
    serde_json::to_string(&values).map_err(|e| e.to_string())
}
pub fn extract_laposte_pdf_json(code: &str, bytes: Vec<u8>, dpi: u16) -> Result<String, String> {
    let pages = pdf_import::normalize(bytes, dpi, false, 100_000_000).map_err(|e| e.to_string())?;
    let stamps = mb_printer_core::laposte::extract(&pages, code).map_err(|e| e.to_string())?;
    let values:Vec<_>=stamps.into_iter().map(|s|serde_json::json!({"page":s.page,"sourcePage":s.page,"slot":s.slot,"widthUm":s.width_um,"heightUm":s.height_um,"rasterWidth":s.raster.width,"rasterHeight":s.raster.height,"pixels":s.raster.pixels})).collect();
    serde_json::to_string(&values).map_err(|e| e.to_string())
}
pub fn render_protocol_plan(input: &str, model: &str) -> Result<String, String> {
    let doc = Document::from_json(input).map_err(|e| e.to_string())?;
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    let raster = render::render_for_printer(&doc, &printer, Default::default())
        .map_err(|e| e.to_string())?;
    let plan = protocol::plan(&printer, &raster, &Default::default()).map_err(|e| e.to_string())?;
    serde_json::to_string(&plan).map_err(|e| e.to_string())
}
#[cfg(target_arch = "wasm32")]
mod bindings {
    use wasm_bindgen::prelude::*;
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
    #[wasm_bindgen(js_name=renderPdf)]
    pub fn render_pdf(input: &str) -> Result<Vec<u8>, JsValue> {
        super::render_pdf(input).map_err(|e| JsValue::from_str(&e))
    }
    #[wasm_bindgen(js_name=renderBatchPdf)]
    pub fn render_batch_pdf(input: &str) -> Result<Vec<u8>, JsValue> {
        super::render_batch_pdf(input).map_err(|e| JsValue::from_str(&e))
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
    #[wasm_bindgen(js_name=renderProtocolPlan)]
    pub fn render_protocol_plan(input: &str, model: &str) -> Result<String, JsValue> {
        super::render_protocol_plan(input, model).map_err(|e| JsValue::from_str(&e))
    }
}
