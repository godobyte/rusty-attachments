//! Write support for the VFS.
//!
//! This module provides copy-on-write (COW) support for the VFS,
//! allowing files to be modified while maintaining the original
//! manifest data.

mod cache;
mod dirty;
mod dirty_dir;
mod export;
mod stats;

pub use cache::{MaterializedCache, MemoryWriteCache, WriteCache, WriteCacheError};
pub use dirty::{DirtyEntry, DirtyFileManager, DirtyFileMetadata, DirtyState};
pub use dirty_dir::{DirtyDir, DirtyDirEntry, DirtyDirManager, DirtyDirState};
pub use export::{DiffManifestExporter, DirtyFileInfo, DirtySummary};
pub use stats::{WritableVfsStats, WritableVfsStatsCollector};
