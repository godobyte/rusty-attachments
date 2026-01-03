//! Statistics collection for ProjFS VFS.
//!
//! Provides types and collectors for monitoring ProjFS state,
//! including memory pool stats, disk cache stats, and modification tracking.

use std::sync::Arc;
use std::time::Instant;

use rusty_attachments_vfs::diskcache::ReadCache;
use rusty_attachments_vfs::memory_pool_v2::{MemoryPool, MemoryPoolStats};

use crate::callbacks::modified_paths::{ModificationSummary, ModifiedPathsDatabase};

/// Statistics for a ProjFS VFS instance.
#[derive(Debug, Clone, Default)]
pub struct ProjFsStats {
    /// Memory pool statistics.
    pub pool_stats: MemoryPoolStats,
    /// Total cache hits (memory pool).
    pub cache_hits: u64,
    /// Total cache allocations (misses that required fetch).
    pub cache_allocations: u64,
    /// Cache hit rate percentage.
    pub cache_hit_rate: f64,
    /// Disk cache size in bytes (if enabled).
    pub disk_cache_size: Option<u64>,
    /// Time since VFS was created.
    pub uptime_secs: u64,
    /// Summary of file/directory modifications.
    pub modification_summary: ModificationSummary,
}

impl ProjFsStats {
    /// Format stats as a display grid (similar to FUSE).
    ///
    /// # Returns
    /// Multi-line string with formatted statistics.
    pub fn display_grid(&self) -> String {
        let mut lines: Vec<String> = Vec::new();

        lines.push("╔══════════════════════════════════════════════════════════╗".to_string());
        lines.push("║                    ProjFS VFS Statistics                 ║".to_string());
        lines.push("╠══════════════════════════════════════════════════════════╣".to_string());

        // Uptime
        lines.push(format!("║ Uptime: {:>47} sec ║", self.uptime_secs));
        lines.push("╠══════════════════════════════════════════════════════════╣".to_string());

        // Memory Pool
        lines.push("║ Memory Pool                                              ║".to_string());
        lines.push(format!(
            "║   Total blocks:  {:>40} ║",
            self.pool_stats.total_blocks
        ));
        lines.push(format!(
            "║   In-use blocks: {:>40} ║",
            self.pool_stats.in_use_blocks
        ));
        lines.push(format!(
            "║   Dirty blocks:  {:>40} ║",
            self.pool_stats.dirty_blocks
        ));
        lines.push(format!(
            "║   Current size:  {:>37} MB ║",
            self.pool_stats.current_size / (1024 * 1024)
        ));
        lines.push(format!(
            "║   Cache hits:    {:>40} ║",
            self.cache_hits
        ));
        lines.push(format!(
            "║   Cache misses:  {:>40} ║",
            self.cache_allocations
        ));
        lines.push(format!(
            "║   Hit rate:      {:>39.1}% ║",
            self.cache_hit_rate * 100.0
        ));
        lines.push("╠══════════════════════════════════════════════════════════╣".to_string());

        // Disk Cache
        lines.push("║ Disk Cache                                               ║".to_string());
        match self.disk_cache_size {
            Some(size) => {
                lines.push(format!(
                    "║   Status:        {:>40} ║",
                    "Enabled"
                ));
                lines.push(format!(
                    "║   Size:          {:>37} MB ║",
                    size / (1024 * 1024)
                ));
            }
            None => {
                lines.push(format!(
                    "║   Status:        {:>40} ║",
                    "Disabled"
                ));
            }
        }
        lines.push("╠══════════════════════════════════════════════════════════╣".to_string());

        // Modifications
        lines.push("║ Modifications                                            ║".to_string());
        lines.push(format!(
            "║   Files created:   {:>38} ║",
            self.modification_summary.created_files.len()
        ));
        lines.push(format!(
            "║   Files modified:  {:>38} ║",
            self.modification_summary.modified_files.len()
        ));
        lines.push(format!(
            "║   Files deleted:   {:>38} ║",
            self.modification_summary.deleted_files.len()
        ));
        lines.push(format!(
            "║   Dirs created:    {:>38} ║",
            self.modification_summary.created_dirs.len()
        ));
        lines.push(format!(
            "║   Dirs deleted:    {:>38} ║",
            self.modification_summary.deleted_dirs.len()
        ));

        lines.push("╚══════════════════════════════════════════════════════════╝".to_string());

        lines.join("\n")
    }
}

/// Collects statistics from a ProjFS VFS.
///
/// Thread-safe and cloneable for use from background stats threads.
#[derive(Clone)]
pub struct ProjFsStatsCollector {
    /// Memory pool reference.
    pool: Arc<MemoryPool>,
    /// Modified paths database reference.
    modified_paths: Arc<ModifiedPathsDatabase>,
    /// Optional disk cache reference.
    read_cache: Option<Arc<ReadCache>>,
    /// VFS start time.
    start_time: Instant,
}

impl ProjFsStatsCollector {
    /// Create a new ProjFS stats collector.
    ///
    /// # Arguments
    /// * `pool` - Memory pool reference
    /// * `modified_paths` - Modified paths database reference
    /// * `read_cache` - Optional disk cache reference
    /// * `start_time` - When the VFS was created
    pub fn new(
        pool: Arc<MemoryPool>,
        modified_paths: Arc<ModifiedPathsDatabase>,
        read_cache: Option<Arc<ReadCache>>,
        start_time: Instant,
    ) -> Self {
        Self {
            pool,
            modified_paths,
            read_cache,
            start_time,
        }
    }

    /// Collect current statistics.
    ///
    /// # Returns
    /// Snapshot of ProjFS VFS statistics.
    pub fn collect(&self) -> ProjFsStats {
        let pool_stats: MemoryPoolStats = self.pool.stats();
        let cache_hits: u64 = self.pool.hit_count();
        let cache_allocations: u64 = self.pool.allocation_count();
        let cache_hit_rate: f64 = self.pool.hit_rate();
        let uptime_secs: u64 = self.start_time.elapsed().as_secs();

        let disk_cache_size: Option<u64> = self.read_cache.as_ref().map(|c| c.current_size());

        let modification_summary: ModificationSummary = self.modified_paths.get_summary();

        ProjFsStats {
            pool_stats,
            cache_hits,
            cache_allocations,
            cache_hit_rate,
            disk_cache_size,
            uptime_secs,
            modification_summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_attachments_vfs::memory_pool_v2::MemoryPoolConfig;

    #[test]
    fn test_projfs_stats_default() {
        let stats = ProjFsStats::default();
        assert_eq!(stats.uptime_secs, 0);
        assert_eq!(stats.cache_hits, 0);
        assert!(stats.disk_cache_size.is_none());
    }

    #[test]
    fn test_projfs_stats_collector() {
        let pool = Arc::new(MemoryPool::new(MemoryPoolConfig::default()));
        let modified_paths = Arc::new(ModifiedPathsDatabase::new());
        let start_time = Instant::now();

        let collector = ProjFsStatsCollector::new(
            pool,
            modified_paths,
            None,
            start_time,
        );

        let stats: ProjFsStats = collector.collect();
        assert!(stats.uptime_secs < 1); // Just created
        assert!(stats.disk_cache_size.is_none());
    }

    #[test]
    fn test_display_grid_format() {
        let stats = ProjFsStats {
            uptime_secs: 120,
            cache_hits: 1000,
            cache_allocations: 50,
            cache_hit_rate: 0.952,
            disk_cache_size: Some(1024 * 1024 * 100), // 100 MB
            ..Default::default()
        };

        let grid: String = stats.display_grid();

        assert!(grid.contains("ProjFS VFS Statistics"));
        assert!(grid.contains("Uptime:"));
        assert!(grid.contains("120"));
        assert!(grid.contains("Cache hits:"));
        assert!(grid.contains("1000"));
        assert!(grid.contains("Enabled"));
        assert!(grid.contains("100 MB"));
    }
}
