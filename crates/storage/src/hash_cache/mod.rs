//! Hash cache for avoiding redundant file hashing.
//!
//! Computing XXH128 hashes for large files is expensive. This module provides
//! a caching layer with a pluggable backend interface.

mod backend;
mod entry;
mod error;
mod sqlite;

pub use backend::HashCacheBackend;
pub use entry::{HashCacheEntry, HashCacheKey};
pub use error::HashCacheError;
pub use sqlite::SqliteHashCache;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rusty_attachments_model::HashAlgorithm;

/// Default TTL for hash cache entries (14 days).
pub const DEFAULT_HASH_CACHE_TTL_DAYS: u32 = 14;

/// Hash cache with configurable backend and TTL.
pub struct HashCache {
    /// The storage backend.
    backend: Box<dyn HashCacheBackend>,
    /// Time-to-live in days.
    ttl_days: u32,
    /// Hash algorithm used.
    hash_alg: HashAlgorithm,
}

impl HashCache {
    /// Create a new hash cache with the given backend and TTL.
    ///
    /// # Arguments
    /// * `backend` - Storage backend implementation
    /// * `ttl_days` - Time-to-live for cache entries in days
    pub fn new(backend: impl HashCacheBackend + 'static, ttl_days: u32) -> Self {
        Self {
            backend: Box::new(backend),
            ttl_days,
            hash_alg: HashAlgorithm::Xxh128,
        }
    }

    /// Create with default TTL (14 days).
    ///
    /// # Arguments
    /// * `backend` - Storage backend implementation
    pub fn with_default_ttl(backend: impl HashCacheBackend + 'static) -> Self {
        Self::new(backend, DEFAULT_HASH_CACHE_TTL_DAYS)
    }

    /// Look up a cached hash.
    ///
    /// # Arguments
    /// * `path` - Relative file path
    /// * `size` - File size in bytes
    /// * `mtime` - Modification time in microseconds since epoch
    ///
    /// # Returns
    /// The cached hash if found and not expired, None otherwise.
    pub async fn get(&self, path: &str, size: u64, mtime: i64) -> Option<String> {
        let key: HashCacheKey = HashCacheKey {
            path: path.to_string(),
            size,
            mtime,
        };
        self.backend.get(&key).await.map(|e| e.hash)
    }

    /// Store a computed hash.
    ///
    /// # Arguments
    /// * `path` - Relative file path
    /// * `size` - File size in bytes
    /// * `mtime` - Modification time in microseconds since epoch
    /// * `hash` - The computed hash value
    pub async fn put(&self, path: &str, size: u64, mtime: i64, hash: String) {
        let key: HashCacheKey = HashCacheKey {
            path: path.to_string(),
            size,
            mtime,
        };
        let now: i64 = current_epoch_seconds();
        let entry: HashCacheEntry = HashCacheEntry {
            hash,
            hash_alg: self.hash_alg,
            created_at: now,
            expires_at: now + (self.ttl_days as i64 * 86400),
        };
        self.backend.put(&key, &entry).await;
    }

    /// Remove a cached entry.
    ///
    /// # Arguments
    /// * `path` - Relative file path
    /// * `size` - File size in bytes
    /// * `mtime` - Modification time in microseconds since epoch
    pub async fn delete(&self, path: &str, size: u64, mtime: i64) {
        let key: HashCacheKey = HashCacheKey {
            path: path.to_string(),
            size,
            mtime,
        };
        self.backend.delete(&key).await;
    }

    /// Bulk lookup - returns map of path -> hash for found entries.
    ///
    /// # Arguments
    /// * `keys` - Slice of (path, size, mtime) tuples
    ///
    /// # Returns
    /// Map of path to hash for entries found in cache.
    pub async fn get_batch(&self, keys: &[(String, u64, i64)]) -> HashMap<String, String> {
        let cache_keys: Vec<HashCacheKey> = keys
            .iter()
            .map(|(path, size, mtime)| HashCacheKey {
                path: path.clone(),
                size: *size,
                mtime: *mtime,
            })
            .collect();

        self.backend
            .get_batch(&cache_keys)
            .await
            .into_iter()
            .map(|(k, v)| (k.path, v.hash))
            .collect()
    }

    /// Bulk store for multiple entries.
    ///
    /// # Arguments
    /// * `entries` - Slice of (path, size, mtime, hash) tuples
    pub async fn put_batch(&self, entries: &[(String, u64, i64, String)]) {
        let now: i64 = current_epoch_seconds();
        let cache_entries: Vec<(HashCacheKey, HashCacheEntry)> = entries
            .iter()
            .map(|(path, size, mtime, hash)| {
                let key: HashCacheKey = HashCacheKey {
                    path: path.clone(),
                    size: *size,
                    mtime: *mtime,
                };
                let entry: HashCacheEntry = HashCacheEntry {
                    hash: hash.clone(),
                    hash_alg: self.hash_alg,
                    created_at: now,
                    expires_at: now + (self.ttl_days as i64 * 86400),
                };
                (key, entry)
            })
            .collect();

        self.backend.put_batch(&cache_entries).await;
    }

    /// Get the configured TTL in days.
    pub fn ttl_days(&self) -> u32 {
        self.ttl_days
    }

    /// Get the hash algorithm used.
    pub fn hash_algorithm(&self) -> HashAlgorithm {
        self.hash_alg
    }
}

/// Get current time as epoch seconds.
fn current_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory backend for testing.
    struct InMemoryBackend {
        data: Mutex<HashMap<HashCacheKey, HashCacheEntry>>,
    }

    impl InMemoryBackend {
        fn new() -> Self {
            Self {
                data: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl HashCacheBackend for InMemoryBackend {
        async fn get(&self, key: &HashCacheKey) -> Option<HashCacheEntry> {
            let data = self.data.lock().unwrap();
            let entry: Option<&HashCacheEntry> = data.get(key);
            entry
                .cloned()
                .filter(|e| e.expires_at > current_epoch_seconds())
        }

        async fn put(&self, key: &HashCacheKey, entry: &HashCacheEntry) {
            let mut data = self.data.lock().unwrap();
            data.insert(key.clone(), entry.clone());
        }

        async fn delete(&self, key: &HashCacheKey) {
            let mut data = self.data.lock().unwrap();
            data.remove(key);
        }

        async fn clear(&self) {
            let mut data = self.data.lock().unwrap();
            data.clear();
        }
    }

    #[tokio::test]
    async fn test_cache_miss() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: HashCache = HashCache::with_default_ttl(backend);

        let result: Option<String> = cache.get("test.txt", 100, 1234567890).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cache_hit() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: HashCache = HashCache::with_default_ttl(backend);

        cache
            .put("test.txt", 100, 1234567890, "abc123".to_string())
            .await;
        let result: Option<String> = cache.get("test.txt", 100, 1234567890).await;
        assert_eq!(result, Some("abc123".to_string()));
    }

    #[tokio::test]
    async fn test_cache_miss_different_size() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: HashCache = HashCache::with_default_ttl(backend);

        cache
            .put("test.txt", 100, 1234567890, "abc123".to_string())
            .await;
        let result: Option<String> = cache.get("test.txt", 200, 1234567890).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cache_miss_different_mtime() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: HashCache = HashCache::with_default_ttl(backend);

        cache
            .put("test.txt", 100, 1234567890, "abc123".to_string())
            .await;
        let result: Option<String> = cache.get("test.txt", 100, 9999999999).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cache_delete() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: HashCache = HashCache::with_default_ttl(backend);

        cache
            .put("test.txt", 100, 1234567890, "abc123".to_string())
            .await;
        cache.delete("test.txt", 100, 1234567890).await;
        let result: Option<String> = cache.get("test.txt", 100, 1234567890).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_batch_operations() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: HashCache = HashCache::with_default_ttl(backend);

        let entries: Vec<(String, u64, i64, String)> = vec![
            ("file1.txt".to_string(), 100, 1000, "hash1".to_string()),
            ("file2.txt".to_string(), 200, 2000, "hash2".to_string()),
            ("file3.txt".to_string(), 300, 3000, "hash3".to_string()),
        ];
        cache.put_batch(&entries).await;

        let keys: Vec<(String, u64, i64)> = vec![
            ("file1.txt".to_string(), 100, 1000),
            ("file2.txt".to_string(), 200, 2000),
            ("file4.txt".to_string(), 400, 4000), // Not in cache
        ];
        let results: HashMap<String, String> = cache.get_batch(&keys).await;

        assert_eq!(results.len(), 2);
        assert_eq!(results.get("file1.txt"), Some(&"hash1".to_string()));
        assert_eq!(results.get("file2.txt"), Some(&"hash2".to_string()));
        assert!(results.get("file4.txt").is_none());
    }

    #[tokio::test]
    async fn test_ttl_configuration() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: HashCache = HashCache::new(backend, 7);

        assert_eq!(cache.ttl_days(), 7);
        assert_eq!(cache.hash_algorithm(), HashAlgorithm::Xxh128);
    }
}
