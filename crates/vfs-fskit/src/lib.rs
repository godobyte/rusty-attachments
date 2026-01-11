//! FSKit-based virtual filesystem for Deadline Cloud job attachments.
//!
//! This crate provides a macOS-native virtual filesystem using Apple's FSKit
//! framework (macOS 15.4+). It bridges the existing VFS primitives to FSKit
//! via the fskit-rs Rust crate.
//!
//! # Architecture
//!
//! ```text
//! FSKitBridge (Swift appex) ←→ TCP + Protobuf ←→ fskit-rs ←→ WritableFsKit
//!                                                              │
//!                                                              ▼
//!                                                    Shared VFS Primitives
//!                                                    (INodeManager, MemoryPool,
//!                                                     DirtyFileManager, etc.)
//! ```
//!
//! # Example
//!
//! ```ignore
//! use rusty_attachments_vfs_fskit::{WritableFsKit, FsKitVfsOptions, FsKitWriteOptions};
//! use rusty_attachments_model::Manifest;
//!
//! let manifest = Manifest::decode(&json_str)?;
//! let store = Arc::new(MyFileStore::new());
//! let vfs = WritableFsKit::new(&manifest, store, FsKitVfsOptions::default(), FsKitWriteOptions::default())?;
//! let session = fskit_rs::mount(vfs, mount_opts).await?;
//! ```

#[cfg(target_os = "macos")]
mod convert;
#[cfg(target_os = "macos")]
mod error;
#[cfg(target_os = "macos")]
mod fskit;
#[cfg(target_os = "macos")]
mod helpers;
#[cfg(target_os = "macos")]
mod options;

#[cfg(target_os = "macos")]
pub use error::FsKitVfsError;
#[cfg(target_os = "macos")]
pub use fskit::WritableFsKit;
#[cfg(target_os = "macos")]
pub use options::{FsKitMountOptions, FsKitVfsOptions, FsKitWriteOptions};

// Re-export shared VFS primitives for convenience
pub use rusty_attachments_vfs::{
    DirtyFileManager, DirtySummary, FileStore, INodeManager, MemoryPool, MemoryPoolConfig,
    PrefetchStrategy, ReadAheadOptions, ReadCacheConfig, StorageClientAdapter, TimeoutOptions,
    VfsError, WritableVfsStats, WritableVfsStatsCollector,
};

// Re-export storage types
pub use rusty_attachments_storage::{S3Location, StorageSettings};
pub use rusty_attachments_storage_crt::CrtStorageClient;

/// Check if FSKit is available on this system.
///
/// # Returns
/// True if running on macOS 15.4+ where FSKit is available.
#[cfg(target_os = "macos")]
pub fn fskit_available() -> bool {
    // FSKit requires macOS 15.4+
    // For now, assume it's available on macOS - runtime will fail if not
    true
}

/// Check if FSKit is available on this system.
///
/// # Returns
/// Always false on non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn fskit_available() -> bool {
    false
}
