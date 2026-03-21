# Storage Design Summary

**Full doc:** `design/storage-design.md`  
**Status:** ✅ IMPLEMENTED in `crates/storage/` and `crates/storage-crt/`

## Purpose
Platform-agnostic storage abstraction for S3 CAS operations with CRT and WASM backends.

## Key Types

### Configuration
- `StorageSettings`: region, credentials, expected_bucket_owner, chunk_size, retry settings
- `S3Location`: bucket, root_prefix, cas_prefix, manifest_prefix
- `UploadOptions` / `DownloadOptions`: concurrency, thresholds, verification flags

### Transfer Types
- `CasUploadRequest` / `CasDownloadRequest`: hash, algorithm, size, source/destination
- `DataSource`: FilePath, FileRange, Bytes
- `DataDestination`: FilePath, FileOffset, Memory
- `TransferProgress`: status, operation, current_key, bytes, overall progress

### Results
- `UploadResult` / `DownloadResult`: hash, bytes_transferred, was_uploaded/data
- `TransferStatistics`: files processed/transferred/skipped, bytes, errors
- `SummaryStatistics`: totals, time, transfer_rate, status

## Core Traits

### StorageClient
```rust
#[async_trait]
pub trait StorageClient: Send + Sync {
    fn expected_bucket_owner(&self) -> Option<&str>;
    async fn head_object(&self, bucket: &str, key: &str) -> Result<Option<u64>, StorageError>;
    async fn head_object_with_metadata(&self, bucket: &str, key: &str) -> Result<Option<ObjectMetadata>, StorageError>;
    async fn put_object(...) -> Result<(), StorageError>;
    async fn put_object_from_file(...) -> Result<(), StorageError>;
    async fn put_object_from_file_range(..., offset: u64, length: u64, ...) -> Result<(), StorageError>;
    async fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError>;
    async fn get_object_to_file(...) -> Result<(), StorageError>;
    async fn get_object_to_file_offset(..., offset: u64, ...) -> Result<(), StorageError>;
    async fn list_objects(&self, bucket: &str, prefix: &str) -> Result<Vec<ObjectInfo>, StorageError>;
}
```

### ContentAddressedDataCache
Unified interface for CAS storage regardless of backend:
```rust
#[async_trait]
pub trait ContentAddressedDataCache: Send + Sync {
    fn get_object_key(&self, hash: &str, algorithm: HashAlgorithm) -> String;
    async fn object_exists(&self, hash: &str, algorithm: HashAlgorithm) -> Result<bool, StorageError>;
    async fn object_size(&self, hash: &str, algorithm: HashAlgorithm) -> Result<Option<u64>, StorageError>;
    async fn put_object(&self, hash: &str, algorithm: HashAlgorithm, data: &[u8]) -> Result<(), StorageError>;
    async fn put_object_from_file(&self, hash: &str, algorithm: HashAlgorithm, file_path: &Path, ...) -> Result<(), StorageError>;
    async fn get_object(&self, hash: &str, algorithm: HashAlgorithm) -> Result<Vec<u8>, StorageError>;
    async fn get_object_to_file(&self, hash: &str, algorithm: HashAlgorithm, file_path: &Path, ...) -> Result<(), StorageError>;
}
```

Implementations:
- `S3DataCache`: Wraps StorageClient for S3 CAS, optional S3CheckCache integration
- `FileSystemDataCache`: Local filesystem CAS (for testing/offline)

### ObjectMetadata
Extended HEAD response with user-defined metadata:
```rust
struct ObjectMetadata {
    size: u64,
    last_modified: Option<i64>,
    content_type: Option<String>,
    etag: Option<String>,
    user_metadata: HashMap<String, String>,
}
```

## Orchestrators
- `UploadOrchestrator`: upload_manifest_contents(), parallel small files, CRT multipart for large
- `DownloadOrchestrator`: download_manifest_contents(), conflict resolution, post-download verification

## Conflict Resolution
- `Skip`: Don't download if exists
- `Overwrite`: Replace existing
- `CreateCopy`: Generate "file (1).ext" style names

## When to Read Full Doc
- Implementing new storage backend
- Modifying upload/download logic
- Understanding progress reporting
- Adding new transfer options
- Implementing ContentAddressedDataCache backends
