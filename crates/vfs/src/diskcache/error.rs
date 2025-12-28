//! Error types for disk cache operations.

use thiserror::Error;

/// Errors from disk cache operations.
#[derive(Debug, Error)]
pub enum DiskCacheError {
    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// File not found.
    #[error("File not found: {0}")]
    NotFound(String),

    /// Cache full.
    #[error("Cache full")]
    CacheFull,

    /// JSON serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Hash mismatch during verification.
    #[error("Hash mismatch: expected {expected}, got {actual}")]
    HashMismatch {
        /// Expected hash value.
        expected: String,
        /// Actual computed hash.
        actual: String,
    },

    /// Size mismatch during verification.
    #[error("Size mismatch: expected {expected}, got {actual}")]
    SizeMismatch {
        /// Expected size in bytes.
        expected: u64,
        /// Actual size in bytes.
        actual: u64,
    },
}
