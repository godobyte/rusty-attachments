//! SQLite backend for hash cache.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use super::backend::HashCacheBackend;
use super::entry::{HashCacheEntry, HashCacheKey};
use super::error::HashCacheError;

use rusty_attachments_model::HashAlgorithm;

/// SQLite-based hash cache backend.
///
/// Stores hash entries in a local SQLite database for persistence across sessions.
/// Uses WAL mode for better concurrent read performance.
pub struct SqliteHashCache {
    /// Database connection (protected by mutex for thread safety).
    conn: Mutex<Connection>,
    /// Machine ID to scope entries to this machine.
    machine_id: String,
    /// Table name (versioned for schema migrations).
    table_name: String,
}

impl SqliteHashCache {
    /// Database schema version.
    const CACHE_DB_VERSION: u32 = 1;

    /// Create or open a SQLite hash cache at the given path.
    ///
    /// # Arguments
    /// * `db_path` - Path to the SQLite database file
    /// * `machine_id` - Machine identifier to scope entries
    ///
    /// # Returns
    /// A new SqliteHashCache instance.
    ///
    /// # Errors
    /// Returns error if database cannot be opened or initialized.
    pub fn open(db_path: &Path, machine_id: impl Into<String>) -> Result<Self, HashCacheError> {
        let conn: Connection =
            Connection::open(db_path).map_err(|e| HashCacheError::Sqlite(e.to_string()))?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| HashCacheError::Sqlite(e.to_string()))?;

        // Set busy timeout to handle concurrent access
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| HashCacheError::Sqlite(e.to_string()))?;

        let table_name: String = format!("hash_cache_v{}", Self::CACHE_DB_VERSION);

        // Create table if it doesn't exist
        let create_sql: String = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                machine_id TEXT NOT NULL,
                path BLOB NOT NULL,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                hash TEXT NOT NULL,
                hash_alg TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                PRIMARY KEY (machine_id, path, size, mtime)
            )",
            table_name
        );
        conn.execute(&create_sql, [])
            .map_err(|e| HashCacheError::Sqlite(e.to_string()))?;

        // Create index for TTL cleanup
        let index_sql: String = format!(
            "CREATE INDEX IF NOT EXISTS idx_{}_expires_at ON {}(expires_at)",
            table_name, table_name
        );
        conn.execute(&index_sql, [])
            .map_err(|e| HashCacheError::Sqlite(e.to_string()))?;

        Ok(Self {
            conn: Mutex::new(conn),
            machine_id: machine_id.into(),
            table_name,
        })
    }

    /// Clean up expired entries from the cache.
    ///
    /// # Returns
    /// Number of entries deleted.
    pub fn cleanup_expired(&self) -> Result<usize, HashCacheError> {
        let now: i64 = current_epoch_seconds();
        let conn = self.conn.lock().unwrap();
        let deleted: usize = conn
            .execute(
                &format!("DELETE FROM {} WHERE expires_at < ?", self.table_name),
                params![now],
            )
            .map_err(|e| HashCacheError::Sqlite(e.to_string()))?;
        Ok(deleted)
    }

    /// Get the number of entries in the cache.
    pub fn count(&self) -> Result<usize, HashCacheError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {}", self.table_name),
                [],
                |row| row.get(0),
            )
            .map_err(|e| HashCacheError::Sqlite(e.to_string()))?;
        Ok(count as usize)
    }
}

#[async_trait::async_trait]
impl HashCacheBackend for SqliteHashCache {
    async fn get(&self, key: &HashCacheKey) -> Option<HashCacheEntry> {
        let now: i64 = current_epoch_seconds();
        let conn = self.conn.lock().unwrap();

        // Encode path as bytes for storage (handles Unicode edge cases)
        let path_bytes: Vec<u8> = key.path.as_bytes().to_vec();

        let result = conn.query_row(
            &format!(
                "SELECT hash, hash_alg, created_at, expires_at FROM {} 
                 WHERE machine_id = ? AND path = ? AND size = ? AND mtime = ? AND expires_at > ?",
                self.table_name
            ),
            params![self.machine_id, path_bytes, key.size as i64, key.mtime, now],
            |row| {
                let hash: String = row.get(0)?;
                let hash_alg_str: String = row.get(1)?;
                let created_at: i64 = row.get(2)?;
                let expires_at: i64 = row.get(3)?;

                // Parse hash algorithm
                let hash_alg: HashAlgorithm = match hash_alg_str.as_str() {
                    "xxh128" => HashAlgorithm::Xxh128,
                    _ => HashAlgorithm::Xxh128, // Default fallback
                };

                Ok(HashCacheEntry {
                    hash,
                    hash_alg,
                    created_at,
                    expires_at,
                })
            },
        );

        match result {
            Ok(entry) => Some(entry),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                log::warn!("Hash cache get error: {}", e);
                None
            }
        }
    }

    async fn put(&self, key: &HashCacheKey, entry: &HashCacheEntry) {
        let conn = self.conn.lock().unwrap();

        // Encode path as bytes for storage
        let path_bytes: Vec<u8> = key.path.as_bytes().to_vec();
        let hash_alg_str: &str = entry.hash_alg.extension();

        let result = conn.execute(
            &format!(
                "INSERT OR REPLACE INTO {} (machine_id, path, size, mtime, hash, hash_alg, created_at, expires_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                self.table_name
            ),
            params![
                self.machine_id,
                path_bytes,
                key.size as i64,
                key.mtime,
                entry.hash,
                hash_alg_str,
                entry.created_at,
                entry.expires_at
            ],
        );

        if let Err(e) = result {
            log::warn!("Hash cache put error: {}", e);
        }
    }

    async fn delete(&self, key: &HashCacheKey) {
        let conn = self.conn.lock().unwrap();

        let path_bytes: Vec<u8> = key.path.as_bytes().to_vec();

        let result = conn.execute(
            &format!(
                "DELETE FROM {} WHERE machine_id = ? AND path = ? AND size = ? AND mtime = ?",
                self.table_name
            ),
            params![self.machine_id, path_bytes, key.size as i64, key.mtime],
        );

        if let Err(e) = result {
            log::warn!("Hash cache delete error: {}", e);
        }
    }

    async fn get_batch(&self, keys: &[HashCacheKey]) -> HashMap<HashCacheKey, HashCacheEntry> {
        let mut results: HashMap<HashCacheKey, HashCacheEntry> = HashMap::new();
        for key in keys {
            if let Some(entry) = self.get(key).await {
                results.insert(key.clone(), entry);
            }
        }
        results
    }

    async fn put_batch(&self, entries: &[(HashCacheKey, HashCacheEntry)]) {
        let conn = self.conn.lock().unwrap();

        // Use a transaction for batch inserts
        let tx = match conn.unchecked_transaction() {
            Ok(tx) => tx,
            Err(e) => {
                log::warn!("Hash cache batch put transaction error: {}", e);
                return;
            }
        };

        for (key, entry) in entries {
            let path_bytes: Vec<u8> = key.path.as_bytes().to_vec();
            let hash_alg_str: &str = entry.hash_alg.extension();

            let result = tx.execute(
                &format!(
                    "INSERT OR REPLACE INTO {} (machine_id, path, size, mtime, hash, hash_alg, created_at, expires_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    self.table_name
                ),
                params![
                    self.machine_id,
                    path_bytes,
                    key.size as i64,
                    key.mtime,
                    entry.hash,
                    hash_alg_str,
                    entry.created_at,
                    entry.expires_at
                ],
            );

            if let Err(e) = result {
                log::warn!("Hash cache batch put error: {}", e);
            }
        }

        if let Err(e) = tx.commit() {
            log::warn!("Hash cache batch put commit error: {}", e);
        }
    }

    async fn clear(&self) {
        let conn = self.conn.lock().unwrap();
        let result = conn.execute(&format!("DELETE FROM {}", self.table_name), []);
        if let Err(e) = result {
            log::warn!("Hash cache clear error: {}", e);
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
    async fn test_sqlite_hash_cache_basic() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");
        let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();

        // Test cache miss
        let key = HashCacheKey::new("test.txt", 100, 1234567890);
        assert!(cache.get(&key).await.is_none());

        // Test put and get
        let entry = HashCacheEntry::new(
            "abc123",
            HashAlgorithm::Xxh128,
            current_epoch_seconds(),
            current_epoch_seconds() + 86400,
        );
        cache.put(&key, &entry).await;

        let retrieved = cache.get(&key).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().hash, "abc123");
    }

    #[tokio::test]
    async fn test_sqlite_hash_cache_expired_entry() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");
        let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();

        let key = HashCacheKey::new("test.txt", 100, 1234567890);

        // Put an already-expired entry
        let entry = HashCacheEntry::new(
            "abc123",
            HashAlgorithm::Xxh128,
            current_epoch_seconds() - 200,
            current_epoch_seconds() - 100, // Expired
        );
        cache.put(&key, &entry).await;

        // Should not be returned
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn test_sqlite_hash_cache_delete() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");
        let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();

        let key = HashCacheKey::new("test.txt", 100, 1234567890);
        let entry = HashCacheEntry::new(
            "abc123",
            HashAlgorithm::Xxh128,
            current_epoch_seconds(),
            current_epoch_seconds() + 86400,
        );
        cache.put(&key, &entry).await;
        assert!(cache.get(&key).await.is_some());

        cache.delete(&key).await;
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn test_sqlite_hash_cache_different_machine_id() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");

        let cache1 = SqliteHashCache::open(&db_path, "machine-1").unwrap();
        let cache2 = SqliteHashCache::open(&db_path, "machine-2").unwrap();

        let key = HashCacheKey::new("test.txt", 100, 1234567890);
        let entry = HashCacheEntry::new(
            "abc123",
            HashAlgorithm::Xxh128,
            current_epoch_seconds(),
            current_epoch_seconds() + 86400,
        );

        cache1.put(&key, &entry).await;

        // Same key, different machine - should not find it
        assert!(cache2.get(&key).await.is_none());

        // Same machine - should find it
        assert!(cache1.get(&key).await.is_some());
    }

    #[tokio::test]
    async fn test_sqlite_hash_cache_unicode_path() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");
        let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();

        // Test various Unicode paths (from Python test cases)
        let unicode_paths = vec![
            "ñ/ñ.txt",
            "😀.txt",
            "דּ.txt",
            "€.txt",
            "ö.txt",
            "\r", // Carriage return
        ];

        for path in unicode_paths {
            let key = HashCacheKey::new(path, 100, 1234567890);
            let entry = HashCacheEntry::new(
                format!("hash_{}", path),
                HashAlgorithm::Xxh128,
                current_epoch_seconds(),
                current_epoch_seconds() + 86400,
            );
            cache.put(&key, &entry).await;

            let retrieved = cache.get(&key).await;
            assert!(retrieved.is_some(), "Failed for path: {}", path);
            assert_eq!(retrieved.unwrap().hash, format!("hash_{}", path));
        }
    }

    #[tokio::test]
    async fn test_sqlite_hash_cache_batch_operations() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");
        let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();

        let now = current_epoch_seconds();
        let entries: Vec<(HashCacheKey, HashCacheEntry)> = (0..10)
            .map(|i| {
                (
                    HashCacheKey::new(format!("file{}.txt", i), 100 + i as u64, 1000 + i as i64),
                    HashCacheEntry::new(
                        format!("hash{}", i),
                        HashAlgorithm::Xxh128,
                        now,
                        now + 86400,
                    ),
                )
            })
            .collect();

        cache.put_batch(&entries).await;

        // Verify all entries were stored
        let keys: Vec<HashCacheKey> = entries.iter().map(|(k, _)| k.clone()).collect();
        let results = cache.get_batch(&keys).await;
        assert_eq!(results.len(), 10);
    }

    #[tokio::test]
    async fn test_sqlite_hash_cache_cleanup_expired() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");
        let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();

        let now = current_epoch_seconds();

        // Add some expired entries
        for i in 0..5 {
            let key = HashCacheKey::new(format!("expired{}.txt", i), 100, 1000);
            let entry = HashCacheEntry::new(
                format!("hash{}", i),
                HashAlgorithm::Xxh128,
                now - 200,
                now - 100, // Expired
            );
            cache.put(&key, &entry).await;
        }

        // Add some valid entries
        for i in 0..5 {
            let key = HashCacheKey::new(format!("valid{}.txt", i), 100, 2000);
            let entry = HashCacheEntry::new(
                format!("hash{}", i),
                HashAlgorithm::Xxh128,
                now,
                now + 86400, // Not expired
            );
            cache.put(&key, &entry).await;
        }

        assert_eq!(cache.count().unwrap(), 10);

        let deleted = cache.cleanup_expired().unwrap();
        assert_eq!(deleted, 5);
        assert_eq!(cache.count().unwrap(), 5);
    }

    #[tokio::test]
    async fn test_sqlite_hash_cache_clear() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");
        let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();

        let now = current_epoch_seconds();
        for i in 0..10 {
            let key = HashCacheKey::new(format!("file{}.txt", i), 100, 1000);
            let entry = HashCacheEntry::new(
                format!("hash{}", i),
                HashAlgorithm::Xxh128,
                now,
                now + 86400,
            );
            cache.put(&key, &entry).await;
        }

        assert_eq!(cache.count().unwrap(), 10);

        cache.clear().await;
        assert_eq!(cache.count().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_sqlite_hash_cache_persistence() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("hash_cache.db");

        let key = HashCacheKey::new("test.txt", 100, 1234567890);
        let now = current_epoch_seconds();

        // Create cache, add entry, drop it
        {
            let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();
            let entry = HashCacheEntry::new("abc123", HashAlgorithm::Xxh128, now, now + 86400);
            cache.put(&key, &entry).await;
        }

        // Reopen and verify entry persists
        {
            let cache = SqliteHashCache::open(&db_path, "test-machine").unwrap();
            let retrieved = cache.get(&key).await;
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().hash, "abc123");
        }
    }
}
