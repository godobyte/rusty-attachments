//! Error types for storage operations.

use thiserror::Error;

/// Errors that can occur during storage operations.
#[derive(Error, Debug, Clone)]
pub enum StorageError {
    /// Object not found in S3.
    #[error("Object not found: s3://{bucket}/{key}")]
    NotFound { bucket: String, key: String },

    /// Access denied.
    #[error("Access denied to s3://{bucket}/{key}: {message}")]
    AccessDenied {
        bucket: String,
        key: String,
        message: String,
    },

    /// Size mismatch (corruption or incomplete upload).
    #[error("Size mismatch for {key}: expected {expected} bytes, got {actual}")]
    SizeMismatch { key: String, expected: u64, actual: u64 },

    /// Network error.
    #[error("Network error: {message}")]
    NetworkError { message: String, retryable: bool },

    /// Local I/O error.
    #[error("I/O error for {path}: {message}")]
    IoError { path: String, message: String },

    /// Operation cancelled by user.
    #[error("Operation cancelled")]
    Cancelled,

    /// Invalid configuration.
    #[error("Invalid configuration: {message}")]
    InvalidConfig { message: String },

    /// Missing asset root in manifest metadata.
    ///
    /// This error occurs when downloading a manifest that doesn't have the
    /// required `asset-root` or `asset-root-json` metadata field.
    #[error("Failed to get asset root from metadata of output manifest: {s3_key}")]
    MissingAssetRoot { s3_key: String },

    /// Other error.
    #[error("{message}")]
    Other { message: String },
}

impl StorageError {
    /// Check if this error is retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            StorageError::NetworkError { retryable, .. } => *retryable,
            StorageError::NotFound { .. } => false,
            StorageError::AccessDenied { .. } => false,
            StorageError::SizeMismatch { .. } => false,
            StorageError::IoError { .. } => false,
            StorageError::Cancelled => false,
            StorageError::InvalidConfig { .. } => false,
            StorageError::MissingAssetRoot { .. } => false,
            StorageError::Other { .. } => false,
        }
    }
}

impl From<std::io::Error> for StorageError {
    fn from(err: std::io::Error) -> Self {
        StorageError::IoError {
            path: String::new(),
            message: err.to_string(),
        }
    }
}

/// Non-fatal error encountered during batch transfer.
#[derive(Debug, Clone)]
pub struct TransferError {
    /// The key/path that failed.
    pub key: String,
    /// The error that occurred.
    pub error: StorageError,
}

impl TransferError {
    /// Create a new transfer error.
    pub fn new(key: impl Into<String>, error: StorageError) -> Self {
        Self {
            key: key.into(),
            error,
        }
    }
}
