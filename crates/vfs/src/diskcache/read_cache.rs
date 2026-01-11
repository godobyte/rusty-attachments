//! Disk cache for immutable CAS content.
//!
//! Provides read-through caching for content fetched from S3.
//! Content is stored by hash, enabling deduplication across files.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::DiskCacheError;

/// Options for read cache behavior.
#[derive(Debug, Clone)]
pub struct ReadCacheOptions {
    /// Directory for CAS cache storage.
    pub cache_dir: PathBuf,
    /// Whether to write through to disk on fetch.
    pub write_through: bool,
}

impl Default for ReadCacheOptions {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from("/tmp/vfs-cache/cas"),
            write_through: true,
        }
    }
}

impl ReadCacheOptions {
    /// Create options with a custom cache directory.
    ///
    /// # Arguments
    /// * `cache_dir` - Directory for CAS cache storage
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            ..Default::default()
        }
    }
}

/// Disk cache for immutable CAS content.
///
/// Stores content by hash for read-through caching from S3.
/// Content is stored as flat files with the hash as filename.
///
/// # Directory Structure
/// ```text
/// cache_dir/
/// ├── abc123def456...     # Content file (hash as filename)
/// ├── 789xyz...
/// └── ...
/// ```
pub struct ReadCache {
    /// Root directory for CAS storage.
    cache_dir: PathBuf,
    /// Current cache size in bytes (approximate).
    current_size: AtomicU64,
    /// Whether write-through is enabled.
    write_through: bool,
}

impl ReadCache {
    /// Create a new read cache.
    ///
    /// # Arguments
    /// * `options` - Cache configuration options
    ///
    /// # Returns
    /// New cache instance. Creates directory if needed.
    pub fn new(options: ReadCacheOptions) -> Result<Self, DiskCacheError> {
        std::fs::create_dir_all(&options.cache_dir)?;

        let cache = Self {
            cache_dir: options.cache_dir,
            current_size: AtomicU64::new(0),
            write_through: options.write_through,
        };

        // Scan existing files to get current size
        cache.scan()?;

        Ok(cache)
    }

    /// Get cached content by hash.
    ///
    /// # Arguments
    /// * `hash` - Content hash (used as filename)
    ///
    /// # Returns
    /// Content bytes if cached, None if not found.
    pub fn get(&self, hash: &str) -> Result<Option<Vec<u8>>, DiskCacheError> {
        let path: PathBuf = self.hash_path(hash);

        if !path.exists() {
            return Ok(None);
        }

        let data: Vec<u8> = std::fs::read(&path)?;
        Ok(Some(data))
    }

    /// Store content by hash (atomic write).
    ///
    /// Skips write if content already exists with matching size.
    ///
    /// # Arguments
    /// * `hash` - Content hash (used as filename)
    /// * `data` - Content bytes to store
    pub fn put(&self, hash: &str, data: &[u8]) -> Result<(), DiskCacheError> {
        if !self.write_through {
            return Ok(());
        }

        let path: PathBuf = self.hash_path(hash);

        // Skip if already cached with correct size
        if let Ok(metadata) = std::fs::metadata(&path) {
            if metadata.len() == data.len() as u64 {
                return Ok(());
            }
        }

        // Write atomically (temp file + rename)
        let temp_path: PathBuf = self.temp_path(hash);

        // Ensure parent directory exists
        if let Some(parent) = temp_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&temp_path, data)?;
        std::fs::rename(&temp_path, &path)?;

        // Update size tracking
        self.current_size
            .fetch_add(data.len() as u64, Ordering::Relaxed);

        Ok(())
    }

    /// Check if content is cached.
    ///
    /// # Arguments
    /// * `hash` - Content hash to check
    ///
    /// # Returns
    /// True if content exists in cache.
    pub fn contains(&self, hash: &str) -> bool {
        self.hash_path(hash).exists()
    }

    /// Check if content is cached with expected size.
    ///
    /// # Arguments
    /// * `hash` - Content hash to check
    /// * `expected_size` - Expected content size in bytes
    ///
    /// # Returns
    /// True if content exists with matching size.
    pub fn contains_with_size(&self, hash: &str, expected_size: u64) -> bool {
        let path: PathBuf = self.hash_path(hash);
        match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len() == expected_size,
            Err(_) => false,
        }
    }

    /// Remove content from cache.
    ///
    /// # Arguments
    /// * `hash` - Content hash to remove
    pub fn remove(&self, hash: &str) -> Result<(), DiskCacheError> {
        let path: PathBuf = self.hash_path(hash);

        if path.exists() {
            let size: u64 = std::fs::metadata(&path)?.len();
            std::fs::remove_file(&path)?;
            self.current_size.fetch_sub(size, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Get current cache size in bytes.
    pub fn current_size(&self) -> u64 {
        self.current_size.load(Ordering::Relaxed)
    }

    /// Get the cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Scan cache directory to rebuild size tracking.
    ///
    /// Called on startup to initialize current_size.
    fn scan(&self) -> Result<(), DiskCacheError> {
        let mut total_size: u64 = 0;

        if self.cache_dir.exists() {
            for entry in std::fs::read_dir(&self.cache_dir)? {
                let entry = entry?;
                let path: PathBuf = entry.path();

                // Skip temp files and directories
                if path.is_file() {
                    let name: String = entry.file_name().to_string_lossy().to_string();
                    if !name.ends_with(".tmp") {
                        if let Ok(metadata) = std::fs::metadata(&path) {
                            total_size += metadata.len();
                        }
                    }
                }
            }
        }

        self.current_size.store(total_size, Ordering::Relaxed);
        Ok(())
    }

    /// Get path for a hash file.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    fn hash_path(&self, hash: &str) -> PathBuf {
        self.cache_dir.join(hash)
    }

    /// Get path for a temp file during atomic write.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    fn temp_path(&self, hash: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.tmp", hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_cache() -> (ReadCache, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let options = ReadCacheOptions::with_cache_dir(temp_dir.path().to_path_buf());
        let cache = ReadCache::new(options).unwrap();
        (cache, temp_dir)
    }

    #[test]
    fn test_put_and_get() {
        let (cache, _temp) = create_test_cache();

        cache.put("hash123", b"hello world").unwrap();
        let data: Option<Vec<u8>> = cache.get("hash123").unwrap();

        assert_eq!(data, Some(b"hello world".to_vec()));
    }

    #[test]
    fn test_get_missing() {
        let (cache, _temp) = create_test_cache();

        let data: Option<Vec<u8>> = cache.get("nonexistent").unwrap();
        assert!(data.is_none());
    }

    #[test]
    fn test_contains() {
        let (cache, _temp) = create_test_cache();

        assert!(!cache.contains("hash123"));

        cache.put("hash123", b"data").unwrap();

        assert!(cache.contains("hash123"));
    }

    #[test]
    fn test_contains_with_size() {
        let (cache, _temp) = create_test_cache();

        cache.put("hash123", b"hello").unwrap();

        assert!(cache.contains_with_size("hash123", 5));
        assert!(!cache.contains_with_size("hash123", 10));
        assert!(!cache.contains_with_size("missing", 5));
    }

    #[test]
    fn test_put_skips_existing() {
        let (cache, temp) = create_test_cache();

        cache.put("hash123", b"original").unwrap();

        // Get the file's mtime
        let path: PathBuf = temp.path().join("hash123");
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

        // Small delay to ensure mtime would change if file was rewritten
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Put same size data - should skip
        cache.put("hash123", b"replaced").unwrap();

        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after);

        // Content should still be original (wasn't overwritten)
        let data: Vec<u8> = cache.get("hash123").unwrap().unwrap();
        assert_eq!(data, b"original".to_vec());
    }

    #[test]
    fn test_put_replaces_wrong_size() {
        let (cache, _temp) = create_test_cache();

        cache.put("hash123", b"short").unwrap();

        // Put different size data - should replace
        cache.put("hash123", b"longer content").unwrap();

        let data: Vec<u8> = cache.get("hash123").unwrap().unwrap();
        assert_eq!(data, b"longer content".to_vec());
    }

    #[test]
    fn test_remove() {
        let (cache, _temp) = create_test_cache();

        cache.put("hash123", b"data").unwrap();
        assert!(cache.contains("hash123"));

        cache.remove("hash123").unwrap();
        assert!(!cache.contains("hash123"));
    }

    #[test]
    fn test_remove_nonexistent() {
        let (cache, _temp) = create_test_cache();

        // Should not error
        cache.remove("nonexistent").unwrap();
    }

    #[test]
    fn test_current_size_tracking() {
        let (cache, _temp) = create_test_cache();

        assert_eq!(cache.current_size(), 0);

        cache.put("hash1", b"hello").unwrap(); // 5 bytes
        assert_eq!(cache.current_size(), 5);

        cache.put("hash2", b"world!").unwrap(); // 6 bytes
        assert_eq!(cache.current_size(), 11);

        cache.remove("hash1").unwrap();
        assert_eq!(cache.current_size(), 6);
    }

    #[test]
    fn test_scan_on_startup() {
        let temp_dir = TempDir::new().unwrap();

        // Pre-populate cache directory
        std::fs::write(temp_dir.path().join("hash1"), b"hello").unwrap();
        std::fs::write(temp_dir.path().join("hash2"), b"world!").unwrap();

        // Create cache - should scan existing files
        let options = ReadCacheOptions::with_cache_dir(temp_dir.path().to_path_buf());
        let cache = ReadCache::new(options).unwrap();

        assert_eq!(cache.current_size(), 11);
        assert!(cache.contains("hash1"));
        assert!(cache.contains("hash2"));
    }

    #[test]
    fn test_write_through_disabled() {
        let temp_dir = TempDir::new().unwrap();
        let options = ReadCacheOptions {
            cache_dir: temp_dir.path().to_path_buf(),
            write_through: false,
        };
        let cache = ReadCache::new(options).unwrap();

        cache.put("hash123", b"data").unwrap();

        // Should not be cached when write_through is disabled
        assert!(!cache.contains("hash123"));
    }

    #[test]
    fn test_empty_file() {
        let (cache, _temp) = create_test_cache();

        cache.put("empty", b"").unwrap();

        let data: Option<Vec<u8>> = cache.get("empty").unwrap();
        assert_eq!(data, Some(Vec::new()));
        assert!(cache.contains_with_size("empty", 0));
    }
}
