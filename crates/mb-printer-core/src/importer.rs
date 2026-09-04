// SPDX-License-Identifier: AGPL-3.0-or-later
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum ImportError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("only v3 documents are importable")]
    Version,
    #[error("missing dimensions")]
    Dimensions,
    #[error("unsupported legacy element: {0}")]
    Element(String),
    #[error("invalid embedded legacy resource")]
    Resource,
}
pub fn import_v3(input: &str) -> Result<Value, ImportError> {
    let v: Value = serde_json::from_str(input)?;
    if v.get("version").and_then(Value::as_u64) != Some(3) {
        return Err(ImportError::Version);
    }
    let width = num(&v, ["widthMm", "labelSize.width"])?;
    let height = num(&v, ["heightMm", "labelSize.height"])?;
    let dpmm = v.get("dotsPerMm").and_then(Value::as_f64).unwrap_or(8.0);
    let mut elements = Vec::new();
    let mut resources = Vec::new();
    let mut legacy_elements = Vec::new();
    for (index, element) in v
        .get("elements")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        flatten_element(element, &format!("legacy-{index}"), &mut legacy_elements)?;
    }
    for (z, e) in legacy_elements.iter().enumerate() {
        elements.push(element(e, z, dpmm, &mut resources)?)
    }
    let memberships: Vec<(String, String)> = elements
        .iter()
        .filter(|e| e["type"] == "group")
        .flat_map(|g| {
            let group = g["id"].as_str().unwrap_or("").to_owned();
            g["children"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |c| c.as_str().map(|id| (id.to_owned(), group.clone())))
        })
        .collect();
    for (element_id, group_id) in memberships {
        if let Some(child) = elements.iter_mut().find(|e| e["id"] == element_id) {
            child["groupId"] = json!(group_id)
        }
    }
    Ok(
        json!({"version":4,"name":v.get("name").and_then(Value::as_str).unwrap_or("Imported v3"),"media":{"width":mm(width),"height":mm(height),"unit":"micrometre","dpi":(dpmm*25.4).round() as u64,"orientation":"portrait","printableBounds":{"x":0,"y":0,"width":mm(width),"height":mm(height)},"shape":if v.get("round").and_then(Value::as_bool).unwrap_or(false){"round"}else{"rectangle"},"continuous":v.get("continuous").and_then(Value::as_bool).unwrap_or(false),"zones":[]},"coordinateSystem":{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"},"elements":elements,"resources":resources,"fields":fields(&v),"extensions":{"makersbrain:legacy-v3":{"dotsPerMm":dpmm}}}),
    )
}


/// Legacy field descriptors carry editor-only keys such as `source` and `binding`;
/// the canonical document keeps only the key and its label.
fn fields(v: &Value) -> Value {
    let fields = v
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|field| {
            let key = field.get("key").and_then(Value::as_str)?;
            let label = field.get("label").and_then(Value::as_str).unwrap_or(key);
            Some(json!({ "key": key, "label": label }))
        })
        .collect::<Vec<_>>();
    Value::Array(fields)
}

/// Flatten legacy groups whose children were embedded objects rather than ID references.
fn flatten_element(
    value: &Value,
    fallback_id: &str,
    output: &mut Vec<Value>,
) -> Result<(), ImportError> {
    let mut current = value
        .as_object()
        .cloned()
        .ok_or_else(|| ImportError::Element("non-object".into()))?;
    let id = current
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(fallback_id)
        .to_owned();
    current.insert("id".into(), json!(id));
    let raw_type = current
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let children = current
        .get("children")
        .or_else(|| current.get("elements"))
        .and_then(Value::as_array)
        .cloned();
    if raw_type == "group" {
        let mut child_ids = Vec::new();
        if let Some(children) = children {
            for (index, child) in children.iter().enumerate() {
                if let Some(child_id) = child.as_str() {
                    child_ids.push(child_id.to_owned());
                } else {
                    let child_id = child
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{id}-child-{index}"));
                    child_ids.push(child_id.clone());
                    flatten_element(child, &child_id, output)?;
                }
            }
        }
        current.insert("children".into(), json!(child_ids));
        current.remove("elements");
    }
    // Parents appear after descendants so equal-z rendering remains deterministic.
    output.push(Value::Object(current));
    Ok(())
}
fn num<const N: usize>(v: &Value, keys: [&str; N]) -> Result<f64, ImportError> {
    for k in keys {
        let mut x = v;
        for p in k.split('.') {
            x = &x[p]
        }
        if let Some(n) = x.as_f64() {
            return Ok(n);
        }
    }
    Err(ImportError::Dimensions)
}
fn mm(v: f64) -> i64 {
    (v * 1000.0).round() as i64
}
fn px(v: &Value, k: &str, dpmm: f64) -> i64 {
    mm(v.get(k).and_then(Value::as_f64).unwrap_or(0.0) / dpmm)
}
fn element(e: &Value, z: usize, d: f64, resources: &mut Vec<Value>) -> Result<Value, ImportError> {
    let raw = e
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let ty = match raw.as_str() {
        "qr" | "qrcode" => "qr",
        "rect" => "rectangle",
        "bar-code" => "barcode",
        "img" => "image",
        x => x,
    };
    let fallback = format!("legacy-{z}");
    let common = json!({"id":e.get("id").and_then(Value::as_str).unwrap_or(&fallback),"transform":{"x":px(e,"x",d),"y":px(e,"y",d),"width":px(e,"width",d).max(1),"height":px(e,"height",d).max(1),"rotationMillidegrees":(e.get("rotation").and_then(Value::as_f64).unwrap_or(0.0)*1000.0).round()as i64},"zOrder":e.get("zOrder").and_then(Value::as_i64).unwrap_or(z as i64),"visible":e.get("visible").and_then(Value::as_bool).unwrap_or(true),"locked":e.get("locked").and_then(Value::as_bool).unwrap_or(false)});
    let mut o = common.as_object().unwrap().clone();
    o.insert(
        "type".into(),
        json!(match ty {
            "qr" => "qr-code",
            x => x,
        }),
    );
    match ty {
        "text" => {
            o.insert("text".into(), e.get("text").cloned().unwrap_or(json!("")));
            o.insert("fontSize".into(), json!(px(e, "fontSize", d)));
            o.insert(
                "horizontalAlign".into(),
                json!(align(
                    e.get("align").and_then(Value::as_str).unwrap_or("left")
                )),
            );
            o.insert("verticalAlign".into(), json!(valign(e)));
            o.insert("overflow".into(), json!("no-wrap"));
            if let Some(data) = e.get("fontData").and_then(Value::as_str) {
                let id = add_resource(resources, data, "font/ttf")?;
                o.insert("fontResource".into(), json!(id));
            }
        }
        "qr" => {
            o.insert("data".into(), e.get("qrData").cloned().unwrap_or(json!("")));
            o.insert("errorCorrection".into(), json!("M"));
        }
        "rectangle" | "ellipse" | "triangle" | "line" => {
            o.insert("strokeWidth".into(), json!(125));
            if ty != "line" {
                o.insert("fill".into(), json!(false));
            }
        }
        "image" => {
            let data = e
                .get("imageData")
                .or_else(|| e.get("src"))
                .and_then(Value::as_str)
                .ok_or(ImportError::Resource)?;
            let id = add_resource(resources, data, "image/png")?;
            o.insert("resource".into(), json!(id));
        }
        "svg" => {
            let data = e
                .get("svgData")
                .or_else(|| e.get("svg"))
                .and_then(Value::as_str)
                .ok_or(ImportError::Resource)?;
            let encoded = if data.trim_start().starts_with('<') {
                format!(
                    "data:image/svg+xml;base64,{}",
                    STANDARD.encode(data.as_bytes())
                )
            } else {
                data.into()
            };
            let id = add_resource(resources, &encoded, "image/svg+xml")?;
            o.insert("resource".into(), json!(id));
        }
        "barcode" => {
            o.insert(
                "data".into(),
                e.get("barcodeData")
                    .or_else(|| e.get("data"))
                    .or_else(|| e.get("text"))
                    .cloned()
                    .unwrap_or(json!("")),
            );
            let raw = e
                .get("symbology")
                .or_else(|| e.get("format"))
                .and_then(Value::as_str)
                .unwrap_or("code128")
                .to_ascii_lowercase()
                .replace([' ', '-'], "");
            let sym = match raw.as_str() {
                "ean13" => "ean13",
                "upca" => "upc-a",
                "code39" => "code39",
                _ => "code128",
            };
            o.insert("symbology".into(), json!(sym));
            o.insert(
                "humanReadable".into(),
                json!(
                    e.get("humanReadable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                ),
            );
        }
        "group" => {
            o.insert(
                "children".into(),
                e.get("children")
                    .or_else(|| e.get("elementIds"))
                    .cloned()
                    .unwrap_or(json!([])),
            );
        }
        x => return Err(ImportError::Element(x.into())),
    }
    Ok(Value::Object(o))
}
fn add_resource(
    resources: &mut Vec<Value>,
    data: &str,
    fallback_media: &str,
) -> Result<String, ImportError> {
    let (media, payload) = if let Some(rest) = data.strip_prefix("data:") {
        let (meta, payload) = rest.split_once(',').ok_or(ImportError::Resource)?;
        (meta.split(';').next().unwrap_or(fallback_media), payload)
    } else {
        (fallback_media, data)
    };
    let bytes = STANDARD
        .decode(payload.as_bytes())
        .map_err(|_| ImportError::Resource)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if let Some(existing) = resources.iter().find(|r| r["sha256"] == hash) {
        return Ok(existing["id"].as_str().unwrap().into());
    }
    let id = format!("res-{}", &hash[..16]);
    resources
        .push(json!({"id":id,"mediaType":media,"sha256":hash,"dataBase64":STANDARD.encode(bytes)}));
    Ok(id)
}
fn align(s: &str) -> &str {
    match s.to_ascii_lowercase().as_str() {
        "centre" | "middle" => "center",
        "end" => "right",
        _ => match s {
            "center" | "right" => s,
            _ => "left",
        },
    }
}
fn valign(e: &Value) -> &str {
    let s = e
        .get("verticalAlign")
        .or_else(|| e.get("valign"))
        .and_then(Value::as_str)
        .unwrap_or("top");
    match s.to_ascii_lowercase().as_str() {
        "center" | "centre" => "middle",
        "bottom" | "end" => "bottom",
        _ => "top",
    }
}
