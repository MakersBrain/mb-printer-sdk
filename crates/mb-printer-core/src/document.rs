// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Document {
    pub version: u8,
    pub name: String,
    pub media: Media,
    pub coordinate_system: CoordinateSystem,
    #[serde(default)]
    pub elements: Vec<Element>,
    #[serde(default)]
    pub resources: Vec<Resource>,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Media {
    pub width: i64,
    pub height: i64,
    pub unit: Unit,
    pub dpi: u16,
    pub orientation: Orientation,
    pub printable_bounds: Bounds,
    pub shape: MediaShape,
    #[serde(default)]
    pub continuous: bool,
    #[serde(default)]
    pub zones: Vec<Zone>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Unit {
    Micrometre,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    Portrait,
    Landscape,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaShape {
    Rectangle,
    Round,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rounding {
    HalfAwayFromZero,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CoordinateSystem {
    pub unit: Unit,
    pub origin: Origin,
    pub rounding: Rounding,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    TopLeft,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Zone {
    pub id: String,
    pub bounds: Bounds,
    #[serde(default)]
    pub clone_of: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Transform {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    #[serde(default)]
    pub rotation_millidegrees: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Constraints {
    #[serde(default)]
    pub preserve_aspect: bool,
    #[serde(default)]
    pub zone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Element {
    Text {
        #[serde(flatten)]
        common: Common,
        text: String,
        #[serde(default)]
        font_resource: Option<String>,
        font_size: i64,
        horizontal_align: HorizontalAlign,
        vertical_align: VerticalAlign,
        overflow: TextOverflow,
    },
    Image {
        #[serde(flatten)]
        common: Common,
        resource: String,
        #[serde(default)]
        crop: Option<Bounds>,
    },
    Svg {
        #[serde(flatten)]
        common: Common,
        resource: String,
    },
    Line {
        #[serde(flatten)]
        common: Common,
        stroke_width: i64,
    },
    Rectangle {
        #[serde(flatten)]
        common: Common,
        stroke_width: i64,
        #[serde(default)]
        fill: bool,
    },
    Ellipse {
        #[serde(flatten)]
        common: Common,
        stroke_width: i64,
        #[serde(default)]
        fill: bool,
    },
    Triangle {
        #[serde(flatten)]
        common: Common,
        stroke_width: i64,
        #[serde(default)]
        fill: bool,
    },
    Barcode {
        #[serde(flatten)]
        common: Common,
        data: String,
        symbology: BarcodeSymbology,
        #[serde(default)]
        human_readable: bool,
    },
    QrCode {
        #[serde(flatten)]
        common: Common,
        data: String,
        error_correction: QrCorrection,
    },
    Group {
        #[serde(flatten)]
        common: Common,
        children: Vec<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Common {
    pub id: String,
    pub transform: Transform,
    pub z_order: i32,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub constraints: Option<Constraints>,
}
fn yes() -> bool {
    true
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HorizontalAlign {
    Left,
    Center,
    Right,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerticalAlign {
    Top,
    Middle,
    Bottom,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextOverflow {
    NoWrap,
    WordWrap,
    Clip,
    ShrinkToFit,
    AutoHeight,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BarcodeSymbology {
    Code128,
    Ean13,
    UpcA,
    Code39,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum QrCorrection {
    L,
    M,
    Q,
    H,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub key: String,
    pub label: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Resource {
    pub id: String,
    pub media_type: String,
    pub sha256: String,
    pub data_base64: String,
}
impl Resource {
    pub fn decoded_bytes(&self) -> Option<Vec<u8>> {
        decode_base64(&self.data_base64)
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum ValidationError {
    #[error("document version must be 4")]
    Version,
    #[error("media dimensions, DPI and printable bounds must be positive and contained")]
    Media,
    #[error("duplicate or empty identifier: {0}")]
    DuplicateId(String),
    #[error("missing resource: {0}")]
    MissingResource(String),
    #[error("resource hash mismatch: {0}")]
    ResourceHash(String),
    #[error("invalid resource encoding: {0}")]
    ResourceEncoding(String),
    #[error("element geometry must be positive: {0}")]
    Geometry(String),
    #[error("extension key must be namespaced: {0}")]
    ExtensionNamespace(String),
    #[error("invalid zone: {0}")]
    Zone(String),
    #[error("invalid group/reference: {0}")]
    Reference(String),
    #[error("group or zone cycle: {0}")]
    Cycle(String),
    #[error("resource has incompatible media type: {0}")]
    ResourceMedia(String),
    #[error("invalid element property: {0}")]
    Element(String),
}

impl Document {
    pub fn from_json(input: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(input)
    }
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut e = Vec::new();
        if self.version != 4 {
            e.push(ValidationError::Version)
        };
        let b = self.media.printable_bounds;
        if self.media.width <= 0
            || self.media.height <= 0
            || self.media.dpi == 0
            || b.width <= 0
            || b.height <= 0
            || b.x < 0
            || b.y < 0
            || b.x + b.width > self.media.width
            || b.y + b.height > self.media.height
        {
            e.push(ValidationError::Media)
        }
        let mut ids = BTreeSet::new();
        let resources: BTreeSet<_> = self.resources.iter().map(|r| r.id.as_str()).collect();
        let resource_map: BTreeMap<_, _> =
            self.resources.iter().map(|r| (r.id.as_str(), r)).collect();
        for r in &self.resources {
            if r.id.is_empty() || !ids.insert(r.id.clone()) {
                e.push(ValidationError::DuplicateId(r.id.clone()))
            };
            match decode_base64(&r.data_base64) {
                Some(bytes) => {
                    let actual = format!("{:x}", Sha256::digest(bytes));
                    if actual != r.sha256.to_ascii_lowercase() {
                        e.push(ValidationError::ResourceHash(r.id.clone()))
                    }
                }
                None => e.push(ValidationError::ResourceEncoding(r.id.clone())),
            }
        }
        let mut zone_ids = BTreeSet::new();
        for z in &self.media.zones {
            if z.id.is_empty() || !zone_ids.insert(z.id.as_str()) {
                e.push(ValidationError::Zone(z.id.clone()))
            }
            let b = z.bounds;
            if b.width <= 0
                || b.height <= 0
                || b.x < 0
                || b.y < 0
                || b.x + b.width > self.media.width
                || b.y + b.height > self.media.height
            {
                e.push(ValidationError::Zone(z.id.clone()))
            }
        }
        for z in &self.media.zones {
            if let Some(parent) = &z.clone_of
                && !zone_ids.contains(parent.as_str())
            {
                e.push(ValidationError::Reference(format!(
                    "zone {} clones missing {parent}",
                    z.id
                )))
            }
        }
        for z in &self.media.zones {
            let mut seen = BTreeSet::new();
            let mut next = Some(z.id.as_str());
            while let Some(id) = next {
                if !seen.insert(id) {
                    e.push(ValidationError::Cycle(format!("zone {id}")));
                    break;
                }
                next = self
                    .media
                    .zones
                    .iter()
                    .find(|x| x.id == id)
                    .and_then(|x| x.clone_of.as_deref())
            }
        }
        let element_ids: BTreeSet<_> = self
            .elements
            .iter()
            .map(|x| x.common().id.as_str())
            .collect();
        let group_ids: BTreeSet<_> = self
            .elements
            .iter()
            .filter_map(|x| {
                if matches!(x, Element::Group { .. }) {
                    Some(x.common().id.as_str())
                } else {
                    None
                }
            })
            .collect();
        for x in &self.elements {
            let c = x.common();
            if c.id.is_empty() || !ids.insert(c.id.clone()) {
                e.push(ValidationError::DuplicateId(c.id.clone()))
            };
            if c.transform.width <= 0 || c.transform.height <= 0 {
                e.push(ValidationError::Geometry(c.id.clone()))
            };
            for id in x.resource_ids() {
                if !resources.contains(id) {
                    e.push(ValidationError::MissingResource(id.into()))
                }
            }
            if let Some(group) = &c.group_id
                && !group_ids.contains(group.as_str())
            {
                e.push(ValidationError::Reference(format!(
                    "{} has missing/non-group parent {group}",
                    c.id
                )))
            }
            if let Some(zone) = c.constraints.as_ref().and_then(|x| x.zone.as_ref())
                && !zone_ids.contains(zone.as_str())
            {
                e.push(ValidationError::Reference(format!(
                    "{} uses missing zone {zone}",
                    c.id
                )))
            }
            match x {
                Element::Text {
                    font_resource: Some(id),
                    font_size,
                    ..
                } => {
                    if *font_size <= 0 {
                        e.push(ValidationError::Element(c.id.clone()))
                    }
                    if let Some(r) = resource_map.get(id.as_str())
                        && !r.media_type.starts_with("font/")
                    {
                        e.push(ValidationError::ResourceMedia(id.clone()))
                    }
                }
                Element::Image { resource, crop, .. } => {
                    if let Some(r) = resource_map.get(resource.as_str())
                        && !matches!(r.media_type.as_str(), "image/png" | "image/jpeg")
                    {
                        e.push(ValidationError::ResourceMedia(resource.clone()))
                    }
                    if crop.is_some_and(|b| b.width <= 0 || b.height <= 0) {
                        e.push(ValidationError::Element(c.id.clone()))
                    }
                }
                Element::Svg { resource, .. } => {
                    if let Some(r) = resource_map.get(resource.as_str())
                        && r.media_type != "image/svg+xml"
                    {
                        e.push(ValidationError::ResourceMedia(resource.clone()))
                    }
                }
                Element::Line { stroke_width, .. }
                | Element::Rectangle { stroke_width, .. }
                | Element::Ellipse { stroke_width, .. }
                | Element::Triangle { stroke_width, .. } => {
                    if *stroke_width <= 0 {
                        e.push(ValidationError::Element(c.id.clone()))
                    }
                }
                Element::Group { children, .. } => {
                    let mut unique = BTreeSet::new();
                    for child in children {
                        if child == &c.id
                            || !element_ids.contains(child.as_str())
                            || !unique.insert(child)
                        {
                            e.push(ValidationError::Reference(format!(
                                "group {} child {child}",
                                c.id
                            )))
                        } else if self
                            .elements
                            .iter()
                            .find(|e| e.common().id == *child)
                            .and_then(|e| e.common().group_id.as_deref())
                            != Some(c.id.as_str())
                        {
                            e.push(ValidationError::Reference(format!(
                                "group {} and child {child} disagree",
                                c.id
                            )))
                        }
                    }
                }
                _ => {}
            }
        }
        for x in &self.elements {
            let mut seen = BTreeSet::new();
            let mut next = Some(x.common().id.as_str());
            while let Some(id) = next {
                if !seen.insert(id) {
                    e.push(ValidationError::Cycle(format!("group {id}")));
                    break;
                }
                next = self
                    .elements
                    .iter()
                    .find(|e| e.common().id == id)
                    .and_then(|e| e.common().group_id.as_deref())
            }
        }
        for k in self.extensions.keys() {
            if !k.contains(':') {
                e.push(ValidationError::ExtensionNamespace(k.clone()))
            }
        }
        if e.is_empty() { Ok(()) } else { Err(e) }
    }
}
impl Element {
    fn common(&self) -> &Common {
        match self {
            Self::Text { common, .. }
            | Self::Image { common, .. }
            | Self::Svg { common, .. }
            | Self::Line { common, .. }
            | Self::Rectangle { common, .. }
            | Self::Ellipse { common, .. }
            | Self::Triangle { common, .. }
            | Self::Barcode { common, .. }
            | Self::QrCode { common, .. }
            | Self::Group { common, .. } => common,
        }
    }
    fn resource_ids(&self) -> Vec<&str> {
        match self {
            Self::Text { font_resource, .. } => font_resource.iter().map(String::as_str).collect(),
            Self::Image { resource, .. } | Self::Svg { resource, .. } => vec![resource],
            _ => vec![],
        }
    }
}

fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let s = s
        .strip_prefix("data:")
        .and_then(|x| x.split_once(",").map(|x| x.1))
        .unwrap_or(s);
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'\r' | b'\n' => continue,
            _ => return None,
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8)
        }
    }
    Some(out)
}
