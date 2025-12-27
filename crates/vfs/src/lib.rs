//! FUSE-based virtual filesystem for Deadline Cloud job attachments.
//!
//! This crate provides a read-only FUSE filesystem that mounts Deadline Cloud
//! job attachment manifests. Files appear as local files but content is fetched
//! on-demand from S3 CAS (Content-Addressable Storage).
//!
//! # Architecture
//!
//! ```text
//! Layer 3: FUSE Interface (fuser::Filesystem impl)
//! Layer 2: VFS Operations (lookup, read, readdir)
//! Layer 1: Primitives (INodeManager, FileStore, MemoryPool)
//! ```
//!
//! # Example
//!
//! ```ignore
//! use rusty_attachments_vfs::{DeadlineVfs, VfsOptions};
//! use rusty_attachments_model::Manifest;
//!
//! let manifest = Manifest::decode(&json_str)?;
//! let store = Arc::new(MyFileStore::new());
//! let vfs = DeadlineVfs::new(&manifest, store, VfsOptions::default())?;
//! fuser::mount2(vfs, "/mnt/assets", &[MountOption::RO])?;
//! ```

pub mod builder;
pub mod content;
pub mod error;
pub mod inode;
pub mod memory_pool;
pub mod options;
pub mod write;

#[cfg(feature = "fuse")]
pub mod fuse;

#[cfg(feature = "fuse")]
pub mod fuse_writable;

pub use error::VfsError;
pub use memory_pool::{
    BlockContentProvider, BlockHandle, BlockKey, MemoryPool, MemoryPoolConfig, MemoryPoolError,
    MemoryPoolStats,
};
pub use options::{
    KernelCacheOptions, PrefetchStrategy, ReadAheadOptions, TimeoutOptions, VfsOptions,
};

pub use builder::build_from_manifest;
pub use content::{FileStore, StorageClientAdapter};
pub use inode::{FileContent, INode, INodeFile, INodeId, INodeManager, INodeType, ROOT_INODE};

// Re-export CRT storage client for convenience
pub use rusty_attachments_storage_crt::{CrtError, CrtStorageClient};
// Re-export commonly used storage types
pub use rusty_attachments_storage::{S3Location, StorageSettings};

pub use write::{
    DiffManifestExporter, DirtyContent, DirtyEntry, DirtyFile, DirtyFileInfo, DirtyFileManager,
    DirtyState, DirtySummary, MaterializedCache, MemoryWriteCache, WritableVfsStats,
    WritableVfsStatsCollector, WriteCache, WriteCacheError,
};

#[cfg(feature = "fuse")]
pub use fuse::{mount, spawn_mount, DeadlineVfs, OpenFileInfo, VfsStats, VfsStatsCollector};

#[cfg(feature = "fuse")]
pub use fuse_writable::{mount_writable, spawn_mount_writable, WritableVfs, WriteOptions};
