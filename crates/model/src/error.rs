//! Error types for manifest operations.

use thiserror::Error;

use crate::hash::HashAlgorithm;
use crate::version::{ManifestVersion, PathStyle, SpecVersion};

/// Errors that can occur during manifest operations.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),

    #[error("Unknown manifest version: {0}")]
    UnknownVersion(String),

    #[error("Unknown specification version: {0}")]
    UnknownSpecVersion(String),

    #[error("Unsupported hash algorithm: {0}")]
    UnsupportedHashAlgorithm(String),

    #[error("Validation error: {0}")]
    Validation(#[from] ValidationError),

    #[error("Cannot merge manifests with different hash algorithms: expected {expected:?}, got {actual:?}")]
    MergeHashAlgorithmMismatch {
        expected: HashAlgorithm,
        actual: HashAlgorithm,
    },

    #[error(
        "Cannot merge manifests with different versions: expected {expected:?}, got {actual:?}"
    )]
    MergeVersionMismatch {
        expected: ManifestVersion,
        actual: ManifestVersion,
    },

    #[error("Manifest type error: {0}")]
    ManifestType(#[from] ManifestTypeError),
}

/// Error when manifest doesn't match expected type.
#[derive(Debug, Clone, Error)]
pub enum ManifestTypeError {
    #[error("Expected {expected:?} paths, got {actual:?}")]
    PathStyleMismatch {
        expected: PathStyle,
        actual: PathStyle,
    },

    #[error("Expected {expected:?} manifest, got {actual:?}")]
    TypeMismatch {
        expected: crate::version::ManifestType,
        actual: crate::version::ManifestType,
    },

    #[error("V2023_03_03 manifests cannot be converted to typed wrappers")]
    LegacyVersionNotSupported,

    #[error("Diff manifest missing parent_manifest_hash")]
    MissingParentHash,

    #[error("Expected spec version {expected:?}, got {actual:?}")]
    SpecVersionMismatch {
        expected: SpecVersion,
        actual: SpecVersion,
    },
}

/// Validation errors for manifest entries.
#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("Deleted entry '{path}' cannot have '{field}' field")]
    DeletedEntryHasField { path: String, field: &'static str },

    #[error("File '{path}' must have exactly one of 'hash', 'chunkhashes', or 'symlink_target'")]
    InvalidContentFields { path: String },

    #[error("File '{path}' must have 'size' field")]
    MissingSize { path: String },

    #[error("File '{path}' must have 'mtime' field")]
    MissingMtime { path: String },

    #[error(
        "File '{path}' with chunkhashes must have size > {chunk_size} (256MB), got {actual_size}"
    )]
    ChunkedFileTooSmall {
        path: String,
        chunk_size: u64,
        actual_size: u64,
    },

    #[error("File '{path}' with size {size} should have {expected} chunks, got {actual}")]
    ChunkCountMismatch {
        path: String,
        size: u64,
        expected: usize,
        actual: usize,
    },

    #[error("Symlink '{path}' target must be a relative path, got absolute: '{target}'")]
    AbsoluteSymlinkTarget { path: String, target: String },

    #[error("Symlink '{path}' target escapes manifest root: '{target}'")]
    EscapingSymlinkTarget { path: String, target: String },

    #[error("Duplicate entry '{path}' has conflicting '{field}' values")]
    DuplicateConflict { path: String, field: &'static str },

    #[error("Snapshot manifests cannot have deletion markers")]
    SnapshotWithDeletion,

    #[error("Diff manifests must have parent_manifest_hash")]
    DiffMissingParentHash,
}
