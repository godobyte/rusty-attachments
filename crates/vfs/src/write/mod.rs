//! Write support for the VFS.
//!
//! This module provides copy-on-write (COW) support for the VFS,
//! allowing files to be modified while maintaining the original
//! manifest data.

mod cache;
mod dirty;
mod export;
mod stats;

pub use cache::{MaterializedCache, MemoryWriteCache, WriteCache, WriteCacheError};
pub use dirty::{DirtyContent, DirtyEntry, DirtyFile, DirtyFileManager, DirtyState};
pub use export::{DiffManifestExporter, DirtyFileInfo, DirtySummary};
pub use stats::{WritableVfsStats, WritableVfsStatsCollector};
