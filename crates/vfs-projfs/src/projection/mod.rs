//! Manifest projection layer.
//!
//! This module provides the in-memory tree structure built from the manifest.
//! All data is kept in memory for fast enumeration and lookup.

mod folder;
mod manifest;
mod types;

pub use folder::{FolderData, SortedFolderEntries};
pub use manifest::ManifestProjection;
pub use types::{ContentHash, FileData, FolderEntry, ProjectedFileInfo, SymlinkData};
