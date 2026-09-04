// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runtime-independent document, rendering, capability, discovery, and printer
//! protocol planning primitives.
//!
//! This crate performs deterministic computation only: platform I/O belongs in
//! `mb-printer-native` or a host adapter, while action execution belongs in
//! `mb-printer-executor`.
#![forbid(unsafe_code)]

pub mod administration;
pub mod capabilities;
pub mod discovery;
pub mod document;
pub mod export;
pub mod importer;
pub mod ipp;
pub mod laposte;
pub mod limits;
pub mod materialize;
pub mod media;
pub mod pdf_import;
pub mod probe;
pub mod protocol;
pub mod providers;
pub mod raster;
pub mod render;
pub mod resources;
pub mod schema_types_generated;
pub mod sheet;
pub mod snmp;
pub mod template;

pub use document::{Document, ValidationError};
