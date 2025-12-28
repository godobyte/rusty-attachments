//! Write support for the VFS.
//!
//! This module provides copy-on-write (COW) support for the VFS,
//! allowing files to be modified while maintaining the original
//! manifest data.

mod dirty;
mod dirty_dir;
mod export;
mod stats;

// Re-export disk cache types for backward compatibility
pub use crate::diskcache::{
    ChunkedFileMeta, DiskCacheError, MaterializedCache, MemoryWriteCache, WriteCache,
};

pub use dirty::{DirtyEntry, DirtyFileManager, DirtyFileMetadata, DirtyState};
pub use dirty_dir::{DirtyDir, DirtyDirEntry, DirtyDirManager, DirtyDirState};
pub use export::{DiffManifestExporter, DirtyFileInfo, DirtySummary};
pub use stats::{WritableVfsStats, WritableVfsStatsCollector};
