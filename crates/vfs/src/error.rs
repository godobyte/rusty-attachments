//! Error types for the VFS crate.

use std::fmt;

/// Errors that can occur during VFS operations.
#[derive(Debug)]
pub enum VfsError {
    /// Inode not found.
    InodeNotFound(u64),

    /// Not a directory.
    NotADirectory(u64),

    /// Not a file.
    NotAFile(u64),

    /// Content retrieval failed.
    ContentRetrievalFailed {
        hash: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Hash mismatch after retrieval.
    HashMismatch { expected: String, actual: String },

    /// Mount operation failed.
    MountFailed(String),

    /// Write cache error.
    WriteCacheError(String),

    /// Cache read failed.
    CacheReadFailed(String),

    /// Chunk not loaded for dirty file.
    ChunkNotLoaded { path: String, chunk_index: u32 },

    /// IO error.
    Io(std::io::Error),

    /// File already exists.
    FileExists(String),

    /// Read-only filesystem.
    ReadOnly,
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VfsError::InodeNotFound(id) => write!(f, "Inode not found: {}", id),
            VfsError::NotADirectory(id) => write!(f, "Not a directory: {}", id),
            VfsError::NotAFile(id) => write!(f, "Not a file: {}", id),
            VfsError::ContentRetrievalFailed { hash, source } => {
                write!(f, "Content retrieval failed for hash {}: {}", hash, source)
            }
            VfsError::HashMismatch { expected, actual } => {
                write!(f, "Hash mismatch: expected {}, got {}", expected, actual)
            }
            VfsError::MountFailed(msg) => write!(f, "Mount failed: {}", msg),
            VfsError::WriteCacheError(msg) => write!(f, "Write cache error: {}", msg),
            VfsError::CacheReadFailed(path) => write!(f, "Cache read failed: {}", path),
            VfsError::ChunkNotLoaded { path, chunk_index } => {
                write!(f, "Chunk {} not loaded for file: {}", chunk_index, path)
            }
            VfsError::Io(e) => write!(f, "IO error: {}", e),
            VfsError::FileExists(path) => write!(f, "File already exists: {}", path),
            VfsError::ReadOnly => write!(f, "Read-only filesystem"),
        }
    }
}

impl std::error::Error for VfsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VfsError::ContentRetrievalFailed { source, .. } => Some(source.as_ref()),
            VfsError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for VfsError {
    fn from(e: std::io::Error) -> Self {
        VfsError::Io(e)
    }
}
