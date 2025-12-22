# Rusty Attachments: Hash Cache Design

**Status: ✅ IMPLEMENTED** - See `crates/storage/src/hash_cache/` and `crates/storage/src/s3_check_cache/`

## Overview

Computing XXH128 hashes for large files is expensive. This document proposes a hash cache layer with a pluggable backend interface supporting SQLite (local) and DynamoDB (distributed).

---

## Dependencies

This module uses utilities from the `common` crate:

```rust
use rusty_attachments_common::{
    hash_file, hash_bytes, 
    DEFAULT_HASH_CACHE_TTL_DAYS,
    HashAlgorithm,
};
```

---

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── storage/
│   │   └── src/
│   │       ├── hash_cache/
│   │       │   ├── mod.rs       # Public API, HashCache wrapper
│   │       │   ├── backend.rs   # HashCacheBackend trait
│   │       │   ├── entry.rs     # HashCacheEntry, HashCacheKey
│   │       │   ├── sqlite.rs    # SQLite backend implementation
│   │       │   └── error.rs     # HashCacheError
│   │       └── ...
```

---

## Data Structures

### Cache Entry

Aligned with the manifest model's file metadata:

```rust
/// Key for looking up a cached hash.
/// Matches the file identity fields from ManifestFilePath.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HashCacheKey {
    /// Relative file path (matches ManifestFilePath.path)
    pub path: String,
    /// File size in bytes (matches ManifestFilePath.size)
    pub size: u64,
    /// Modification time in microseconds since epoch (matches ManifestFilePath.mtime)
    pub mtime: i64,
}

/// Cached hash entry.
#[derive(Debug, Clone)]
pub struct HashCacheEntry {
    /// The computed hash value
    pub hash: String,
    /// Hash algorithm used (for future-proofing)
    pub hash_alg: HashAlgorithm,
    /// When this entry was created (epoch seconds)
    pub created_at: i64,
    /// When this entry expires (epoch seconds)
    pub expires_at: i64,
}
```

---

## Backend Trait

```rust
/// Pluggable backend for hash cache storage.
/// 
/// Implementations handle their own error recovery - on failure,
/// methods return None/Ok to allow graceful degradation (rehash the file).
#[async_trait]
pub trait HashCacheBackend: Send + Sync {
    /// Look up a cached hash by file identity.
    /// Returns None if not found, expired, or on error.
    async fn get(&self, key: &HashCacheKey) -> Option<HashCacheEntry>;

    /// Store a hash entry.
    /// Silently fails on error (caller will just rehash next time).
    async fn put(&self, key: &HashCacheKey, entry: &HashCacheEntry);

    /// Remove an entry (optional, for explicit invalidation).
    async fn delete(&self, key: &HashCacheKey);

    /// Bulk lookup for multiple keys.
    /// Returns a map of found entries; missing/expired keys are omitted.
    async fn get_batch(&self, keys: &[HashCacheKey]) -> HashMap<HashCacheKey, HashCacheEntry> {
        // Default implementation: sequential gets
        let mut results = HashMap::new();
        for key in keys {
            if let Some(entry) = self.get(key).await {
                results.insert(key.clone(), entry);
            }
        }
        results
    }

    /// Bulk store for multiple entries.
    async fn put_batch(&self, entries: &[(HashCacheKey, HashCacheEntry)]) {
        // Default implementation: sequential puts
        for (key, entry) in entries {
            self.put(key, entry).await;
        }
    }
}
```

---

## High-Level Wrapper

```rust
use rusty_attachments_common::DEFAULT_HASH_CACHE_TTL_DAYS;

/// Hash cache with configurable backend and TTL.
pub struct HashCache {
    backend: Box<dyn HashCacheBackend>,
    ttl_days: u32,
    hash_alg: HashAlgorithm,
}

impl HashCache {
    /// Create a new hash cache with the given backend and TTL.
    pub fn new(backend: impl HashCacheBackend + 'static, ttl_days: u32) -> Self {
        Self {
            backend: Box::new(backend),
            ttl_days,
            hash_alg: HashAlgorithm::Xxh128,
        }
    }
    
    /// Create with default TTL from common constants.
    pub fn with_default_ttl(backend: impl HashCacheBackend + 'static) -> Self {
        Self::new(backend, DEFAULT_HASH_CACHE_TTL_DAYS)
    }

    /// Look up a cached hash. Returns None if not cached or expired.
    pub async fn get(&self, path: &str, size: u64, mtime: i64) -> Option<String> {
        let key = HashCacheKey { path: path.to_string(), size, mtime };
        self.backend.get(&key).await.map(|e| e.hash)
    }

    /// Store a computed hash.
    pub async fn put(&self, path: &str, size: u64, mtime: i64, hash: String) {
        let key = HashCacheKey { path: path.to_string(), size, mtime };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let entry = HashCacheEntry {
            hash,
            hash_alg: self.hash_alg,
            created_at: now,
            expires_at: now + (self.ttl_days as i64 * 86400),
        };
        self.backend.put(&key, &entry).await;
    }

    /// Bulk lookup - returns map of path -> hash for found entries.
    pub async fn get_batch(&self, keys: &[(String, u64, i64)]) -> HashMap<String, String> {
        let cache_keys: Vec<_> = keys.iter()
            .map(|(path, size, mtime)| HashCacheKey {
                path: path.clone(),
                size: *size,
                mtime: *mtime,
            })
            .collect();
        
        self.backend.get_batch(&cache_keys).await
            .into_iter()
            .map(|(k, v)| (k.path, v.hash))
            .collect()
    }
}
```

---

## SQLite Backend

For local/single-machine use (submitter, worker).

```rust
pub struct SqliteHashCache {
    pool: SqlitePool,  // or rusqlite::Connection with mutex
    machine_id: String,
}

impl SqliteHashCache {
    /// Create or open a SQLite cache at the given path.
    pub async fn open(db_path: &Path, machine_id: String) -> Result<Self, HashCacheError>;
}
```

### Schema

```sql
CREATE TABLE IF NOT EXISTS hash_cache (
    -- Composite key: machine + file identity
    machine_id TEXT NOT NULL,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    
    -- Cached data
    hash TEXT NOT NULL,
    hash_alg TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    
    PRIMARY KEY (machine_id, path, size, mtime)
);

-- Index for TTL cleanup
CREATE INDEX IF NOT EXISTS idx_expires_at ON hash_cache(expires_at);
```

---

## DynamoDB Backend (TODO)

> **Not implemented yet.** This section documents the planned design for distributed cache sharing.

For distributed use (farm-wide cache sharing).

```rust
pub struct DynamoDbHashCache {
    client: DynamoDbClient,
    table_name: String,
    worker_id: String,
}

impl DynamoDbHashCache {
    pub fn new(client: DynamoDbClient, table_name: String, worker_id: String) -> Self;
}
```

### Table Design

| Attribute | Type | Description |
|-----------|------|-------------|
| `pk` | String (Partition Key) | `{worker_id}#{path}` |
| `sk` | String (Sort Key) | `{size}#{mtime}` |
| `hash` | String | Computed hash value |
| `hash_alg` | String | Algorithm identifier |
| `created_at` | Number | Epoch seconds |
| `ttl` | Number | DynamoDB TTL attribute (epoch seconds) |

DynamoDB's native TTL handles expiration automatically.

---

## Usage Example

```rust
use rusty_attachments_storage::hash_cache::{HashCache, SqliteHashCache};
use rusty_attachments_common::hash_file;

// Local cache (submitter or worker)
let sqlite = SqliteHashCache::open(&cache_path, machine_id).await?;
let cache = HashCache::with_default_ttl(sqlite);

// Usage in manifest creation
for file in files {
    let hash = match cache.get(&file.path, file.size, file.mtime).await {
        Some(h) => h,
        None => {
            // Use hash_file from common crate
            let h = hash_file(&file.full_path, HashAlgorithm::Xxh128)?;
            cache.put(&file.path, file.size, file.mtime, h.clone()).await;
            h
        }
    };
}
```

---

## Error Handling

All backend errors are logged and swallowed - the cache degrades gracefully:

```rust
#[derive(Debug, thiserror::Error)]
pub enum HashCacheError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    
    #[error("DynamoDB error: {0}")]
    DynamoDb(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

The `HashCacheBackend` trait methods don't return `Result` - implementations catch errors internally and return `None` / no-op, allowing the caller to simply rehash.

---

## Integration Points

- **Manifest creation** (`file_system.rs`): Check cache before hashing, store after computing
- **Bulk operations**: Use `get_batch`/`put_batch` for efficiency when processing many files

---

# S3 Check Cache Design

## Overview

The S3 Check Cache is a separate cache that tracks which CAS objects have already been uploaded to S3. This avoids redundant HEAD requests to check object existence before upload.

**Key distinction:**
- **Hash Cache**: `(path, size, mtime) → hash` - avoids re-hashing local files
- **S3 Check Cache**: `s3_key → last_seen_time` - avoids HEAD requests to S3

---

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── storage/
│   │   └── src/
│   │       ├── s3_check_cache/
│   │       │   ├── mod.rs       # Public API, S3CheckCache wrapper
│   │       │   ├── backend.rs   # S3CheckCacheBackend trait
│   │       │   ├── entry.rs     # S3CheckCacheEntry, S3CheckCacheKey
│   │       │   ├── sqlite.rs    # SQLite backend implementation
│   │       │   └── error.rs     # S3CheckCacheError
│   │       └── ...
```

---

## Data Structures

```rust
/// Key for looking up a cached S3 existence check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct S3CheckCacheKey {
    /// Full S3 key: "{bucket}/{key}"
    pub s3_key: String,
}

/// Cached S3 existence entry.
#[derive(Debug, Clone)]
pub struct S3CheckCacheEntry {
    /// Full S3 key: "{bucket}/{key}"
    pub s3_key: String,
    /// When this entry was last verified (epoch seconds as string for compatibility)
    pub last_seen_time: String,
}
```

---

## Backend Trait

```rust
/// Pluggable backend for S3 check cache storage.
/// Same graceful degradation philosophy as HashCacheBackend.
#[async_trait]
pub trait S3CheckCacheBackend: Send + Sync {
    /// Check if an S3 key was previously seen as uploaded.
    /// Returns None if not found or on error.
    async fn get(&self, key: &S3CheckCacheKey) -> Option<S3CheckCacheEntry>;

    /// Record that an S3 key exists.
    async fn put(&self, entry: &S3CheckCacheEntry);

    /// Remove an entry (for cache invalidation).
    async fn delete(&self, key: &S3CheckCacheKey);

    /// Clear all entries (for cache reset).
    async fn clear(&self);
}
```

---

## High-Level Wrapper

```rust
/// S3 check cache with configurable backend.
pub struct S3CheckCache {
    backend: Box<dyn S3CheckCacheBackend>,
}

impl S3CheckCache {
    pub fn new(backend: impl S3CheckCacheBackend + 'static) -> Self {
        Self { backend: Box::new(backend) }
    }

    /// Check if an S3 object was previously uploaded.
    pub async fn exists(&self, bucket: &str, key: &str) -> bool {
        let cache_key = S3CheckCacheKey {
            s3_key: format!("{}/{}", bucket, key),
        };
        self.backend.get(&cache_key).await.is_some()
    }

    /// Record that an S3 object exists.
    pub async fn mark_uploaded(&self, bucket: &str, key: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();
        let entry = S3CheckCacheEntry {
            s3_key: format!("{}/{}", bucket, key),
            last_seen_time: now,
        };
        self.backend.put(&entry).await;
    }

    /// Reset the entire cache (used when integrity check fails).
    pub async fn reset(&self) {
        self.backend.clear().await;
    }
}
```

---

## SQLite Backend

```rust
pub struct SqliteS3CheckCache {
    pool: SqlitePool,
}

impl SqliteS3CheckCache {
    pub async fn open(db_path: &Path) -> Result<Self, S3CheckCacheError>;
}
```

### Schema

```sql
CREATE TABLE IF NOT EXISTS s3_check_cache (
    s3_key TEXT PRIMARY KEY,
    last_seen_time TEXT NOT NULL
);
```

---

## Cache Integrity Verification

The cache can become stale if S3 objects are deleted externally. Before using the cache, verify a sample of entries:

```rust
impl S3CheckCache {
    /// Verify cache integrity by sampling entries and checking S3.
    /// Returns false if any sampled entry is missing from S3.
    pub async fn verify_integrity<C: StorageClient>(
        &self,
        client: &C,
        manifest: &Manifest,
        bucket: &str,
        cas_prefix: &str,
        sample_size: usize,
    ) -> bool {
        // 1. Get all S3 keys from manifest
        let s3_keys: Vec<String> = manifest.files()
            .iter()
            .filter_map(|f| f.hash.as_ref())
            .map(|h| format!("{}/{}.xxh128", cas_prefix, h))
            .collect();

        // 2. Shuffle and sample
        let mut rng = rand::thread_rng();
        let sampled: Vec<_> = s3_keys.choose_multiple(&mut rng, sample_size).collect();

        // 3. Check each sampled key
        for key in sampled {
            let cache_key = S3CheckCacheKey {
                s3_key: format!("{}/{}", bucket, key),
            };
            
            // Only check keys that are in our cache
            if self.backend.get(&cache_key).await.is_some() {
                // Verify it actually exists in S3
                if client.head_object(bucket, key).await?.is_none() {
                    return false; // Cache is stale
                }
            }
        }

        true
    }
}
```

---

## Usage Example

```rust
use rusty_attachments_storage::s3_check_cache::{S3CheckCache, SqliteS3CheckCache};

let sqlite = SqliteS3CheckCache::open(&cache_path).await?;
let s3_cache = S3CheckCache::new(sqlite);

// Before uploading, verify cache integrity
if !s3_cache.verify_integrity(&client, &manifest, bucket, cas_prefix, 30).await {
    s3_cache.reset().await;
}

// During upload
for file in manifest.files() {
    let s3_key = format!("{}/{}.xxh128", cas_prefix, file.hash);
    
    // Check cache first
    if s3_cache.exists(bucket, &s3_key).await {
        continue; // Skip - already uploaded
    }
    
    // Check S3 (cache miss)
    if client.head_object(bucket, &s3_key).await?.is_some() {
        s3_cache.mark_uploaded(bucket, &s3_key).await;
        continue; // Skip - already in S3
    }
    
    // Upload the file
    client.put_object_from_file(bucket, &s3_key, &local_path, None, None, None).await?;
    s3_cache.mark_uploaded(bucket, &s3_key).await;
}
```

---

## Related Documents

- [common.md](common.md) - Shared hash utilities and constants
- [model-design.md](model-design.md) - ManifestFilePath with path/size/mtime fields
- [file_system.md](file_system.md) - Manifest creation from directories
- [storage-design.md](storage-design.md) - Upload orchestration using both caches
