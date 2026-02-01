# Pipelined Hash+Upload Design

## Overview

A combined hash+upload operation that reads files once into memory, computes the hash, and uploads from the same buffer. This eliminates the double-read pattern of the current sequential approach.

## Problem Statement

Current workflow:
1. Read file → compute hash → discard buffer
2. Read file again → upload to S3

This doubles I/O for every file, which is the primary bottleneck for large uploads.

## Goals

1. Single file read per file (hash + upload from same buffer)
2. Concurrent processing across files
3. Hash deduplication (duplicate files uploaded once)
4. Memory backpressure (bounded memory usage)
5. Reuse existing `ContentAddressedDataCache` trait

## Architecture

### Pyramid Structure

```
┌─────────────────────────────────────────────────────────────┐
│                   hash_upload_abs_manifest()                │
│              (Top-level API for manifest processing)        │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    HashUploadPipeline                       │
│         (Orchestrates concurrent file processing)           │
└─────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
┌─────────────────┐ ┌─────────────────┐ ┌─────────────────────┐
│   MemoryPool    │ │UploadDeduplicator│ │  ProgressTracker   │
│ (Backpressure)  │ │ (Dedup uploads)  │ │ (Progress reports) │
└─────────────────┘ └─────────────────┘ └─────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│              ContentAddressedDataCache (trait)              │
│                   (Existing abstraction)                    │
└─────────────────────────────────────────────────────────────┘
```

### Component Reuse

- `ContentAddressedDataCache` - Existing trait for CAS operations
- `HashCache` - Existing hash cache for mtime-based lookups
- `Xxh3Hasher` - Existing hasher from `common` crate
- `Manifest` types - Existing manifest structures from `model` crate

## Data Structures

### WorkItem

```rust
/// Work item for the pipeline.
#[derive(Debug, Clone)]
pub struct WorkItem {
    /// Absolute path to the file.
    pub path: String,
    /// File size in bytes.
    pub size: u64,
    /// Modification time in microseconds.
    pub mtime: i64,
}
```

### ProcessedItem

```rust
/// Result of processing a single file.
#[derive(Debug, Clone)]
pub struct ProcessedItem {
    /// Original path.
    pub path: String,
    /// Computed hash.
    pub hash: String,
    /// File size.
    pub size: u64,
    /// Whether upload was skipped (already existed or deduplicated).
    pub upload_skipped: bool,
    /// Whether hash was from cache.
    pub hash_cached: bool,
}
```

### HashUploadResult

```rust
/// Result of hash_upload_abs_manifest operation.
#[derive(Debug)]
pub struct HashUploadResult {
    /// Updated manifest with all hashes filled in.
    pub manifest: Manifest,
    /// Transfer statistics.
    pub statistics: TransferStatistics,
    /// Detailed progress at completion.
    pub progress: HashUploadProgress,
}
```

## Implementation

### Module Structure

```
crates/storage/src/hash_upload/
├── mod.rs              # Public API, hash_upload_abs_manifest()
├── pipeline.rs         # HashUploadPipeline implementation
├── options.rs          # HashUploadOptions configuration
├── deduplication.rs    # UploadDeduplicator
├── memory_pool.rs      # MemoryPool with backpressure
└── progress.rs         # ProgressTracker
```

### Pipeline Flow

```rust
/// Process a single work item.
async fn process_item(&self, item: WorkItem) -> Result<ProcessedItem, StorageError> {
    // Step 1: Check hash cache
    let cached_hash: Option<String> = self.get_cached_hash(&item).await;

    // Step 2: Check if we can skip entirely (hash cached + exists in S3)
    if let Some(ref hash) = cached_hash {
        if self.data_cache.object_exists(hash, self.hash_alg).await? {
            return Ok(ProcessedItem::skipped(item, hash.clone()));
        }
    }

    // Step 3: Allocate memory and read file
    let _permit = self.memory_pool.allocate(item.size).await;
    let data: Vec<u8> = read_file(&item.path).await?;

    // Step 4: Compute hash (if not cached)
    let hash: String = match cached_hash {
        Some(h) => h,
        None => {
            let h: String = compute_hash(&data).await?;
            self.update_hash_cache(&item, &h).await;
            h
        }
    };

    // Step 5: Upload (with deduplication)
    let upload_skipped: bool = self.upload_with_dedup(&hash, &data).await?;

    Ok(ProcessedItem { path: item.path, hash, size: item.size, upload_skipped, ... })
}
```

### Concurrent Processing

```rust
pub async fn process(&self, items: Vec<WorkItem>) -> Result<Vec<ProcessedItem>, StorageError> {
    let results: Vec<Result<ProcessedItem, StorageError>> = stream::iter(items)
        .map(|item| self.process_item(item))
        .buffer_unordered(self.options.max_concurrency)
        .collect()
        .await;

    // Collect results, propagating first error
    results.into_iter().collect()
}
```

## Configuration

### HashUploadOptions

```rust
#[derive(Debug, Clone)]
pub struct HashUploadOptions {
    /// Maximum memory for in-flight data (default: 5GB).
    pub max_memory_bytes: u64,
    /// Maximum concurrent operations (default: 32).
    pub max_concurrency: usize,
    /// Chunk size for large files (default: 256MB).
    pub chunk_size: u64,
    /// Whether to use hash cache.
    pub use_hash_cache: bool,
    /// Whether to use S3 check cache.
    pub use_s3_check_cache: bool,
    /// Force rehash even if cached.
    pub force_rehash: bool,
}
```

### Auto-Memory Detection

```rust
/// Calculate memory based on system resources.
/// Uses heuristic: min(16GB, max(256MB, quarter_of_total, available - 1GB))
pub fn auto_memory() -> u64 {
    #[cfg(target_os = "linux")]
    {
        // Parse /proc/meminfo for MemTotal and MemAvailable
        // ...
    }
    // Default fallback: 1GB
    1024 * 1024 * 1024
}
```

## Public API

### hash_upload_abs_manifest

```rust
/// Hash and upload manifest contents in a pipelined manner.
///
/// # Arguments
/// * `manifest` - Manifest with files to process (hashes may be empty)
/// * `source_root` - Root directory where files are located
/// * `data_cache` - Content-addressable storage destination
/// * `hash_cache` - Optional hash cache for efficiency
/// * `options` - Pipeline configuration options
///
/// # Returns
/// Result containing updated manifest and statistics.
pub async fn hash_upload_abs_manifest<C: ContentAddressedDataCache + 'static>(
    manifest: Manifest,
    source_root: &str,
    data_cache: &C,
    hash_cache: Option<&HashCache>,
    options: HashUploadOptions,
) -> Result<HashUploadResult, StorageError>
```

## Integration with Existing Code

### Preserving Existing APIs

This module is an optimization, not a replacement. Existing standalone functions remain:

- `rusty_attachments_common::hash_file()` - Hash a single file
- `FileSystemScanner::snapshot()` - Hash all files in directory
- `UploadOrchestrator::upload_manifest_contents()` - Upload all manifest files

### Usage Example

```rust
use rusty_attachments_storage::hash_upload::{hash_upload_abs_manifest, HashUploadOptions};

let result = hash_upload_abs_manifest(
    manifest,
    "/source/root",
    &data_cache,
    Some(&hash_cache),
    HashUploadOptions::default(),
).await?;

println!("Uploaded {} files, {} bytes", 
    result.statistics.files_transferred,
    result.statistics.bytes_transferred);
```

## Performance Characteristics

| Metric | Sequential | Pipelined |
|--------|------------|-----------|
| File reads | 2x per file | 1x per file |
| Memory usage | Unbounded | Bounded by pool |
| Duplicate uploads | All uploaded | Deduplicated |
| Concurrency | Serial | Configurable |

## Testing Strategy

1. Unit tests with mock `ContentAddressedDataCache`
2. Tests for duplicate file deduplication
3. Tests for memory backpressure (pool exhaustion)
4. Tests for hash cache integration
5. Benchmark: sequential vs pipelined

## Dependencies

- `futures` - For `stream::iter` and `buffer_unordered`
- `tokio` - For `spawn_blocking` (hash computation)
- `bytes` - For efficient buffer handling

## References

- [Supporting Infrastructure](./05-supporting-infrastructure.md) - MemoryPool, Deduplicator, Progress
- [Data Cache](./04-data-cache.md) - ContentAddressedDataCache implementations
- [Existing hash module](../../crates/common/src/hash.rs)
