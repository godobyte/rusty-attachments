//! SQLite backend for S3 check cache.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use super::backend::S3CheckCacheBackend;
use super::entry::{S3CheckCacheEntry, S3CheckCacheKey};
use super::error::S3CheckCacheError;

/// SQLite-based S3 check cache backend.
///
/// Stores S3 existence checks in a local SQLite database for persistence across sessions.
/// Uses WAL mode for better concurrent read performance.
pub struct SqliteS3CheckCache {
    /// Database connection (protected by mutex for thread safety).
    conn: Mutex<Connection>,
    /// Table name (versioned for schema migrations).
    table_name: String,
    /// Entry expiry in days.
    entry_expiry_days: u32,
}

impl SqliteS3CheckCache {
    /// Database schema version.
    const CACHE_DB_VERSION: u32 = 1;

    /// Default entry expiry (30 days).
    const DEFAULT_ENTRY_EXPIRY_DAYS: u32 = 30;

    /// Create or open a SQLite S3 check cache at the given path.
    ///
    /// # Arguments
    /// * `db_path` - Path to the SQLite database file
    ///
    /// # Returns
    /// A new SqliteS3CheckCache instance.
    ///
    /// # Errors
    /// Returns error if database cannot be opened or initialized.
    pub fn open(db_path: &Path) -> Result<Self, S3CheckCacheError> {
        Self::open_with_expiry(db_path, Self::DEFAULT_ENTRY_EXPIRY_DAYS)
    }

    /// Create or open a SQLite S3 check cache with custom expiry.
    ///
    /// # Arguments
    /// * `db_path` - Path to the SQLite database file
    /// * `entry_expiry_days` - Number of days before entries expire
    ///
    /// # Returns
    /// A new SqliteS3CheckCache instance.
    ///
    /// # Errors
    /// Returns error if database cannot be opened or initialized.
    pub fn open_with_expiry(
        db_path: &Path,
        entry_expiry_days: u32,
    ) -> Result<Self, S3CheckCacheError> {
        let conn: Connection =
            Connection::open(db_path).map_err(|e| S3CheckCacheError::Sqlite(e.to_string()))?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| S3CheckCacheError::Sqlite(e.to_string()))?;

        // Set busy timeout to handle concurrent access
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| S3CheckCacheError::Sqlite(e.to_string()))?;

        let table_name: String = format!("s3_check_cache_v{}", Self::CACHE_DB_VERSION);

        // Create table if it doesn't exist
        let create_sql: String = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                s3_key TEXT PRIMARY KEY,
                last_seen_time TEXT NOT NULL
            )",
            table_name
        );
        conn.execute(&create_sql, [])
            .map_err(|e| S3CheckCacheError::Sqlite(e.to_string()))?;

        Ok(Self {
            conn: Mutex::new(conn),
            table_name,
            entry_expiry_days,
        })
    }

    /// Clean up expired entries from the cache.
    ///
    /// # Returns
    /// Number of entries deleted.
    pub fn cleanup_expired(&self) -> Result<usize, S3CheckCacheError> {
        let now: i64 = current_epoch_seconds();
        let expiry_threshold: i64 = now - (self.entry_expiry_days as i64 * 86400);

        let conn = self.conn.lock().unwrap();
        let deleted: usize = conn
            .execute(
                &format!(
                    "DELETE FROM {} WHERE CAST(last_seen_time AS INTEGER) < ?",
                    self.table_name
                ),
                params![expiry_threshold],
            )
            .map_err(|e| S3CheckCacheError::Sqlite(e.to_string()))?;
        Ok(deleted)
    }

    /// Get the number of entries in the cache.
    pub fn count(&self) -> Result<usize, S3CheckCacheError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {}", self.table_name),
                [],
                |row| row.get(0),
            )
            .map_err(|e| S3CheckCacheError::Sqlite(e.to_string()))?;
        Ok(count as usize)
    }

    /// Check if an entry is expired.
    ///
    /// # Arguments
    /// * `last_seen_time` - Last seen time as string (epoch seconds)
    ///
    /// # Returns
    /// `true` if the entry is expired.
    fn is_expired(&self, last_seen_time: &str) -> bool {
        let now: i64 = current_epoch_seconds();
        let expiry_threshold: i64 = now - (self.entry_expiry_days as i64 * 86400);

        match last_seen_time.parse::<i64>() {
            Ok(timestamp) => timestamp < expiry_threshold,
            Err(_) => {
                log::warn!("Invalid timestamp in S3 check cache: {}", last_seen_time);
                true // Treat invalid timestamps as expired
            }
        }
    }
}

#[async_trait::async_trait]
impl S3CheckCacheBackend for SqliteS3CheckCache {
    async fn get(&self, key: &S3CheckCacheKey) -> Option<S3CheckCacheEntry> {
        let conn = self.conn.lock().unwrap();

        let result = conn.query_row(
            &format!(
                "SELECT s3_key, last_seen_time FROM {} WHERE s3_key = ?",
                self.table_name
            ),
            params![key.s3_key],
            |row| {
                let s3_key: String = row.get(0)?;
                let last_seen_time: String = row.get(1)?;
                Ok(S3CheckCacheEntry {
                    s3_key,
                    last_seen_time,
                })
            },
        );

        match result {
            Ok(entry) => {
                // Check if entry is expired
                if self.is_expired(&entry.last_seen_time) {
                    None
                } else {
                    Some(entry)
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                log::warn!("S3 check cache get error: {}", e);
                None
            }
        }
    }

    async fn put(&self, entry: &S3CheckCacheEntry) {
        let conn = self.conn.lock().unwrap();

        let result = conn.execute(
            &format!(
                "INSERT OR REPLACE INTO {} (s3_key, last_seen_time) VALUES (?, ?)",
                self.table_name
            ),
            params![entry.s3_key, entry.last_seen_time],
        );

        if let Err(e) = result {
            log::warn!("S3 check cache put error: {}", e);
        }
    }

    async fn delete(&self, key: &S3CheckCacheKey) {
        let conn = self.conn.lock().unwrap();

        let result = conn.execute(
            &format!("DELETE FROM {} WHERE s3_key = ?", self.table_name),
            params![key.s3_key],
        );

        if let Err(e) = result {
            log::warn!("S3 check cache delete error: {}", e);
        }
    }

    async fn clear(&self) {
        let conn = self.conn.lock().unwrap();
        let result = conn.execute(&format!("DELETE FROM {}", self.table_name), []);
        if let Err(e) = result {
            log::warn!("S3 check cache clear error: {}", e);
        }
    }

    async fn get_batch(&self, keys: &[S3CheckCacheKey]) -> Vec<S3CheckCacheEntry> {
        let mut results: Vec<S3CheckCacheEntry> = Vec::new();
        for key in keys {
            if let Some(entry) = self.get(key).await {
                results.push(entry);
            }
        }
        results
    }

    async fn put_batch(&self, entries: &[S3CheckCacheEntry]) {
        let conn = self.conn.lock().unwrap();

        // Use a transaction for batch inserts
        let tx = match conn.unchecked_transaction() {
            Ok(tx) => tx,
            Err(e) => {
                log::warn!("S3 check cache batch put transaction error: {}", e);
                return;
            }
        };

        for entry in entries {
            let result = tx.execute(
                &format!(
                    "INSERT OR REPLACE INTO {} (s3_key, last_seen_time) VALUES (?, ?)",
                    self.table_name
                ),
                params![entry.s3_key, entry.last_seen_time],
            );

            if let Err(e) = result {
                log::warn!("S3 check cache batch put error: {}", e);
            }
        }

        if let Err(e) = tx.commit() {
            log::warn!("S3 check cache batch put commit error: {}", e);
        }
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
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_basic() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open(&db_path).unwrap();

        // Test cache miss
        let key = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");
        assert!(cache.get(&key).await.is_none());

        // Test put and get
        let entry = S3CheckCacheEntry::new(
            "my-bucket",
            "Data/abc123.xxh128",
            current_epoch_seconds().to_string(),
        );
        cache.put(&entry).await;

        let retrieved = cache.get(&key).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().s3_key, "my-bucket/Data/abc123.xxh128");
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_expired_entry() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open_with_expiry(&db_path, 30).unwrap();

        let key = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");

        // Put an already-expired entry (31 days ago)
        let old_timestamp = current_epoch_seconds() - (31 * 86400);
        let entry =
            S3CheckCacheEntry::new("my-bucket", "Data/abc123.xxh128", old_timestamp.to_string());
        cache.put(&entry).await;

        // Should not be returned (expired)
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_not_expired() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open_with_expiry(&db_path, 30).unwrap();

        let key = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");

        // Put a recent entry (1 day ago)
        let recent_timestamp = current_epoch_seconds() - 86400;
        let entry = S3CheckCacheEntry::new(
            "my-bucket",
            "Data/abc123.xxh128",
            recent_timestamp.to_string(),
        );
        cache.put(&entry).await;

        // Should be returned (not expired)
        assert!(cache.get(&key).await.is_some());
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_delete() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open(&db_path).unwrap();

        let key = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");
        let entry = S3CheckCacheEntry::new(
            "my-bucket",
            "Data/abc123.xxh128",
            current_epoch_seconds().to_string(),
        );
        cache.put(&entry).await;
        assert!(cache.get(&key).await.is_some());

        cache.delete(&key).await;
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_different_buckets() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open(&db_path).unwrap();

        let entry1 = S3CheckCacheEntry::new(
            "bucket-a",
            "Data/abc123.xxh128",
            current_epoch_seconds().to_string(),
        );
        cache.put(&entry1).await;

        let key_a = S3CheckCacheKey::new("bucket-a", "Data/abc123.xxh128");
        let key_b = S3CheckCacheKey::new("bucket-b", "Data/abc123.xxh128");

        assert!(cache.get(&key_a).await.is_some());
        assert!(cache.get(&key_b).await.is_none());
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_batch_operations() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open(&db_path).unwrap();

        let now = current_epoch_seconds().to_string();
        let entries: Vec<S3CheckCacheEntry> = (0..10)
            .map(|i| {
                S3CheckCacheEntry::new("my-bucket", &format!("Data/hash{}.xxh128", i), now.clone())
            })
            .collect();

        cache.put_batch(&entries).await;

        // Verify all entries were stored
        let keys: Vec<S3CheckCacheKey> = (0..10)
            .map(|i| S3CheckCacheKey::new("my-bucket", &format!("Data/hash{}.xxh128", i)))
            .collect();
        let results = cache.get_batch(&keys).await;
        assert_eq!(results.len(), 10);
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_cleanup_expired() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open_with_expiry(&db_path, 30).unwrap();

        let now = current_epoch_seconds();
        let old_timestamp = (now - (31 * 86400)).to_string();
        let recent_timestamp = (now - 86400).to_string();

        // Add some expired entries
        for i in 0..5 {
            let entry = S3CheckCacheEntry::new(
                "my-bucket",
                &format!("Data/expired{}.xxh128", i),
                old_timestamp.clone(),
            );
            cache.put(&entry).await;
        }

        // Add some valid entries
        for i in 0..5 {
            let entry = S3CheckCacheEntry::new(
                "my-bucket",
                &format!("Data/valid{}.xxh128", i),
                recent_timestamp.clone(),
            );
            cache.put(&entry).await;
        }

        assert_eq!(cache.count().unwrap(), 10);

        let deleted = cache.cleanup_expired().unwrap();
        assert_eq!(deleted, 5);
        assert_eq!(cache.count().unwrap(), 5);
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_clear() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open(&db_path).unwrap();

        let now = current_epoch_seconds().to_string();
        for i in 0..10 {
            let entry =
                S3CheckCacheEntry::new("my-bucket", &format!("Data/hash{}.xxh128", i), now.clone());
            cache.put(&entry).await;
        }

        assert_eq!(cache.count().unwrap(), 10);

        cache.clear().await;
        assert_eq!(cache.count().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_persistence() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");

        let key = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");
        let now = current_epoch_seconds().to_string();

        // Create cache, add entry, drop it
        {
            let cache = SqliteS3CheckCache::open(&db_path).unwrap();
            let entry = S3CheckCacheEntry::new("my-bucket", "Data/abc123.xxh128", now.clone());
            cache.put(&entry).await;
        }

        // Reopen and verify entry persists
        {
            let cache = SqliteS3CheckCache::open(&db_path).unwrap();
            let retrieved = cache.get(&key).await;
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().s3_key, "my-bucket/Data/abc123.xxh128");
        }
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_invalid_timestamp() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open(&db_path).unwrap();

        let key = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");

        // Put entry with invalid timestamp
        let entry = S3CheckCacheEntry::new("my-bucket", "Data/abc123.xxh128", "not-a-number");
        cache.put(&entry).await;

        // Should not be returned (invalid timestamp treated as expired)
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn test_sqlite_s3_check_cache_update_timestamp() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("s3_check_cache.db");
        let cache = SqliteS3CheckCache::open(&db_path).unwrap();

        let key = S3CheckCacheKey::new("my-bucket", "Data/abc123.xxh128");

        // Put initial entry
        let old_time = (current_epoch_seconds() - 1000).to_string();
        let entry1 = S3CheckCacheEntry::new("my-bucket", "Data/abc123.xxh128", old_time.clone());
        cache.put(&entry1).await;

        let retrieved1 = cache.get(&key).await.unwrap();
        assert_eq!(retrieved1.last_seen_time, old_time);

        // Update with new timestamp
        let new_time = current_epoch_seconds().to_string();
        let entry2 = S3CheckCacheEntry::new("my-bucket", "Data/abc123.xxh128", new_time.clone());
        cache.put(&entry2).await;

        let retrieved2 = cache.get(&key).await.unwrap();
        assert_eq!(retrieved2.last_seen_time, new_time);
    }
}
