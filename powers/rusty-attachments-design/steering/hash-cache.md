# Hash Cache Design Summary

**Full doc:** `design/hash-cache.md`  
**Status:** ✅ IMPLEMENTED in `crates/storage/src/hash_cache/` and `crates/storage/src/s3_check_cache/` (including SQLite backends)

## Purpose
Avoid expensive re-computation of file hashes and redundant S3 HEAD requests.

## Hash Cache

Caches file content hashes to avoid re-hashing unchanged files.

### Key Structure
```rust
struct HashCacheKey {
    path: String,   // Relative file path
    size: u64,      // File size in bytes
    mtime: i64,     // Modification time (microseconds)
}

struct HashCacheEntry {
    hash: String,
    hash_alg: HashAlgorithm,
    created_at: i64,
    expires_at: i64,
}
```

### Backend Trait
```rust
#[async_trait]
pub trait HashCacheBackend: Send + Sync {
    async fn get(&self, key: &HashCacheKey) -> Option<HashCacheEntry>;
    async fn put(&self, key: &HashCacheKey, entry: &HashCacheEntry);
    async fn delete(&self, key: &HashCacheKey);
    async fn get_batch(&self, keys: &[HashCacheKey]) -> HashMap<HashCacheKey, HashCacheEntry>;
    async fn put_batch(&self, entries: &[(HashCacheKey, HashCacheEntry)]);
}
```

### SQLite Schema
```sql
CREATE TABLE hash_cache (
    machine_id TEXT NOT NULL,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    hash TEXT NOT NULL,
    hash_alg TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (machine_id, path, size, mtime)
);
```

## S3 Check Cache

Tracks which CAS objects have been uploaded to avoid HEAD requests.

### Key Structure
```rust
struct S3CheckCacheKey {
    s3_key: String,  // "{bucket}/{key}"
}

struct S3CheckCacheEntry {
    s3_key: String,
    last_seen_time: String,
}
```

### Usage
```rust
// Before upload
if s3_cache.exists(bucket, &s3_key).await {
    continue;  // Skip - already uploaded
}

// After upload
s3_cache.mark_uploaded(bucket, &s3_key).await;
```

### Integrity Verification
Sample entries and HEAD check to detect stale cache (objects deleted externally).

## Key Distinction
- **Hash Cache**: `(path, size, mtime) → hash` - avoids re-hashing local files
- **S3 Check Cache**: `s3_key → last_seen_time` - avoids HEAD requests to S3

## When to Read Full Doc
- Implementing cache backends
- Understanding cache invalidation
- SQLite backend details (implemented in `sqlite.rs`)
- Integrity verification logic
