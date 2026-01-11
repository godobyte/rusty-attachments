//! Hash cache entry and key types.

use rusty_attachments_model::HashAlgorithm;

/// Key for looking up a cached hash.
///
/// Matches the file identity fields from ManifestFilePath.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HashCacheKey {
    /// Relative file path (matches ManifestFilePath.path).
    pub path: String,
    /// File size in bytes (matches ManifestFilePath.size).
    pub size: u64,
    /// Modification time in microseconds since epoch (matches ManifestFilePath.mtime).
    pub mtime: i64,
}

impl HashCacheKey {
    /// Create a new hash cache key.
    ///
    /// # Arguments
    /// * `path` - Relative file path
    /// * `size` - File size in bytes
    /// * `mtime` - Modification time in microseconds since epoch
    pub fn new(path: impl Into<String>, size: u64, mtime: i64) -> Self {
        Self {
            path: path.into(),
            size,
            mtime,
        }
    }
}

/// Cached hash entry.
#[derive(Debug, Clone)]
pub struct HashCacheEntry {
    /// The computed hash value.
    pub hash: String,
    /// Hash algorithm used (for future-proofing).
    pub hash_alg: HashAlgorithm,
    /// When this entry was created (epoch seconds).
    pub created_at: i64,
    /// When this entry expires (epoch seconds).
    pub expires_at: i64,
}

impl HashCacheEntry {
    /// Create a new hash cache entry.
    ///
    /// # Arguments
    /// * `hash` - The computed hash value
    /// * `hash_alg` - Hash algorithm used
    /// * `created_at` - Creation time (epoch seconds)
    /// * `expires_at` - Expiration time (epoch seconds)
    pub fn new(
        hash: impl Into<String>,
        hash_alg: HashAlgorithm,
        created_at: i64,
        expires_at: i64,
    ) -> Self {
        Self {
            hash: hash.into(),
            hash_alg,
            created_at,
            expires_at,
        }
    }

    /// Check if this entry has expired.
    ///
    /// # Arguments
    /// * `now` - Current time in epoch seconds
    pub fn is_expired(&self, now: i64) -> bool {
        now >= self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_cache_key_new() {
        let key: HashCacheKey = HashCacheKey::new("test/file.txt", 1024, 1234567890);
        assert_eq!(key.path, "test/file.txt");
        assert_eq!(key.size, 1024);
        assert_eq!(key.mtime, 1234567890);
    }

    #[test]
    fn test_hash_cache_key_equality() {
        let key1: HashCacheKey = HashCacheKey::new("file.txt", 100, 1000);
        let key2: HashCacheKey = HashCacheKey::new("file.txt", 100, 1000);
        let key3: HashCacheKey = HashCacheKey::new("file.txt", 100, 2000);

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_hash_cache_entry_new() {
        let entry: HashCacheEntry =
            HashCacheEntry::new("abc123", HashAlgorithm::Xxh128, 1000, 2000);
        assert_eq!(entry.hash, "abc123");
        assert_eq!(entry.hash_alg, HashAlgorithm::Xxh128);
        assert_eq!(entry.created_at, 1000);
        assert_eq!(entry.expires_at, 2000);
    }

    #[test]
    fn test_hash_cache_entry_is_expired() {
        let entry: HashCacheEntry =
            HashCacheEntry::new("abc123", HashAlgorithm::Xxh128, 1000, 2000);

        assert!(!entry.is_expired(1500)); // Before expiry
        assert!(entry.is_expired(2000)); // At expiry
        assert!(entry.is_expired(2500)); // After expiry
    }
}
