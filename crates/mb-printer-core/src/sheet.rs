// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic imposition of physical label documents onto paper sheets.

use crate::document::{Document, ValidationError};
use crate::export::{self, ExportError, PackedPdfPage};
use crate::limits::ProcessingLimits;
use crate::raster::{MonoRaster, PackedMonoRaster, RasterError};
use crate::render::{self, RenderError};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU16;
use thiserror::Error;

const MAX_LAYOUT_ID_BYTES: usize = 128;
const CUT_MARK_LENGTH_UM: i64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FillOrder {
    RowMajor,
    ColumnMajor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SheetSlot {
    pub x_um: i64,
    pub y_um: i64,
    pub width_um: i64,
    pub height_um: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SheetGrid {
    pub id: String,
    pub paper_width_um: i64,
    pub paper_height_um: i64,
    pub rows: u16,
    pub columns: u16,
    pub label_width_um: i64,
    pub label_height_um: i64,
    pub margin_left_um: i64,
    pub margin_top_um: i64,
    pub gap_x_um: i64,
    pub gap_y_um: i64,
    pub fill_order: FillOrder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SheetLayout {
    pub id: String,
    pub paper_width_um: i64,
    pub paper_height_um: i64,
    pub slots: Vec<SheetSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SheetDefinition {
    Grid {
        #[serde(flatten)]
        grid: SheetGrid,
    },
    Explicit {
        #[serde(flatten)]
        layout: SheetLayout,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SheetOptions {
    pub first_slot: usize,
    pub dpi: NonZeroU16,
}

/// Non-serialized decorations for the legacy CLI raster compatibility path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SheetRasterOptions {
    pub cut_marks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SheetPlanInput {
    pub item_count: u32,
    pub label_width_um: i64,
    pub label_height_um: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SheetPlacement {
    pub item: usize,
    pub page: usize,
    pub slot: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SheetPlan {
    pub page_count: usize,
    pub layout: SheetLayout,
    pub placements: Vec<SheetPlacement>,
}

/// A pre-rendered label for hosts that apply content transforms before imposition.
#[derive(Debug, Clone, Copy)]
pub struct SheetRasterItem<'a> {
    pub raster: &'a MonoRaster,
    pub width_um: i64,
    pub height_um: i64,
}

#[derive(Debug, Error)]
pub enum SheetError {
    #[error("sheet jobs require at least one item")]
    EmptyJob,
    #[error("sheet layout contains no slots")]
    EmptyLayout,
    #[error("sheet layout identifier is empty or too long")]
    InvalidLayoutId,
    #[error("invalid paper dimensions: {width_um} x {height_um} micrometres")]
    InvalidPaper { width_um: i64, height_um: i64 },
    #[error("invalid sheet grid")]
    InvalidGrid,
    #[error("slot {index} is outside the paper")]
    SlotOutsidePaper { index: usize },
    #[error("slots {left} and {right} overlap")]
    OverlappingSlots { left: usize, right: usize },
    #[error("first slot {first_slot} is outside the layout")]
    InvalidFirstSlot { first_slot: usize },
    #[error(
        "label {item} has dimensions {actual_width_um} x {actual_height_um}; slot expects {expected_width_um} x {expected_height_um} micrometres"
    )]
    LabelSizeMismatch {
        item: usize,
        actual_width_um: i64,
        actual_height_um: i64,
        expected_width_um: i64,
        expected_height_um: i64,
    },
    #[error("sheet raster exceeds the configured allocation limit")]
    RasterTooLarge,
    #[error("sheet PDF exceeds the configured {limit}-byte output limit")]
    OutputTooLarge { limit: usize },
    #[error("sheet job exceeds configured limits")]
    LimitExceeded,
    #[error("sheet geometry overflowed")]
    GeometryOverflow,
    #[error("label {item} is invalid")]
    InvalidDocument {
        item: usize,
        errors: Vec<ValidationError>,
    },
    #[error("label {item} could not be rendered")]
    Render {
        item: usize,
        #[source]
        source: RenderError,
    },
    #[error(transparent)]
    Raster(#[from] RasterError),
    #[error(transparent)]
    Export(#[from] ExportError),
}

impl SheetError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyJob => "sheet.empty_job",
            Self::EmptyLayout => "sheet.empty_layout",
            Self::InvalidLayoutId => "sheet.invalid_layout_id",
            Self::InvalidPaper { .. } => "sheet.invalid_paper",
            Self::InvalidGrid => "sheet.invalid_grid",
            Self::SlotOutsidePaper { .. } => "sheet.slot_outside_paper",
            Self::OverlappingSlots { .. } => "sheet.overlapping_slots",
            Self::InvalidFirstSlot { .. } => "sheet.invalid_first_slot",
            Self::LabelSizeMismatch { .. } => "sheet.label_size_mismatch",
            Self::RasterTooLarge => "sheet.raster_too_large",
            Self::OutputTooLarge { .. } => "sheet.output_too_large",
            Self::LimitExceeded => "sheet.limit_exceeded",
            Self::GeometryOverflow => "sheet.geometry_overflow",
            Self::InvalidDocument { .. } => "sheet.invalid_document",
            Self::Render { .. } => "sheet.render_failed",
            Self::Raster(_) => "sheet.raster_failed",
            Self::Export(ExportError::OutputTooLarge { .. }) => "sheet.output_too_large",
            Self::Export(_) => "sheet.export_failed",
        }
    }
}

pub fn normalize_layout(definition: &SheetDefinition) -> Result<SheetLayout, SheetError> {
    normalize_layout_with_limits(definition, &ProcessingLimits::default())
}

fn normalize_layout_with_limits(
    definition: &SheetDefinition,
    limits: &ProcessingLimits,
) -> Result<SheetLayout, SheetError> {
    match definition {
        SheetDefinition::Explicit { layout } => {
            validate_layout_with_limits(layout, limits)?;
            Ok(layout.clone())
        }
        SheetDefinition::Grid { grid } => {
            if grid.rows == 0
                || grid.columns == 0
                || grid.label_width_um <= 0
                || grid.label_height_um <= 0
                || grid.margin_left_um < 0
                || grid.margin_top_um < 0
                || grid.gap_x_um < 0
                || grid.gap_y_um < 0
            {
                return Err(SheetError::InvalidGrid);
            }
            let count = usize::from(grid.rows)
                .checked_mul(usize::from(grid.columns))
                .ok_or(SheetError::GeometryOverflow)?;
            if count > limits.max_sheet_slots {
                return Err(SheetError::LimitExceeded);
            }
            let mut slots = Vec::with_capacity(count);
            let mut push = |row: u16, column: u16| -> Result<(), SheetError> {
                let pitch_x = grid
                    .label_width_um
                    .checked_add(grid.gap_x_um)
                    .ok_or(SheetError::GeometryOverflow)?;
                let pitch_y = grid
                    .label_height_um
                    .checked_add(grid.gap_y_um)
                    .ok_or(SheetError::GeometryOverflow)?;
                let x_um = i64::from(column)
                    .checked_mul(pitch_x)
                    .and_then(|value| grid.margin_left_um.checked_add(value))
                    .ok_or(SheetError::GeometryOverflow)?;
                let y_um = i64::from(row)
                    .checked_mul(pitch_y)
                    .and_then(|value| grid.margin_top_um.checked_add(value))
                    .ok_or(SheetError::GeometryOverflow)?;
                slots.push(SheetSlot {
                    x_um,
                    y_um,
                    width_um: grid.label_width_um,
                    height_um: grid.label_height_um,
                });
                Ok(())
            };
            match grid.fill_order {
                FillOrder::RowMajor => {
                    for row in 0..grid.rows {
                        for column in 0..grid.columns {
                            push(row, column)?;
                        }
                    }
                }
                FillOrder::ColumnMajor => {
                    for column in 0..grid.columns {
                        for row in 0..grid.rows {
                            push(row, column)?;
                        }
                    }
                }
            }
            let layout = SheetLayout {
                id: grid.id.clone(),
                paper_width_um: grid.paper_width_um,
                paper_height_um: grid.paper_height_um,
                slots,
            };
            validate_layout_with_limits(&layout, limits)?;
            Ok(layout)
        }
    }
}

pub fn validate_layout(layout: &SheetLayout) -> Result<(), SheetError> {
    validate_layout_with_limits(layout, &ProcessingLimits::default())
}

fn validate_layout_with_limits(
    layout: &SheetLayout,
    limits: &ProcessingLimits,
) -> Result<(), SheetError> {
    if layout.id.is_empty() || layout.id.len() > MAX_LAYOUT_ID_BYTES {
        return Err(SheetError::InvalidLayoutId);
    }
    if layout.paper_width_um <= 0 || layout.paper_height_um <= 0 {
        return Err(SheetError::InvalidPaper {
            width_um: layout.paper_width_um,
            height_um: layout.paper_height_um,
        });
    }
    if layout.slots.is_empty() {
        return Err(SheetError::EmptyLayout);
    }
    if layout.slots.len() > limits.max_sheet_slots {
        return Err(SheetError::LimitExceeded);
    }
    for (index, slot) in layout.slots.iter().enumerate() {
        let right = slot
            .x_um
            .checked_add(slot.width_um)
            .ok_or(SheetError::GeometryOverflow)?;
        let bottom = slot
            .y_um
            .checked_add(slot.height_um)
            .ok_or(SheetError::GeometryOverflow)?;
        if slot.x_um < 0
            || slot.y_um < 0
            || slot.width_um <= 0
            || slot.height_um <= 0
            || right > layout.paper_width_um
            || bottom > layout.paper_height_um
        {
            return Err(SheetError::SlotOutsidePaper { index });
        }
    }
    for left in 0..layout.slots.len() {
        for right in left + 1..layout.slots.len() {
            if overlaps(layout.slots[left], layout.slots[right])? {
                return Err(SheetError::OverlappingSlots { left, right });
            }
        }
    }
    Ok(())
}

fn overlaps(left: SheetSlot, right: SheetSlot) -> Result<bool, SheetError> {
    let left_right = left
        .x_um
        .checked_add(left.width_um)
        .ok_or(SheetError::GeometryOverflow)?;
    let right_right = right
        .x_um
        .checked_add(right.width_um)
        .ok_or(SheetError::GeometryOverflow)?;
    let left_bottom = left
        .y_um
        .checked_add(left.height_um)
        .ok_or(SheetError::GeometryOverflow)?;
    let right_bottom = right
        .y_um
        .checked_add(right.height_um)
        .ok_or(SheetError::GeometryOverflow)?;
    Ok(left.x_um < right_right
        && right.x_um < left_right
        && left.y_um < right_bottom
        && right.y_um < left_bottom)
}

pub fn plan(
    input: SheetPlanInput,
    definition: &SheetDefinition,
    options: SheetOptions,
    limits: &ProcessingLimits,
) -> Result<SheetPlan, SheetError> {
    let span = tracing::debug_span!(
        "sheet.plan",
        item_count = input.item_count,
        dpi = options.dpi.get(),
        first_slot = options.first_slot
    );
    let _entered = span.enter();
    if input.item_count == 0 {
        return Err(SheetError::EmptyJob);
    }
    if input.item_count > limits.max_sheet_items {
        return Err(SheetError::LimitExceeded);
    }
    let layout = normalize_layout_with_limits(definition, limits)?;
    if options.first_slot >= layout.slots.len() {
        return Err(SheetError::InvalidFirstSlot {
            first_slot: options.first_slot,
        });
    }
    let (paper_width, paper_height) = paper_dots(&layout, options.dpi, limits)?;
    let pixels_per_page = u64::from(paper_width)
        .checked_mul(u64::from(paper_height))
        .ok_or(SheetError::GeometryOverflow)?;
    let first_capacity = layout.slots.len() - options.first_slot;
    let item_count = usize::try_from(input.item_count).map_err(|_| SheetError::LimitExceeded)?;
    let remaining = item_count.saturating_sub(first_capacity);
    let additional_pages = if remaining == 0 {
        0
    } else {
        remaining.div_ceil(layout.slots.len())
    };
    let page_count = 1usize
        .checked_add(additional_pages)
        .ok_or(SheetError::GeometryOverflow)?;
    if page_count > usize::try_from(limits.max_pages).map_err(|_| SheetError::LimitExceeded)? {
        return Err(SheetError::LimitExceeded);
    }
    let total_pixels = pixels_per_page
        .checked_mul(u64::try_from(page_count).map_err(|_| SheetError::GeometryOverflow)?)
        .ok_or(SheetError::GeometryOverflow)?;
    if total_pixels > limits.max_total_pixels {
        return Err(SheetError::RasterTooLarge);
    }
    let mut placements = Vec::with_capacity(item_count);
    for item in 0..item_count {
        let (page, slot) = if item < first_capacity {
            (0, options.first_slot + item)
        } else {
            let offset = item - first_capacity;
            (1 + offset / layout.slots.len(), offset % layout.slots.len())
        };
        let target = layout.slots[slot];
        if input.label_width_um != target.width_um || input.label_height_um != target.height_um {
            return Err(SheetError::LabelSizeMismatch {
                item,
                actual_width_um: input.label_width_um,
                actual_height_um: input.label_height_um,
                expected_width_um: target.width_um,
                expected_height_um: target.height_um,
            });
        }
        placements.push(SheetPlacement { item, page, slot });
    }
    Ok(SheetPlan {
        page_count,
        layout,
        placements,
    })
}

pub fn pdf(
    documents: &[Document],
    definition: &SheetDefinition,
    options: SheetOptions,
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, SheetError> {
    let first = documents.first().ok_or(SheetError::EmptyJob)?;
    if documents.len()
        > usize::try_from(limits.max_sheet_items).map_err(|_| SheetError::LimitExceeded)?
    {
        return Err(SheetError::LimitExceeded);
    }
    let plan = plan(
        SheetPlanInput {
            item_count: u32::try_from(documents.len()).map_err(|_| SheetError::LimitExceeded)?,
            label_width_um: first.media.width,
            label_height_um: first.media.height,
        },
        definition,
        options,
        limits,
    )?;
    for (item, document) in documents.iter().enumerate() {
        validate_document_limits(document, limits)?;
        document
            .validate()
            .map_err(|errors| SheetError::InvalidDocument { item, errors })?;
        let target = plan.layout.slots[plan.placements[item].slot];
        ensure_size(item, document.media.width, document.media.height, target)?;
    }
    pdf_documents_planned(documents, plan, options, limits)
}

fn validate_document_limits(
    document: &Document,
    limits: &ProcessingLimits,
) -> Result<(), SheetError> {
    if document.elements.len() > limits.max_elements
        || document.resources.len() > limits.max_resources
    {
        return Err(SheetError::LimitExceeded);
    }
    for resource in &document.resources {
        let encoded = resource.data_base64.len();
        if encoded > limits.max_resource_bytes {
            return Err(SheetError::LimitExceeded);
        }
        let estimated_decoded = encoded
            .checked_add(3)
            .and_then(|value| value.checked_div(4))
            .and_then(|value| value.checked_mul(3))
            .ok_or(SheetError::LimitExceeded)?;
        if estimated_decoded > limits.max_decoded_resource_bytes {
            return Err(SheetError::LimitExceeded);
        }
    }
    Ok(())
}

fn pdf_documents_planned(
    documents: &[Document],
    plan: SheetPlan,
    options: SheetOptions,
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, SheetError> {
    let span = tracing::debug_span!(
        "sheet.export",
        page_count = plan.page_count,
        item_count = documents.len(),
        dpi = options.dpi.get(),
        output_bytes = tracing::field::Empty
    );
    let _entered = span.enter();
    let (page_width, page_height) = paper_dots(&plan.layout, options.dpi, limits)?;
    let mut pages = Vec::with_capacity(plan.page_count);
    for page_index in 0..plan.page_count {
        let page_span = tracing::debug_span!("sheet.page", page_index);
        let _page_entered = page_span.enter();
        let mut page = MonoRaster::try_new(page_width, page_height, limits.max_canvas_pixels)
            .map_err(|_| SheetError::RasterTooLarge)?;
        for placement in plan
            .placements
            .iter()
            .filter(|placement| placement.page == page_index)
        {
            let document = &documents[placement.item];
            let mut render_document = document.clone();
            render_document.media.dpi = options.dpi.get();
            let mut render_options = render::options_for_document(&render_document);
            render_options.max_pixels = limits.max_canvas_pixels;
            let raster = render::render_with_resource_limit(
                &render_document,
                render_options,
                limits.max_resource_pixels,
            )
            .map_err(|source| SheetError::Render {
                item: placement.item,
                source,
            })?;
            let slot = plan.layout.slots[placement.slot];
            page.blit(
                &raster,
                dots_u32(slot.x_um, options.dpi)?,
                dots_u32(slot.y_um, options.dpi)?,
            )?;
        }
        pages.push(PackedPdfPage {
            raster: PackedMonoRaster::from_mono(&page, limits.max_canvas_pixels)?,
            width_um: plan.layout.paper_width_um,
            height_um: plan.layout.paper_height_um,
        });
    }
    let output = export_pages(&pages, limits)?;
    span.record("output_bytes", output.len());
    Ok(output)
}

pub fn pdf_rasters(
    items: &[SheetRasterItem<'_>],
    definition: &SheetDefinition,
    options: SheetOptions,
    raster_options: SheetRasterOptions,
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, SheetError> {
    let first = items.first().ok_or(SheetError::EmptyJob)?;
    let plan = plan(
        SheetPlanInput {
            item_count: u32::try_from(items.len()).map_err(|_| SheetError::LimitExceeded)?,
            label_width_um: first.width_um,
            label_height_um: first.height_um,
        },
        definition,
        options,
        limits,
    )?;
    for (item, raster_item) in items.iter().enumerate() {
        raster_item.raster.validate()?;
        let target = plan.layout.slots[plan.placements[item].slot];
        ensure_size(item, raster_item.width_um, raster_item.height_um, target)?;
        let expected_width = dots_u32(raster_item.width_um, options.dpi)?;
        let expected_height = dots_u32(raster_item.height_um, options.dpi)?;
        if raster_item.raster.width != expected_width
            || raster_item.raster.height != expected_height
        {
            return Err(SheetError::RasterTooLarge);
        }
    }
    pdf_rasters_planned(items, plan, options, raster_options, limits)
}

fn pdf_rasters_planned(
    items: &[SheetRasterItem<'_>],
    plan: SheetPlan,
    options: SheetOptions,
    raster_options: SheetRasterOptions,
    limits: &ProcessingLimits,
) -> Result<Vec<u8>, SheetError> {
    let span = tracing::debug_span!(
        "sheet.export",
        page_count = plan.page_count,
        item_count = items.len(),
        dpi = options.dpi.get(),
        output_bytes = tracing::field::Empty
    );
    let _entered = span.enter();
    let (page_width, page_height) = paper_dots(&plan.layout, options.dpi, limits)?;
    let mut pages = Vec::with_capacity(plan.page_count);
    for page_index in 0..plan.page_count {
        let page_span = tracing::debug_span!("sheet.page", page_index);
        let _page_entered = page_span.enter();
        let mut page = MonoRaster::try_new(page_width, page_height, limits.max_canvas_pixels)
            .map_err(|_| SheetError::RasterTooLarge)?;
        for placement in plan
            .placements
            .iter()
            .filter(|placement| placement.page == page_index)
        {
            let slot = plan.layout.slots[placement.slot];
            let x = dots_u32(slot.x_um, options.dpi)?;
            let y = dots_u32(slot.y_um, options.dpi)?;
            page.blit(items[placement.item].raster, x, y)?;
            if raster_options.cut_marks {
                draw_cut_marks(&mut page, x, y, items[placement.item].raster, options.dpi)?;
            }
        }
        pages.push(PackedPdfPage {
            raster: PackedMonoRaster::from_mono(&page, limits.max_canvas_pixels)?,
            width_um: plan.layout.paper_width_um,
            height_um: plan.layout.paper_height_um,
        });
    }
    let output = export_pages(&pages, limits)?;
    span.record("output_bytes", output.len());
    Ok(output)
}

fn export_pages(pages: &[PackedPdfPage], limits: &ProcessingLimits) -> Result<Vec<u8>, SheetError> {
    export::pdf_packed_pages_physical(pages, limits.max_output_bytes).map_err(|error| match error {
        ExportError::OutputTooLarge { limit } => SheetError::OutputTooLarge { limit },
        other => SheetError::Export(other),
    })
}

fn ensure_size(
    item: usize,
    actual_width_um: i64,
    actual_height_um: i64,
    target: SheetSlot,
) -> Result<(), SheetError> {
    if actual_width_um == target.width_um && actual_height_um == target.height_um {
        Ok(())
    } else {
        Err(SheetError::LabelSizeMismatch {
            item,
            actual_width_um,
            actual_height_um,
            expected_width_um: target.width_um,
            expected_height_um: target.height_um,
        })
    }
}

fn paper_dots(
    layout: &SheetLayout,
    dpi: NonZeroU16,
    limits: &ProcessingLimits,
) -> Result<(u32, u32), SheetError> {
    let width = dots_u32(layout.paper_width_um, dpi)?;
    let height = dots_u32(layout.paper_height_um, dpi)?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(SheetError::GeometryOverflow)?;
    if pixels > limits.max_canvas_pixels {
        return Err(SheetError::RasterTooLarge);
    }
    Ok((width, height))
}

fn dots_u32(value_um: i64, dpi: NonZeroU16) -> Result<u32, SheetError> {
    let value = render::micrometres_to_dots(value_um, dpi.get());
    u32::try_from(value).map_err(|_| SheetError::GeometryOverflow)
}

fn draw_cut_marks(
    page: &mut MonoRaster,
    left: u32,
    top: u32,
    label: &MonoRaster,
    dpi: NonZeroU16,
) -> Result<(), SheetError> {
    let length = dots_u32(CUT_MARK_LENGTH_UM, dpi)?.max(1);
    for delta in 0..length {
        let points = [
            (left.checked_sub(delta), Some(top)),
            (Some(left), top.checked_sub(delta)),
            (
                left.checked_add(label.width)
                    .and_then(|x| x.checked_add(delta)),
                Some(top),
            ),
            (
                Some(left),
                top.checked_add(label.height)
                    .and_then(|y| y.checked_add(delta)),
            ),
        ];
        for (x, y) in points {
            if let (Some(x), Some(y)) = (x, y)
                && x < page.width
                && y < page.height
            {
                let index = u64::from(y)
                    .checked_mul(u64::from(page.width))
                    .and_then(|value| value.checked_add(u64::from(x)))
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or(SheetError::GeometryOverflow)?;
                page.pixels[index] = 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(order: FillOrder) -> SheetDefinition {
        SheetDefinition::Grid {
            grid: SheetGrid {
                id: "test".into(),
                paper_width_um: 100_000,
                paper_height_um: 100_000,
                rows: 2,
                columns: 2,
                label_width_um: 20_000,
                label_height_um: 10_000,
                margin_left_um: 5_000,
                margin_top_um: 7_000,
                gap_x_um: 2_000,
                gap_y_um: 3_000,
                fill_order: order,
            },
        }
    }

    fn options(first_slot: usize) -> SheetOptions {
        SheetOptions {
            first_slot,
            dpi: NonZeroU16::new(100).unwrap(),
        }
    }

    #[test]
    fn expands_grid_in_declared_order() {
        let row = normalize_layout(&grid(FillOrder::RowMajor)).unwrap();
        let column = normalize_layout(&grid(FillOrder::ColumnMajor)).unwrap();
        assert_eq!((row.slots[1].x_um, row.slots[1].y_um), (27_000, 7_000));
        assert_eq!(
            (column.slots[1].x_um, column.slots[1].y_um),
            (5_000, 20_000)
        );
    }

    #[test]
    fn first_slot_only_applies_to_first_page() {
        let result = plan(
            SheetPlanInput {
                item_count: 5,
                label_width_um: 20_000,
                label_height_um: 10_000,
            },
            &grid(FillOrder::RowMajor),
            options(2),
            &ProcessingLimits::default(),
        )
        .unwrap();
        assert_eq!(result.page_count, 2);
        assert_eq!(result.placements[0].slot, 2);
        assert_eq!(result.placements[2].slot, 0);
        assert_eq!(result.placements[2].page, 1);
    }

    #[test]
    fn rejects_overlap_and_mismatch() {
        let mut layout = normalize_layout(&grid(FillOrder::RowMajor)).unwrap();
        layout.slots[1].x_um = layout.slots[0].x_um;
        assert!(matches!(
            validate_layout(&layout),
            Err(SheetError::OverlappingSlots { .. })
        ));
        assert!(matches!(
            plan(
                SheetPlanInput {
                    item_count: 1,
                    label_width_um: 1,
                    label_height_um: 1,
                },
                &grid(FillOrder::RowMajor),
                options(0),
                &ProcessingLimits::default(),
            ),
            Err(SheetError::LabelSizeMismatch { .. })
        ));
        assert!(matches!(
            plan(
                SheetPlanInput {
                    item_count: 1,
                    label_width_um: 20_000,
                    label_height_um: 10_000,
                },
                &grid(FillOrder::RowMajor),
                options(4),
                &ProcessingLimits::default(),
            ),
            Err(SheetError::InvalidFirstSlot { .. })
        ));

        let overflow = SheetLayout {
            id: "overflow".into(),
            paper_width_um: i64::MAX,
            paper_height_um: 10,
            slots: vec![SheetSlot {
                x_um: i64::MAX,
                y_um: 0,
                width_um: 1,
                height_um: 1,
            }],
        };
        assert!(matches!(
            validate_layout(&overflow),
            Err(SheetError::GeometryOverflow)
        ));
    }

    #[test]
    fn compatibility_cut_marks_match_legacy_points() {
        let dpi = NonZeroU16::new(254).unwrap();
        let mut page = MonoRaster::try_new(100, 100, 10_000).unwrap();
        let label = MonoRaster::try_new(20, 10, 200).unwrap();
        draw_cut_marks(&mut page, 30, 40, &label, dpi).unwrap();
        let black = |x: u32, y: u32| page.pixels[(y * page.width + x) as usize] == 1;
        assert!(black(30, 40));
        assert!(black(29, 40));
        assert!(black(30, 39));
        assert!(black(50, 40));
        assert!(black(30, 50));
        assert!(!black(31, 41));
    }

    #[test]
    fn raster_export_is_bounded_and_exact_size() {
        let raster = MonoRaster::try_new(79, 39, 10_000).unwrap();
        let item = SheetRasterItem {
            raster: &raster,
            width_um: 20_000,
            height_um: 10_000,
        };
        let definition = SheetDefinition::Explicit {
            layout: SheetLayout {
                id: "single".into(),
                paper_width_um: 30_000,
                paper_height_um: 20_000,
                slots: vec![SheetSlot {
                    x_um: 5_000,
                    y_um: 5_000,
                    width_um: 20_000,
                    height_um: 10_000,
                }],
            },
        };
        let pdf = pdf_rasters(
            &[item],
            &definition,
            options(0),
            SheetRasterOptions::default(),
            &ProcessingLimits::default(),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/MediaBox [0 0 85.039370 56.692913]"));

        let limits = ProcessingLimits {
            max_output_bytes: 100,
            ..ProcessingLimits::default()
        };
        assert!(matches!(
            pdf_rasters(
                &[item],
                &definition,
                options(0),
                SheetRasterOptions::default(),
                &limits,
            ),
            Err(SheetError::OutputTooLarge { .. })
        ));
    }
}
