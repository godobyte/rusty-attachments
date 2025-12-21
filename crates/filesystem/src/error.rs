//! File system error types.

use rusty_attachments_common::PathError;
use thiserror::Error;

/// Errors that can occur during file system operations.
#[derive(Debug, Error)]
pub enum FileSystemError {
    /// Path-related errors (from common crate).
    #[error(transparent)]
    Path(#[from] PathError),

    /// Path not found.
    #[error("Path not found: {path}")]
    PathNotFound {
        /// The path that was not found.
        path: String,
    },

    /// Path is outside the expected root directory.
    #[error("Path is outside root: {path} not in {root}")]
    PathOutsideRoot {
        /// The path that was checked.
        path: String,
        /// The root directory it should be within.
        root: String,
    },

    /// Directory not found.
    #[error("Directory not found: {path}")]
    DirectoryNotFound {
        /// The directory path.
        path: String,
    },

    /// Permission denied.
    #[error("Permission denied: {path}")]
    PermissionDenied {
        /// The path where permission was denied.
        path: String,
    },

    /// Invalid glob pattern.
    #[error("Invalid glob pattern: {pattern}: {reason}")]
    InvalidGlobPattern {
        /// The invalid pattern.
        pattern: String,
        /// Reason why it's invalid.
        reason: String,
    },

    /// Symlink target escapes root directory.
    #[error("Symlink target escapes root: {symlink} -> {target}")]
    SymlinkEscapesRoot {
        /// Path to the symlink.
        symlink: String,
        /// The symlink target.
        target: String,
    },

    /// Symlink has absolute target.
    #[error("Symlink has absolute target: {symlink} -> {target}")]
    SymlinkAbsoluteTarget {
        /// Path to the symlink.
        symlink: String,
        /// The absolute target.
        target: String,
    },

    /// Symlink not allowed when opening file.
    #[error("Symlink not allowed when opening file: {path}")]
    SymlinkNotAllowed {
        /// Path to the symlink.
        path: String,
    },

    /// IO error.
    #[error("IO error at {path}: {source}")]
    IoError {
        /// Path where error occurred.
        path: String,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// Hash computation error.
    #[error("Hash error: {message}")]
    HashError {
        /// Error message.
        message: String,
    },

    /// Operation was cancelled.
    #[error("Operation cancelled")]
    Cancelled,

    /// Manifest error.
    #[error("Manifest error: {message}")]
    ManifestError {
        /// Error message.
        message: String,
    },

    /// Invalid path.
    #[error("Invalid path: {path}")]
    InvalidPath {
        /// The invalid path.
        path: String,
    },
}

impl FileSystemError {
    /// Create an IoError from std::io::Error.
    ///
    /// # Arguments
    /// * `path` - Path where the error occurred
    /// * `source` - The underlying IO error
    pub fn io_error(path: impl Into<String>, source: std::io::Error) -> Self {
        Self::IoError {
            path: path.into(),
            source,
        }
    }
}
