//! ProjFS-based virtual filesystem for Deadline Cloud job attachments.
//!
//! This crate provides a Windows-native virtual filesystem using Microsoft's
//! Projected File System (ProjFS). The design is heavily influenced by VFSForGit,
//! a production-grade ProjFS implementation.
//!
//! # Platform Support
//!
//! This crate is Windows-only. On non-Windows platforms, only stub types are
//! available and `projfs_available()` returns false.
//!
//! # Architecture
//!
//! ```text
//! Layer 3: ProjFsVirtualizer (ProjFS callbacks)
//! Layer 2: VfsCallbacks (coordination & dirty state)
//! Layer 1: ManifestProjection (in-memory manifest tree)
//! Layer 0: Shared VFS primitives (INodeManager, MemoryPool, etc.)
//! ```
//!
//! # Example
//!
//! ```ignore
//! use rusty_attachments_vfs_projfs::{WritableProjFs, ProjFsOptions, ProjFsWriteOptions};
//! use rusty_attachments_model::Manifest;
//!
//! let manifest = Manifest::decode(&json_str)?;
//! let store = Arc::new(MyFileStore::new());
//! let vfs = WritableProjFs::new(&manifest, store, ProjFsOptions::default(), ProjFsWriteOptions::default())?;
//! vfs.start()?;
//! ```

#[cfg(target_os = "windows")]
mod callbacks;
#[cfg(target_os = "windows")]
mod error;
#[cfg(target_os = "windows")]
mod options;
#[cfg(target_os = "windows")]
mod projection;
#[cfg(target_os = "windows")]
mod util;
#[cfg(target_os = "windows")]
mod virtualizer;

#[cfg(target_os = "windows")]
pub use error::ProjFsError;
#[cfg(target_os = "windows")]
pub use options::{NotificationMask, ProjFsOptions, ProjFsWriteOptions, ReadCacheConfig};
#[cfg(target_os = "windows")]
pub use virtualizer::WritableProjFs;

// Export callbacks layer types for advanced usage
#[cfg(target_os = "windows")]
pub use callbacks::{
    ModificationSummary, ModifiedPathsDatabase, PathRegistry, ProjFsStats, ProjFsStatsCollector,
    VfsCallbacks,
};

// Re-export shared VFS primitives for convenience
pub use rusty_attachments_vfs::{
    DirtyFileManager, DirtySummary, FileStore, INodeManager, MemoryPool,
    MemoryPoolConfig, PrefetchStrategy, ReadAheadOptions, StorageClientAdapter,
    TimeoutOptions, VfsError, WritableVfsStats, WritableVfsStatsCollector,
};
#[cfg(target_os = "windows")]
pub use rusty_attachments_vfs::write::DirtyDirManager;
#[cfg(target_os = "windows")]
pub use rusty_attachments_vfs::diskcache::ReadCache;

// Re-export storage types
pub use rusty_attachments_storage::{S3Location, StorageSettings};
pub use rusty_attachments_storage_crt::CrtStorageClient;

/// Check if ProjFS is available on this system.
///
/// # Returns
/// True on Windows where ProjFS is available.
#[cfg(target_os = "windows")]
pub fn projfs_available() -> bool {
    // ProjFS is available on Windows 10 1809+ and Windows Server 2019+
    // For now, assume it's available - runtime will fail if not enabled
    true
}

/// Check if ProjFS is available on this system.
///
/// # Returns
/// Always false on non-Windows platforms.
#[cfg(not(target_os = "windows"))]
pub fn projfs_available() -> bool {
    false
}
