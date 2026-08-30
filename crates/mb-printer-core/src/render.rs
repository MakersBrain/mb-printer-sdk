// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::document::{
    BarcodeSymbology, Common, Document, Element, HorizontalAlign, TextOverflow, VerticalAlign,
};
use crate::raster::{Dither, GrayRaster, MonoRaster};
use crate::{
    capabilities::{Alignment, PrinterDefinition, Protocol},
    limits::ProcessingLimits,
    protocol,
};
use font8x8::{BASIC_FONTS, UnicodeFonts};
use qrcode::{EcLevel, QrCode};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("document validation failed: {0}")]
    Validation(String),
    #[error("canvas is too large")]
    TooLarge,
    #[error("unsupported resource element: {0}")]
    Resource(String),
    #[error("invalid barcode data: {0}")]
    Barcode(String),
    #[error("QR data is invalid or too large")]
    Qr,
    #[error("embedded font is invalid: {0}")]
    Font(String),
}
#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    pub dither: Dither,
    pub max_pixels: u64,
}
impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            dither: Dither::Threshold(128),
            max_pixels: 100_000_000,
        }
    }
}

/// Resolve the canonical document dither extension for every host target.
pub fn options_for_document(document: &Document) -> RenderOptions {
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
        Some("auto") => Dither::Auto,
        Some("threshold") => Dither::Threshold(threshold),
        Some("bayer") => Dither::Bayer4,
        Some("floyd-steinberg") => Dither::FloydSteinberg,
        Some("atkinson") => Dither::Atkinson,
        Some(_) | None => RenderOptions::default().dither,
    };
    RenderOptions {
        dither,
        ..Default::default()
    }
}
pub fn micrometres_to_dots(value: i64, dpi: u16) -> i64 {
    round_ratio(value as i128 * dpi as i128, 25_400)
}
fn round_ratio(n: i128, d: i128) -> i64 {
    let sign = if n < 0 { -1 } else { 1 };
    let n = n.abs();
    (sign * ((n + d / 2) / d)) as i64
}
pub fn render(doc: &Document, options: RenderOptions) -> Result<MonoRaster, RenderError> {
    render_with_limits(doc, options, &ProcessingLimits::default())
}

/// Render with an explicit decoded resource pixel limit.
pub fn render_with_resource_limit(
    doc: &Document,
    options: RenderOptions,
    max_resource_pixels: u64,
) -> Result<MonoRaster, RenderError> {
    let limits = ProcessingLimits {
        max_resource_pixels,
        ..ProcessingLimits::default()
    };
    render_with_limits(doc, options, &limits)
}

pub fn render_with_limits(
    doc: &Document,
    mut options: RenderOptions,
    limits: &ProcessingLimits,
) -> Result<MonoRaster, RenderError> {
    doc.validate_with_limits(limits).map_err(|e| {
        RenderError::Validation(
            e.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    let w = micrometres_to_dots(doc.media.width, doc.media.dpi);
    let h = micrometres_to_dots(doc.media.height, doc.media.dpi);
    let (Ok(width), Ok(height)) = (u32::try_from(w), u32::try_from(h)) else {
        return Err(RenderError::TooLarge);
    };
    options.max_pixels = options.max_pixels.min(limits.max_canvas_pixels);
    let canvas_pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(RenderError::TooLarge)?;
    let mut budget = RenderBudget::new(limits.max_total_pixels);
    budget.add(canvas_pixels)?;
    budget.add(canvas_pixels)?;
    let mut canvas = GrayRaster::try_new(width, height, 255, options.max_pixels)
        .map_err(|_| RenderError::TooLarge)?;
    let mut elements: Vec<_> = doc.elements.iter().collect();
    elements.sort_by_key(|e| common(e).z_order);
    for e in elements {
        if effectively_visible(e, doc) {
            let source = effective_zone(e, doc);
            draw(
                &mut canvas,
                e,
                doc.media.dpi,
                doc,
                source,
                limits,
                &mut budget,
            )?;
            if let Some(source) = source {
                for zone in &doc.media.zones {
                    if zone.id != source && zone_clones(&doc.media.zones, &zone.id, source) {
                        draw(
                            &mut canvas,
                            e,
                            doc.media.dpi,
                            doc,
                            Some(&zone.id),
                            limits,
                            &mut budget,
                        )?
                    }
                }
            }
        }
    }
    canvas
        .dither(options.dither)
        .map_err(|_| RenderError::TooLarge)
}
fn effectively_visible(element: &Element, doc: &Document) -> bool {
    if !common(element).visible {
        return false;
    }
    let mut group = common(element).group_id.as_deref();
    for _ in 0..doc.elements.len() {
        let Some(parent) =
            group.and_then(|id| doc.elements.iter().find(|item| common(item).id == id))
        else {
            return group.is_none();
        };
        if !common(parent).visible {
            return false;
        }
        group = common(parent).group_id.as_deref();
    }
    group.is_none()
}
fn image_is_inverted(doc: &Document, element_id: &str) -> bool {
    doc.extensions
        .get("makersbrain.render:images")
        .and_then(|value| value.get(element_id))
        .and_then(|value| value.get("invert"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}
fn effective_zone<'a>(element: &'a Element, doc: &'a Document) -> Option<&'a str> {
    let mut current = Some(element);
    for _ in 0..=doc.elements.len() {
        let item = current?;
        if let Some(zone) = common(item)
            .constraints
            .as_ref()
            .and_then(|constraints| constraints.zone.as_deref())
        {
            return Some(zone);
        }
        current = common(item).group_id.as_deref().and_then(|id| {
            doc.elements
                .iter()
                .find(|candidate| common(candidate).id == id)
        });
    }
    None
}
/// Render, apply model rotation/head alignment, and pack for protocol planning.
pub fn render_for_printer(
    doc: &Document,
    printer: &PrinterDefinition,
    options: RenderOptions,
) -> Result<protocol::Raster, RenderError> {
    render_for_printer_with_limits(doc, printer, options, &ProcessingLimits::default())
}

pub fn render_for_printer_with_limits(
    doc: &Document,
    printer: &PrinterDefinition,
    options: RenderOptions,
    limits: &ProcessingLimits,
) -> Result<protocol::Raster, RenderError> {
    let mut raster = render_with_limits(doc, options, limits)?;
    let brother_62x29 = printer.protocol == Protocol::Brother
        && ((doc.media.width.abs_diff(62_000) <= 1_500
            && doc.media.height.abs_diff(29_000) <= 1_500)
            || (doc.media.width.abs_diff(29_000) <= 1_500
                && doc.media.height.abs_diff(62_000) <= 1_500));
    if brother_62x29 {
        // DK-11209 has a 696 x 271 printable rectangle on the 1296-dot
        // QL-1100-series head, offset 56 dots from the right edge.
        raster = fit_mono_to_box(&raster, 696, 271)?;
    }
    if printer.rotated {
        raster = raster.rotate(crate::raster::Rotation::Clockwise90);
    }
    let head = printer
        .width_px()
        .ok_or_else(|| RenderError::Resource("printer has media-dependent head width".into()))?;
    let fit = match printer.alignment {
        Alignment::Left => crate::raster::Fit::Left,
        Alignment::Center => crate::raster::Fit::Center,
        Alignment::Right => crate::raster::Fit::Right,
    };
    let fitted_pixels = u64::from(head)
        .checked_mul(u64::from(raster.height))
        .ok_or(RenderError::TooLarge)?;
    if fitted_pixels > limits.max_canvas_pixels || fitted_pixels > limits.max_total_pixels {
        return Err(RenderError::TooLarge);
    }
    let fitted = if brother_62x29 {
        raster.place_on_head(head, crate::raster::Fit::Right, -56, 0)
    } else {
        raster.place_on_head_byte_aligned(head, fit, 0, 0)
    }
    .map_err(|_| RenderError::TooLarge)?;
    let data = fitted.pack_msb().map_err(|_| RenderError::TooLarge)?;
    Ok(protocol::Raster {
        width_bytes: head.div_ceil(8) as u16,
        height: fitted.height,
        data,
    })
}

fn fit_mono_to_box(image: &MonoRaster, width: u32, height: u32) -> Result<MonoRaster, RenderError> {
    if width == 0 || height == 0 || image.width == 0 || image.height == 0 {
        return Err(RenderError::TooLarge);
    }
    let scale_by_width =
        u64::from(width) * u64::from(image.height) <= u64::from(height) * u64::from(image.width);
    let (scaled_width, scaled_height) = if scale_by_width {
        (
            width,
            ((u64::from(image.height) * u64::from(width) + u64::from(image.width) / 2)
                / u64::from(image.width)) as u32,
        )
    } else {
        (
            ((u64::from(image.width) * u64::from(height) + u64::from(image.height) / 2)
                / u64::from(image.height)) as u32,
            height,
        )
    };
    let mut scaled = MonoRaster::try_new(
        scaled_width.max(1),
        scaled_height.max(1),
        u64::from(width) * u64::from(height),
    )
    .map_err(|_| RenderError::TooLarge)?;
    for y in 0..scaled.height {
        for x in 0..scaled.width {
            let source_x = u64::from(x) * u64::from(image.width) / u64::from(scaled.width);
            let source_y = u64::from(y) * u64::from(image.height) / u64::from(scaled.height);
            scaled.pixels[(y * scaled.width + x) as usize] =
                image.pixels[(source_y as u32 * image.width + source_x as u32) as usize];
        }
    }
    let mut output = MonoRaster::try_new(width, height, u64::from(width) * u64::from(height))
        .map_err(|_| RenderError::TooLarge)?;
    let left = (width - scaled.width) / 2;
    let top = (height - scaled.height) / 2;
    for y in 0..scaled.height {
        let source = (y * scaled.width) as usize;
        let destination = ((top + y) * width + left) as usize;
        output.pixels[destination..destination + scaled.width as usize]
            .copy_from_slice(&scaled.pixels[source..source + scaled.width as usize]);
    }
    Ok(output)
}
fn common(e: &Element) -> &Common {
    match e {
        Element::Text { common, .. }
        | Element::Image { common, .. }
        | Element::Svg { common, .. }
        | Element::Line { common, .. }
        | Element::Rectangle { common, .. }
        | Element::Ellipse { common, .. }
        | Element::Triangle { common, .. }
        | Element::Barcode { common, .. }
        | Element::QrCode { common, .. }
        | Element::Group { common, .. } => common,
    }
}
#[derive(Debug, Clone, Copy)]
struct Placement {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    rotation_millidegrees: i32,
}
struct RenderBudget {
    pixels: u64,
    limit: u64,
}
impl RenderBudget {
    fn new(limit: u64) -> Self {
        Self { pixels: 0, limit }
    }
    fn add(&mut self, pixels: u64) -> Result<(), RenderError> {
        self.pixels = self
            .pixels
            .checked_add(pixels)
            .ok_or(RenderError::TooLarge)?;
        if self.pixels > self.limit {
            Err(RenderError::TooLarge)
        } else {
            Ok(())
        }
    }
}
const TRIG_SCALE: i64 = 1_000_000_000;
fn fixed_trig(millidegrees: i32) -> (i64, i64) {
    const ATAN_MDEG: [i32; 17] = [
        45_000, 26_565, 14_036, 7_125, 3_576, 1_790, 895, 448, 224, 112, 56, 28, 14, 7, 3, 2, 1,
    ];
    match millidegrees.rem_euclid(360_000) {
        0 => return (TRIG_SCALE, 0),
        90_000 => return (0, TRIG_SCALE),
        180_000 => return (-TRIG_SCALE, 0),
        270_000 => return (0, -TRIG_SCALE),
        _ => {}
    }
    let mut angle = millidegrees.rem_euclid(360_000);
    if angle > 180_000 {
        angle -= 360_000;
    }
    let sign = if angle > 90_000 {
        angle -= 180_000;
        -1
    } else if angle < -90_000 {
        angle += 180_000;
        -1
    } else {
        1
    };
    // CORDIC gain compensation, rounded to the shared 1e9 fixed-point scale.
    let mut x = 607_252_935i64;
    let mut y = 0i64;
    let mut remaining = angle;
    for (shift, step) in ATAN_MDEG.into_iter().enumerate() {
        let divisor = 1i64 << shift;
        let (next_x, next_y) = if remaining >= 0 {
            (x - y / divisor, y + x / divisor)
        } else {
            (x + y / divisor, y - x / divisor)
        };
        x = next_x;
        y = next_y;
        remaining += if remaining >= 0 { -step } else { step };
    }
    (x * sign, y * sign)
}
fn rotate_fixed(x: i64, y: i64, angle: i32) -> (i64, i64) {
    let (cos, sin) = fixed_trig(angle);
    (
        round_ratio(
            i128::from(cos) * i128::from(x) - i128::from(sin) * i128::from(y),
            i128::from(TRIG_SCALE),
        ),
        round_ratio(
            i128::from(sin) * i128::from(x) + i128::from(cos) * i128::from(y),
            i128::from(TRIG_SCALE),
        ),
    )
}
fn placement_px(c: &Common, dpi: u16, doc: &Document, zone_override: Option<&str>) -> Placement {
    let t = &c.transform;
    // Centres use doubled micrometres so half-unit geometry stays exact.
    let mut cx2 = 2 * t.x + t.width;
    let mut cy2 = 2 * t.y + t.height;
    let mut rotation = t.rotation_millidegrees;
    let mut group = c.group_id.as_deref();
    let mut depth = 0;
    while let Some(id) = group {
        if depth >= 32 {
            break;
        }
        let Some(g) = doc.elements.iter().find(|e| common(e).id == id) else {
            break;
        };
        let gt = &common(g).transform;
        let (dx, dy) = rotate_fixed(cx2 - gt.width, cy2 - gt.height, gt.rotation_millidegrees);
        cx2 = 2 * gt.x + gt.width + dx;
        cy2 = 2 * gt.y + gt.height + dy;
        rotation = rotation.wrapping_add(gt.rotation_millidegrees);
        group = common(g).group_id.as_deref();
        depth += 1
    }
    if let Some(zone) = zone_override
        .or_else(|| c.constraints.as_ref().and_then(|x| x.zone.as_deref()))
        .and_then(|id| doc.media.zones.iter().find(|z| z.id == id))
    {
        cx2 += 2 * zone.bounds.x;
        cy2 += 2 * zone.bounds.y;
    }
    let w = micrometres_to_dots(t.width, dpi).max(1) as i32;
    let h = micrometres_to_dots(t.height, dpi).max(1) as i32;
    let center_x = round_ratio(i128::from(cx2) * i128::from(dpi), 50_800) as i32;
    let center_y = round_ratio(i128::from(cy2) * i128::from(dpi), 50_800) as i32;
    Placement {
        x: center_x - w / 2,
        y: center_y - h / 2,
        w,
        h,
        rotation_millidegrees: rotation.rem_euclid(360_000),
    }
}
fn draw(
    c: &mut GrayRaster,
    e: &Element,
    dpi: u16,
    doc: &Document,
    zone_override: Option<&str>,
    limits: &ProcessingLimits,
    budget: &mut RenderBudget,
) -> Result<(), RenderError> {
    if matches!(e, Element::Group { .. }) {
        return Ok(());
    }
    let placement = placement_px(common(e), dpi, doc, zone_override);
    if placement.rotation_millidegrees == 0 {
        return draw_at(c, e, dpi, doc, placement, limits, budget);
    }
    let pixels = u64::from(c.width)
        .checked_mul(u64::from(c.height))
        .ok_or(RenderError::TooLarge)?;
    budget.add(pixels)?;
    let mut layer = GrayRaster::try_new(c.width, c.height, 255, limits.max_canvas_pixels)
        .map_err(|_| RenderError::TooLarge)?;
    draw_at(&mut layer, e, dpi, doc, placement, limits, budget)?;
    rotate_layer(c, &layer, placement);
    Ok(())
}
fn draw_at(
    c: &mut GrayRaster,
    e: &Element,
    dpi: u16,
    doc: &Document,
    placement: Placement,
    limits: &ProcessingLimits,
    budget: &mut RenderBudget,
) -> Result<(), RenderError> {
    let Placement { x, y, w, h, .. } = placement;
    match e {
        Element::Line { stroke_width, .. } => line(
            c,
            x,
            y,
            x + w - 1,
            y + h - 1,
            micrometres_to_dots(*stroke_width, dpi).max(1) as i32,
        ),
        Element::Rectangle {
            stroke_width, fill, ..
        } => rect(
            c,
            x,
            y,
            w,
            h,
            *fill,
            micrometres_to_dots(*stroke_width, dpi).max(1) as i32,
        ),
        Element::Ellipse {
            stroke_width, fill, ..
        } => ellipse(
            c,
            x,
            y,
            w,
            h,
            *fill,
            micrometres_to_dots(*stroke_width, dpi).max(1) as i32,
        ),
        Element::Triangle {
            stroke_width, fill, ..
        } => triangle(
            c,
            x,
            y,
            w,
            h,
            *fill,
            micrometres_to_dots(*stroke_width, dpi).max(1) as i32,
        ),
        Element::Text {
            text: value,
            font_resource,
            font_size,
            horizontal_align,
            vertical_align,
            overflow,
            ..
        } => {
            let layout = TextLayout {
                x,
                y,
                w,
                h,
                size: micrometres_to_dots(*font_size, dpi).max(8) as i32,
                horizontal: *horizontal_align,
                vertical: *vertical_align,
                overflow: *overflow,
            };
            if let Some(id) = font_resource {
                let resource = doc
                    .resources
                    .iter()
                    .find(|r| r.id == *id)
                    .ok_or_else(|| RenderError::Resource(id.clone()))?;
                let bytes = resource
                    .decoded_bytes_with_limits(limits)
                    .map_err(|_| RenderError::Font(id.clone()))?;
                text_embedded(c, layout, value, &bytes)?
            } else {
                text(c, layout, value)
            }
        }
        Element::Barcode {
            data,
            symbology,
            human_readable,
            ..
        } => barcode(
            c,
            BarcodeLayout {
                x,
                y,
                w,
                h,
                symbology: *symbology,
                human_readable: *human_readable,
            },
            data,
        )?,
        Element::QrCode {
            data,
            error_correction,
            ..
        } => qr(c, x, y, w, h, data, *error_correction)?,
        Element::Image { resource, crop, .. } => {
            let item = doc
                .resources
                .iter()
                .find(|r| r.id == *resource)
                .ok_or_else(|| RenderError::Resource(resource.clone()))?;
            let image = crate::resources::normalize_with_limits(item, limits)
                .map_err(|e| RenderError::Resource(e.to_string()))?;
            budget.add(u64::from(image.width) * u64::from(image.height))?;
            let mut cropped = if let Some(bounds) = crop {
                let cropped = crop_source(&image, *bounds, limits.max_resource_pixels)?;
                budget.add(u64::from(cropped.width) * u64::from(cropped.height))?;
                cropped
            } else {
                image
            };
            if image_is_inverted(doc, &common(e).id) {
                for pixel in &mut cropped.pixels {
                    *pixel = 255 - *pixel;
                }
            }
            paste_fit(
                c,
                &cropped,
                x,
                y,
                w,
                h,
                common(e)
                    .constraints
                    .as_ref()
                    .is_some_and(|c| c.preserve_aspect),
            );
        }
        Element::Svg { resource, .. } => {
            let item = doc
                .resources
                .iter()
                .find(|r| r.id == *resource)
                .ok_or_else(|| RenderError::Resource(resource.clone()))?;
            let image = crate::resources::normalize_with_limits(item, limits)
                .map_err(|e| RenderError::Resource(e.to_string()))?;
            budget.add(u64::from(image.width) * u64::from(image.height))?;
            paste_fit(
                c,
                &image,
                x,
                y,
                w,
                h,
                common(e)
                    .constraints
                    .as_ref()
                    .is_some_and(|c| c.preserve_aspect),
            );
        }
        Element::Group { .. } => {}
    }
    Ok(())
}
fn rotate_layer(canvas: &mut GrayRaster, layer: &GrayRaster, placement: Placement) {
    let angle = placement.rotation_millidegrees;
    let (cos, sin) = fixed_trig(angle);
    let cx2 = i64::from(2 * placement.x + placement.w - 1);
    let cy2 = i64::from(2 * placement.y + placement.h - 1);
    let diagonal_squared =
        u64::try_from(i64::from(placement.w).pow(2) + i64::from(placement.h).pow(2))
            .unwrap_or(u64::MAX);
    let radius = integer_sqrt_ceil(diagonal_squared).div_ceil(2) as i32 + 2;
    let min_x = (placement.x + placement.w / 2 - radius).max(0);
    let max_x = (placement.x + placement.w / 2 + radius).min(canvas.width as i32 - 1);
    let min_y = (placement.y + placement.h / 2 - radius).max(0);
    let max_y = (placement.y + placement.h / 2 + radius).min(canvas.height as i32 - 1);
    for dy in min_y..=max_y {
        for dx in min_x..=max_x {
            let vx2 = i64::from(2 * dx) - cx2;
            let vy2 = i64::from(2 * dy) - cy2;
            let sx2 = cx2
                + round_ratio(
                    i128::from(cos) * i128::from(vx2) + i128::from(sin) * i128::from(vy2),
                    i128::from(TRIG_SCALE),
                );
            let sy2 = cy2
                + round_ratio(
                    -i128::from(sin) * i128::from(vx2) + i128::from(cos) * i128::from(vy2),
                    i128::from(TRIG_SCALE),
                );
            let sx = round_ratio(i128::from(sx2), 2) as i32;
            let sy = round_ratio(i128::from(sy2), 2) as i32;
            if sx >= 0 && sy >= 0 && sx < layer.width as i32 && sy < layer.height as i32 {
                let source = layer.pixels[sy as usize * layer.width as usize + sx as usize];
                if source < 255 {
                    let target = dy as usize * canvas.width as usize + dx as usize;
                    canvas.pixels[target] = canvas.pixels[target].min(source);
                }
            }
        }
    }
}
fn integer_sqrt_ceil(value: u64) -> u64 {
    if value <= 1 {
        return value;
    }
    let mut low = 1u64;
    let mut high = value.min(u64::from(u32::MAX) + 1);
    while low < high {
        let middle = low + (high - low) / 2;
        if middle.saturating_mul(middle) >= value {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}
fn zone_clones(zones: &[crate::document::Zone], candidate: &str, source: &str) -> bool {
    let mut next = zones
        .iter()
        .find(|z| z.id == candidate)
        .and_then(|z| z.clone_of.as_deref());
    for _ in 0..zones.len() {
        match next {
            Some(id) if id == source => return true,
            Some(id) => {
                next = zones
                    .iter()
                    .find(|z| z.id == id)
                    .and_then(|z| z.clone_of.as_deref())
            }
            None => return false,
        }
    }
    false
}
fn crop_source(
    source: &GrayRaster,
    b: crate::document::Bounds,
    max_pixels: u64,
) -> Result<GrayRaster, RenderError> {
    let x = b.x.max(0).min(source.width as i64) as u32;
    let y = b.y.max(0).min(source.height as i64) as u32;
    let w = b.width.max(1).min(source.width.saturating_sub(x) as i64) as u32;
    let h = b.height.max(1).min(source.height.saturating_sub(y) as i64) as u32;
    let mut out = GrayRaster::try_new(w, h, 255, max_pixels).map_err(|_| RenderError::TooLarge)?;
    for yy in 0..h {
        let from = ((y + yy) * source.width + x) as usize;
        let to = (yy * w) as usize;
        out.pixels[to..to + w as usize].copy_from_slice(&source.pixels[from..from + w as usize])
    }
    Ok(out)
}
fn paste_fit(
    c: &mut GrayRaster,
    source: &GrayRaster,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    preserve: bool,
) {
    if !preserve {
        paste_scaled(c, source, x, y, w, h);
        return;
    }
    let scale_num = (w as i64 * source.height as i64).min(h as i64 * source.width as i64);
    let out_w = (source.width as i64 * scale_num / (source.width as i64 * source.height as i64))
        .max(1) as i32;
    let out_h = (source.height as i64 * scale_num / (source.width as i64 * source.height as i64))
        .max(1) as i32;
    paste_scaled(
        c,
        source,
        x + (w - out_w) / 2,
        y + (h - out_h) / 2,
        out_w,
        out_h,
    )
}
fn paste_scaled(c: &mut GrayRaster, source: &GrayRaster, x: i32, y: i32, w: i32, h: i32) {
    for yy in 0..h {
        for xx in 0..w {
            let sx = (xx as u64 * source.width as u64 / w as u64)
                .min(source.width.saturating_sub(1) as u64) as u32;
            let sy = (yy as u64 * source.height as u64 / h as u64)
                .min(source.height.saturating_sub(1) as u64) as u32;
            set(
                c,
                x + xx,
                y + yy,
                source.pixels[(sy * source.width + sx) as usize],
            )
        }
    }
}
fn set(c: &mut GrayRaster, x: i32, y: i32, v: u8) {
    if x >= 0 && y >= 0 && (x as u32) < c.width && (y as u32) < c.height {
        c.pixels[(y as u32 * c.width + x as u32) as usize] = v
    }
}
fn line(c: &mut GrayRaster, mut x0: i32, mut y0: i32, x1: i32, y1: i32, t: i32) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        for yy in -(t / 2)..=(t / 2) {
            for xx in -(t / 2)..=(t / 2) {
                set(c, x0 + xx, y0 + yy, 0)
            }
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e = 2 * err;
        if e >= dy {
            err += dy;
            x0 += sx
        }
        if e <= dx {
            err += dx;
            y0 += sy
        }
    }
}
fn rect(c: &mut GrayRaster, x: i32, y: i32, w: i32, h: i32, fill: bool, t: i32) {
    if fill {
        for yy in y..y + h {
            for xx in x..x + w {
                set(c, xx, yy, 0)
            }
        }
    } else {
        for i in 0..t {
            line(c, x, y + i, x + w - 1, y + i, 1);
            line(c, x, y + h - 1 - i, x + w - 1, y + h - 1 - i, 1);
            line(c, x + i, y, x + i, y + h - 1, 1);
            line(c, x + w - 1 - i, y, x + w - 1 - i, y + h - 1, 1)
        }
    }
}
fn ellipse(c: &mut GrayRaster, x: i32, y: i32, w: i32, h: i32, fill: bool, t: i32) {
    let rx = w as i64;
    let ry = h as i64;
    let cx = 2 * x as i64 + w as i64 - 1;
    let cy = 2 * y as i64 + h as i64 - 1;
    let edge = (t.max(1) * 4) as i64;
    for yy in y..y + h {
        for xx in x..x + w {
            let dx = 2 * xx as i64 - cx;
            let dy = 2 * yy as i64 - cy;
            let lhs = dx * dx * ry * ry + dy * dy * rx * rx;
            let rhs = rx * rx * ry * ry;
            if lhs <= rhs && (fill || rhs - lhs <= edge * (rx * ry).max(1)) {
                set(c, xx, yy, 0)
            }
        }
    }
}
fn triangle(c: &mut GrayRaster, x: i32, y: i32, w: i32, h: i32, fill: bool, t: i32) {
    let a = (x + w / 2, y);
    let b = (x, y + h - 1);
    let d = (x + w - 1, y + h - 1);
    if fill {
        for yy in y..y + h {
            let rel = yy - y;
            let half = (rel * w / 2) / h.max(1);
            for xx in a.0 - half..=a.0 + half {
                set(c, xx, yy, 0)
            }
        }
    } else {
        line(c, a.0, a.1, b.0, b.1, t);
        line(c, b.0, b.1, d.0, d.1, t);
        line(c, d.0, d.1, a.0, a.1, t)
    }
}
#[derive(Clone, Copy)]
struct TextLayout {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    size: i32,
    horizontal: HorizontalAlign,
    vertical: VerticalAlign,
    overflow: TextOverflow,
}
fn text_embedded(
    c: &mut GrayRaster,
    mut layout: TextLayout,
    s: &str,
    bytes: &[u8],
) -> Result<(), RenderError> {
    let face = rustybuzz::Face::from_slice(bytes, 0)
        .ok_or_else(|| RenderError::Font("cannot parse OpenType face".into()))?;
    let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
        .map_err(|e| RenderError::Font(e.to_string()))?;
    if matches!(layout.overflow, TextOverflow::ShrinkToFit) {
        while layout.size > 1
            && (embedded_advance(&face, s, layout.size) > layout.w || layout.size > layout.h)
        {
            layout.size -= 1;
        }
    }
    let mut lines = if matches!(
        layout.overflow,
        TextOverflow::WordWrap | TextOverflow::AutoHeight
    ) {
        embedded_wrap(&face, s, layout.size, layout.w)
    } else {
        vec![s.to_owned()]
    };
    if !matches!(layout.overflow, TextOverflow::AutoHeight) {
        lines.truncate((layout.h / layout.size.max(1)).max(1) as usize);
    }
    let total_height = lines.len() as i32 * layout.size;
    let top = match layout.vertical {
        VerticalAlign::Top => layout.y,
        VerticalAlign::Middle => layout.y + (layout.h - total_height) / 2,
        VerticalAlign::Bottom => layout.y + layout.h - total_height,
    };
    for (line_index, line) in lines.iter().enumerate() {
        draw_embedded_line(
            c,
            layout,
            &face,
            &font,
            line,
            top + line_index as i32 * layout.size + layout.size,
        );
    }
    Ok(())
}
fn embedded_advance(face: &rustybuzz::Face<'_>, text: &str, size: i32) -> i32 {
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    let shaped = rustybuzz::shape(face, &[], buffer);
    let advance: i64 = shaped
        .glyph_positions()
        .iter()
        .map(|position| i64::from(position.x_advance))
        .sum();
    round_ratio(
        i128::from(advance) * i128::from(size),
        i128::from(face.units_per_em()),
    ) as i32
}
fn embedded_wrap(face: &rustybuzz::Face<'_>, text: &str, size: i32, width: i32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if embedded_advance(face, &candidate, size) <= width {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            for character in word.chars() {
                let candidate = format!("{line}{character}");
                if !line.is_empty() && embedded_advance(face, &candidate, size) > width {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(character);
            }
        }
        lines.push(std::mem::take(&mut line));
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}
fn draw_embedded_line(
    canvas: &mut GrayRaster,
    layout: TextLayout,
    face: &rustybuzz::Face<'_>,
    font: &fontdue::Font,
    text: &str,
    baseline: i32,
) {
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    let shaped = rustybuzz::shape(face, &[], buffer);
    let advance = embedded_advance(face, text, layout.size);
    let mut pen_units = 0i64;
    let origin = match layout.horizontal {
        HorizontalAlign::Left => layout.x,
        HorizontalAlign::Center => layout.x + (layout.w - advance) / 2,
        HorizontalAlign::Right => layout.x + layout.w - advance,
    };
    for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        let scale = |units: i64| {
            round_ratio(
                i128::from(units) * i128::from(layout.size),
                i128::from(face.units_per_em()),
            ) as i32
        };
        let (metrics, bitmap) = font.rasterize_indexed(info.glyph_id as u16, layout.size as f32);
        let gx = origin + scale(pen_units + i64::from(position.x_offset)) + metrics.xmin;
        let gy =
            baseline - scale(i64::from(position.y_offset)) - metrics.height as i32 - metrics.ymin;
        for yy in 0..metrics.height {
            for xx in 0..metrics.width {
                let px = gx + xx as i32;
                let py = gy + yy as i32;
                if px >= layout.x
                    && px < layout.x + layout.w
                    && py >= layout.y
                    && (matches!(layout.overflow, TextOverflow::AutoHeight)
                        || py < layout.y + layout.h)
                {
                    let alpha = bitmap[yy * metrics.width + xx];
                    if alpha > 0 {
                        set(canvas, px, py, 255 - alpha)
                    }
                }
            }
        }
        pen_units += i64::from(position.x_advance);
    }
}
fn text(c: &mut GrayRaster, layout: TextLayout, s: &str) {
    let TextLayout {
        x,
        y,
        w,
        h,
        size,
        horizontal: ha,
        vertical: va,
        overflow,
    } = layout;
    let mut scale = (size / 8).max(1);
    if matches!(overflow, TextOverflow::ShrinkToFit) {
        while scale > 1 && ((s.chars().count() as i32 * 8 * scale) > w || 8 * scale > h) {
            scale -= 1
        }
    }
    let max_chars = (w / (8 * scale)).max(1) as usize;
    let mut lines = if matches!(overflow, TextOverflow::WordWrap | TextOverflow::AutoHeight) {
        wrap(s, max_chars)
    } else {
        vec![s.chars().take(max_chars).collect()]
    };
    let max_lines = if matches!(overflow, TextOverflow::AutoHeight) {
        usize::MAX
    } else {
        (h / (8 * scale)).max(1) as usize
    };
    lines.truncate(max_lines);
    let total = lines.len() as i32 * 8 * scale;
    let mut cy = match va {
        VerticalAlign::Top => y,
        VerticalAlign::Middle => y + (h - total) / 2,
        VerticalAlign::Bottom => y + h - total,
    };
    for line_text in lines {
        let line_width = line_text.chars().count() as i32 * 8 * scale;
        let mut cx = match ha {
            HorizontalAlign::Left => x,
            HorizontalAlign::Center => x + (w - line_width) / 2,
            HorizontalAlign::Right => x + w - line_width,
        };
        for ch in line_text.chars() {
            let Some(g) = BASIC_FONTS.get(ch) else {
                cx += 8 * scale;
                continue;
            };
            for (row, bits) in g.iter().enumerate() {
                for col in 0..8 {
                    if bits & (1 << col) != 0 {
                        for yy in 0..scale {
                            for xx in 0..scale {
                                let px = cx + col * scale + xx;
                                let py = cy + row as i32 * scale + yy;
                                if px >= x
                                    && px < x + w
                                    && py >= y
                                    && (matches!(overflow, TextOverflow::AutoHeight) || py < y + h)
                                {
                                    set(c, px, py, 0)
                                }
                            }
                        }
                    }
                }
            }
            cx += 8 * scale
        }
        cy += 8 * scale
    }
}
fn wrap(s: &str, max: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > max {
            lines.push(line);
            line = String::new()
        }
        if !line.is_empty() {
            line.push(' ')
        }
        for ch in word.chars() {
            if line.chars().count() == max {
                lines.push(line);
                line = String::new()
            }
            line.push(ch)
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line)
    }
    lines
}
struct BarcodeLayout {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    symbology: BarcodeSymbology,
    human_readable: bool,
}
fn barcode(c: &mut GrayRaster, layout: BarcodeLayout, data: &str) -> Result<(), RenderError> {
    let BarcodeLayout {
        x,
        y,
        w,
        h,
        symbology: s,
        human_readable,
    } = layout;
    let bits = match s {
        BarcodeSymbology::Code39 => code39(data)?,
        BarcodeSymbology::Ean13 => ean(data, 13)?,
        BarcodeSymbology::UpcA => ean(data, 12)?,
        BarcodeSymbology::Code128 => barcoders::sym::code128::Code128::new(format!("Ɓ{data}"))
            .map_err(|e| RenderError::Barcode(e.to_string()))?
            .encode()
            .into_iter()
            .map(|x| x != 0)
            .collect(),
    };
    let module = (w / bits.len() as i32).max(1);
    let total = module * bits.len() as i32;
    let start = x + (w - total) / 2;
    let bar_height = if human_readable { (h - 10).max(1) } else { h };
    for (i, b) in bits.into_iter().enumerate() {
        if b {
            rect(c, start + i as i32 * module, y, module, bar_height, true, 1)
        }
    }
    if human_readable {
        text(
            c,
            TextLayout {
                x,
                y: y + bar_height,
                w,
                h: h - bar_height,
                size: 8,
                horizontal: HorizontalAlign::Center,
                vertical: VerticalAlign::Bottom,
                overflow: TextOverflow::ShrinkToFit,
            },
            data,
        )
    }
    Ok(())
}
fn code39(data: &str) -> Result<Vec<bool>, RenderError> {
    const CHARS: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-. $/+%*";
    const PAT: [u16; 44] = [
        0x034, 0x121, 0x061, 0x160, 0x031, 0x130, 0x070, 0x025, 0x124, 0x064, 0x109, 0x049, 0x148,
        0x019, 0x118, 0x058, 0x00d, 0x10c, 0x04c, 0x01c, 0x103, 0x043, 0x142, 0x013, 0x112, 0x052,
        0x007, 0x106, 0x046, 0x016, 0x181, 0x0c1, 0x1c0, 0x091, 0x190, 0x0d0, 0x085, 0x184, 0x0c4,
        0x094, 0x0a8, 0x0a2, 0x08a, 0x02a,
    ];
    let full = format!("*{}*", data.to_ascii_uppercase());
    let mut out = Vec::new();
    for (ch_i, ch) in full.chars().enumerate() {
        let idx = CHARS
            .find(ch)
            .ok_or_else(|| RenderError::Barcode(format!("unsupported Code 39 character {ch}")))?;
        let p = PAT[idx];
        for i in 0..9 {
            let wide = p & (1 << (8 - i)) != 0;
            let black = i % 2 == 0;
            out.extend(std::iter::repeat_n(black, if wide { 3 } else { 1 }))
        }
        if ch_i + 1 < full.len() {
            out.push(false)
        }
    }
    Ok(out)
}
fn ean(data: &str, len: usize) -> Result<Vec<bool>, RenderError> {
    if data.len() != len || !data.bytes().all(|b| b.is_ascii_digit()) {
        return Err(RenderError::Barcode(format!("expected {len} digits")));
    }
    let digits: Vec<_> = data.bytes().map(|b| (b - b'0') as usize).collect();
    let check: usize = digits[..len - 1]
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| d * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    if (10 - check % 10) % 10 != digits[len - 1] {
        return Err(RenderError::Barcode("check digit mismatch".into()));
    }
    const L: [&str; 10] = [
        "0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011",
        "0110111", "0001011",
    ];
    const G: [&str; 10] = [
        "0100111", "0110011", "0011011", "0100001", "0011101", "0111001", "0000101", "0010001",
        "0001001", "0010111",
    ];
    const R: [&str; 10] = [
        "1110010", "1100110", "1101100", "1000010", "1011100", "1001110", "1010000", "1000100",
        "1001000", "1110100",
    ];
    const PAR: [&str; 10] = [
        "LLLLLL", "LLGLGG", "LLGGLG", "LLGGGL", "LGLLGG", "LGGLLG", "LGGGLL", "LGLGLG", "LGLGGL",
        "LGGLGL",
    ];
    let (first, left, right) = if len == 13 {
        (digits[0], &digits[1..7], &digits[7..])
    } else {
        (0, &digits[..6], &digits[6..])
    };
    let mut s = "101".to_string();
    for (i, &d) in left.iter().enumerate() {
        s.push_str(if len == 13 && PAR[first].as_bytes()[i] == b'G' {
            G[d]
        } else {
            L[d]
        })
    }
    s.push_str("01010");
    for &d in right {
        s.push_str(R[d])
    }
    s.push_str("101");
    Ok(s.bytes().map(|b| b == b'1').collect())
}
fn qr(
    c: &mut GrayRaster,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    data: &str,
    level: crate::document::QrCorrection,
) -> Result<(), RenderError> {
    let ec = match level {
        crate::document::QrCorrection::L => EcLevel::L,
        crate::document::QrCorrection::M => EcLevel::M,
        crate::document::QrCorrection::Q => EcLevel::Q,
        crate::document::QrCorrection::H => EcLevel::H,
    };
    let q =
        QrCode::with_error_correction_level(data.as_bytes(), ec).map_err(|_| RenderError::Qr)?;
    let n = q.width() as i32;
    let scale = (w.min(h) / (n + 8)).max(1);
    let ox = x + (w - (n + 8) * scale) / 2 + 4 * scale;
    let oy = y + (h - (n + 8) * scale) / 2 + 4 * scale;
    for yy in 0..n {
        for xx in 0..n {
            if q[(xx as usize, yy as usize)] == qrcode::Color::Dark {
                rect(c, ox + xx * scale, oy + yy * scale, scale, scale, true, 1)
            }
        }
    }
    Ok(())
}
