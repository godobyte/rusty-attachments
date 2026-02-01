# CRT Transfer Manager Design

## Overview

Add AWS S3 Transfer Manager support to `storage-crt` as the new default for high-performance S3 operations. The Transfer Manager provides automatic multipart uploads and parallel byte-range downloads without manual chunking logic.

## Goals

1. Make CRT with Transfer Manager the default networking layer across the project
2. Reuse existing `StorageClient` trait - no API changes for consumers
3. Provide automatic multipart uploads for large files
4. Enable parallel byte-range downloads for faster throughput
5. Maintain backward compatibility with existing `CrtStorageClient`

## Architecture

### Pyramid Structure

```
┌─────────────────────────────────────────────────────────────┐
│                    User-Facing APIs                         │
│  UploadOrchestrator, DownloadOrchestrator, hash_upload      │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                ContentAddressedDataCache                    │
│         S3DataCache, OwnedS3DataCache, FileSystemDataCache  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                     StorageClient Trait                     │
│              (Abstraction over S3 operations)               │
└─────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
┌─────────────────────────┐     ┌─────────────────────────────┐
│    CrtStorageClient     │     │  TransferManagerClient      │
│   (Basic AWS SDK S3)    │     │  (High-perf Transfer Mgr)   │
└─────────────────────────┘     └─────────────────────────────┘
```

### Component Reuse

The new `TransferManagerClient` implements the existing `StorageClient` trait, allowing:
- Drop-in replacement for `CrtStorageClient`
- All existing orchestrators and caches work unchanged
- Feature flag to select implementation at compile time

## Implementation

### New Dependencies

```toml
# crates/storage-crt/Cargo.toml
[dependencies]
aws-sdk-s3-transfer-manager = "0.1"  # Add transfer manager
```

### Module Structure

```
crates/storage-crt/src/
├── lib.rs                    # Re-exports, feature flags
├── client.rs                 # Existing CrtStorageClient
├── transfer_manager.rs       # NEW: TransferManagerClient
├── error.rs                  # Shared error types
└── config.rs                 # NEW: Shared configuration builder
```

### Shared Configuration Builder

Extract common configuration logic to reduce duplication:

```rust
// crates/storage-crt/src/config.rs

/// Shared configuration for AWS S3 clients.
pub struct S3Config {
    pub sdk_config: aws_config::SdkConfig,
    pub expected_bucket_owner: Option<String>,
}

impl S3Config {
    /// Build configuration from StorageSettings.
    ///
    /// # Arguments
    /// * `settings` - Storage settings with region and credentials
    ///
    /// # Returns
    /// Configured S3Config ready for client construction.
    pub async fn from_settings(settings: StorageSettings) -> Result<Self, StorageError> {
        let config_loader = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(settings.region.clone()));

        let config_loader = if let Some(ref creds) = settings.credentials {
            let credentials = Credentials::new(
                &creds.access_key_id,
                &creds.secret_access_key,
                creds.session_token.clone(),
                None,
                "rusty-attachments",
            );
            config_loader.credentials_provider(credentials)
        } else {
            config_loader
        };

        let sdk_config = config_loader.load().await;

        Ok(Self {
            sdk_config,
            expected_bucket_owner: settings.expected_bucket_owner,
        })
    }
}
```

### TransferManagerClient Implementation

```rust
// crates/storage-crt/src/transfer_manager.rs

/// StorageClient implementation using AWS S3 Transfer Manager.
///
/// Provides high-performance S3 operations with:
/// - Automatic multipart uploads (no manual chunking)
/// - Parallel byte-range downloads
/// - Optimized memory usage for large files
pub struct TransferManagerClient {
    /// Underlying S3 client for simple operations (HEAD, LIST).
    s3_client: S3Client,
    /// Transfer manager for high-performance uploads/downloads.
    transfer_manager: TransferManager,
    /// Expected bucket owner for security validation.
    expected_bucket_owner: Option<String>,
}

impl TransferManagerClient {
    /// Create a new transfer manager client.
    ///
    /// # Arguments
    /// * `settings` - Storage settings including region and credentials
    ///
    /// # Returns
    /// A new transfer manager client.
    pub async fn new(settings: StorageSettings) -> Result<Self, StorageError> {
        let config = S3Config::from_settings(settings).await?;
        let s3_client = S3Client::new(&config.sdk_config);

        // Create transfer manager from environment config
        let tm_config = aws_sdk_s3_transfer_manager::from_env().load().await;
        let transfer_manager = TransferManager::new(tm_config);

        Ok(Self {
            s3_client,
            transfer_manager,
            expected_bucket_owner: config.expected_bucket_owner,
        })
    }
}
```

### Key Method Implementations

#### Upload with Transfer Manager

```rust
async fn put_object(
    &self,
    bucket: &str,
    key: &str,
    data: &[u8],
    content_type: Option<&str>,
    _metadata: Option<&HashMap<String, String>>,
) -> Result<(), StorageError> {
    let stream = InputStream::from(bytes::Bytes::copy_from_slice(data));

    let mut upload = self
        .transfer_manager
        .upload()
        .bucket(bucket)
        .key(key)
        .body(stream);

    if let Some(ct) = content_type {
        upload = upload.content_type(ct);
    }

    let handle = upload
        .initiate()
        .map_err(|e| StorageError::NetworkError {
            message: format!("Failed to initiate upload: {}", e),
            retryable: true,
        })?;

    handle
        .join()
        .await
        .map_err(|e| StorageError::NetworkError {
            message: format!("Upload failed: {}", e),
            retryable: true,
        })?;

    Ok(())
}
```

#### Download with Transfer Manager

```rust
async fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError> {
    let mut handle = self
        .transfer_manager
        .download()
        .bucket(bucket)
        .key(key)
        .initiate()
        .map_err(|e| StorageError::NetworkError {
            message: format!("Failed to initiate download: {}", e),
            retryable: true,
        })?;

    let mut data: Vec<u8> = Vec::new();
    while let Some(chunk_result) = handle.body_mut().next().await {
        let chunk = chunk_result
            .map_err(|e| StorageError::NetworkError {
                message: format!("Download chunk failed: {}", e),
                retryable: true,
            })?;
        data.extend_from_slice(&chunk.data.into_bytes());
    }

    Ok(data)
}
```

### Feature Flags

```toml
# crates/storage-crt/Cargo.toml
[features]
default = ["transfer-manager"]
transfer-manager = ["aws-sdk-s3-transfer-manager"]
basic = []  # Use CrtStorageClient only
```

```rust
// crates/storage-crt/src/lib.rs

mod client;
mod config;
mod error;

#[cfg(feature = "transfer-manager")]
mod transfer_manager;

pub use client::CrtStorageClient;
pub use config::S3Config;
pub use error::CrtError;

#[cfg(feature = "transfer-manager")]
pub use transfer_manager::TransferManagerClient;

/// Default client type based on features.
#[cfg(feature = "transfer-manager")]
pub type DefaultClient = TransferManagerClient;

#[cfg(not(feature = "transfer-manager"))]
pub type DefaultClient = CrtStorageClient;
```

## Operations Delegation

For operations not optimized by Transfer Manager, delegate to the underlying S3 client:

| Operation | Implementation |
|-----------|----------------|
| `head_object` | S3Client (simple HEAD request) |
| `head_object_with_metadata` | S3Client (simple HEAD request) |
| `put_object` | TransferManager (auto multipart) |
| `put_object_from_file` | TransferManager (streaming) |
| `put_object_from_file_range` | S3Client (range reads are manual) |
| `get_object` | TransferManager (parallel download) |
| `get_object_to_file` | TransferManager (streaming) |
| `get_object_to_file_offset` | TransferManager (streaming to offset) |
| `list_objects` | S3Client (pagination) |

## Migration Path

1. Add `TransferManagerClient` alongside existing `CrtStorageClient`
2. Enable `transfer-manager` feature by default
3. Update documentation to recommend `TransferManagerClient`
4. Deprecate direct use of `CrtStorageClient` (keep for testing/fallback)

## Testing Strategy

1. Unit tests with mock S3 responses
2. Integration tests against LocalStack
3. Benchmark comparison: `CrtStorageClient` vs `TransferManagerClient`
4. Large file tests (>100MB) to verify multipart behavior

## Limitations

- Transfer Manager doesn't support custom metadata in upload builder directly
  - Workaround: Use S3Client for metadata-heavy operations
- Range uploads still require manual file reading
  - Transfer Manager optimizes whole-file uploads

## References

- [AWS SDK S3 Transfer Manager](https://docs.rs/aws-sdk-s3-transfer-manager)
- [Existing StorageClient trait](../storage/src/traits.rs)
- [Performance analysis](./perf.md)
