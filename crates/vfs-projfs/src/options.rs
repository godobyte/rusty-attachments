//! Configuration options for ProjFS VFS.

use std::path::PathBuf;
use std::time::Duration;

use rusty_attachments_vfs::{ExecutorConfig, MemoryPoolConfig};
use windows::core::GUID;

/// Configuration for disk-based read cache.
#[derive(Debug, Clone)]
pub struct ReadCacheConfig {
    /// Whether disk caching is enabled.
    pub enabled: bool,
    /// Directory for CAS cache storage.
    pub cache_dir: PathBuf,
    /// Whether to write through to disk on fetch.
    pub write_through: bool,
}

impl Default for ReadCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_dir: PathBuf::from("C:\\Temp\\vfs-read-cache"),
            write_through: true,
        }
    }
}

impl ReadCacheConfig {
    /// Create a disabled read cache config.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    /// Create config with custom cache directory.
    ///
    /// # Arguments
    /// * `cache_dir` - Directory for CAS cache storage
    pub fn with_cache_dir(mut self, cache_dir: PathBuf) -> Self {
        self.cache_dir = cache_dir;
        self
    }

    /// Enable or disable the cache.
    ///
    /// # Arguments
    /// * `enabled` - Whether to enable disk caching
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Configuration for ProjFS virtualizer.
#[derive(Debug, Clone)]
pub struct ProjFsOptions {
    /// Virtualization root path.
    pub root_path: PathBuf,

    /// Instance GUID (unique per mount).
    pub instance_guid: GUID,

    /// Number of worker threads for async executor.
    pub worker_threads: usize,

    /// Default timeout for async operations.
    pub default_timeout: Option<Duration>,

    /// ProjFS pool thread count (0 = let ProjFS decide).
    pub pool_thread_count: u32,

    /// ProjFS concurrent thread count (0 = let ProjFS decide).
    pub concurrent_thread_count: u32,

    /// Memory pool configuration.
    pub memory_pool: MemoryPoolConfig,

    /// Notifications to receive.
    pub notifications: NotificationMask,

    /// Read cache configuration (disk cache for S3 content).
    pub read_cache: ReadCacheConfig,
}

impl ProjFsOptions {
    /// Create options with specified root path.
    ///
    /// # Arguments
    /// * `root_path` - Virtualization root directory
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            root_path,
            instance_guid: GUID::new().unwrap(),
            worker_threads: 4,
            default_timeout: None,
            pool_thread_count: 0,
            concurrent_thread_count: 0,
            memory_pool: MemoryPoolConfig::default(),
            notifications: NotificationMask::for_writable(),
            read_cache: ReadCacheConfig::default(),
        }
    }

    /// Set worker thread count.
    ///
    /// # Arguments
    /// * `count` - Number of worker threads
    pub fn with_worker_threads(mut self, count: usize) -> Self {
        self.worker_threads = count;
        self
    }

    /// Set default timeout.
    ///
    /// # Arguments
    /// * `timeout` - Default timeout duration
    pub fn with_default_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Set memory pool configuration.
    ///
    /// # Arguments
    /// * `config` - Memory pool configuration
    pub fn with_memory_pool(mut self, config: MemoryPoolConfig) -> Self {
        self.memory_pool = config;
        self
    }

    /// Set notification mask.
    ///
    /// # Arguments
    /// * `mask` - Notification mask
    pub fn with_notifications(mut self, mask: NotificationMask) -> Self {
        self.notifications = mask;
        self
    }

    /// Set read cache configuration.
    ///
    /// # Arguments
    /// * `config` - Read cache configuration
    pub fn with_read_cache(mut self, config: ReadCacheConfig) -> Self {
        self.read_cache = config;
        self
    }

    /// Get executor configuration.
    pub fn executor_config(&self) -> ExecutorConfig {
        ExecutorConfig::default()
            .with_worker_threads(self.worker_threads)
            .with_default_timeout(self.default_timeout)
    }
}

/// Write options for ProjFS VFS.
#[derive(Debug, Clone)]
pub struct ProjFsWriteOptions {
    /// Directory for write cache storage.
    pub cache_dir: PathBuf,

    /// Whether to use disk cache for writes.
    pub use_disk_cache: bool,
}

impl Default for ProjFsWriteOptions {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from("C:\\Temp\\vfs-write-cache"),
            use_disk_cache: true,
        }
    }
}

impl ProjFsWriteOptions {
    /// Create write options with specified cache directory.
    ///
    /// # Arguments
    /// * `cache_dir` - Directory for write cache storage
    pub fn with_cache_dir(mut self, cache_dir: PathBuf) -> Self {
        self.cache_dir = cache_dir;
        self
    }

    /// Enable or disable disk cache.
    ///
    /// # Arguments
    /// * `enabled` - Whether to use disk cache
    pub fn with_disk_cache(mut self, enabled: bool) -> Self {
        self.use_disk_cache = enabled;
        self
    }
}

/// Notification mask for ProjFS callbacks.
#[derive(Debug, Clone, Default)]
pub struct NotificationMask {
    /// Track new file creation.
    pub new_file_created: bool,
    /// Track file modifications.
    pub file_modified: bool,
    /// Track file deletions.
    pub file_deleted: bool,
    /// Track file renames.
    pub file_renamed: bool,
    /// Pre-delete notification (can veto).
    pub pre_delete: bool,
    /// Pre-rename notification (can veto).
    pub pre_rename: bool,
}

impl NotificationMask {
    /// Mask for writable VFS (track all modifications).
    pub fn for_writable() -> Self {
        Self {
            new_file_created: true,
            file_modified: true,
            file_deleted: true,
            file_renamed: true,
            pre_delete: false,
            pre_rename: false,
        }
    }

    /// Mask for read-only VFS (no notifications).
    pub fn for_readonly() -> Self {
        Self::default()
    }
}
