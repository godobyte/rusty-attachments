//! Job Attachments manifest model for Deadline Cloud.
//!
//! This crate provides Rust implementations of the manifest formats:
//! - v2023-03-03: Original format with files only
//! - v2025-12: Extended format with directories, symlinks, chunking, and typed wrappers
//!
//! # Operations
//!
//! The `operations` module provides composable manifest operations:
//! - `compose` - Combine manifests (later entries override earlier)
//! - `diff` - Compute difference between manifests
//! - `filter` - Filter manifest entries by predicate
//! - `subtree` - Extract subtree as relative manifest
//! - `partition` - Split manifest into (root, RelManifest) pairs
//! - `join` - Prepend prefix to paths (inverse of subtree)

pub mod error;
pub mod hash;
pub mod manifest_types;
pub mod merge;
pub mod operations;
pub mod version;

pub mod v2023_03_03;
pub mod v2025_12;

mod decode;
mod encode;

pub use decode::decode_manifest;
pub use error::{ManifestError, ManifestTypeError};
pub use hash::HashAlgorithm;
pub use manifest_types::{AbsSnapshot, AbsSnapshotDiff, Snapshot, SnapshotDiff};
pub use merge::{merge_manifests, merge_manifests_chronologically};
pub use version::{ManifestType, ManifestVersion, PathStyle, SpecVersion};

/// Version-agnostic manifest wrapper.
#[derive(Debug, Clone)]
#[allow(non_camel_case_types)]
pub enum Manifest {
    /// Legacy v2023-03-03 format.
    V2023_03_03(v2023_03_03::AssetManifest),
    /// Current v2025-12 format with spec version metadata.
    V2025_12(v2025_12::AssetManifest),
}

impl Manifest {
    /// Decode a manifest from JSON string, auto-detecting version.
    pub fn decode(json: &str) -> Result<Self, ManifestError> {
        decode_manifest(json)
    }

    /// Encode the manifest to canonical JSON string.
    pub fn encode(&self) -> Result<String, ManifestError> {
        match self {
            Manifest::V2023_03_03(m) => m.encode(),
            Manifest::V2025_12(m) => m.encode(),
        }
    }

    /// Get the manifest version.
    pub fn version(&self) -> ManifestVersion {
        match self {
            Manifest::V2023_03_03(_) => ManifestVersion::V2023_03_03,
            Manifest::V2025_12(_) => ManifestVersion::V2025_12,
        }
    }

    /// Get the specification version (v2025-12 only).
    pub fn spec_version(&self) -> Option<SpecVersion> {
        match self {
            Manifest::V2023_03_03(_) => None,
            Manifest::V2025_12(m) => Some(m.spec_version),
        }
    }

    /// Get the hash algorithm used.
    pub fn hash_alg(&self) -> HashAlgorithm {
        match self {
            Manifest::V2023_03_03(m) => m.hash_alg,
            Manifest::V2025_12(m) => m.hash_alg,
        }
    }

    /// Get the total size of all files.
    pub fn total_size(&self) -> u64 {
        match self {
            Manifest::V2023_03_03(m) => m.total_size,
            Manifest::V2025_12(m) => m.total_size,
        }
    }

    /// Get the number of file entries.
    pub fn file_count(&self) -> usize {
        match self {
            Manifest::V2023_03_03(m) => m.paths.len(),
            Manifest::V2025_12(m) => m.files.len(),
        }
    }

    /// Get the manifest type (snapshot or diff).
    pub fn manifest_type(&self) -> ManifestType {
        match self {
            Manifest::V2023_03_03(_) => ManifestType::Snapshot,
            Manifest::V2025_12(m) => m.manifest_type(),
        }
    }

    /// Get the path style (absolute or relative).
    pub fn path_style(&self) -> PathStyle {
        match self {
            Manifest::V2023_03_03(_) => PathStyle::Relative,
            Manifest::V2025_12(m) => m.path_style(),
        }
    }

    /// Check if this manifest has absolute paths.
    pub fn is_absolute(&self) -> bool {
        self.path_style() == PathStyle::Absolute
    }

    /// Check if this manifest has relative paths.
    pub fn is_relative(&self) -> bool {
        self.path_style() == PathStyle::Relative
    }
}
