//! Configuration options for the VFS.
//!
//! This module provides configuration for VFS behavior including caching,
//! prefetching, and performance tuning.

use std::path::PathBuf;

use crate::executor::ExecutorConfig;
use crate::memory_pool_v2::MemoryPoolConfig;

/// Configuration options for the VFS.
///
/// Controls caching behavior, prefetching strategy, and performance tuning.
///
/// # Example
///
/// ```ignore
/// let options = VfsOptions::default()
///     .with_prefetch(PrefetchStrategy::OnOpen { chunks: 2 })
///     .with_pool_config(MemoryPoolConfig::with_max_size(16 * GB));
///
/// let vfs = DeadlineVfs::new(manifest, store, options)?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct VfsOptions {
    /// Memory pool configuration.
    pub pool: MemoryPoolConfig,
    /// Prefetch strategy for chunk loading.
    pub prefetch: PrefetchStrategy,
    /// Kernel cache settings.
    pub kernel_cache: KernelCacheOptions,
    /// Read-ahead configuration.
    pub read_ahead: ReadAheadOptions,
    /// Timeout settings.
    pub timeouts: TimeoutOptions,
    /// Read cache configuration (disk cache for immutable content).
    pub read_cache: ReadCacheConfig,
    /// Async executor configuration.
    pub executor: ExecutorConfig,
}

impl VfsOptions {
    /// Create options with custom memory pool configuration.
    ///
    /// # Arguments
    /// * `pool` - Memory pool configuration
    pub fn with_pool_config(mut self, pool: MemoryPoolConfig) -> Self {
        self.pool = pool;
        self
    }

    /// Set the prefetch strategy.
    ///
    /// # Arguments
    /// * `prefetch` - Prefetch strategy to use
    pub fn with_prefetch(mut self, prefetch: PrefetchStrategy) -> Self {
        self.prefetch = prefetch;
        self
    }

    /// Set kernel cache options.
    ///
    /// # Arguments
    /// * `kernel_cache` - Kernel cache configuration
    pub fn with_kernel_cache(mut self, kernel_cache: KernelCacheOptions) -> Self {
        self.kernel_cache = kernel_cache;
        self
    }

    /// Set read-ahead options.
    ///
    /// # Arguments
    /// * `read_ahead` - Read-ahead configuration
    pub fn with_read_ahead(mut self, read_ahead: ReadAheadOptions) -> Self {
        self.read_ahead = read_ahead;
        self
    }

    /// Set timeout options.
    ///
    /// # Arguments
    /// * `timeouts` - Timeout configuration
    pub fn with_timeouts(mut self, timeouts: TimeoutOptions) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// Set read cache configuration.
    ///
    /// # Arguments
    /// * `read_cache` - Read cache configuration
    pub fn with_read_cache(mut self, read_cache: ReadCacheConfig) -> Self {
        self.read_cache = read_cache;
        self
    }

    /// Set executor configuration.
    ///
    /// # Arguments
    /// * `executor` - Async executor configuration
    pub fn with_executor(mut self, executor: ExecutorConfig) -> Self {
        self.executor = executor;
        self
    }
}

// ============================================================================
// Read Cache Configuration
// ============================================================================

/// Configuration for the disk-based read cache.
///
/// Controls caching of immutable CAS content to disk.
#[derive(Debug, Clone)]
pub struct ReadCacheConfig {
    /// Whether disk caching is enabled.
    pub enabled: bool,
    /// Directory for CAS cache storage.
    pub cache_dir: PathBuf,
    /// Whether to write through to disk on S3 fetch.
    pub write_through: bool,
}

impl Default for ReadCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cache_dir: PathBuf::from("/tmp/vfs-cache/cas"),
            write_through: true,
        }
    }
}

impl ReadCacheConfig {
    /// Create an enabled read cache with the specified directory.
    ///
    /// # Arguments
    /// * `cache_dir` - Directory for CAS cache storage
    pub fn enabled(cache_dir: PathBuf) -> Self {
        Self {
            enabled: true,
            cache_dir,
            write_through: true,
        }
    }

    /// Create a disabled read cache configuration.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

// ============================================================================
// Prefetch Strategy
// ============================================================================

/// Strategy for prefetching chunks.
///
/// Controls when and how chunks are loaded into the memory pool
/// before they are explicitly requested by a read operation.
#[derive(Debug, Clone, Default)]
pub enum PrefetchStrategy {
    /// No prefetching - chunks loaded only on read (lazy).
    /// Minimizes memory usage and S3 requests but has higher first-read latency.
    #[default]
    None,

    /// Prefetch first N chunks when file is opened.
    /// Reduces first-read latency at the cost of potentially unused downloads.
    OnOpen {
        /// Number of chunks to prefetch (starting from chunk 0).
        chunks: u32,
    },

    /// Prefetch next chunk when current chunk is being read.
    /// Good for sequential access patterns.
    Sequential {
        /// Number of chunks to prefetch ahead.
        look_ahead: u32,
    },

    /// Prefetch all chunks when file is opened.
    /// Best for small files or when entire file will be read.
    /// Use with caution for large files.
    Eager,
}

impl PrefetchStrategy {
    /// Create an OnOpen strategy with the specified number of chunks.
    ///
    /// # Arguments
    /// * `chunks` - Number of chunks to prefetch on open
    pub fn on_open(chunks: u32) -> Self {
        Self::OnOpen { chunks }
    }

    /// Create a Sequential strategy with the specified look-ahead.
    ///
    /// # Arguments
    /// * `look_ahead` - Number of chunks to prefetch ahead
    pub fn sequential(look_ahead: u32) -> Self {
        Self::Sequential { look_ahead }
    }

    /// Check if any prefetching is enabled.
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Get the number of chunks to prefetch on open.
    ///
    /// # Returns
    /// Number of chunks to prefetch, or 0 if not applicable.
    pub fn on_open_chunks(&self) -> u32 {
        match self {
            Self::OnOpen { chunks } => *chunks,
            Self::Eager => u32::MAX,
            _ => 0,
        }
    }

    /// Get the sequential look-ahead count.
    ///
    /// # Returns
    /// Number of chunks to prefetch ahead, or 0 if not applicable.
    pub fn sequential_look_ahead(&self) -> u32 {
        match self {
            Self::Sequential { look_ahead } => *look_ahead,
            _ => 0,
        }
    }
}

// ============================================================================
// Kernel Cache Options
// ============================================================================

/// Options for kernel-level caching (FUSE).
///
/// Controls how the kernel caches file data and attributes.
#[derive(Debug, Clone)]
pub struct KernelCacheOptions {
    /// Enable kernel page cache for file data.
    /// When true, the kernel caches read data and may serve subsequent
    /// reads from cache without calling into the VFS.
    pub enable_page_cache: bool,

    /// Enable kernel attribute cache.
    /// When true, the kernel caches file attributes (size, mtime, etc.).
    pub enable_attr_cache: bool,

    /// Attribute cache timeout in seconds.
    /// How long the kernel caches file attributes before re-querying.
    pub attr_timeout_secs: u64,

    /// Entry cache timeout in seconds.
    /// How long the kernel caches directory entry lookups.
    pub entry_timeout_secs: u64,
}

impl Default for KernelCacheOptions {
    fn default() -> Self {
        Self {
            enable_page_cache: true,
            enable_attr_cache: true,
            attr_timeout_secs: 86400,  // 24 hours (immutable content)
            entry_timeout_secs: 86400, // 24 hours
        }
    }
}

impl KernelCacheOptions {
    /// Create options optimized for immutable content (long cache times).
    pub fn immutable() -> Self {
        Self {
            enable_page_cache: true,
            enable_attr_cache: true,
            attr_timeout_secs: 86400 * 7, // 1 week
            entry_timeout_secs: 86400 * 7,
        }
    }

    /// Create options with no kernel caching.
    pub fn no_cache() -> Self {
        Self {
            enable_page_cache: false,
            enable_attr_cache: false,
            attr_timeout_secs: 0,
            entry_timeout_secs: 0,
        }
    }
}

// ============================================================================
// Read-Ahead Options
// ============================================================================

/// Options for read-ahead behavior.
///
/// Controls how the VFS anticipates and pre-loads data for sequential reads.
#[derive(Debug, Clone)]
pub struct ReadAheadOptions {
    /// Enable sequential read detection.
    /// When true, the VFS tracks read patterns and prefetches
    /// upcoming chunks for sequential access.
    pub detect_sequential: bool,

    /// Minimum sequential reads before triggering prefetch.
    /// Number of consecutive sequential reads required before
    /// the VFS starts prefetching.
    pub sequential_threshold: u32,

    /// Maximum concurrent prefetch operations.
    /// Limits the number of simultaneous background fetches.
    pub max_concurrent_prefetch: u32,
}

impl Default for ReadAheadOptions {
    fn default() -> Self {
        Self {
            detect_sequential: true,
            sequential_threshold: 2,
            max_concurrent_prefetch: 4,
        }
    }
}

impl ReadAheadOptions {
    /// Create options with aggressive read-ahead.
    pub fn aggressive() -> Self {
        Self {
            detect_sequential: true,
            sequential_threshold: 1,
            max_concurrent_prefetch: 8,
        }
    }

    /// Create options with no read-ahead.
    pub fn disabled() -> Self {
        Self {
            detect_sequential: false,
            sequential_threshold: 0,
            max_concurrent_prefetch: 0,
        }
    }
}

// ============================================================================
// Timeout Options
// ============================================================================

/// Timeout settings for VFS operations.
#[derive(Debug, Clone)]
pub struct TimeoutOptions {
    /// Timeout for S3 fetch operations in seconds.
    /// Individual chunk downloads will fail if they exceed this duration.
    pub fetch_timeout_secs: u64,

    /// Timeout for open operations in seconds.
    /// Includes any prefetch time if prefetch is enabled.
    pub open_timeout_secs: u64,
}

impl Default for TimeoutOptions {
    fn default() -> Self {
        Self {
            fetch_timeout_secs: 300, // 5 minutes (large chunks)
            open_timeout_secs: 60,   // 1 minute
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_options() {
        let opts: VfsOptions = VfsOptions::default();
        assert!(!opts.prefetch.is_enabled());
        assert!(opts.kernel_cache.enable_page_cache);
        assert!(opts.read_ahead.detect_sequential);
        assert_eq!(opts.executor.worker_threads, 4);
    }

    #[test]
    fn test_builder_pattern() {
        let opts: VfsOptions = VfsOptions::default()
            .with_prefetch(PrefetchStrategy::on_open(3))
            .with_kernel_cache(KernelCacheOptions::immutable())
            .with_executor(ExecutorConfig::default().with_worker_threads(8));

        assert_eq!(opts.prefetch.on_open_chunks(), 3);
        assert_eq!(opts.kernel_cache.attr_timeout_secs, 86400 * 7);
        assert_eq!(opts.executor.worker_threads, 8);
    }

    #[test]
    fn test_prefetch_strategy() {
        assert!(!PrefetchStrategy::None.is_enabled());
        assert!(PrefetchStrategy::on_open(2).is_enabled());
        assert!(PrefetchStrategy::sequential(4).is_enabled());
        assert!(PrefetchStrategy::Eager.is_enabled());

        assert_eq!(PrefetchStrategy::on_open(5).on_open_chunks(), 5);
        assert_eq!(PrefetchStrategy::Eager.on_open_chunks(), u32::MAX);
        assert_eq!(PrefetchStrategy::sequential(3).sequential_look_ahead(), 3);
    }
}
