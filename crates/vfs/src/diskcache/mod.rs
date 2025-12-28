//! Disk cache implementations for VFS content storage.
//!
//! This module provides disk-based caching for both:
//! - Read-only CAS content (immutable files from manifest)
//! - Dirty file content (modified/new files in writable VFS)

mod error;
mod materialized;
mod memory_cache;
mod read_cache;
mod traits;

pub use error::DiskCacheError;
pub use materialized::{ChunkedFileMeta, MaterializedCache};
pub use memory_cache::MemoryWriteCache;
pub use read_cache::{ReadCache, ReadCacheOptions};
pub use traits::WriteCache;
