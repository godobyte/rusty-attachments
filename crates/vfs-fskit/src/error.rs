//! Error types for the FSKit VFS.

use thiserror::Error;

/// Errors that can occur in the FSKit VFS.
#[derive(Error, Debug)]
pub enum FsKitVfsError {
    /// Failed to build inode tree from manifest.
    #[error("Failed to build inode tree: {0}")]
    InodeBuildError(String),

    /// Failed to initialize memory pool.
    #[error("Failed to initialize memory pool: {0}")]
    MemoryPoolError(String),

    /// Failed to initialize read cache.
    #[error("Failed to initialize read cache: {0}")]
    ReadCacheError(String),

    /// Failed to initialize write cache.
    #[error("Failed to initialize write cache: {0}")]
    WriteCacheError(String),

    /// Failed to mount filesystem.
    #[error("Failed to mount filesystem: {0}")]
    MountError(String),

    /// FSKit is not available on this platform.
    #[error("FSKit requires macOS 15.4 or later")]
    FsKitNotAvailable,

    /// Underlying VFS error.
    #[error("VFS error: {0}")]
    VfsError(#[from] rusty_attachments_vfs::VfsError),

    /// IO error.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}
