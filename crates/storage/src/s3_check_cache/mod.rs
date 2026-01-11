//! S3 check cache for avoiding redundant HEAD requests.
//!
//! The S3 Check Cache tracks which CAS objects have already been uploaded to S3.
//! This avoids redundant HEAD requests to check object existence before upload.
//!
//! Key distinction:
//! - Hash Cache: `(path, size, mtime) → hash` - avoids re-hashing local files
//! - S3 Check Cache: `s3_key → last_seen_time` - avoids HEAD requests to S3

mod backend;
mod entry;
mod error;
mod sqlite;

pub use backend::S3CheckCacheBackend;
pub use entry::{S3CheckCacheEntry, S3CheckCacheKey};
pub use error::S3CheckCacheError;
pub use sqlite::SqliteS3CheckCache;

use std::time::{SystemTime, UNIX_EPOCH};

/// S3 check cache with configurable backend.
pub struct S3CheckCache {
    /// The storage backend.
    backend: Box<dyn S3CheckCacheBackend>,
}

impl S3CheckCache {
    /// Create a new S3 check cache with the given backend.
    ///
    /// # Arguments
    /// * `backend` - Storage backend implementation
    pub fn new(backend: impl S3CheckCacheBackend + 'static) -> Self {
        Self {
            backend: Box::new(backend),
        }
    }

    /// Check if an S3 object was previously uploaded.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `key` - S3 object key
    ///
    /// # Returns
    /// `true` if the object was previously seen as uploaded.
    pub async fn exists(&self, bucket: &str, key: &str) -> bool {
        let cache_key: S3CheckCacheKey = S3CheckCacheKey {
            s3_key: format!("{}/{}", bucket, key),
        };
        self.backend.get(&cache_key).await.is_some()
    }

    /// Record that an S3 object exists.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `key` - S3 object key
    pub async fn mark_uploaded(&self, bucket: &str, key: &str) {
        let now: String = current_epoch_seconds().to_string();
        let entry: S3CheckCacheEntry = S3CheckCacheEntry {
            s3_key: format!("{}/{}", bucket, key),
            last_seen_time: now,
        };
        self.backend.put(&entry).await;
    }

    /// Remove an entry from the cache.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `key` - S3 object key
    pub async fn remove(&self, bucket: &str, key: &str) {
        let cache_key: S3CheckCacheKey = S3CheckCacheKey {
            s3_key: format!("{}/{}", bucket, key),
        };
        self.backend.delete(&cache_key).await;
    }

    /// Reset the entire cache (used when integrity check fails).
    pub async fn reset(&self) {
        self.backend.clear().await;
    }

    /// Get the last seen time for an S3 object.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `key` - S3 object key
    ///
    /// # Returns
    /// The last seen time as a string, or None if not in cache.
    pub async fn get_last_seen(&self, bucket: &str, key: &str) -> Option<String> {
        let cache_key: S3CheckCacheKey = S3CheckCacheKey {
            s3_key: format!("{}/{}", bucket, key),
        };
        self.backend.get(&cache_key).await.map(|e| e.last_seen_time)
    }

    /// Bulk check for multiple S3 keys.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `keys` - Slice of S3 object keys
    ///
    /// # Returns
    /// Vector of keys that exist in the cache.
    pub async fn exists_batch(&self, bucket: &str, keys: &[&str]) -> Vec<String> {
        let cache_keys: Vec<S3CheckCacheKey> = keys
            .iter()
            .map(|key| S3CheckCacheKey {
                s3_key: format!("{}/{}", bucket, key),
            })
            .collect();

        self.backend
            .get_batch(&cache_keys)
            .await
            .into_iter()
            .map(|e| e.s3_key)
            .collect()
    }

    /// Bulk mark multiple S3 keys as uploaded.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `keys` - Slice of S3 object keys
    pub async fn mark_uploaded_batch(&self, bucket: &str, keys: &[&str]) {
        let now: String = current_epoch_seconds().to_string();
        let entries: Vec<S3CheckCacheEntry> = keys
            .iter()
            .map(|key| S3CheckCacheEntry {
                s3_key: format!("{}/{}", bucket, key),
                last_seen_time: now.clone(),
            })
            .collect();

        self.backend.put_batch(&entries).await;
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
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory backend for testing.
    struct InMemoryBackend {
        data: Mutex<HashMap<String, S3CheckCacheEntry>>,
    }

    impl InMemoryBackend {
        fn new() -> Self {
            Self {
                data: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl S3CheckCacheBackend for InMemoryBackend {
        async fn get(&self, key: &S3CheckCacheKey) -> Option<S3CheckCacheEntry> {
            let data = self.data.lock().unwrap();
            data.get(&key.s3_key).cloned()
        }

        async fn put(&self, entry: &S3CheckCacheEntry) {
            let mut data = self.data.lock().unwrap();
            data.insert(entry.s3_key.clone(), entry.clone());
        }

        async fn delete(&self, key: &S3CheckCacheKey) {
            let mut data = self.data.lock().unwrap();
            data.remove(&key.s3_key);
        }

        async fn clear(&self) {
            let mut data = self.data.lock().unwrap();
            data.clear();
        }
    }

    #[tokio::test]
    async fn test_cache_miss() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: S3CheckCache = S3CheckCache::new(backend);

        let result: bool = cache.exists("my-bucket", "Data/abc123.xxh128").await;
        assert!(!result);
    }

    #[tokio::test]
    async fn test_cache_hit() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: S3CheckCache = S3CheckCache::new(backend);

        cache.mark_uploaded("my-bucket", "Data/abc123.xxh128").await;
        let result: bool = cache.exists("my-bucket", "Data/abc123.xxh128").await;
        assert!(result);
    }

    #[tokio::test]
    async fn test_cache_remove() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: S3CheckCache = S3CheckCache::new(backend);

        cache.mark_uploaded("my-bucket", "Data/abc123.xxh128").await;
        cache.remove("my-bucket", "Data/abc123.xxh128").await;
        let result: bool = cache.exists("my-bucket", "Data/abc123.xxh128").await;
        assert!(!result);
    }

    #[tokio::test]
    async fn test_cache_reset() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: S3CheckCache = S3CheckCache::new(backend);

        cache.mark_uploaded("my-bucket", "Data/abc123.xxh128").await;
        cache.mark_uploaded("my-bucket", "Data/def456.xxh128").await;
        cache.reset().await;

        assert!(!cache.exists("my-bucket", "Data/abc123.xxh128").await);
        assert!(!cache.exists("my-bucket", "Data/def456.xxh128").await);
    }

    #[tokio::test]
    async fn test_get_last_seen() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: S3CheckCache = S3CheckCache::new(backend);

        cache.mark_uploaded("my-bucket", "Data/abc123.xxh128").await;
        let last_seen: Option<String> =
            cache.get_last_seen("my-bucket", "Data/abc123.xxh128").await;
        assert!(last_seen.is_some());

        // Should be a valid epoch timestamp
        let timestamp: i64 = last_seen.unwrap().parse().unwrap();
        assert!(timestamp > 0);
    }

    #[tokio::test]
    async fn test_batch_operations() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: S3CheckCache = S3CheckCache::new(backend);

        let keys: Vec<&str> = vec![
            "Data/hash1.xxh128",
            "Data/hash2.xxh128",
            "Data/hash3.xxh128",
        ];
        cache.mark_uploaded_batch("my-bucket", &keys).await;

        // Check individual keys
        assert!(cache.exists("my-bucket", "Data/hash1.xxh128").await);
        assert!(cache.exists("my-bucket", "Data/hash2.xxh128").await);
        assert!(cache.exists("my-bucket", "Data/hash3.xxh128").await);
        assert!(!cache.exists("my-bucket", "Data/hash4.xxh128").await);

        // Check batch
        let check_keys: Vec<&str> = vec![
            "Data/hash1.xxh128",
            "Data/hash2.xxh128",
            "Data/hash4.xxh128", // Not in cache
        ];
        let found: Vec<String> = cache.exists_batch("my-bucket", &check_keys).await;
        assert_eq!(found.len(), 2);
    }

    #[tokio::test]
    async fn test_different_buckets() {
        let backend: InMemoryBackend = InMemoryBackend::new();
        let cache: S3CheckCache = S3CheckCache::new(backend);

        cache.mark_uploaded("bucket-a", "Data/abc123.xxh128").await;

        assert!(cache.exists("bucket-a", "Data/abc123.xxh128").await);
        assert!(!cache.exists("bucket-b", "Data/abc123.xxh128").await);
    }
}
