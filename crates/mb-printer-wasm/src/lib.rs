// SPDX-License-Identifier: AGPL-3.0-or-later
#![forbid(unsafe_code)]
use mb_printer_core::{
    Document, capabilities, export, importer, media, pdf_import, protocol, render, template,
};
use std::collections::BTreeMap;

fn document_render_options(document: &Document) -> render::RenderOptions {
    let setting = document.extensions.get("makersbrain.render:dither");
    let algorithm = setting
        .and_then(|value| value.get("algorithm"))
        .and_then(serde_json::Value::as_str);
    let threshold = setting
        .and_then(|value| value.get("threshold"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(128);
    let dither = match algorithm {
        Some("auto") => mb_printer_core::raster::Dither::Auto,
        Some("threshold") => mb_printer_core::raster::Dither::Threshold(threshold),
        Some("bayer") => mb_printer_core::raster::Dither::Bayer4,
        Some("floyd-steinberg") => mb_printer_core::raster::Dither::FloydSteinberg,
        Some("atkinson") => mb_printer_core::raster::Dither::Atkinson,
        Some(_) | None => render::RenderOptions::default().dither,
    };
    render::RenderOptions {
        dither,
        ..Default::default()
    }
}

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
    let plan = protocol::plan(&printer, &raster, &options).map_err(|e| e.to_string())?;
    serde_json::to_string(&plan).map_err(|e| e.to_string())
}
pub fn render_packed(input: &str) -> Result<Vec<u8>, String> {
    let doc = Document::from_json(input).map_err(|e| e.to_string())?;
    render::render(&doc, document_render_options(&doc))
        .map_err(|e| e.to_string())?
        .pack_msb()
        .map_err(|e| e.to_string())
}
pub fn render_png(input: &str) -> Result<Vec<u8>, String> {
    let doc = Document::from_json(input).map_err(|e| e.to_string())?;
    let raster = render::render(&doc, document_render_options(&doc)).map_err(|e| e.to_string())?;
    export::png(&raster, doc.media.dpi).map_err(|e| e.to_string())
}
pub fn render_pdf(input: &str) -> Result<Vec<u8>, String> {
    let doc = Document::from_json(input).map_err(|e| e.to_string())?;
    let raster = render::render(&doc, document_render_options(&doc)).map_err(|e| e.to_string())?;
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
        rasters.push(
            render::render(document, document_render_options(document))
                .map_err(|e| e.to_string())?,
        );
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
    let frames: Vec<Vec<u8>> = serde_json::from_str(frames_json).map_err(|e| e.to_string())?;
    serde_json::to_string(&protocol::phomemo_parse_status(&frames)).map_err(|e| e.to_string())
}
/// Decodes the 32-byte reply a Brother printer returns to `ESC i S`.
pub fn parse_brother_status_json(data: &[u8]) -> Result<String, String> {
    let status = protocol::brother_parse_status(data).map_err(|e| e.to_owned())?;
    serde_json::to_string(&status).map_err(|e| e.to_string())
}
pub fn render_protocol_plan(input: &str, model: &str) -> Result<String, String> {
    render_protocol_plan_with_options(input, model, "{}")
}
pub fn render_protocol_plan_with_options(
    input: &str,
    model: &str,
    options_json: &str,
) -> Result<String, String> {
    let doc = Document::from_json(input).map_err(|e| e.to_string())?;
    doc.validate().map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let printer = capabilities::by_id(model).ok_or_else(|| format!("unknown model: {model}"))?;
    let raster = render::render_for_printer(&doc, &printer, document_render_options(&doc))
        .map_err(|e| e.to_string())?;
    let mut options: protocol::Options =
        serde_json::from_str(options_json).map_err(|e| e.to_string())?;
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
    let plan = protocol::plan(&printer, &raster, &options).map_err(|e| e.to_string())?;
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
