# Content-Addressable Data Cache Design

## Overview

Concrete implementations of the `ContentAddressedDataCache` trait for S3 and filesystem backends. These provide a unified interface for storing and retrieving content by hash.

## Problem Statement

The existing `ContentAddressedDataCache` trait defines the interface, but implementations are scattered or missing:
- S3 implementation was inline in upload/download code
- No filesystem implementation for local caching
- No owned (Arc-wrapped) variant for `'static` lifetime requirements

## Goals

1. Provide reusable S3 data cache implementation
2. Provide filesystem data cache for local operations
3. Support both borrowed and owned (Arc) variants
4. Integrate with S3 check cache to avoid redundant HEAD requests
5. Follow existing trait interface exactly

## Architecture

### Pyramid Structure

```
┌─────────────────────────────────────────────────────────────┐
│                    User-Facing APIs                         │
│        hash_upload, UploadOrchestrator, DownloadOrchestrator│
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│              ContentAddressedDataCache (trait)              │
│                   (Existing abstraction)                    │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌───────────────┐     ┌───────────────┐     ┌───────────────┐
│  S3DataCache  │     │OwnedS3DataCache│    │FileSystemData │
│  (borrowed)   │     │   (Arc)        │    │    Cache      │
└───────────────┘     └───────────────┘     └───────────────┘
        │                     │
        └──────────┬──────────┘
                   ▼
        ┌─────────────────────┐
        │    StorageClient    │
        │      (trait)        │
        └─────────────────────┘
```

### Component Reuse

- `StorageClient` trait - Existing S3 operations interface
- `S3CheckCache` - Existing cache for object existence checks
- `HashAlgorithm` - Existing enum from `model` crate

## Data Structures

### S3DataCache (Borrowed)

```rust
/// S3-backed content-addressable data cache.
///
/// Stores content in S3 with keys formatted as `{prefix}/{hash}.{algorithm}`.
pub struct S3DataCache<'a, C: StorageClient> {
    client: &'a C,
    bucket: String,
    key_prefix: String,
    s3_check_cache: Option<&'a S3CheckCache>,
}
```

### OwnedS3DataCache (Arc-wrapped)

```rust
/// Owned S3-backed content-addressable data cache.
///
/// Unlike `S3DataCache`, this version owns the client via `Arc`,
/// making it `'static`. Use when passing to functions requiring
/// `'static` lifetime (e.g., spawned tasks).
pub struct OwnedS3DataCache<C: StorageClient> {
    client: Arc<C>,
    bucket: String,
    key_prefix: String,
    s3_check_cache: Option<Arc<S3CheckCache>>,
}
```

### FileSystemDataCache

```rust
/// Local filesystem content-addressable data cache.
///
/// Stores content in a directory with files named `{hash}.{algorithm}`.
pub struct FileSystemDataCache {
    root_path: PathBuf,
}
```

## Implementation

### Module Location

```
crates/storage/src/
├── data_cache.rs           # NEW: All data cache implementations
├── traits.rs               # Existing ContentAddressedDataCache trait
├── s3_check_cache/         # Existing S3 check cache
└── ...
```

### S3DataCache Implementation

```rust
impl<'a, C: StorageClient> S3DataCache<'a, C> {
    /// Create a new S3 data cache.
    ///
    /// # Arguments
    /// * `client` - S3 storage client
    /// * `bucket` - S3 bucket name
    /// * `key_prefix` - Prefix for all keys (e.g., "Data")
    pub fn new(client: &'a C, bucket: impl Into<String>, key_prefix: impl Into<String>) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            key_prefix: key_prefix.into(),
            s3_check_cache: None,
        }
    }

    /// Add an S3 check cache for existence lookups.
    pub fn with_check_cache(mut self, cache: &'a S3CheckCache) -> Self {
        self.s3_check_cache = Some(cache);
        self
    }
}

#[async_trait]
impl<C: StorageClient> ContentAddressedDataCache for S3DataCache<'_, C> {
    fn get_object_key(&self, hash: &str, algorithm: HashAlgorithm) -> String {
        format!("{}/{}.{}", self.key_prefix, hash, algorithm.extension())
    }

    async fn object_exists(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<bool, StorageError> {
        let key: String = self.get_object_key(hash, algorithm);

        // Check local cache first (avoids S3 HEAD request)
        if let Some(cache) = self.s3_check_cache {
            if cache.exists(&self.bucket, &key).await {
                return Ok(true);
            }
        }

        // Check S3
        let exists: bool = self.client.head_object(&self.bucket, &key).await?.is_some();

        // Update cache if exists
        if exists {
            if let Some(cache) = self.s3_check_cache {
                cache.mark_uploaded(&self.bucket, &key).await;
            }
        }

        Ok(exists)
    }

    // ... other trait methods
}
```

### FileSystemDataCache Implementation

```rust
impl FileSystemDataCache {
    /// Create a new filesystem data cache.
    ///
    /// # Arguments
    /// * `root_path` - Root directory for the cache (must be absolute)
    ///
    /// # Errors
    /// Returns error if path is not absolute.
    pub fn new(root_path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root: PathBuf = root_path.into();
        if !root.is_absolute() {
            return Err(StorageError::InvalidConfig {
                message: format!(
                    "FileSystemDataCache root must be absolute: {}",
                    root.display()
                ),
            });
        }
        Ok(Self { root_path: root })
    }

    /// Get the full path for a cached object.
    fn get_full_path(&self, hash: &str, algorithm: HashAlgorithm) -> PathBuf {
        self.root_path.join(format!("{}.{}", hash, algorithm.extension()))
    }
}

#[async_trait]
impl ContentAddressedDataCache for FileSystemDataCache {
    fn get_object_key(&self, hash: &str, algorithm: HashAlgorithm) -> String {
        format!("{}.{}", hash, algorithm.extension())
    }

    async fn object_exists(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<bool, StorageError> {
        let path: PathBuf = self.get_full_path(hash, algorithm);
        Ok(path.exists())
    }

    async fn put_object(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        data: &[u8],
    ) -> Result<(), StorageError> {
        let path: PathBuf = self.get_full_path(hash, algorithm);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::IoError {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }

        std::fs::write(&path, data).map_err(|e| StorageError::IoError {
            path: path.display().to_string(),
            message: e.to_string(),
        })
    }

    // ... other trait methods
}
```

## Usage Examples

### S3DataCache with Check Cache

```rust
use rusty_attachments_storage::{S3DataCache, S3CheckCache};
use rusty_attachments_storage_crt::TransferManagerClient;

let client = TransferManagerClient::new(settings).await?;
let check_cache = S3CheckCache::new();

let data_cache = S3DataCache::new(&client, "my-bucket", "Data")
    .with_check_cache(&check_cache);

// Use with hash_upload
let result = hash_upload_abs_manifest(
    manifest,
    source_root,
    &data_cache,
    Some(&hash_cache),
    options,
).await?;
```

### OwnedS3DataCache for Spawned Tasks

```rust
use std::sync::Arc;
use rusty_attachments_storage::OwnedS3DataCache;

let client = Arc::new(TransferManagerClient::new(settings).await?);
let check_cache = Arc::new(S3CheckCache::new());

let data_cache = Arc::new(
    OwnedS3DataCache::new(client, "my-bucket", "Data")
        .with_check_cache(check_cache)
);

// Can be moved into spawned tasks
let result = hash_upload_abs_manifest_staged(
    manifest,
    source_root,
    data_cache,  // Arc<OwnedS3DataCache>
    hash_cache,
    options,
).await?;
```

### FileSystemDataCache for Local Operations

```rust
use rusty_attachments_storage::FileSystemDataCache;

let cache = FileSystemDataCache::new("/var/cache/rusty-attachments")?;

// Store content
cache.put_object("abc123", HashAlgorithm::Xxh128, &data).await?;

// Retrieve content
let data: Vec<u8> = cache.get_object("abc123", HashAlgorithm::Xxh128).await?;
```

## Key Design Decisions

### Why Two S3 Variants?

1. **S3DataCache (borrowed)**: Efficient when client lifetime is known, avoids Arc overhead
2. **OwnedS3DataCache (Arc)**: Required for `'static` bounds in spawned tasks

### Why Require Absolute Paths for FileSystem?

Prevents ambiguity about relative path resolution. Callers must be explicit about cache location.

### S3CheckCache Integration

The check cache is optional but recommended:
- Avoids redundant HEAD requests for recently uploaded objects
- Particularly valuable in upload loops where same hash may be checked multiple times

## Testing Strategy

1. Unit tests with mock `StorageClient`
2. Tests for S3CheckCache integration
3. Tests for FileSystemDataCache with tempdir
4. Tests for error cases (missing files, permission errors)

## Exports

```rust
// crates/storage/src/lib.rs
pub mod data_cache;

pub use data_cache::{S3DataCache, OwnedS3DataCache, FileSystemDataCache};
```

## References

- [ContentAddressedDataCache trait](../../crates/storage/src/traits.rs)
- [S3CheckCache](../../crates/storage/src/s3_check_cache/)
- [CRT Transfer Manager](./01-crt-transfer-manager.md)
