# VFS Read-Only Disk Cache

**Status: ✅ CORE IMPLEMENTED | 🚧 EVICTION DEFERRED**

Tasks 1-4 and 6 are complete. See `crates/vfs/src/diskcache/read_cache.rs` for the implementation.
Cache eviction (Task 5) is deferred to Future Considerations.

## Problem

Currently, immutable files from the manifest are only cached in the in-memory `MemoryPool`. When memory pressure causes LRU eviction, content is discarded and must be re-fetched from S3 on the next read. This is inefficient for large files or repeated access patterns.

## Solution

Add a disk-based read-through cache for immutable CAS content that sits between the memory pool and S3. Content is stored by hash, enabling deduplication across files.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Read Flow                                │
│                                                                  │
│  MemoryPool::acquire()                                          │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────┐  hit   ┌──────────────┐                            │
│  │ Memory  │───────►│ Return data  │                            │
│  │  Pool   │        └──────────────┘                            │
│  └────┬────┘                                                     │
│       │ miss                                                     │
│       ▼                                                          │
│  ┌─────────┐  hit   ┌──────────────┐  ┌──────────────┐          │
│  │  Disk   │───────►│ Load to pool │─►│ Return data  │          │
│  │  Cache  │        └──────────────┘  └──────────────┘          │
│  └────┬────┘                                                     │
│       │ miss                                                     │
│       ▼                                                          │
│  ┌─────────┐        ┌──────────────┐  ┌──────────────┐          │
│  │   S3    │───────►│Write to disk │─►│ Load to pool │          │
│  │  Fetch  │        │    cache     │  └──────┬───────┘          │
│  └─────────┘        └──────────────┘         │                  │
│                                              ▼                   │
│                                     ┌──────────────┐            │
│                                     │ Return data  │            │
│                                     └──────────────┘            │
└─────────────────────────────────────────────────────────────────┘
```

## Directory Structure

```
cache_root/
├── cas/                    # Read-only CAS content (by hash)
│   ├── abc123def456...     # Content file (hash as filename)
│   ├── 789xyz...
│   └── ...
├── dirty/                  # Mutable file cache (existing MaterializedCache)
│   ├── .deleted/
│   ├── .meta/
│   └── path/to/file.txt
└── metadata.json           # Optional: cache metadata/stats
```

## Code Organization

### File Restructure

Move shared cache code to a `diskcache` module:

```
crates/vfs/src/
├── diskcache/
│   ├── mod.rs              # Module exports
│   ├── traits.rs           # WriteCache trait (moved from write/cache.rs)
│   ├── memory_cache.rs     # MemoryWriteCache (for testing)
│   ├── materialized.rs     # MaterializedCache (dirty files)
│   └── read_cache.rs       # NEW: ReadCache (CAS content)
├── write/
│   ├── mod.rs              # Re-export from diskcache
│   ├── dirty.rs
│   ├── dirty_dir.rs
│   ├── export.rs
│   └── stats.rs
└── ...
```

### Data Structures

```rust
/// Metadata for a cached CAS entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntryMeta {
    /// Content hash (also the filename).
    pub hash: String,
    /// Size in bytes.
    pub size: u64,
    /// Time when cached (Unix timestamp).
    pub cached_at: u64,
    /// Last access time (for LRU eviction).
    pub last_accessed: u64,
}

/// Disk cache for immutable CAS content.
pub struct ReadCache {
    /// Root directory for CAS storage.
    cache_dir: PathBuf,
    /// Maximum cache size in bytes (0 = unlimited).
    max_size: u64,
    /// Current cache size in bytes.
    current_size: AtomicU64,
}
```

## Implementation Tasks

### Task 1: Create diskcache module structure ✅

1. Create `crates/vfs/src/diskcache/mod.rs`
2. Create `crates/vfs/src/diskcache/error.rs` - `DiskCacheError` type
3. Create `crates/vfs/src/diskcache/traits.rs` - `WriteCache` trait
4. Create `crates/vfs/src/diskcache/memory_cache.rs` - `MemoryWriteCache`
5. Create `crates/vfs/src/diskcache/materialized.rs` - `MaterializedCache` and `ChunkedFileMeta`
6. Update `crates/vfs/src/write/mod.rs` to re-export from diskcache
7. Delete `crates/vfs/src/write/cache.rs`
8. Update imports in `dirty.rs` and other files

### Task 2: Implement ReadCache core ✅

1. Create `crates/vfs/src/diskcache/read_cache.rs`
2. Implement `ReadCache::new(options)`
3. Implement `ReadCache::get(hash) -> Option<Vec<u8>>`
4. Implement `ReadCache::put(hash, data) -> Result<()>` with atomic write
5. Implement `ReadCache::contains(hash) -> bool`
6. Implement `ReadCache::contains_with_size(hash, size) -> bool`
7. Implement `ReadCache::remove(hash) -> Result<()>`

### Task 3: Add cache metadata tracking ✅

1. Track `current_size` for cache size monitoring
2. Implement `ReadCache::scan()` to rebuild state from disk on startup
3. Implement `ReadCache::current_size()` for stats

### Task 4: Integrate with MemoryPool and FUSE ✅

1. Add `ReadCacheConfig` to `VfsOptions`
2. Add `ReadCache` to `DeadlineVfs` (read-only FUSE)
3. Modify read path to check disk cache before S3 fetch
4. Add write-through on successful S3 fetch
5. Add `ReadCache` to `WritableVfs`
6. Add `ReadCache` to `DirtyFileManager` for COW source reads

### Task 5: Add cache eviction (DEFERRED - Future Consideration)

See Future Considerations section.

### Task 6: Wire up in WritableVfs ✅

1. Add `ReadCache` to `WritableVfs` construction
2. Pass to `DirtyFileManager` via `set_read_cache()`
3. Add `ReadCacheConfig` to `VfsOptions`

## Test Cases

### Read Path Tests

#### TC-R1: First read stores to disk cache
```rust
#[tokio::test]
async fn test_first_read_caches_to_disk() {
    // Setup: Empty disk cache, content in mock S3
    // Action: Read file via VFS
    // Assert: 
    //   - Data returned correctly
    //   - File exists in disk cache at `cas/{hash}`
    //   - File content matches original
}
```

#### TC-R2: Second read hits disk cache (memory miss)
```rust
#[tokio::test]
async fn test_memory_miss_disk_hit() {
    // Setup: Content in disk cache, NOT in memory pool
    // Action: Read file via VFS
    // Assert:
    //   - Data returned correctly
    //   - S3 was NOT called
    //   - Content loaded into memory pool
}
```

#### TC-R3: Memory hit bypasses disk cache
```rust
#[tokio::test]
async fn test_memory_hit_bypasses_disk() {
    // Setup: Content in memory pool AND disk cache
    // Action: Read file via VFS
    // Assert:
    //   - Data returned correctly
    //   - Disk cache was NOT accessed
    //   - S3 was NOT called
}
```

#### TC-R4: Corrupted disk cache entry triggers re-fetch
```rust
#[tokio::test]
async fn test_corrupted_cache_refetches() {
    // Setup: Corrupted/truncated file in disk cache
    // Action: Read file via VFS
    // Assert:
    //   - Data returned correctly (from S3)
    //   - Disk cache entry replaced with correct content
    //   - Hash verification detected corruption
}
```

#### TC-R5: Concurrent reads for same hash
```rust
#[tokio::test]
async fn test_concurrent_reads_single_fetch() {
    // Setup: Empty caches, slow S3 mock
    // Action: 5 concurrent reads for same hash
    // Assert:
    //   - All reads return correct data
    //   - S3 called only once (fetch coordination)
    //   - Single disk cache entry created
}
```

#### TC-R6: Read non-existent hash
```rust
#[tokio::test]
async fn test_read_missing_hash() {
    // Setup: Hash not in S3
    // Action: Read file via VFS
    // Assert:
    //   - Error returned (ContentRetrievalFailed)
    //   - No disk cache entry created
}
```

### Write-Through Tests

#### TC-W1: Write-through only on cache miss
```rust
#[tokio::test]
async fn test_write_through_only_on_miss() {
    // Setup: Content already in disk cache
    // Action: Trigger fetch path (memory miss)
    // Assert:
    //   - Disk cache file NOT overwritten
    //   - Original mtime preserved
    //   - No unnecessary I/O
}
```

#### TC-W2: Write-through validates size before skip
```rust
#[tokio::test]
async fn test_write_through_checks_size() {
    // Setup: Disk cache has file with wrong size (partial write)
    // Action: Read file via VFS
    // Assert:
    //   - S3 fetch triggered
    //   - Disk cache entry replaced
    //   - Correct size after replacement
}
```

#### TC-W3: Atomic write prevents partial files
```rust
#[tokio::test]
async fn test_atomic_write() {
    // Setup: Empty disk cache
    // Action: Write large file, simulate crash mid-write
    // Assert:
    //   - No partial file at final path
    //   - Temp file cleaned up (or left for manual cleanup)
    //   - Subsequent read fetches from S3
}
```

#### TC-W4: Write-through with disk full
```rust
#[tokio::test]
async fn test_write_through_disk_full() {
    // Setup: Mock filesystem that returns ENOSPC
    // Action: Read file via VFS
    // Assert:
    //   - Data still returned correctly (from S3/memory)
    //   - Error logged but not propagated
    //   - VFS continues functioning
}
```

#### TC-W5: Write-through respects max cache size
```rust
#[tokio::test]
async fn test_write_through_respects_max_size() {
    // Setup: Cache at 90% capacity, max_size = 1GB
    // Action: Fetch 200MB file
    // Assert:
    //   - LRU entries evicted to make room
    //   - New file cached
    //   - Total size <= max_size
}
```

### Cache Integrity Tests

#### TC-I1: Hash verification on read
```rust
#[tokio::test]
async fn test_hash_verification_on_read() {
    // Setup: Disk cache file with valid size but wrong content
    // Action: Read file via VFS
    // Assert:
    //   - Hash mismatch detected
    //   - S3 fetch triggered
    //   - Cache entry replaced
}
```

#### TC-I2: Startup scan rebuilds state
```rust
#[tokio::test]
async fn test_startup_scan() {
    // Setup: Disk cache with existing files, no in-memory state
    // Action: Create new ReadCache instance
    // Assert:
    //   - current_size reflects actual disk usage
    //   - All entries discoverable via contains()
}
```

#### TC-I3: Handle missing cache directory
```rust
#[tokio::test]
async fn test_missing_cache_dir_created() {
    // Setup: cache_dir does not exist
    // Action: Create ReadCache, write entry
    // Assert:
    //   - Directory created automatically
    //   - Entry written successfully
}
```

### Edge Cases

#### TC-E1: Empty file (zero bytes)
```rust
#[tokio::test]
async fn test_empty_file_cached() {
    // Setup: Empty file in manifest
    // Action: Read file via VFS
    // Assert:
    //   - Empty file cached to disk
    //   - Subsequent reads return empty data
    //   - No special-case failures
}
```

#### TC-E2: Very large file (multi-chunk)
```rust
#[tokio::test]
async fn test_large_file_chunks_cached_independently() {
    // Setup: 1GB file (4 chunks)
    // Action: Read first chunk only
    // Assert:
    //   - Only first chunk cached to disk
    //   - Other chunks NOT pre-fetched
    //   - Subsequent read of chunk 2 fetches and caches
}
```

#### TC-E3: Special characters in hash
```rust
#[tokio::test]
async fn test_hash_with_special_chars() {
    // Setup: Hash containing characters that might be problematic
    // Action: Cache and retrieve
    // Assert:
    //   - Filename properly escaped/encoded
    //   - Round-trip successful
}
```

#### TC-E4: Race condition - concurrent put for same hash
```rust
#[tokio::test]
async fn test_concurrent_put_same_hash() {
    // Setup: Two threads trying to cache same hash simultaneously
    // Action: Concurrent put() calls
    // Assert:
    //   - No corruption
    //   - Final file has correct content
    //   - Atomic rename ensures consistency
}
```

## Configuration

```rust
/// Configuration for the disk-based read cache.
///
/// Controls caching of immutable CAS content to disk.
#[derive(Debug, Clone)]
pub struct ReadCacheConfig {
    /// Whether disk caching is enabled.
    pub enabled: bool,
    /// Directory for CAS cache storage.
    pub cache_dir: PathBuf,
    /// Whether to write through to disk on S3 fetch.
    pub write_through: bool,
}

impl Default for ReadCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cache_dir: PathBuf::from("/tmp/vfs-cache/cas"),
            write_through: true,
        }
    }
}

// Usage in VfsOptions:
let options = VfsOptions::default()
    .with_read_cache(ReadCacheConfig::enabled(PathBuf::from("/mnt/cache/cas")));
```

## Future Considerations

1. **Cache eviction** - Implement LRU eviction when cache exceeds max size:
   - Track access times in metadata
   - Implement `ReadCache::evict_lru(target_size)`
   - Add `max_size` configuration option
2. **Shared cache across VFS instances** - Multiple mounts could share the same CAS cache
3. **Cache warming** - Pre-populate cache from manifest before job starts
4. **Compression** - Compress cached content (trade CPU for disk space)
5. **Cache statistics** - Track hit rates, eviction counts for monitoring
6. **Remote cache** - Use shared network storage for multi-node scenarios
7. **Hash verification on read** - Optional verification of cached content integrity
