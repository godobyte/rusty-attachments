# Rusty Attachments: Storage Module Design

## Overview

This document outlines the design for a platform-agnostic storage abstraction layer that enables upload and download operations to S3 Content-Addressable Storage (CAS). The design supports two backend implementations:

1. **Rust/CRT** - Native Rust using AWS CRT for high-performance operations (also used by Python via PyO3 bindings)
2. **WASM/JS SDK** - WebAssembly using AWS SDK for JavaScript v3

## Goals

1. **Platform Agnostic Interface** - Define traits/interfaces that can be implemented by different backends
2. **Shared Business Logic** - Manifest processing, chunking decisions, and CAS key generation in Rust core
3. **Backend Flexibility** - Swap implementations without changing application logic
4. **Async-First** - All I/O operations are async for maximum throughput
5. **Progress Reporting** - Unified progress callback mechanism across all backends

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           Application Layer                                  │
│                    (Upload/Download Orchestration)                          │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           Storage Interface                                  │
│                     (Traits: StorageClient, etc.)                           │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    ▼                               ▼
        ┌───────────────────┐           ┌───────────────────┐
        │   CRT Backend     │           │   JS SDK Backend  │
        │   (Rust + PyO3)   │           │   (WASM)          │
        └───────────────────┘           └───────────────────┘
```

---

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── model/                    # Existing - manifest models
│   │
│   ├── storage/                  # NEW - Storage abstraction
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types.rs          # Shared data structures
│   │       ├── traits.rs         # Storage traits/interfaces
│   │       ├── cas.rs            # CAS key generation, chunking logic
│   │       ├── upload.rs         # Upload orchestration
│   │       ├── download.rs       # Download orchestration
│   │       └── error.rs          # Error types
│   │
│   ├── storage-crt/              # NEW - CRT implementation
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── client.rs         # CRT-based StorageClient
│   │
│   ├── python/                   # Existing - PyO3 bindings (uses CRT backend)
│   │   └── src/
│   │       └── lib.rs
│   │
│   └── wasm/                     # Existing - WASM bindings
│       └── src/
│           └── storage.rs        # WASM storage bindings (calls JS SDK)
│
└── design/
    └── storage-design.md         # This document
```

---

## Shared Data Structures

### Constants

Constants are defined in the `common` crate and re-exported here for convenience:

```rust
// Re-exported from rusty_attachments_common::constants
pub use rusty_attachments_common::{
    CHUNK_SIZE_V2,           // 256MB - chunk size for v2 format
    CHUNK_SIZE_NONE,         // 0 - no chunking
    SMALL_FILE_THRESHOLD,    // 80MB - small vs large file threshold
};
```

### Storage Settings

```rust
/// Configuration settings for storage operations
#[derive(Debug, Clone)]
pub struct StorageSettings {
    /// AWS region
    pub region: String,
    /// AWS credentials (access key, secret key, session token)
    pub credentials: Option<AwsCredentials>,
    /// Expected bucket owner account ID for security validation
    /// When set, all S3 operations will include ExpectedBucketOwner parameter
    pub expected_bucket_owner: Option<String>,
    /// Chunk size for large files (use CHUNK_SIZE_V2 or CHUNK_SIZE_NONE)
    pub chunk_size: u64,
    /// Upload retry settings
    pub upload_retry: RetrySettings,
    /// Download retry settings
    pub download_retry: RetrySettings,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            region: "us-west-2".into(),
            credentials: None, // Use default credential chain
            expected_bucket_owner: None, // No bucket owner validation by default
            chunk_size: CHUNK_SIZE_V2,
            upload_retry: RetrySettings::default(),
            download_retry: RetrySettings::default(),
        }
    }
}

/// AWS credentials
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Retry settings for transfer operations
#[derive(Debug, Clone)]
pub struct RetrySettings {
    /// Maximum number of retry attempts
    pub max_attempts: u32,
    /// Initial backoff delay in milliseconds
    pub initial_backoff_ms: u64,
    /// Maximum backoff delay in milliseconds
    pub max_backoff_ms: u64,
    /// Backoff multiplier (exponential backoff)
    pub backoff_multiplier: f64,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 30_000,
            backoff_multiplier: 2.0,
        }
    }
}
```

### S3 Location Configuration

```rust
/// S3 bucket and prefix configuration for CAS storage
#[derive(Debug, Clone)]
pub struct S3Location {
    /// S3 bucket name
    pub bucket: String,
    /// Root prefix for all operations (e.g., "DeadlineCloud")
    pub root_prefix: String,
    /// CAS data prefix (e.g., "Data")
    pub cas_prefix: String,
    /// Manifest prefix (e.g., "Manifests")
    pub manifest_prefix: String,
}

impl S3Location {
    /// Generate the full S3 key for a CAS object
    /// Returns: "{root_prefix}/{cas_prefix}/{hash}.{algorithm}"
    pub fn cas_key(&self, hash: &str, algorithm: HashAlgorithm) -> String;
    
    /// Generate the full S3 key for a manifest
    /// Returns: "{root_prefix}/{manifest_prefix}/{path}"
    pub fn manifest_key(&self, path: &str) -> String;
}
```

### Transfer Request Types

```rust
/// Request to upload a single object to CAS
#[derive(Debug, Clone)]
pub struct CasUploadRequest {
    /// Content hash (becomes the S3 key)
    pub hash: String,
    /// Hash algorithm used
    pub algorithm: HashAlgorithm,
    /// Expected size in bytes
    pub size: u64,
    /// Data source - either file path or in-memory bytes
    pub source: DataSource,
}

/// Request to download a single object from CAS
#[derive(Debug, Clone)]
pub struct CasDownloadRequest {
    /// Content hash (the S3 key)
    pub hash: String,
    /// Hash algorithm used
    pub algorithm: HashAlgorithm,
    /// Expected size in bytes (for validation)
    pub expected_size: u64,
    /// Destination - either file path or in-memory buffer
    pub destination: DataDestination,
}

/// Source of data for upload
#[derive(Debug, Clone)]
pub enum DataSource {
    /// Read from file at path
    FilePath(String),
    /// Read from file at path, specific byte range (for chunked uploads)
    FileRange { path: String, offset: u64, length: u64 },
    /// In-memory bytes
    Bytes(Vec<u8>),
}

/// Destination for downloaded data
#[derive(Debug, Clone)]
pub enum DataDestination {
    /// Write to file at path
    FilePath(String),
    /// Write to file at path, specific offset (for chunked downloads)
    FileOffset { path: String, offset: u64 },
    /// Return as in-memory bytes
    Memory,
}
```

### Transfer Results

```rust
/// Result of a single upload operation
#[derive(Debug, Clone)]
pub struct UploadResult {
    /// The hash/key that was uploaded
    pub hash: String,
    /// Bytes transferred (0 if skipped due to existence check)
    pub bytes_transferred: u64,
    /// Whether the object was actually uploaded (false if already existed)
    pub was_uploaded: bool,
}

/// Result of a single download operation
#[derive(Debug, Clone)]
pub struct DownloadResult {
    /// The hash/key that was downloaded
    pub hash: String,
    /// Bytes transferred
    pub bytes_transferred: u64,
    /// Downloaded data (only populated if destination was Memory)
    pub data: Option<Vec<u8>>,
}

/// Aggregated statistics for batch operations
#[derive(Debug, Clone, Default)]
pub struct TransferStatistics {
    /// Total files processed
    pub files_processed: u64,
    /// Files actually transferred (not skipped)
    pub files_transferred: u64,
    /// Files skipped (already existed or unchanged)
    pub files_skipped: u64,
    /// Total bytes transferred
    pub bytes_transferred: u64,
    /// Total bytes skipped
    pub bytes_skipped: u64,
    /// Errors encountered (non-fatal)
    pub errors: Vec<TransferError>,
}
```

### Progress Reporting

Progress reporting uses the generic `ProgressCallback<T>` trait from the `common` crate:

```rust
use rusty_attachments_common::ProgressCallback;

/// Progress update for transfer operations
#[derive(Debug, Clone)]
pub struct TransferProgress {
    /// Current operation status
    pub status: ProgressStatus,
    /// Current operation type
    pub operation: OperationType,
    /// Current file/object being processed
    pub current_key: String,
    /// Bytes transferred so far for current object
    pub current_bytes: u64,
    /// Total bytes for current object
    pub current_total: u64,
    /// Overall progress: objects completed
    pub overall_completed: u64,
    /// Overall progress: total objects
    pub overall_total: u64,
    /// Overall bytes transferred
    pub overall_bytes: u64,
    /// Overall total bytes
    pub overall_total_bytes: u64,
}

/// Status of the overall transfer operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressStatus {
    /// Preparing/hashing files before upload
    PreparingInProgress,
    /// Upload in progress
    UploadInProgress,
    /// Download in progress
    DownloadInProgress,
    /// Snapshot (local copy) in progress
    SnapshotInProgress,
    /// Operation completed successfully
    Complete,
    /// Operation was cancelled
    Cancelled,
    /// Operation failed
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub enum OperationType {
    CheckingExistence,
    Uploading,
    Downloading,
    Hashing,
}

/// Summary statistics returned after operation completion or cancellation
#[derive(Debug, Clone, Default)]
pub struct SummaryStatistics {
    /// Total files processed
    pub total_files: u64,
    /// Total bytes processed
    pub total_bytes: u64,
    /// Files that were transferred
    pub processed_files: u64,
    /// Bytes that were transferred
    pub processed_bytes: u64,
    /// Files that were skipped (already existed)
    pub skipped_files: u64,
    /// Bytes that were skipped
    pub skipped_bytes: u64,
    /// Total time elapsed in seconds
    pub total_time: f64,
    /// Transfer rate in bytes per second
    pub transfer_rate: f64,
    /// Final status
    pub status: ProgressStatus,
}

/// Type alias for transfer progress callbacks.
/// Uses the generic ProgressCallback<T> from common crate.
pub type TransferProgressCallback = dyn ProgressCallback<TransferProgress>;
```

### Error Types

```rust
#[derive(Debug, Clone)]
pub enum StorageError {
    /// Object not found in S3
    NotFound { bucket: String, key: String },
    /// Access denied
    AccessDenied { bucket: String, key: String, message: String },
    /// Bucket owner mismatch (ExpectedBucketOwner validation failed)
    BucketOwnerMismatch { 
        bucket: String, 
        expected_owner: String,
        message: String,
    },
    /// Size mismatch (corruption or incomplete upload)
    SizeMismatch { key: String, expected: u64, actual: u64 },
    /// Network error
    NetworkError { message: String, retryable: bool },
    /// Local I/O error
    IoError { path: String, message: String },
    /// Operation cancelled by user
    Cancelled { partial_stats: SummaryStatistics },
    /// Other error
    Other { message: String },
}
```

---

## Storage Traits (Interface)

### Core Storage Client Trait

```rust
/// Low-level S3 operations - implemented by each backend
#[async_trait]
pub trait StorageClient: Send + Sync {
    /// Get the expected bucket owner (for security validation)
    fn expected_bucket_owner(&self) -> Option<&str>;
    
    /// Check if an object exists and return its size
    /// Returns None if object doesn't exist
    /// Includes ExpectedBucketOwner if configured
    async fn head_object(&self, bucket: &str, key: &str) -> Result<Option<u64>, StorageError>;
    
    /// Upload bytes to S3
    /// Includes ExpectedBucketOwner if configured
    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        data: &[u8],
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
    ) -> Result<(), StorageError>;
    
    /// Upload from file path to S3 (for large files, enables streaming)
    /// Includes ExpectedBucketOwner if configured
    async fn put_object_from_file(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;
    
    /// Upload a byte range from file to S3 (for chunked uploads)
    async fn put_object_from_file_range(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        offset: u64,
        length: u64,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;
    
    /// Download object to bytes
    async fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError>;
    
    /// Download object to file path (for large files, enables streaming)
    async fn get_object_to_file(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;
    
    /// Download object to file at specific offset (for chunked downloads)
    async fn get_object_to_file_offset(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        offset: u64,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;
    
    /// List objects with prefix.
    /// 
    /// This method handles S3 pagination internally, returning all objects
    /// under the given prefix. For large result sets (thousands of objects),
    /// the implementation should use the S3 ListObjectsV2 paginator.
    /// 
    /// # Returns
    /// All objects under the prefix, with their metadata including LastModified.
    async fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> Result<Vec<ObjectInfo>, StorageError>;
}

/// Information about an S3 object from list/head operations
#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    /// Unix timestamp (seconds since epoch)
    pub last_modified: Option<i64>,
    pub etag: Option<String>,
}
```

---

## Upload Module

### Upload Orchestrator

The upload orchestrator uses the `StorageClient` trait and shared business logic:

```rust
/// High-level upload operations using any StorageClient implementation
pub struct UploadOrchestrator<C: StorageClient> {
    client: C,
    location: S3Location,
}

impl<C: StorageClient> UploadOrchestrator<C> {
    /// Upload files from a manifest to CAS
    /// 
    /// This function:
    /// 1. Iterates through manifest entries
    /// 2. Generates CAS keys from hashes
    /// 3. Checks existence (skip if already uploaded)
    /// 4. Handles chunked files (>256MB)
    /// 5. Reports progress
    pub async fn upload_manifest(
        &self,
        manifest: &AssetManifest,
        source_root: &str,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError>;
    
    /// Upload a single file to CAS
    /// Returns the hash and whether it was actually uploaded
    pub async fn upload_file(
        &self,
        request: CasUploadRequest,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<UploadResult, StorageError>;
    
    /// Upload manifest JSON to S3
    pub async fn upload_manifest_file(
        &self,
        manifest: &AssetManifest,
        manifest_path: &str,
        content_type: &str,
    ) -> Result<String, StorageError>;
    
    /// Check if a CAS object already exists with correct size
    pub async fn cas_object_exists(&self, hash: &str, expected_size: u64) -> Result<bool, StorageError>;
}
```

### Upload Logic (Shared)

```rust
/// Core upload logic - shared across all backends
/// This is pure Rust, no I/O - just decision making
pub mod upload_logic {
    /// Determine if a file needs chunking
    pub fn needs_chunking(size: u64, chunk_size: u64) -> bool {
        size > chunk_size
    }
    
    /// Generate chunk information for a large file
    pub fn generate_chunks(size: u64, chunk_size: u64) -> Vec<ChunkInfo> {
        let mut chunks = Vec::new();
        let mut offset = 0u64;
        let mut index = 0usize;
        
        while offset < size {
            let length = std::cmp::min(chunk_size, size - offset);
            chunks.push(ChunkInfo { index, offset, length });
            offset += length;
            index += 1;
        }
        
        chunks
    }
    
    /// Generate CAS S3 key from hash
    pub fn cas_key(prefix: &str, hash: &str, algorithm: HashAlgorithm) -> String {
        format!("{}/{}.{}", prefix, hash, algorithm.extension())
    }
    
    /// Determine upload strategy based on file size
    pub fn upload_strategy(size: u64, chunk_size: u64) -> UploadStrategy {
        if size <= chunk_size {
            UploadStrategy::SingleObject
        } else {
            UploadStrategy::ChunkedCas
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChunkInfo {
    pub index: usize,
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum UploadStrategy {
    /// Upload as single S3 object (file hash is the key)
    SingleObject,
    /// Upload as multiple CAS objects (chunk hashes are the keys)
    ChunkedCas,
}
```

---

## Download Module

### Download Orchestrator

```rust
/// High-level download operations using any StorageClient implementation
pub struct DownloadOrchestrator<C: StorageClient> {
    client: C,
    location: S3Location,
}

impl<C: StorageClient> DownloadOrchestrator<C> {
    /// Download files from a manifest to local filesystem
    /// 
    /// This function:
    /// 1. Iterates through manifest entries
    /// 2. Resolves CAS keys from hashes
    /// 3. Handles chunked files (reassembles from chunks)
    /// 4. Handles symlinks (creates local symlinks)
    /// 5. Sets file permissions (mtime, runnable)
    /// 6. Reports progress
    pub async fn download_manifest(
        &self,
        manifest: &AssetManifest,
        destination_root: &str,
        conflict_resolution: ConflictResolution,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError>;
    
    /// Download files to pre-resolved absolute paths.
    /// 
    /// Unlike `download_manifest()` which joins manifest paths with a root,
    /// this takes paths that are already resolved to absolute local destinations.
    /// This is useful for:
    /// - Cross-storage-profile downloads where paths have been mapped
    /// - Incremental downloads where paths come from multiple manifests
    /// - Custom download destinations
    /// 
    /// # Arguments
    /// * `files` - Pre-resolved paths from `resolve_manifest_paths()`
    /// * `hash_algorithm` - Hash algorithm used by the manifest
    /// * `conflict_resolution` - How to handle existing files
    /// * `options` - Download options (size verification, mtime setting, etc.)
    /// * `progress` - Optional progress callback
    /// 
    /// # Example
    /// ```
    /// use rusty_attachments_storage::{
    ///     DownloadOrchestrator, ConflictResolution, DownloadOptions,
    ///     path_mapping::{resolve_manifest_paths, PathMappingApplier, PathFormat},
    /// };
    /// 
    /// // Download manifest and resolve paths with mapping
    /// let (manifest, metadata) = download_manifest(&client, bucket, key).await?;
    /// let applier = PathMappingApplier::new(&rules)?;
    /// let resolved = resolve_manifest_paths(
    ///     &manifest,
    ///     &metadata.asset_root,
    ///     PathFormat::Windows,
    ///     Some(&applier),
    /// )?;
    /// 
    /// // Download to resolved paths with default options (verification enabled)
    /// let stats = orchestrator.download_to_resolved_paths(
    ///     &resolved.resolved,
    ///     manifest.hash_alg(),
    ///     ConflictResolution::CreateCopy,
    ///     DownloadOptions::default(),
    ///     Some(&progress_callback),
    /// ).await?;
    /// 
    /// // Handle unmapped paths
    /// for unmapped in &resolved.unmapped {
    ///     eprintln!("Skipped (no mapping): {}", unmapped.absolute_source_path);
    /// }
    /// ```
    pub async fn download_to_resolved_paths(
        &self,
        files: &[ResolvedManifestPath],
        hash_algorithm: HashAlgorithm,
        conflict_resolution: ConflictResolution,
        options: DownloadOptions,
        progress: Option<&dyn ProgressCallback<TransferProgress>>,
    ) -> Result<TransferStatistics, StorageError>;
    
    /// Download a single file from CAS
    pub async fn download_file(
        &self,
        request: CasDownloadRequest,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<DownloadResult, StorageError>;
    
    /// Download and decode a manifest from S3
    pub async fn download_manifest_file(
        &self,
        manifest_path: &str,
    ) -> Result<AssetManifest, StorageError>;
    
    /// List output manifests for a job/step/task
    pub async fn list_output_manifests(
        &self,
        farm_id: &str,
        queue_id: &str,
        job_id: &str,
        step_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<Vec<ManifestInfo>, StorageError>;
}

#[derive(Debug, Clone, Copy)]
pub enum ConflictResolution {
    /// Skip files that already exist locally
    Skip,
    /// Overwrite existing files
    Overwrite,
    /// Create copy with suffix: "file (1).ext"
    CreateCopy,
}

/// Options for download operations.
/// 
/// Controls post-download verification and file metadata handling.
/// All verification options default to `true` for safety.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Verify downloaded file size matches manifest.
    /// 
    /// When enabled (default), after each file download the orchestrator
    /// checks that the file size on disk matches the expected size from
    /// the manifest. This catches truncated downloads or corruption.
    /// 
    /// Default: `true`
    pub verify_size: bool,
    
    /// Set file modification time from manifest.
    /// 
    /// When enabled (default), after each file download the orchestrator
    /// sets the file's mtime to match the value stored in the manifest.
    /// This preserves the original modification time from the source.
    /// 
    /// The manifest stores mtime in microseconds since Unix epoch.
    /// 
    /// Default: `true`
    pub set_mtime: bool,
    
    /// Set executable bit on runnable files (Unix only).
    /// 
    /// When enabled (default), files marked as `runnable: true` in the
    /// manifest will have their executable bit set after download.
    /// 
    /// Default: `true`
    pub set_executable: bool,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            verify_size: true,
            set_mtime: true,
            set_executable: true,
        }
    }
}

impl DownloadOptions {
    /// Create options with all verification enabled (default).
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Create options with no post-download processing.
    /// 
    /// Useful for testing or when maximum download speed is needed
    /// and verification will be done separately.
    pub fn no_verification() -> Self {
        Self {
            verify_size: false,
            set_mtime: false,
            set_executable: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManifestInfo {
    pub s3_key: String,
    pub last_modified: i64,
    pub session_action_id: String,
}
```

### Download Logic (Shared)

```rust
/// Core download logic - shared across all backends
pub mod download_logic {
    /// Determine download strategy from manifest entry
    pub fn download_strategy(entry: &ManifestFilePath) -> DownloadStrategy {
        if entry.symlink_target.is_some() {
            DownloadStrategy::Symlink
        } else if entry.chunkhashes.is_some() {
            DownloadStrategy::ChunkedCas
        } else {
            DownloadStrategy::SingleObject
        }
    }
    
    /// Generate local file path from manifest entry
    pub fn local_path(destination_root: &str, entry_path: &str) -> String {
        // Handle path separators, directory creation, etc.
    }
    
    /// Resolve conflict for existing file.
    /// 
    /// For CreateCopy mode, generates unique paths like:
    /// - "file (1).ext"
    /// - "file (2).ext"
    /// - etc.
    /// 
    /// Thread-safe: uses atomic file creation to handle concurrent downloads.
    pub fn resolve_conflict(
        path: &Path,
        resolution: ConflictResolution,
    ) -> Result<ConflictAction, FileSystemError> {
        match resolution {
            ConflictResolution::Skip => Ok(ConflictAction::Skip),
            ConflictResolution::Overwrite => Ok(ConflictAction::Overwrite),
            ConflictResolution::CreateCopy => {
                let new_path = generate_unique_copy_path(path)?;
                Ok(ConflictAction::UseNewPath(new_path))
            }
        }
    }
    
    /// Generate a unique file path for conflict resolution.
    /// 
    /// Tries "file (1).ext", "file (2).ext", etc. until finding
    /// a path that doesn't exist. Uses atomic file creation to
    /// handle race conditions in concurrent downloads.
    /// 
    /// # Algorithm
    /// 1. Start with counter = 1
    /// 2. Generate candidate path: "{stem} ({counter}){extension}"
    /// 3. Try to atomically create the file (O_CREAT | O_EXCL)
    /// 4. If file exists, increment counter and retry
    /// 5. Return the successfully created path
    pub fn generate_unique_copy_path(original: &Path) -> Result<PathBuf, FileSystemError> {
        let stem = original.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let extension = original.extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e))
            .unwrap_or_default();
        let parent = original.parent().unwrap_or(Path::new(""));
        
        let mut counter = 1u32;
        loop {
            let new_name = format!("{} ({}){}", stem, counter, extension);
            let candidate = parent.join(&new_name);
            
            // Try atomic creation
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)  // Fails if file exists
                .open(&candidate)
            {
                Ok(_) => return Ok(candidate),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    counter += 1;
                    if counter > 10000 {
                        return Err(FileSystemError::TooManyConflicts {
                            path: original.display().to_string(),
                        });
                    }
                }
                Err(e) => return Err(FileSystemError::IoError {
                    path: candidate.display().to_string(),
                    source: e,
                }),
            }
        }
    }
}

/// Action to take for a file conflict
#[derive(Debug, Clone)]
pub enum ConflictAction {
    /// Skip downloading this file
    Skip,
    /// Overwrite the existing file
    Overwrite,
    /// Use a new path (for CreateCopy mode)
    UseNewPath(PathBuf),
}

#[derive(Debug, Clone, Copy)]
pub enum DownloadStrategy {
    /// Download single CAS object
    SingleObject,
    /// Download multiple chunks and reassemble
    ChunkedCas,
    /// Create local symlink (no download)
    Symlink,
}

// ============================================================================
// Post-Download Verification and Metadata
// ============================================================================

/// Verify downloaded file size matches expected size.
/// 
/// Called after download when `DownloadOptions::verify_size` is true.
/// 
/// # Errors
/// Returns `StorageError::SizeMismatch` if sizes don't match.
pub fn verify_file_size(path: &Path, expected_size: u64) -> Result<(), StorageError> {
    let actual_size = std::fs::metadata(path)
        .map_err(|e| StorageError::IoError {
            path: path.display().to_string(),
            message: e.to_string(),
        })?
        .len();
    
    if actual_size != expected_size {
        return Err(StorageError::SizeMismatch {
            key: path.display().to_string(),
            expected: expected_size,
            actual: actual_size,
        });
    }
    
    Ok(())
}

/// Set file modification time from manifest mtime.
/// 
/// Called after download when `DownloadOptions::set_mtime` is true.
/// 
/// # Arguments
/// * `path` - Path to the downloaded file
/// * `mtime_us` - Modification time in microseconds since Unix epoch
///                (as stored in manifest)
/// 
/// # Example
/// ```rust
/// // Manifest stores mtime in microseconds
/// let mtime_us: i64 = 1703001200_000_000; // 2023-12-19T14:00:00Z
/// set_file_mtime(Path::new("/tmp/file.txt"), mtime_us)?;
/// ```
pub fn set_file_mtime(path: &Path, mtime_us: i64) -> Result<(), StorageError> {
    use std::time::{Duration, UNIX_EPOCH};
    
    // Convert microseconds to seconds and nanoseconds
    let secs = (mtime_us / 1_000_000) as u64;
    let nanos = ((mtime_us % 1_000_000) * 1000) as u32;
    
    let mtime = UNIX_EPOCH + Duration::new(secs, nanos);
    
    // Use filetime crate for cross-platform mtime setting
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(mtime))
        .map_err(|e| StorageError::IoError {
            path: path.display().to_string(),
            message: format!("failed to set mtime: {}", e),
        })
}

/// Set executable bit on a file (Unix only).
/// 
/// Called after download when `DownloadOptions::set_executable` is true
/// and the manifest entry has `runnable: true`.
/// 
/// On Windows, this is a no-op.
#[cfg(unix)]
pub fn set_file_executable(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;
    
    let metadata = std::fs::metadata(path)
        .map_err(|e| StorageError::IoError {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
    
    let mut perms = metadata.permissions();
    // Add execute bit for owner, group, and others (matching read bits)
    let mode = perms.mode();
    let new_mode = mode | ((mode & 0o444) >> 2); // Copy read bits to execute bits
    perms.set_mode(new_mode);
    
    std::fs::set_permissions(path, perms)
        .map_err(|e| StorageError::IoError {
            path: path.display().to_string(),
            message: format!("failed to set executable: {}", e),
        })
}

#[cfg(not(unix))]
pub fn set_file_executable(_path: &Path) -> Result<(), StorageError> {
    // No-op on non-Unix platforms
    Ok(())
}

/// Apply post-download processing based on options.
/// 
/// This is called after each file download to:
/// 1. Verify file size (if `options.verify_size`)
/// 2. Set modification time (if `options.set_mtime`)
/// 3. Set executable bit (if `options.set_executable` and entry is runnable)
pub fn apply_post_download_options(
    path: &Path,
    entry: &ManifestFilePath,
    options: &DownloadOptions,
) -> Result<(), StorageError> {
    // 1. Verify size
    if options.verify_size {
        if let Some(expected_size) = entry.size {
            verify_file_size(path, expected_size)?;
        }
    }
    
    // 2. Set mtime
    if options.set_mtime {
        if let Some(mtime_us) = entry.mtime {
            set_file_mtime(path, mtime_us)?;
        }
    }
    
    // 3. Set executable
    if options.set_executable && entry.runnable {
        set_file_executable(path)?;
    }
    
    Ok(())
}
```

---

## Backend Implementations

### CRT Backend (Rust + Python via PyO3)

```rust
// crates/storage-crt/src/client.rs

/// StorageClient implementation using AWS CRT
/// Used directly by Rust applications and exposed to Python via PyO3
pub struct CrtStorageClient {
    s3_client: aws_crt_s3::Client,
    settings: StorageSettings,
}

impl CrtStorageClient {
    pub fn new(settings: StorageSettings) -> Result<Self, StorageError>;
}

impl StorageClient for CrtStorageClient {
    fn expected_bucket_owner(&self) -> Option<&str> {
        self.settings.expected_bucket_owner.as_deref()
    }
    
    // Implement all trait methods using CRT APIs
    // - Uses aws-crt-s3 for high-performance transfers
    // - Automatic multipart for large objects
    // - Connection pooling and retry logic (uses settings.upload_retry / download_retry)
    // - All operations include ExpectedBucketOwner when configured
}
```

#### CRT Thread Pool Management

The AWS CRT manages its own internal thread pool for I/O operations. We do NOT need to create a separate thread pool for parallel uploads:

- **CRT handles parallelism internally**: The CRT S3 client automatically parallelizes multipart uploads and manages connection pooling
- **No external ThreadPoolExecutor needed**: Unlike the Python implementation which uses `concurrent.futures.ThreadPoolExecutor`, the CRT handles this at the C layer
- **Configuration via CRT settings**: Concurrency is controlled through CRT client configuration, not Rust-side thread pools

```rust
// CRT client configuration handles parallelism
let s3_client_config = aws_crt_s3::ClientConfig::new()
    .with_throughput_target_gbps(10.0)  // CRT auto-tunes parallelism
    .with_part_size(8 * 1024 * 1024);   // 8MB parts for multipart
```

The small/large file separation logic (see Upload Module) is still useful for:
- Prioritizing which files to start first
- Reducing wasted bandwidth on cancellation
- But the actual parallel execution is handled by CRT, not our code

### WASM/JS SDK Backend

For WASM, we use `wasm-bindgen` to call JavaScript functions that use the AWS SDK:

```rust
// crates/wasm/src/storage.rs

/// StorageClient implementation that delegates to JS SDK
pub struct JsStorageClient {
    // Holds references to JS callback functions
}

#[wasm_bindgen]
extern "C" {
    // JS functions that wrap AWS SDK v3 calls
    async fn js_head_object(bucket: &str, key: &str) -> JsValue;
    async fn js_put_object(bucket: &str, key: &str, data: &[u8]) -> JsValue;
    async fn js_get_object(bucket: &str, key: &str) -> JsValue;
    // ... etc
}

impl StorageClient for JsStorageClient {
    // Implement trait methods by calling JS functions
    // Marshal data between Rust and JS
}
```

JavaScript side (to be provided by the application):

```typescript
// Example JS SDK wrapper that Rust calls into
import { S3Client, HeadObjectCommand, PutObjectCommand, GetObjectCommand } from "@aws-sdk/client-s3";

const s3Client = new S3Client({ region: "us-west-2" });

export async function js_head_object(bucket: string, key: string): Promise<number | null> {
    try {
        const response = await s3Client.send(new HeadObjectCommand({ Bucket: bucket, Key: key }));
        return response.ContentLength ?? null;
    } catch (e) {
        if (e.name === "NotFound") return null;
        throw e;
    }
}

// ... similar wrappers for other operations
```

---

## Usage Examples

### Rust (CRT Backend)

```rust
use rusty_attachments_storage::{UploadOrchestrator, S3Location, StorageSettings, CHUNK_SIZE_V2};
use rusty_attachments_storage_crt::CrtStorageClient;

let settings = StorageSettings {
    region: "us-west-2".into(),
    credentials: None, // Use default credential chain
    chunk_size: CHUNK_SIZE_V2,
    ..Default::default()
};

let client = CrtStorageClient::new(settings)?;
let location = S3Location {
    bucket: "my-bucket".into(),
    root_prefix: "DeadlineCloud".into(),
    cas_prefix: "Data".into(),
    manifest_prefix: "Manifests".into(),
};

let orchestrator = UploadOrchestrator::new(client, location);
let stats = orchestrator.upload_manifest(&manifest, "/local/root", Some(&progress_callback)).await?;
```

### WASM (JS SDK Backend)

```typescript
import { initWasm, JsStorageClient, UploadOrchestrator } from 'rusty-attachments-wasm';
import { createS3Callbacks } from './s3-callbacks';

await initWasm();

const callbacks = createS3Callbacks(s3Client);
const client = new JsStorageClient(callbacks);
const orchestrator = new UploadOrchestrator(client, location);

const stats = await orchestrator.uploadManifest(manifest, '/local/root', progressCallback);
```

---

## Implementation Plan

### Phase 1: Core Types and Traits
- [ ] Create `storage` crate
- [ ] Define shared data structures (`types.rs`)
- [ ] Define `StorageClient` trait (`traits.rs`)
- [ ] Implement CAS key generation and chunking logic (`cas.rs`)
- [ ] Define error types (`error.rs`)

### Phase 2: Upload Module
- [ ] Implement upload logic (`upload.rs`)
- [ ] Implement `UploadOrchestrator`
- [ ] Unit tests for upload logic

### Phase 3: Download Module
- [ ] Implement download logic (`download.rs`)
- [ ] Implement `DownloadOrchestrator`
- [ ] Unit tests for download logic

### Phase 4: CRT Backend
- [ ] Create `storage-crt` crate
- [ ] Implement `CrtStorageClient`
- [ ] Integration tests with LocalStack

### Phase 5: WASM Backend
- [ ] Add storage bindings to `wasm` crate
- [ ] Implement `JsStorageClient`
- [ ] Create TypeScript type definitions
- [ ] Browser tests

### Phase 6: Python Bindings
- [ ] Expose CRT backend to Python via PyO3
- [ ] Integration tests from Python

---

## Model Integration

The storage module works with the `model` crate's manifest types. Here's how they integrate:

### Manifest Types from Model Crate

```rust
// From crates/model - the unified manifest enum
pub enum Manifest {
    V2023_03_03(v2023_03_03::AssetManifest),
    V2025_12_04_beta(v2025_12_04::AssetManifest),
}

// V1 file entry - always has hash
pub struct v2023_03_03::ManifestPath {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub mtime: i64,
}

// V2 file entry - may have hash, chunkhashes, or symlink_target
pub struct v2025_12_04::ManifestFilePath {
    pub path: String,
    pub hash: Option<String>,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    pub runnable: bool,
    pub chunkhashes: Option<Vec<String>>,
    pub symlink_target: Option<String>,
    pub deleted: bool,
}
```

### Upload Business Logic Example

```rust
use rusty_attachments_model::{Manifest, HashAlgorithm};
use rusty_attachments_model::v2025_12_04::ManifestFilePath;
use rusty_attachments_storage::{
    UploadOrchestrator, S3Location, StorageSettings, 
    CasUploadRequest, DataSource, TransferStatistics, StorageError,
    upload_logic::{needs_chunking, generate_chunks},
    CHUNK_SIZE_V2,
};

impl<C: StorageClient> UploadOrchestrator<C> {
    /// Upload all files referenced by a manifest to CAS
    pub async fn upload_manifest(
        &self,
        manifest: &Manifest,
        source_root: &str,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let mut stats = TransferStatistics::default();
        let hash_alg = manifest.hash_alg();
        
        match manifest {
            Manifest::V2023_03_03(m) => {
                // V1: All files have single hash
                for entry in &m.paths {
                    let result = self.upload_v1_entry(entry, source_root, hash_alg, progress).await?;
                    stats.merge(result);
                }
            }
            Manifest::V2025_12_04_beta(m) => {
                // V2: Files may have hash, chunkhashes, or be symlinks
                for entry in &m.paths {
                    let result = self.upload_v2_entry(entry, source_root, hash_alg, progress).await?;
                    stats.merge(result);
                }
            }
        }
        
        Ok(stats)
    }
    
    /// Upload a v1 file entry (always single hash)
    async fn upload_v1_entry(
        &self,
        entry: &v2023_03_03::ManifestPath,
        source_root: &str,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let local_path = format!("{}/{}", source_root, entry.path);
        let s3_key = self.location.cas_key(&entry.hash, hash_alg);
        
        // Check if already exists
        if let Some(existing_size) = self.client.head_object(&self.location.bucket, &s3_key).await? {
            if existing_size == entry.size {
                return Ok(TransferStatistics::skipped(entry.size));
            }
        }
        
        // Upload the file
        self.client.put_object_from_file(
            &self.location.bucket,
            &s3_key,
            &local_path,
            None,
            None,
            progress,
        ).await?;
        
        Ok(TransferStatistics::uploaded(entry.size))
    }
    
    /// Upload a v2 file entry (may be single hash, chunked, or symlink)
    async fn upload_v2_entry(
        &self,
        entry: &ManifestFilePath,
        source_root: &str,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        // Skip deleted entries and symlinks (nothing to upload)
        if entry.deleted || entry.symlink_target.is_some() {
            return Ok(TransferStatistics::default());
        }
        
        let local_path = format!("{}/{}", source_root, entry.path);
        let size = entry.size.unwrap_or(0);
        
        if let Some(ref hash) = entry.hash {
            // Single file upload
            self.upload_single_file(&local_path, hash, size, hash_alg, progress).await
        } else if let Some(ref chunkhashes) = entry.chunkhashes {
            // Chunked file upload
            self.upload_chunked_file(&local_path, chunkhashes, size, hash_alg, progress).await
        } else {
            Err(StorageError::Other { 
                message: format!("Entry {} has no hash or chunkhashes", entry.path) 
            })
        }
    }
    
    /// Upload a single file to CAS
    async fn upload_single_file(
        &self,
        local_path: &str,
        hash: &str,
        size: u64,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let s3_key = self.location.cas_key(hash, hash_alg);
        
        // Check existence
        if let Some(existing_size) = self.client.head_object(&self.location.bucket, &s3_key).await? {
            if existing_size == size {
                return Ok(TransferStatistics::skipped(size));
            }
        }
        
        // Upload
        self.client.put_object_from_file(
            &self.location.bucket,
            &s3_key,
            local_path,
            None,
            None,
            progress,
        ).await?;
        
        Ok(TransferStatistics::uploaded(size))
    }
    
    /// Upload a chunked file to CAS (each chunk is a separate CAS object)
    async fn upload_chunked_file(
        &self,
        local_path: &str,
        chunkhashes: &[String],
        total_size: u64,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let chunks = generate_chunks(total_size, CHUNK_SIZE_V2);
        let mut stats = TransferStatistics::default();
        
        for (chunk, hash) in chunks.iter().zip(chunkhashes.iter()) {
            let s3_key = self.location.cas_key(hash, hash_alg);
            
            // Check existence
            if let Some(existing_size) = self.client.head_object(&self.location.bucket, &s3_key).await? {
                if existing_size == chunk.length {
                    stats.files_skipped += 1;
                    stats.bytes_skipped += chunk.length;
                    continue;
                }
            }
            
            // Upload chunk
            self.client.put_object_from_file_range(
                &self.location.bucket,
                &s3_key,
                local_path,
                chunk.offset,
                chunk.length,
                progress,
            ).await?;
            
            stats.files_transferred += 1;
            stats.bytes_transferred += chunk.length;
        }
        
        stats.files_processed = 1; // One logical file
        Ok(stats)
    }
}
```

### Download Business Logic Example

```rust
use crate::path_mapping::ResolvedManifestPath;

impl<C: StorageClient> DownloadOrchestrator<C> {
    /// Download all files from a manifest to local filesystem
    pub async fn download_manifest(
        &self,
        manifest: &Manifest,
        destination_root: &str,
        conflict_resolution: ConflictResolution,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let mut stats = TransferStatistics::default();
        let hash_alg = manifest.hash_alg();
        
        match manifest {
            Manifest::V2023_03_03(m) => {
                for entry in &m.paths {
                    let result = self.download_v1_entry(
                        entry, destination_root, hash_alg, conflict_resolution, progress
                    ).await?;
                    stats.merge(result);
                }
            }
            Manifest::V2025_12_04_beta(m) => {
                // First create directories
                for dir in &m.dirs {
                    if !dir.deleted {
                        std::fs::create_dir_all(format!("{}/{}", destination_root, dir.path))?;
                    }
                }
                
                // Then download files
                for entry in &m.paths {
                    let result = self.download_v2_entry(
                        entry, destination_root, hash_alg, conflict_resolution, progress
                    ).await?;
                    stats.merge(result);
                }
            }
        }
        
        Ok(stats)
    }
    
    /// Download files to pre-resolved absolute paths.
    /// 
    /// This is the core download function for cross-storage-profile downloads
    /// and incremental download scenarios where paths have already been resolved
    /// and mapped to local destinations.
    pub async fn download_to_resolved_paths(
        &self,
        files: &[ResolvedManifestPath],
        hash_algorithm: HashAlgorithm,
        conflict_resolution: ConflictResolution,
        progress: Option<&dyn ProgressCallback<TransferProgress>>,
    ) -> Result<TransferStatistics, StorageError> {
        let mut stats = TransferStatistics::default();
        let total_files = files.len() as u64;
        let total_bytes: u64 = files.iter()
            .filter_map(|f| f.manifest_entry.size)
            .sum();
        
        for (index, resolved) in files.iter().enumerate() {
            let entry = &resolved.manifest_entry;
            let local_path = &resolved.local_path;
            
            // Report progress
            if let Some(cb) = progress {
                let progress_update = TransferProgress {
                    status: ProgressStatus::DownloadInProgress,
                    operation: OperationType::Downloading,
                    current_key: local_path.display().to_string(),
                    current_bytes: 0,
                    current_total: entry.size.unwrap_or(0),
                    overall_completed: index as u64,
                    overall_total: total_files,
                    overall_bytes: stats.bytes_transferred,
                    overall_total_bytes: total_bytes,
                };
                if !cb.on_progress(&progress_update) {
                    return Err(StorageError::Cancelled {
                        partial_stats: SummaryStatistics::from(&stats),
                    });
                }
            }
            
            // Skip deleted entries
            if entry.deleted {
                continue;
            }
            
            // Handle symlinks
            if let Some(ref target) = entry.symlink_target {
                self.create_symlink(local_path, target)?;
                stats.files_processed += 1;
                continue;
            }
            
            // Handle conflict resolution
            let final_path = if local_path.exists() {
                match conflict_resolution {
                    ConflictResolution::Skip => {
                        stats.files_skipped += 1;
                        stats.bytes_skipped += entry.size.unwrap_or(0);
                        continue;
                    }
                    ConflictResolution::Overwrite => local_path.clone(),
                    ConflictResolution::CreateCopy => {
                        download_logic::generate_unique_copy_path(local_path)?
                    }
                }
            } else {
                local_path.clone()
            };
            
            // Create parent directories
            if let Some(parent) = final_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            
            let size = entry.size.unwrap_or(0);
            
            // Download based on entry type
            if let Some(ref hash) = entry.hash {
                // Single file download
                let s3_key = self.location.cas_key(hash, hash_algorithm);
                self.client.get_object_to_file(
                    &self.location.bucket,
                    &s3_key,
                    &final_path.to_string_lossy(),
                    progress,
                ).await?;
            } else if let Some(ref chunkhashes) = entry.chunkhashes {
                // Chunked file download - reassemble from chunks
                let chunks = upload_logic::generate_chunks(size, CHUNK_SIZE_V2);
                for (chunk, hash) in chunks.iter().zip(chunkhashes.iter()) {
                    let s3_key = self.location.cas_key(hash, hash_algorithm);
                    self.client.get_object_to_file_offset(
                        &self.location.bucket,
                        &s3_key,
                        &final_path.to_string_lossy(),
                        chunk.offset,
                        progress,
                    ).await?;
                }
            } else {
                return Err(StorageError::Other {
                    message: format!("Entry {} has no hash or chunkhashes", entry.path),
                });
            }
            
            // Set mtime
            if let Some(mtime) = entry.mtime {
                set_file_mtime(&final_path, mtime)?;
            }
            
            // Set executable bit
            #[cfg(unix)]
            if entry.runnable {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&final_path)?.permissions();
                perms.set_mode(perms.mode() | 0o111);
                std::fs::set_permissions(&final_path, perms)?;
            }
            
            stats.files_processed += 1;
            stats.files_transferred += 1;
            stats.bytes_transferred += size;
        }
        
        Ok(stats)
    }
    
    /// Create a symlink at the given path.
    fn create_symlink(&self, link_path: &Path, target: &str) -> Result<(), StorageError> {
        // Create parent directories
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::IoError {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }
        
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link_path).map_err(|e| StorageError::IoError {
                path: link_path.display().to_string(),
                message: e.to_string(),
            })?;
        }
        
        #[cfg(windows)]
        {
            // On Windows, we need to determine if target is a file or directory
            // For simplicity, try file first, then directory
            if std::os::windows::fs::symlink_file(target, link_path).is_err() {
                std::os::windows::fs::symlink_dir(target, link_path).map_err(|e| StorageError::IoError {
                    path: link_path.display().to_string(),
                    message: e.to_string(),
                })?;
            }
        }
        
        Ok(())
    }
    
    /// Download a v2 file entry
    async fn download_v2_entry(
        &self,
        entry: &ManifestFilePath,
        destination_root: &str,
        hash_alg: HashAlgorithm,
        conflict_resolution: ConflictResolution,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        // Skip deleted entries
        if entry.deleted {
            return Ok(TransferStatistics::default());
        }
        
        let local_path = format!("{}/{}", destination_root, entry.path);
        
        // Handle symlinks
        if let Some(ref target) = entry.symlink_target {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &local_path)?;
            return Ok(TransferStatistics::default());
        }
        
        // Handle conflict resolution
        if std::path::Path::new(&local_path).exists() {
            match conflict_resolution {
                ConflictResolution::Skip => return Ok(TransferStatistics::skipped(entry.size.unwrap_or(0))),
                ConflictResolution::Overwrite => { /* continue */ }
                ConflictResolution::CreateCopy => { /* generate new path */ }
            }
        }
        
        // Create parent directories
        if let Some(parent) = std::path::Path::new(&local_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        let size = entry.size.unwrap_or(0);
        
        if let Some(ref hash) = entry.hash {
            // Single file download
            let s3_key = self.location.cas_key(hash, hash_alg);
            self.client.get_object_to_file(&self.location.bucket, &s3_key, &local_path, progress).await?;
        } else if let Some(ref chunkhashes) = entry.chunkhashes {
            // Chunked file download - reassemble from chunks
            let chunks = generate_chunks(size, CHUNK_SIZE_V2);
            for (chunk, hash) in chunks.iter().zip(chunkhashes.iter()) {
                let s3_key = self.location.cas_key(hash, hash_alg);
                self.client.get_object_to_file_offset(
                    &self.location.bucket,
                    &s3_key,
                    &local_path,
                    chunk.offset,
                    progress,
                ).await?;
            }
        }
        
        // Set mtime
        if let Some(mtime) = entry.mtime {
            // Convert microseconds to system time and set
            set_file_mtime(&local_path, mtime)?;
        }
        
        // Set executable bit
        #[cfg(unix)]
        if entry.runnable {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&local_path)?.permissions();
            perms.set_mode(perms.mode() | 0o111);
            std::fs::set_permissions(&local_path, perms)?;
        }
        
        Ok(TransferStatistics::uploaded(size))
    }
}
```

### Key Integration Points

| Model Type | Storage Handling |
|------------|------------------|
| `v2023_03_03::ManifestPath` | Always single hash → single CAS object |
| `v2025_12_04::ManifestFilePath` with `hash` | Single hash → single CAS object |
| `v2025_12_04::ManifestFilePath` with `chunkhashes` | Multiple hashes → multiple CAS objects |
| `v2025_12_04::ManifestFilePath` with `symlink_target` | No upload/download, create local symlink |
| `v2025_12_04::ManifestFilePath` with `deleted: true` | Skip (nothing to transfer) |
| `v2025_12_04::ManifestDirectoryPath` | Create directory on download, skip on upload |

---

## Design Decisions

### Async Runtime

The CRT backend uses **tokio** as the async runtime. Tokio is the de-facto standard async runtime for Rust, providing:
- Efficient task scheduling and I/O polling
- Work-stealing thread pool for CPU-bound tasks
- Timers, channels, and synchronization primitives

For WASM, we use `wasm-bindgen-futures` to bridge Rust futures with JavaScript Promises - no tokio needed since the browser event loop handles async execution.

### Chunking Strategy

Chunking is sufficient for large files - no streaming needed. The 256MB chunk size provides:
- Resumable uploads (only re-upload failed chunks)
- Efficient partial file updates (only changed chunks re-uploaded)
- Reasonable memory footprint (one chunk in memory at a time)

### Small vs Large File Upload Strategy

Files are separated into two queues for optimal upload performance:

```rust
/// Default threshold for small vs large file classification.
/// Small files benefit from parallel object uploads.
/// Large files benefit from serial processing with parallel multipart.
pub const SMALL_FILE_THRESHOLD: u64 = 8 * 1024 * 1024 * 10; // 80MB default

/// Separate files into small and large queues.
pub fn separate_files_by_size<'a>(
    files: &'a [impl ManifestEntry],
    threshold: u64,
) -> (Vec<&'a impl ManifestEntry>, Vec<&'a impl ManifestEntry>) {
    let mut small_queue = Vec::new();
    let mut large_queue = Vec::new();
    
    for file in files {
        if file.size() <= threshold {
            small_queue.push(file);
        } else {
            large_queue.push(file);
        }
    }
    
    (small_queue, large_queue)
}
```

Upload strategy:
1. **Small files**: Upload in parallel using a thread pool (e.g., 10 concurrent uploads)
2. **Large files**: Upload serially, but each file uses parallel multipart internally

This prevents bandwidth waste if uploads are cancelled mid-way through multiple large files.

### Cancellation Support

Operations support cancellation via the `ProgressCallback` return value:

```rust
use rusty_attachments_common::ProgressCallback;

// The generic ProgressCallback<T> from common returns false to cancel
// See common.md for the trait definition

/// Error returned when operation is cancelled.
#[derive(Debug, Clone)]
pub struct CancelledError {
    /// Statistics at the time of cancellation.
    pub partial_stats: TransferStatistics,
    /// Message describing what was cancelled.
    pub message: String,
}
```

Cancellation flow:
1. Progress callback returns `false`
2. Current operation completes (or is aborted if backend supports it)
3. `StorageError::Cancelled` is returned with partial statistics
4. Caller can inspect `partial_stats` to see what was completed

```rust
impl<C: StorageClient> UploadOrchestrator<C> {
    async fn upload_with_cancellation(
        &self,
        files: &[ManifestEntry],
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let mut stats = TransferStatistics::default();
        
        for file in files {
            // Check for cancellation before each file
            if let Some(cb) = progress {
                if !cb.on_progress(&TransferProgress::checking(&file.path)) {
                    return Err(StorageError::Cancelled {
                        partial_stats: stats,
                        message: "Upload cancelled by user".into(),
                    });
                }
            }
            
            let result = self.upload_file(file, progress).await?;
            stats.merge(result);
        }
        
        Ok(stats)
    }
}
```

### Retry Policy

Retry logic is implemented in the `StorageClient` trait implementations (not the orchestrator), with configurable settings via `StorageSettings`:
- `upload_retry`: Settings for upload operations
- `download_retry`: Settings for download operations

This keeps the orchestrator focused on business logic while backends handle transport-level concerns.

### Credential Handling

AWS credentials are passed via `StorageSettings.credentials`. If `None`, the backend uses the default AWS credential chain (environment variables, config files, IAM roles, etc.).

### CAS Data Files

CAS data files (the actual file content) are uploaded without special headers - they are raw binary uploads keyed by their content hash. Only manifests have metadata headers (see [manifest-storage.md](manifest-storage.md)).

---

## Future Work (TODO)

### Business Logic
- [ ] Implement business logic to upload files from a manifest
- [ ] Implement logic to upload the manifest file itself to S3
- [ ] Implement business logic to download files from a manifest

### File/Folder Operations
- [ ] File folder scanning - walk directory tree, collect file metadata
- [ ] Manifest utilities:
  - [ ] Snapshot folder - create manifest from directory
  - [ ] Diff manifest - compute changes between two manifests
  - [ ] Diff a folder - compare folder state to existing manifest
  - [ ] Merge manifests - combine multiple manifests with proper time ordering

### CLI Features
- [ ] **Snapshot mode**: Copy files to a local directory structure matching S3 layout instead of uploading. Useful for offline testing, debugging, and creating portable job bundles.

### Testing & Edge Cases
- [ ] Fuzz testing with weird file paths (unicode, special chars, long paths)
- [ ] Edge cases for manifest merging (time ordering, conflict resolution)
- [ ] Compatibility testing with Python manifest file naming conventions
- [ ] S3 object tags for manifests (metadata, versioning)

### Python Compatibility
- [ ] Ensure manifest file names match Python implementation format
- [ ] Verify hash output matches Python xxhash implementation
- [ ] Test roundtrip: Python create → Rust read → Python read

---

## Related Documents

- [common.md](common.md) - Shared constants, hash utilities, progress callback
- [hash-cache.md](hash-cache.md) - Hash cache and S3 check cache designs
- [manifest-storage.md](manifest-storage.md) - Manifest upload/download with metadata
- [storage-profiles.md](storage-profiles.md) - Storage profiles and file system locations
- [job-submission.md](job-submission.md) - Converting manifests to job submission format
