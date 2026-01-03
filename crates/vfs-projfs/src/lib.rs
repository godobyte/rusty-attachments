//! ProjFS-based virtual filesystem for Deadline Cloud job attachments.
//!
//! This crate provides a Windows-native virtual filesystem using Microsoft's
//! Projected File System (ProjFS). The design is heavily influenced by VFSForGit,
//! a production-grade ProjFS implementation.
//!
//! # Platform Support
//!
//! This crate is Windows-only. It will fail to compile on other platforms.
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

// This crate is Windows-only
#[cfg(not(target_os = "windows"))]
compile_error!("rusty-attachments-vfs-projfs is only supported on Windows");

mod callbacks;
mod error;
mod options;
mod projection;
mod util;
mod virtualizer;

pub use error::ProjFsError;
pub use options::{NotificationMask, ProjFsOptions, ProjFsWriteOptions};
pub use virtualizer::WritableProjFs;

// Re-export shared VFS primitives for convenience
pub use rusty_attachments_vfs::{
    DirtyFileManager, DirtySummary, FileStore, INodeManager, MemoryPool,
    MemoryPoolConfig, PrefetchStrategy, ReadAheadOptions, ReadCacheConfig, StorageClientAdapter,
    TimeoutOptions, VfsError, WritableVfsStats, WritableVfsStatsCollector,
};
pub use rusty_attachments_vfs::write::DirtyDirManager;

// Re-export storage types
pub use rusty_attachments_storage::{S3Location, StorageSettings};
pub use rusty_attachments_storage_crt::CrtStorageClient;

/// Check if ProjFS is available on this system.
///
/// # Returns
/// True on Windows where ProjFS is available.
pub fn projfs_available() -> bool {
    // ProjFS is available on Windows 10 1809+ and Windows Server 2019+
    // For now, assume it's available - runtime will fail if not enabled
    true
}
