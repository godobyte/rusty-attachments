//! S3 check cache entry and key types.

/// Key for looking up a cached S3 existence check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct S3CheckCacheKey {
    /// Full S3 key: "{bucket}/{key}".
    pub s3_key: String,
}

impl S3CheckCacheKey {
    /// Create a new S3 check cache key.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `key` - S3 object key
    pub fn new(bucket: &str, key: &str) -> Self {
        Self {
            s3_key: format!("{}/{}", bucket, key),
        }
    }

    /// Create from a full S3 key string.
    ///
    /// # Arguments
    /// * `s3_key` - Full S3 key in format "{bucket}/{key}"
    pub fn from_s3_key(s3_key: impl Into<String>) -> Self {
        Self {
            s3_key: s3_key.into(),
        }
    }

    /// Extract the bucket name from the S3 key.
    ///
    /// # Returns
    /// The bucket name, or None if the key format is invalid.
    pub fn bucket(&self) -> Option<&str> {
        self.s3_key.split('/').next()
    }

    /// Extract the object key from the S3 key.
    ///
    /// # Returns
    /// The object key (everything after the first '/'), or None if invalid.
    pub fn key(&self) -> Option<&str> {
        self.s3_key.split_once('/').map(|(_, k)| k)
    }
}

/// Cached S3 existence entry.
#[derive(Debug, Clone)]
pub struct S3CheckCacheEntry {
    /// Full S3 key: "{bucket}/{key}".
    pub s3_key: String,
    /// When this entry was last verified (epoch seconds as string).
    pub last_seen_time: String,
}

impl S3CheckCacheEntry {
    /// Create a new S3 check cache entry.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `key` - S3 object key
    /// * `last_seen_time` - Last seen time as epoch seconds string
    pub fn new(bucket: &str, key: &str, last_seen_time: impl Into<String>) -> Self {
        Self {
            s3_key: format!("{}/{}", bucket, key),
            last_seen_time: last_seen_time.into(),
        }
    }

    /// Create from a full S3 key string.
    ///
    /// # Arguments
    /// * `s3_key` - Full S3 key in format "{bucket}/{key}"
    /// * `last_seen_time` - Last seen time as epoch seconds string
    pub fn from_s3_key(s3_key: impl Into<String>, last_seen_time: impl Into<String>) -> Self {
        Self {
            s3_key: s3_key.into(),
            last_seen_time: last_seen_time.into(),
        }
    }

    /// Get the last seen time as an i64 timestamp.
    ///
    /// # Returns
    /// The timestamp, or None if parsing fails.
    pub fn last_seen_timestamp(&self) -> Option<i64> {
        self.last_seen_time.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s3_check_cache_key_new() {
        let key: S3CheckCacheKey = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");
        assert_eq!(key.s3_key, "my-bucket/Data/abc123.xxh128");
    }

    #[test]
    fn test_s3_check_cache_key_from_s3_key() {
        let key: S3CheckCacheKey = S3CheckCacheKey::from_s3_key("my-bucket/Data/abc123.xxh128");
        assert_eq!(key.s3_key, "my-bucket/Data/abc123.xxh128");
    }

    #[test]
    fn test_s3_check_cache_key_bucket() {
        let key: S3CheckCacheKey = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");
        assert_eq!(key.bucket(), Some("my-bucket"));
    }

    #[test]
    fn test_s3_check_cache_key_key() {
        let key: S3CheckCacheKey = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");
        assert_eq!(key.key(), Some("Data/abc123.xxh128"));
    }

    #[test]
    fn test_s3_check_cache_key_equality() {
        let key1: S3CheckCacheKey = S3CheckCacheKey::new("bucket", "key");
        let key2: S3CheckCacheKey = S3CheckCacheKey::new("bucket", "key");
        let key3: S3CheckCacheKey = S3CheckCacheKey::new("bucket", "other");

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_s3_check_cache_entry_new() {
        let entry: S3CheckCacheEntry =
            S3CheckCacheEntry::new("my-bucket", "Data/abc.xxh128", "1234567890");
        assert_eq!(entry.s3_key, "my-bucket/Data/abc.xxh128");
        assert_eq!(entry.last_seen_time, "1234567890");
    }

    #[test]
    fn test_s3_check_cache_entry_from_s3_key() {
        let entry: S3CheckCacheEntry =
            S3CheckCacheEntry::from_s3_key("my-bucket/Data/abc.xxh128", "1234567890");
        assert_eq!(entry.s3_key, "my-bucket/Data/abc.xxh128");
        assert_eq!(entry.last_seen_time, "1234567890");
    }

    #[test]
    fn test_s3_check_cache_entry_last_seen_timestamp() {
        let entry: S3CheckCacheEntry = S3CheckCacheEntry::new("bucket", "key", "1234567890");
        assert_eq!(entry.last_seen_timestamp(), Some(1234567890));

        let invalid_entry: S3CheckCacheEntry =
            S3CheckCacheEntry::new("bucket", "key", "not-a-number");
        assert_eq!(invalid_entry.last_seen_timestamp(), None);
    }
}
