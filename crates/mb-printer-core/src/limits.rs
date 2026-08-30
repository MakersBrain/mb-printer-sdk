// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit resource limits for untrusted host input and bounded processing.

/// Limits enforced by a host before it parses an encoded request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireLimits {
    pub max_input_bytes: usize,
    pub max_request_documents: u32,
}

impl Default for WireLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_request_documents: 1_000,
        }
    }
}

/// Limits enforced by typed core operations after deserialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingLimits {
    pub max_elements: usize,
    pub max_resources: usize,
    pub max_resource_bytes: usize,
    pub max_decoded_resource_bytes: usize,
    pub max_resource_pixels: u64,
    pub max_canvas_pixels: u64,
    pub max_total_pixels: u64,
    pub max_pages: u32,
    pub max_copies: u16,
    pub max_plan_actions: usize,
    pub max_plan_bytes: usize,
    pub max_output_bytes: usize,
    pub max_sheet_slots: usize,
    pub max_sheet_items: u32,
}

impl Default for ProcessingLimits {
    fn default() -> Self {
        Self {
            max_elements: 10_000,
            max_resources: 1_000,
            max_resource_bytes: 8 * 1024 * 1024,
            max_decoded_resource_bytes: 8 * 1024 * 1024,
            max_resource_pixels: 25_000_000,
            max_canvas_pixels: 100_000_000,
            max_total_pixels: 400_000_000,
            max_pages: 100,
            max_copies: 1_000,
            max_plan_actions: 100_000,
            max_plan_bytes: 128 * 1024 * 1024,
            max_output_bytes: 128 * 1024 * 1024,
            max_sheet_slots: 1_000,
            max_sheet_items: 10_000,
        }
    }
}
