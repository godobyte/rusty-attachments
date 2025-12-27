//! Statistics collection for writable VFS.
//!
//! Provides types and collectors for monitoring dirty file state
//! in the writable VFS layer.

use std::sync::Arc;
use std::time::Instant;

use crate::memory_pool::{MemoryPool, MemoryPoolStats};
use crate::write::{DirtyEntry, DirtyFileManager, DirtyState};

use super::export::{DirtyFileInfo, DirtySummary};

/// Statistics for a writable VFS.
///
/// Extends base VFS stats with dirty file tracking.
#[derive(Debug, Clone, Default)]
pub struct WritableVfsStats {
    /// Number of inodes in the filesystem.
    pub inode_count: usize,
    /// Memory pool statistics.
    pub pool_stats: MemoryPoolStats,
    /// Total cache hits.
    pub cache_hits: u64,
    /// Total cache allocations (misses that required fetch).
    pub cache_allocations: u64,
    /// Cache hit rate percentage.
    pub cache_hit_rate: f64,
    /// Time since VFS was created.
    pub uptime_secs: u64,
    /// Summary counts of dirty files.
    pub dirty_summary: DirtySummary,
    /// List of modified files (state = Modified).
    pub modified_files: Vec<DirtyFileInfo>,
    /// List of newly created files (state = New).
    pub new_files: Vec<DirtyFileInfo>,
    /// List of deleted file paths (state = Deleted).
    pub deleted_files: Vec<String>,
}

/// Collects statistics from a writable VFS.
///
/// Wraps memory pool stats and adds dirty file tracking.
/// Thread-safe and cloneable for use from background stats threads.
#[derive(Clone)]
pub struct WritableVfsStatsCollector {
    /// Memory pool reference.
    pool: Arc<MemoryPool>,
    /// Reference to dirty file manager.
    dirty_manager: Arc<DirtyFileManager>,
    /// Number of inodes.
    inode_count: usize,
    /// VFS start time.
    start_time: Instant,
}

impl WritableVfsStatsCollector {
    /// Create a new writable stats collector.
    ///
    /// # Arguments
    /// * `pool` - Memory pool reference
    /// * `dirty_manager` - Dirty file manager reference
    /// * `inode_count` - Number of inodes in the filesystem
    /// * `start_time` - When the VFS was created
    pub fn new(
        pool: Arc<MemoryPool>,
        dirty_manager: Arc<DirtyFileManager>,
        inode_count: usize,
        start_time: Instant,
    ) -> Self {
        Self {
            pool,
            dirty_manager,
            inode_count,
            start_time,
        }
    }

    /// Collect current statistics.
    ///
    /// # Returns
    /// Snapshot of writable VFS statistics including dirty file lists.
    pub fn collect(&self) -> WritableVfsStats {
        let entries: Vec<DirtyEntry> = self.dirty_manager.get_dirty_entries();

        let mut modified_files: Vec<DirtyFileInfo> = Vec::new();
        let mut new_files: Vec<DirtyFileInfo> = Vec::new();
        let mut deleted_files: Vec<String> = Vec::new();

        for entry in &entries {
            match entry.state {
                DirtyState::Modified => {
                    modified_files.push(DirtyFileInfo {
                        path: entry.path.clone(),
                        size: entry.size,
                    });
                }
                DirtyState::New => {
                    new_files.push(DirtyFileInfo {
                        path: entry.path.clone(),
                        size: entry.size,
                    });
                }
                DirtyState::Deleted => {
                    deleted_files.push(entry.path.clone());
                }
            }
        }

        let dirty_summary = DirtySummary {
            new_count: new_files.len(),
            modified_count: modified_files.len(),
            deleted_count: deleted_files.len(),
            new_dir_count: 0,
            deleted_dir_count: 0,
        };

        let pool_stats: MemoryPoolStats = self.pool.stats();
        let cache_hits: u64 = self.pool.hit_count();
        let cache_allocations: u64 = self.pool.allocation_count();
        let cache_hit_rate: f64 = self.pool.hit_rate();
        let uptime_secs: u64 = self.start_time.elapsed().as_secs();

        WritableVfsStats {
            inode_count: self.inode_count,
            pool_stats,
            cache_hits,
            cache_allocations,
            cache_hit_rate,
            uptime_secs,
            dirty_summary,
            modified_files,
            new_files,
            deleted_files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_writable_vfs_stats_default() {
        let stats = WritableVfsStats::default();
        assert_eq!(stats.inode_count, 0);
        assert_eq!(stats.dirty_summary.total(), 0);
        assert!(stats.modified_files.is_empty());
        assert!(stats.new_files.is_empty());
        assert!(stats.deleted_files.is_empty());
    }
}
