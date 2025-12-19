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

```rust
/// Chunk size for manifest v2 format (256MB)
/// Files larger than this are split into chunks with individual hashes
pub const CHUNK_SIZE_V2: u64 = 256 * 1024 * 1024; // 268435456 bytes

/// No chunking (for v1 format or when chunking is disabled)
pub const CHUNK_SIZE_NONE: u64 = 0;
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

```rust
/// Progress update for transfer operations
#[derive(Debug, Clone)]
pub struct TransferProgress {
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

#[derive(Debug, Clone, Copy)]
pub enum OperationType {
    CheckingExistence,
    Uploading,
    Downloading,
    Hashing,
}

/// Callback trait for progress reporting
pub trait ProgressCallback: Send + Sync {
    /// Called with progress updates
    /// Returns false to cancel the operation
    fn on_progress(&self, progress: &TransferProgress) -> bool;
}
```

### Error Types

```rust
#[derive(Debug, Clone)]
pub enum StorageError {
    /// Object not found in S3
    NotFound { bucket: String, key: String },
    /// Access denied
    AccessDenied { bucket: String, key: String, message: String },
    /// Size mismatch (corruption or incomplete upload)
    SizeMismatch { key: String, expected: u64, actual: u64 },
    /// Network error
    NetworkError { message: String, retryable: bool },
    /// Local I/O error
    IoError { path: String, message: String },
    /// Operation cancelled by user
    Cancelled,
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
    /// Check if an object exists and return its size
    /// Returns None if object doesn't exist
    async fn head_object(&self, bucket: &str, key: &str) -> Result<Option<u64>, StorageError>;
    
    /// Upload bytes to S3
    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        data: &[u8],
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
    ) -> Result<(), StorageError>;
    
    /// Upload from file path to S3 (for large files, enables streaming)
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
    
    /// List objects with prefix
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
    pub last_modified: Option<i64>,  // Unix timestamp
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
    
    /// Resolve conflict for existing file
    pub fn resolve_conflict(
        path: &str,
        resolution: ConflictResolution,
        existing_files: &HashSet<String>,
    ) -> ConflictAction {
        // Returns Skip, Overwrite, or new path for CreateCopy
    }
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
    // Implement all trait methods using CRT APIs
    // - Uses aws-crt-s3 for high-performance transfers
    // - Automatic multipart for large objects
    // - Connection pooling and retry logic (uses settings.upload_retry / download_retry)
}
```

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

### Retry Policy

Retry logic is implemented in the `StorageClient` trait implementations (not the orchestrator), with configurable settings via `StorageSettings`:
- `upload_retry`: Settings for upload operations
- `download_retry`: Settings for download operations

This keeps the orchestrator focused on business logic while backends handle transport-level concerns.

### Credential Handling

AWS credentials are passed via `StorageSettings.credentials`. If `None`, the backend uses the default AWS credential chain (environment variables, config files, IAM roles, etc.).

---

## Open Questions

1. **Cancellation:** How do we propagate cancellation from progress callbacks through async operations? Need to design a clean cancellation token pattern that works across Rust async, WASM/JS Promises, and Python.
