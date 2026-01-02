//! Configuration options for the FSKit VFS.

use std::path::PathBuf;

use rusty_attachments_vfs::{
    MemoryPoolConfig, PrefetchStrategy, ReadAheadOptions, ReadCacheConfig, TimeoutOptions,
};

/// Configuration for FSKit VFS.
///
/// Mirrors `VfsOptions` from the FUSE implementation for consistency.
/// FSKit-specific options (volume_name, volume_id) are added.
#[derive(Debug, Clone)]
pub struct FsKitVfsOptions {
    /// Memory pool configuration.
    pub pool: MemoryPoolConfig,
    /// Prefetch strategy for chunk loading.
    pub prefetch: PrefetchStrategy,
    /// Read-ahead configuration.
    pub read_ahead: ReadAheadOptions,
    /// Timeout settings.
    pub timeouts: TimeoutOptions,
    /// Read cache configuration (disk cache for immutable content).
    pub read_cache: ReadCacheConfig,
    /// Volume name displayed in Finder.
    pub volume_name: String,
    /// Unique volume identifier (UUID recommended).
    pub volume_id: String,
}

impl Default for FsKitVfsOptions {
    fn default() -> Self {
        Self {
            pool: MemoryPoolConfig::default(),
            prefetch: PrefetchStrategy::default(),
            read_ahead: ReadAheadOptions::default(),
            timeouts: TimeoutOptions::default(),
            read_cache: ReadCacheConfig::default(),
            volume_name: "Deadline Assets".to_string(),
            volume_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl FsKitVfsOptions {
    /// Set memory pool configuration.
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

    /// Set volume name displayed in Finder.
    ///
    /// # Arguments
    /// * `name` - Volume name
    pub fn with_volume_name(mut self, name: impl Into<String>) -> Self {
        self.volume_name = name.into();
        self
    }

    /// Set volume identifier.
    ///
    /// # Arguments
    /// * `id` - Volume UUID
    pub fn with_volume_id(mut self, id: impl Into<String>) -> Self {
        self.volume_id = id.into();
        self
    }
}

/// Configuration for write behavior.
#[derive(Debug, Clone)]
pub struct FsKitWriteOptions {
    /// Directory for materialized cache.
    pub cache_dir: PathBuf,
    /// Whether to sync to disk on every write (slower but safer).
    pub sync_on_write: bool,
    /// Maximum dirty file size before forcing flush.
    pub max_dirty_size: u64,
}

impl Default for FsKitWriteOptions {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from("/tmp/vfs-fskit-cache"),
            sync_on_write: true,
            max_dirty_size: 1024 * 1024 * 1024, // 1GB
        }
    }
}

impl FsKitWriteOptions {
    /// Set cache directory.
    ///
    /// # Arguments
    /// * `dir` - Directory path for write cache
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = dir.into();
        self
    }

    /// Set sync on write behavior.
    ///
    /// # Arguments
    /// * `sync` - Whether to sync after each write
    pub fn with_sync_on_write(mut self, sync: bool) -> Self {
        self.sync_on_write = sync;
        self
    }

    /// Set maximum dirty size.
    ///
    /// # Arguments
    /// * `size` - Maximum bytes before forcing flush
    pub fn with_max_dirty_size(mut self, size: u64) -> Self {
        self.max_dirty_size = size;
        self
    }
}

/// FSKit mount options.
#[derive(Debug, Clone)]
pub struct FsKitMountOptions {
    /// FSKit extension bundle ID.
    pub fskit_id: String,
    /// Mount point path.
    pub mount_point: PathBuf,
    /// Force unmount existing mount.
    pub force: bool,
}

impl Default for FsKitMountOptions {
    fn default() -> Self {
        Self {
            fskit_id: "network.debox.fskitbridge.fskitext".to_string(),
            mount_point: PathBuf::from("/tmp/deadline-assets"),
            force: true,
        }
    }
}

impl FsKitMountOptions {
    /// Set FSKit extension bundle ID.
    ///
    /// # Arguments
    /// * `id` - Bundle identifier
    pub fn with_fskit_id(mut self, id: impl Into<String>) -> Self {
        self.fskit_id = id.into();
        self
    }

    /// Set mount point path.
    ///
    /// # Arguments
    /// * `path` - Mount point directory
    pub fn with_mount_point(mut self, path: impl Into<PathBuf>) -> Self {
        self.mount_point = path.into();
        self
    }

    /// Set force unmount behavior.
    ///
    /// # Arguments
    /// * `force` - Whether to force unmount existing mount
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }
}

#[cfg(target_os = "macos")]
impl From<FsKitMountOptions> for fskit_rs::MountOptions {
    fn from(opts: FsKitMountOptions) -> Self {
        fskit_rs::MountOptions {
            fskit_id: opts.fskit_id,
            mount_point: opts.mount_point,
            force: opts.force,
        }
    }
}
