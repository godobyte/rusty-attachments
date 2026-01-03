//! Manifest projection layer.
//!
//! This module provides the in-memory tree structure built from the manifest.
//! All data is kept in memory for fast enumeration and lookup.

mod folder;
mod manifest;
pub mod types;

pub use manifest::ManifestProjection;
