//! File stat caching to avoid redundant filesystem calls.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use lru::LruCache;
use rusty_attachments_common::DEFAULT_STAT_CACHE_CAPACITY;

/// Cached stat result for a file or directory.
#[derive(Debug, Clone)]
pub struct StatResult {
    /// File size in bytes.
    pub size: u64,
    /// Modification time in microseconds since Unix epoch.
    pub mtime_us: i64,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// Whether this is a symlink.
    pub is_symlink: bool,
    /// File mode (Unix permissions).
    pub mode: u32,
}

/// Cache for file stat results to avoid redundant filesystem calls.
///
/// Thread-safe via internal mutex.
pub struct StatCache {
    cache: Mutex<LruCache<PathBuf, Option<StatResult>>>,
}

impl StatCache {
    /// Create a new stat cache with the given capacity.
    ///
    /// # Arguments
    /// * `capacity` - Maximum number of entries to cache
    pub fn new(capacity: usize) -> Self {
        let cap: NonZeroUsize = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::new(1).unwrap());
        Self {
            cache: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Create with default capacity from common constants.
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_STAT_CACHE_CAPACITY)
    }

    /// Get stat for a path, using cache if available.
    ///
    /// # Arguments
    /// * `path` - Path to stat
    ///
    /// # Returns
    /// `Some(StatResult)` if path exists, `None` otherwise.
    pub fn stat(&self, path: &Path) -> Option<StatResult> {
        let mut cache = self.cache.lock().unwrap();

        if let Some(cached) = cache.get(path) {
            return cached.clone();
        }

        // Not in cache, perform actual stat
        let result: Option<StatResult> = Self::do_stat(path);
        cache.put(path.to_path_buf(), result.clone());
        result
    }

    /// Check if path exists using cached stat.
    ///
    /// # Arguments
    /// * `path` - Path to check
    pub fn exists(&self, path: &Path) -> bool {
        self.stat(path).is_some()
    }

    /// Check if path is a directory using cached stat.
    ///
    /// # Arguments
    /// * `path` - Path to check
    pub fn is_dir(&self, path: &Path) -> bool {
        self.stat(path).map(|s: StatResult| s.is_dir).unwrap_or(false)
    }

    /// Check if path is a symlink using cached stat.
    ///
    /// # Arguments
    /// * `path` - Path to check
    pub fn is_symlink(&self, path: &Path) -> bool {
        self.stat(path)
            .map(|s: StatResult| s.is_symlink)
            .unwrap_or(false)
    }

    /// Get file size using cached stat.
    ///
    /// # Arguments
    /// * `path` - Path to get size for
    ///
    /// # Returns
    /// File size in bytes, or 0 if path doesn't exist.
    pub fn size(&self, path: &Path) -> u64 {
        self.stat(path).map(|s: StatResult| s.size).unwrap_or(0)
    }

    /// Get modification time using cached stat.
    ///
    /// # Arguments
    /// * `path` - Path to get mtime for
    ///
    /// # Returns
    /// Modification time in microseconds since epoch, or 0 if path doesn't exist.
    pub fn mtime_us(&self, path: &Path) -> i64 {
        self.stat(path)
            .map(|s: StatResult| s.mtime_us)
            .unwrap_or(0)
    }

    /// Clear the cache.
    pub fn clear(&self) {
        self.cache.lock().unwrap().clear();
    }

    /// Get the current number of cached entries.
    pub fn len(&self) -> usize {
        self.cache.lock().unwrap().len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Perform actual stat on a path.
    fn do_stat(path: &Path) -> Option<StatResult> {
        // Use symlink_metadata to not follow symlinks
        let metadata: std::fs::Metadata = match path.symlink_metadata() {
            Ok(m) => m,
            Err(_) => return None,
        };

        let is_symlink: bool = metadata.file_type().is_symlink();

        // For symlinks, we need the target's metadata for size
        let (size, is_dir): (u64, bool) = if is_symlink {
            match path.metadata() {
                Ok(target_meta) => (target_meta.len(), target_meta.is_dir()),
                Err(_) => (0, false), // Broken symlink
            }
        } else {
            (metadata.len(), metadata.is_dir())
        };

        let mtime_us: i64 = metadata
            .modified()
            .ok()
            .and_then(|t: std::time::SystemTime| t.duration_since(UNIX_EPOCH).ok())
            .map(|d: std::time::Duration| d.as_micros() as i64)
            .unwrap_or(0);

        #[cfg(unix)]
        let mode: u32 = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode: u32 = 0;

        Some(StatResult {
            size,
            mtime_us,
            is_dir,
            is_symlink,
            mode,
        })
    }
}

impl Default for StatCache {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_stat_file() {
        let dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = dir.path().join("test.txt");
        let mut file: std::fs::File = std::fs::File::create(&file_path).unwrap();
        file.write_all(b"hello world").unwrap();
        drop(file);

        let cache: StatCache = StatCache::new(10);
        let stat: StatResult = cache.stat(&file_path).unwrap();

        assert_eq!(stat.size, 11);
        assert!(!stat.is_dir);
        assert!(!stat.is_symlink);
    }

    #[test]
    fn test_stat_directory() {
        let dir: TempDir = TempDir::new().unwrap();
        let subdir: PathBuf = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let cache: StatCache = StatCache::new(10);
        let stat: StatResult = cache.stat(&subdir).unwrap();

        assert!(stat.is_dir);
        assert!(!stat.is_symlink);
    }

    #[test]
    fn test_stat_nonexistent() {
        let cache: StatCache = StatCache::new(10);
        let result: Option<StatResult> = cache.stat(Path::new("/nonexistent/path"));
        assert!(result.is_none());
    }

    #[test]
    fn test_exists() {
        let dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = dir.path().join("test.txt");
        std::fs::File::create(&file_path).unwrap();

        let cache: StatCache = StatCache::new(10);
        assert!(cache.exists(&file_path));
        assert!(!cache.exists(&dir.path().join("nonexistent.txt")));
    }

    #[test]
    fn test_cache_hit() {
        let dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = dir.path().join("test.txt");
        std::fs::File::create(&file_path).unwrap();

        let cache: StatCache = StatCache::new(10);

        // First call - cache miss
        assert!(cache.stat(&file_path).is_some());
        assert_eq!(cache.len(), 1);

        // Second call - cache hit
        assert!(cache.stat(&file_path).is_some());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_clear() {
        let dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = dir.path().join("test.txt");
        std::fs::File::create(&file_path).unwrap();

        let cache: StatCache = StatCache::new(10);
        cache.stat(&file_path);
        assert_eq!(cache.len(), 1);

        cache.clear();
        assert_eq!(cache.len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_stat_symlink() {
        let dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = dir.path().join("target.txt");
        let link_path: PathBuf = dir.path().join("link.txt");

        let mut file: std::fs::File = std::fs::File::create(&file_path).unwrap();
        file.write_all(b"hello").unwrap();
        drop(file);

        std::os::unix::fs::symlink(&file_path, &link_path).unwrap();

        let cache: StatCache = StatCache::new(10);
        let stat: StatResult = cache.stat(&link_path).unwrap();

        assert!(stat.is_symlink);
        assert_eq!(stat.size, 5); // Size of target
    }
}
