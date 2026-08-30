// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic template materialization and zone-batch placement.

use crate::document::{Constraints, Document, Element, ValidationError, Zone};
use crate::limits::ProcessingLimits;
use crate::template::{self, TemplateError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct MaterializeOptions<'a> {
    pub locale: &'a str,
    pub current_date: &'a str,
}

impl Default for MaterializeOptions<'static> {
    fn default() -> Self {
        Self {
            locale: "en",
            current_date: "1970-01-01",
        }
    }
}

#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error("document is invalid ({count} validation errors)")]
    InvalidDocument { count: usize },
    #[error("template evaluation failed at element {element}")]
    Template {
        element: usize,
        #[source]
        source: TemplateError,
    },
    #[error("materialization exceeds configured limits")]
    LimitExceeded,
    #[error("zone batch requires at least one zone")]
    NoZones,
    #[error("zone selection {index} is duplicated")]
    DuplicateZone { index: usize },
    #[error("zone selection {index} does not exist in the document")]
    UnknownZone { index: usize },
}

impl MaterializeError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidDocument { .. } => "materialize.invalid_document",
            Self::Template { .. } => "materialize.template",
            Self::LimitExceeded => "materialize.limit_exceeded",
            Self::NoZones => "batch.no_zones",
            Self::DuplicateZone { .. } => "batch.duplicate_zone",
            Self::UnknownZone { .. } => "batch.unknown_zone",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BatchPlacement {
    pub record: u32,
    pub page: u32,
    pub zone: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ZoneBatchPlan {
    pub page_count: u32,
    pub placements: Vec<BatchPlacement>,
}

pub fn materialize_record(
    document: &Document,
    fields: &BTreeMap<String, String>,
    options: MaterializeOptions<'_>,
) -> Result<Document, MaterializeError> {
    materialize_record_with_limits(document, fields, options, &ProcessingLimits::default())
}

pub fn materialize_record_with_limits(
    document: &Document,
    fields: &BTreeMap<String, String>,
    options: MaterializeOptions<'_>,
    limits: &ProcessingLimits,
) -> Result<Document, MaterializeError> {
    let span = tracing::debug_span!(
        "label.materialize",
        operation = "record",
        element_count = document.elements.len(),
        record_field_count = fields.len()
    );
    span.in_scope(|| {
        validate_inputs(document, fields, limits)?;
        let output = materialize_unchecked(document, fields, options, limits)?;
        if serde_json::to_vec(&output)
            .map_err(|_| MaterializeError::LimitExceeded)?
            .len()
            > limits.max_output_bytes
        {
            return Err(MaterializeError::LimitExceeded);
        }
        Ok(output)
    })
}

pub fn plan_zone_batch(
    document: &Document,
    record_count: u32,
    zone_ids: &[String],
) -> Result<ZoneBatchPlan, MaterializeError> {
    plan_zone_batch_with_limits(
        document,
        record_count,
        zone_ids,
        &ProcessingLimits::default(),
    )
}

pub fn plan_zone_batch_with_limits(
    document: &Document,
    record_count: u32,
    zone_ids: &[String],
    limits: &ProcessingLimits,
) -> Result<ZoneBatchPlan, MaterializeError> {
    let _span = tracing::debug_span!(
        "label.materialize",
        operation = "zone_batch_plan",
        record_count,
        zone_count = zone_ids.len()
    )
    .entered();
    if zone_ids.is_empty() {
        return Err(MaterializeError::NoZones);
    }
    validate_document(document, limits)?;
    if record_count > limits.max_sheet_items {
        return Err(MaterializeError::LimitExceeded);
    }
    let zones = selected_zones(&document.media.zones, zone_ids)?;
    let slots = u32::try_from(zones.len()).map_err(|_| MaterializeError::LimitExceeded)?;
    let page_count = if record_count == 0 {
        0
    } else {
        record_count
            .checked_add(slots - 1)
            .ok_or(MaterializeError::LimitExceeded)?
            / slots
    };
    if page_count > limits.max_pages {
        return Err(MaterializeError::LimitExceeded);
    }
    let capacity = usize::try_from(record_count).map_err(|_| MaterializeError::LimitExceeded)?;
    let mut placements = Vec::with_capacity(capacity);
    for record in 0..record_count {
        let zone =
            zones[usize::try_from(record % slots).map_err(|_| MaterializeError::LimitExceeded)?];
        placements.push(BatchPlacement {
            record,
            page: record / slots,
            zone: zone.id.clone(),
        });
    }
    Ok(ZoneBatchPlan {
        page_count,
        placements,
    })
}

pub fn materialize_zone_batch(
    document: &Document,
    records: &[BTreeMap<String, String>],
    zone_ids: &[String],
    options: MaterializeOptions<'_>,
) -> Result<Vec<Document>, MaterializeError> {
    materialize_zone_batch_with_limits(
        document,
        records,
        zone_ids,
        options,
        &ProcessingLimits::default(),
    )
}

pub fn materialize_zone_batch_with_limits(
    document: &Document,
    records: &[BTreeMap<String, String>],
    zone_ids: &[String],
    options: MaterializeOptions<'_>,
    limits: &ProcessingLimits,
) -> Result<Vec<Document>, MaterializeError> {
    let _span = tracing::debug_span!(
        "label.materialize",
        operation = "zone_batch",
        record_count = records.len(),
        zone_count = zone_ids.len()
    )
    .entered();
    let record_count = u32::try_from(records.len()).map_err(|_| MaterializeError::LimitExceeded)?;
    let plan = plan_zone_batch_with_limits(document, record_count, zone_ids, limits)?;
    if plan.page_count == 0 {
        return Ok(Vec::new());
    }
    let page_elements = document
        .elements
        .len()
        .checked_mul(zone_ids.len())
        .ok_or(MaterializeError::LimitExceeded)?;
    if page_elements > limits.max_elements {
        return Err(MaterializeError::LimitExceeded);
    }
    let document_bytes = serde_json::to_vec(document)
        .map_err(|_| MaterializeError::LimitExceeded)?
        .len();
    let page_capacity =
        usize::try_from(plan.page_count).map_err(|_| MaterializeError::LimitExceeded)?;
    if document_bytes
        .checked_mul(page_capacity)
        .is_none_or(|bytes| bytes > limits.max_output_bytes)
    {
        return Err(MaterializeError::LimitExceeded);
    }

    let mut pages = Vec::with_capacity(page_capacity);
    for page in 0..plan.page_count {
        let mut output = document.clone();
        output.name = format!("{} page {}", document.name, page + 1);
        output.elements.clear();
        pages.push(output);
    }
    for placement in &plan.placements {
        let record_index =
            usize::try_from(placement.record).map_err(|_| MaterializeError::LimitExceeded)?;
        validate_fields(&records[record_index], limits)?;
        let mut materialized =
            materialize_unchecked(document, &records[record_index], options, limits)?;
        rewrite_elements(&mut materialized.elements, placement, limits)?;
        let page_index =
            usize::try_from(placement.page).map_err(|_| MaterializeError::LimitExceeded)?;
        let page = &mut pages[page_index];
        for mut element in materialized.elements {
            element.common_mut().z_order =
                i32::try_from(page.elements.len()).map_err(|_| MaterializeError::LimitExceeded)?;
            page.elements.push(element);
        }
    }
    let output_bytes = serde_json::to_vec(&pages)
        .map_err(|_| MaterializeError::LimitExceeded)?
        .len();
    if output_bytes > limits.max_output_bytes {
        return Err(MaterializeError::LimitExceeded);
    }
    Ok(pages)
}

fn validate_inputs(
    document: &Document,
    fields: &BTreeMap<String, String>,
    limits: &ProcessingLimits,
) -> Result<(), MaterializeError> {
    validate_document(document, limits)?;
    validate_fields(fields, limits)
}

fn validate_fields(
    fields: &BTreeMap<String, String>,
    limits: &ProcessingLimits,
) -> Result<(), MaterializeError> {
    if fields.len() > limits.max_elements {
        return Err(MaterializeError::LimitExceeded);
    }
    let field_bytes = fields.iter().try_fold(0usize, |total, (key, value)| {
        total.checked_add(key.len())?.checked_add(value.len())
    });
    if field_bytes.is_none_or(|bytes| bytes > limits.max_output_bytes) {
        return Err(MaterializeError::LimitExceeded);
    }
    Ok(())
}

fn materialize_unchecked(
    document: &Document,
    fields: &BTreeMap<String, String>,
    options: MaterializeOptions<'_>,
    limits: &ProcessingLimits,
) -> Result<Document, MaterializeError> {
    let mut output = document.clone();
    let mut output_bytes = 0usize;
    for (element, item) in output.elements.iter_mut().enumerate() {
        let value = match item {
            Element::Text { text, .. } => Some(text),
            Element::Barcode { data, .. } | Element::QrCode { data, .. } => Some(data),
            _ => None,
        };
        if let Some(value) = value {
            *value = template::evaluate_with_context(
                value,
                template::Context {
                    fields,
                    locale: options.locale,
                    current_date: options.current_date,
                },
            )
            .map_err(|source| MaterializeError::Template { element, source })?;
            output_bytes = output_bytes
                .checked_add(value.len())
                .ok_or(MaterializeError::LimitExceeded)?;
            if output_bytes > limits.max_output_bytes {
                return Err(MaterializeError::LimitExceeded);
            }
        }
    }
    Ok(output)
}

fn validate_document(
    document: &Document,
    limits: &ProcessingLimits,
) -> Result<(), MaterializeError> {
    if document.elements.len() > limits.max_elements
        || document.resources.len() > limits.max_resources
    {
        return Err(MaterializeError::LimitExceeded);
    }
    let mut total_resource_bytes = 0usize;
    for resource in &document.resources {
        let encoded = resource.data_base64.len();
        let decoded = encoded
            .checked_add(3)
            .and_then(|bytes| bytes.checked_div(4))
            .and_then(|groups| groups.checked_mul(3))
            .ok_or(MaterializeError::LimitExceeded)?;
        if encoded > limits.max_resource_bytes || decoded > limits.max_decoded_resource_bytes {
            return Err(MaterializeError::LimitExceeded);
        }
        total_resource_bytes = total_resource_bytes
            .checked_add(encoded)
            .ok_or(MaterializeError::LimitExceeded)?;
        if total_resource_bytes > limits.max_output_bytes {
            return Err(MaterializeError::LimitExceeded);
        }
    }
    document.validate().map_err(
        |errors: Vec<ValidationError>| MaterializeError::InvalidDocument {
            count: errors.len(),
        },
    )
}

fn selected_zones<'a>(
    zones: &'a [Zone],
    zone_ids: &[String],
) -> Result<Vec<&'a Zone>, MaterializeError> {
    let mut seen = BTreeSet::new();
    zone_ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            if !seen.insert(id.as_str()) {
                return Err(MaterializeError::DuplicateZone { index });
            }
            zones
                .iter()
                .find(|zone| zone.id == *id)
                .ok_or(MaterializeError::UnknownZone { index })
        })
        .collect()
}

fn rewrite_elements(
    elements: &mut [Element],
    placement: &BatchPlacement,
    limits: &ProcessingLimits,
) -> Result<(), MaterializeError> {
    let ids = elements
        .iter()
        .map(|element| {
            let id = &element.common().id;
            let rewritten = format!("{id}:record:{}:zone:{}", placement.record, placement.zone);
            if rewritten.len() > limits.max_output_bytes {
                Err(MaterializeError::LimitExceeded)
            } else {
                Ok((id.clone(), rewritten))
            }
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    for element in elements {
        let common = element.common_mut();
        common.id = ids
            .get(&common.id)
            .cloned()
            .ok_or(MaterializeError::LimitExceeded)?;
        if let Some(group_id) = common.group_id.as_mut() {
            *group_id = ids
                .get(group_id)
                .cloned()
                .ok_or(MaterializeError::LimitExceeded)?;
        }
        let constraints = common.constraints.get_or_insert(Constraints {
            preserve_aspect: false,
            zone: None,
        });
        constraints.zone = Some(placement.zone.clone());
        if let Element::Group { children, .. } = element {
            for child in children {
                *child = ids
                    .get(child)
                    .cloned()
                    .ok_or(MaterializeError::LimitExceeded)?;
            }
        }
    }
    Ok(())
}
