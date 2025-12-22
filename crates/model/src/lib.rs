//! Job Attachments manifest model for Deadline Cloud.
//!
//! This crate provides Rust implementations of the manifest formats:
//! - v2023-03-03: Original format with files only
//! - v2025-12-04-beta: Extended format with directories, symlinks, chunking

pub mod error;
pub mod hash;
pub mod merge;
pub mod version;

pub mod v2023_03_03;
pub mod v2025_12_04;

mod decode;
mod encode;

pub use decode::decode_manifest;
pub use error::ManifestError;
pub use hash::HashAlgorithm;
pub use merge::{merge_manifests, merge_manifests_chronologically};
pub use version::{ManifestType, ManifestVersion};

/// Version-agnostic manifest wrapper.
#[derive(Debug, Clone)]
#[allow(non_camel_case_types)]
pub enum Manifest {
    V2023_03_03(v2023_03_03::AssetManifest),
    V2025_12_04_beta(v2025_12_04::AssetManifest),
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
            Manifest::V2025_12_04_beta(m) => m.encode(),
        }
    }

    /// Get the manifest version.
    pub fn version(&self) -> ManifestVersion {
        match self {
            Manifest::V2023_03_03(_) => ManifestVersion::V2023_03_03,
            Manifest::V2025_12_04_beta(_) => ManifestVersion::V2025_12_04_beta,
        }
    }

    /// Get the hash algorithm used.
    pub fn hash_alg(&self) -> HashAlgorithm {
        match self {
            Manifest::V2023_03_03(m) => m.hash_alg,
            Manifest::V2025_12_04_beta(m) => m.hash_alg,
        }
    }

    /// Get the total size of all files.
    pub fn total_size(&self) -> u64 {
        match self {
            Manifest::V2023_03_03(m) => m.total_size,
            Manifest::V2025_12_04_beta(m) => m.total_size,
        }
    }

    /// Get the number of file entries.
    pub fn file_count(&self) -> usize {
        match self {
            Manifest::V2023_03_03(m) => m.paths.len(),
            Manifest::V2025_12_04_beta(m) => m.paths.len(),
        }
    }
}
