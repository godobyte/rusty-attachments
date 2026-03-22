//! Core types for relaxed consistency file resolution.

use serde::{Deserialize, Serialize};

use rusty_attachments_model::HashAlgorithm;

/// A key that identifies a file within a relaxed consistency root.
/// Derived from the relative path within the root, not from file content.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelaxedFileKey {
    /// The relaxed root this file belongs to.
    pub root_id: String,
    /// The file's path relative to the root (posix-normalized).
    pub relative_path: String,
    /// XXH128 hash of the relative_path, used as the S3 object key.
    pub path_key: String,
}

/// Priority level for file upload requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestPriority {
    /// File is needed immediately (VFS read is blocking on it).
    High,
    /// File may be needed soon (prefetch / warm-up).
    AsyncEventual,
}

/// Resolution status for a relaxed consistency file.
#[derive(Debug, Clone)]
pub enum RelaxedResolution {
    /// File has been uploaded. Content is available in CAS at this hash.
    Available {
        /// CAS content hash.
        content_hash: String,
        /// Hash algorithm used.
        hash_algorithm: HashAlgorithm,
        /// File size in bytes.
        size: u64,
        /// Chunk hashes if the file was chunked (>256MB).
        chunk_hashes: Option<Vec<String>>,
    },
    /// File has not been uploaded yet. A request has been enqueued.
    Pending,
    /// File request failed permanently (e.g., file not found on-prem).
    Failed {
        /// Reason for failure.
        reason: String,
    },
}
