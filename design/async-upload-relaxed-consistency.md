# Async Upload: Relaxed Consistency for On-Demand File Fetching

**Status:** DRAFT  
**Date:** 2026-03-21  
**Extends:** Job Attachments V2, VFS Module  

---

## Problem Statement

Studios often have terabytes of data on their on-premises network storage. They don't know upfront which files a cloud render job will actually need, and they don't want to upload everything preemptively. Today, job attachments require strong consistency: every file is hashed, uploaded to S3 CAS, and referenced by content hash in the manifest before a job can run. This creates a hard requirement to upload all input data before job submission.

### Pain Points

1. **Upload latency**: TBs of data take hours/days to upload, blocking job submission
2. **Wasted bandwidth**: Many uploaded files may never be accessed by the job
3. **Unknown access patterns**: Studios can't predict which files a render job will touch
4. **Storage duplication**: All data must exist in both on-prem and S3 simultaneously

---

## Vision

Support a mixed upload ecosystem where job attachments can contain both:

- **Strongly consistent files**: Hashed, uploaded to S3 CAS before job runs (existing behavior)
- **Relaxed consistency files**: Referenced by path, fetched on-demand from on-prem when accessed

### Key Insight

Not all files need content-hash guarantees. For many studio workflows, "give me the latest version of this texture file" is perfectly acceptable. The file may change between job submission and job execution, and that's fine.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Cloud (AWS)                                          │
│                                                                              │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────────────────────┐  │
│  │  Deadline     │    │  S3 CAS      │    │  SQS Queues                  │  │
│  │  Worker +     │◄──►│  (strongly   │    │  ┌────────────────────────┐  │  │
│  │  VFS Mount    │    │   consistent)│    │  │ High Priority          │  │  │
│  │              │    └──────────────┘    │  │ (blocking file needs)  │  │  │
│  │              │                        │  ├────────────────────────┤  │  │
│  │              │────────────────────────►  │ Async Eventual         │  │  │
│  │              │   file request msgs    │  │ (prefetch / warm-up)   │  │  │
│  └──────┬───────┘                        │  └────────────────────────┘  │  │
│         │                                └──────────────┬───────────────┘  │
│         │ poll for uploaded file                         │                  │
│         ▼                                               │                  │
│  ┌──────────────┐                                       │                  │
│  │  S3 Pending  │                                       │                  │
│  │  Uploads     │                                       │                  │
│  │  (relaxed)   │                                       │                  │
│  └──────────────┘                                       │                  │
└─────────────────────────────────────────────────────────┼──────────────────┘
                                                          │
                                                          │ SQS poll
                                                          ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                         On-Premises                                          │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │  Upload Agent (Waiter Service)                                        │  │
│  │  - Polls SQS queues for file requests                                │  │
│  │  - Reads files from network storage                                  │  │
│  │  - Hashes and uploads to S3 CAS                                      │  │
│  │  - Writes completion marker to S3 Pending Uploads                    │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
│                              │                                              │
│                              ▼                                              │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │  Network Storage (NAS/SAN)                                            │  │
│  │  /mnt/studio/projects/...  (TBs of data)                            │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```


---

## Data Structures

### Manifest Extension: Relaxed Consistency Roots

The `Attachments` struct (from `job-submission.md`) gains a new field to declare folders that use relaxed consistency. These folders are path-mapped but have no content hashes at submission time.

```rust
/// A folder root that uses relaxed consistency (on-demand upload).
/// Files under this root are not hashed or uploaded at submission time.
/// Instead, they are fetched on-demand when accessed by the VFS.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaxedConsistencyRoot {
    /// Source path on the submitter's machine (e.g., "/mnt/studio/projects/film1").
    pub source_path: String,
    /// Path format of the source path.
    pub source_path_format: PathFormat,
    /// Stable identifier for this root, derived from source_path.
    /// Used as the S3 prefix for pending uploads and queue message routing.
    /// Generated via: SHAKE-256(source_path)[..20 hex chars]
    pub root_id: String,
    /// Optional file system location name (from storage profile).
    pub file_system_location_name: Option<String>,
}

/// Extended job attachments payload with relaxed consistency support.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachments {
    /// Strongly consistent manifests (existing behavior).
    pub manifests: Vec<ManifestProperties>,
    /// File system mode: "COPIED" or "VIRTUAL".
    pub file_system: String,
    /// Relaxed consistency roots for on-demand upload (new).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relaxed_roots: Vec<RelaxedConsistencyRoot>,
}
```

### JSON Output (Extended)

```json
{
  "manifests": [
    {
      "rootPath": "/mnt/studio/projects/film1/shots/sh010",
      "rootPathFormat": "posix",
      "inputManifestPath": "farm-123/queue-456/Inputs/abc123_input",
      "inputManifestHash": "def456789...",
      "fileSystemLocationName": "ProjectFiles"
    }
  ],
  "relaxedRoots": [
    {
      "sourcePath": "/mnt/studio/projects/film1/assets",
      "sourcePathFormat": "posix",
      "rootId": "a1b2c3d4e5f6a7b8c9d0",
      "fileSystemLocationName": "StudioAssets"
    },
    {
      "sourcePath": "/mnt/studio/projects/film1/textures",
      "sourcePathFormat": "posix",
      "rootId": "f0e1d2c3b4a5f6e7d8c9",
      "fileSystemLocationName": "StudioTextures"
    }
  ],
  "fileSystem": "VIRTUAL"
}
```

### Root ID Generation

The `root_id` provides a stable, collision-resistant identifier for a relaxed root. It is used as the S3 prefix for pending uploads and as the routing key in SQS messages.

```rust
/// Generate a stable root ID from a source path.
///
/// # Arguments
/// * `source_path` - The source path on the submitter's machine.
///
/// # Returns
/// A 20-character hex string derived from SHAKE-256 of the source path.
pub fn generate_root_id(source_path: &str) -> String {
    // SHAKE-256 truncated to 10 bytes (20 hex chars)
    // Same approach as get_unique_dest_dir_name() in path-mapping.md
    let hash: [u8; 10] = shake256(source_path.as_bytes());
    hex::encode(hash)
}
```

**Why SHAKE-256 of the path?**
- Deterministic: same path always produces the same ID
- Collision-resistant: 80 bits is sufficient for routing (not security)
- Compact: 20 hex chars fits in S3 key prefixes and SQS attributes
- Consistent with existing `get_unique_dest_dir_name()` pattern

**Alternatives considered:**
1. **Raw path as key**: Paths contain `/`, spaces, unicode — messy for S3 keys and SQS attributes
2. **UUID per root**: Not deterministic — submitter and agent would need to coordinate
3. **XXH128 of path**: Also viable, but SHAKE-256 is already used for path mapping in the codebase


---

## File Identification: Path Key

When the VFS needs a file from a relaxed root, it must communicate which file it wants. Since there's no content hash at request time, we need a different identifier.

### Approach: Hashed Relative Path

```rust
/// A key that identifies a file within a relaxed consistency root.
/// Derived from the relative path within the root, not from file content.
///
/// # Fields
/// * `root_id` - The relaxed root this file belongs to.
/// * `relative_path` - The file's path relative to the root (posix-normalized).
/// * `path_key` - XXH128 hash of the relative_path, used as the S3 object key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelaxedFileKey {
    pub root_id: String,
    pub relative_path: String,
    pub path_key: String,
}

/// Generate a path-based key for a relaxed consistency file.
///
/// # Arguments
/// * `root_id` - The relaxed root identifier.
/// * `relative_path` - File path relative to the root (posix-normalized).
///
/// # Returns
/// A `RelaxedFileKey` with the XXH128 hash of the relative path.
pub fn relaxed_file_key(root_id: &str, relative_path: &str) -> RelaxedFileKey {
    let normalized: String = normalize_for_manifest(relative_path);
    let path_key: String = xxh128_hex(normalized.as_bytes());
    RelaxedFileKey {
        root_id: root_id.to_string(),
        relative_path: normalized,
        path_key,
    }
}
```

**Why hash the path?**
- S3 keys have a 1024-byte limit; studio paths can be very long
- Hashing normalizes encoding issues (unicode, case sensitivity)
- XXH128 is already the standard hash in the codebase
- The hash is deterministic: VFS and upload agent compute the same key independently

**S3 key format for pending uploads:**
```
{root_prefix}/PendingUploads/{root_id}/{path_key}.xxh128
```

Example:
```
DeadlineCloud/PendingUploads/a1b2c3d4e5f6a7b8c9d0/7f3a9b2c1d4e5f6a7b8c9d0e1f2a3b4c.xxh128
```

**What's stored at this key?**

Once the upload agent uploads the file, it writes a small JSON completion marker:

```rust
/// Completion marker written by the upload agent after uploading a file.
/// Stored at the PendingUploads S3 key for the path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadCompletionMarker {
    /// The CAS hash of the uploaded file content.
    pub content_hash: String,
    /// Hash algorithm used.
    pub hash_algorithm: HashAlgorithm,
    /// File size in bytes.
    pub size: u64,
    /// Upload timestamp (epoch seconds).
    pub uploaded_at: f64,
    /// The original relative path (for debugging/auditing).
    pub relative_path: String,
    /// Chunk hashes if the file was chunked (>256MB).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_hashes: Option<Vec<String>>,
}
```

This is analogous to an HTTP 303 "See Other" redirect — the pending upload key doesn't contain the file data, it points to where the data lives in CAS. S3 doesn't support native redirects for `GetObject`, so we use a small JSON document instead.


---

## VFS Integration: RelaxedFileStore

The VFS currently uses the `FileStore` trait to fetch content by hash. For relaxed consistency files, we don't have a hash at mount time. We need a new content resolution layer.

### New Trait: RelaxedFileStore

```rust
/// Resolution status for a relaxed consistency file.
#[derive(Debug, Clone)]
pub enum RelaxedResolution {
    /// File has been uploaded. Content is available in CAS at this hash.
    Available {
        content_hash: String,
        hash_algorithm: HashAlgorithm,
        size: u64,
        chunk_hashes: Option<Vec<String>>,
    },
    /// File has not been uploaded yet. A request has been enqueued.
    Pending {
        /// Estimated wait time based on queue depth (if known).
        estimated_wait: Option<Duration>,
    },
    /// File request failed permanently (e.g., file not found on-prem).
    Failed {
        reason: String,
    },
}

/// Trait for resolving relaxed consistency files.
/// Implementations check S3 for completion markers and enqueue upload requests.
#[async_trait]
pub trait RelaxedFileStore: Send + Sync {
    /// Check if a relaxed file has been uploaded and resolve its CAS location.
    /// If not uploaded, enqueue an upload request.
    ///
    /// # Arguments
    /// * `key` - The relaxed file key (root_id + path_key).
    /// * `priority` - Request priority (affects which SQS queue is used).
    ///
    /// # Returns
    /// The resolution status of the file.
    async fn resolve(
        &self,
        key: &RelaxedFileKey,
        priority: RequestPriority,
    ) -> Result<RelaxedResolution, VfsError>;

    /// Poll for a previously requested file's availability.
    /// Does not re-enqueue if already pending.
    ///
    /// # Arguments
    /// * `key` - The relaxed file key.
    ///
    /// # Returns
    /// The current resolution status.
    async fn poll(&self, key: &RelaxedFileKey) -> Result<RelaxedResolution, VfsError>;
}

/// Priority level for file upload requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPriority {
    /// File is needed immediately (VFS read is blocking on it).
    High,
    /// File may be needed soon (prefetch / warm-up).
    AsyncEventual,
}
```

### VFS INode Extension for Relaxed Files

Relaxed consistency files don't have a hash at mount time. We extend `FileContent` to represent this:

```rust
/// File content source - handles strongly consistent, chunked, and relaxed files.
pub enum FileContent {
    /// Single hash for entire file (V1 and small V2 files).
    SingleHash(String),
    /// Chunk hashes for large V2 files (>256MB).
    Chunked(Vec<String>),
    /// Relaxed consistency: no hash known, resolved on-demand.
    Relaxed(RelaxedFileKey),
}
```

### Read Flow for Relaxed Files

```
read(ino, fh, offset, size):
  1. Get FileContent from INode
  2. Match on content type:
     
     FileContent::SingleHash(hash) | FileContent::Chunked(_):
       → Existing flow: pool.acquire → fetch from S3 CAS
     
     FileContent::Relaxed(key):
       → Check local read cache (disk cache) for previously resolved content
       → If cached: return from cache
       → Call relaxed_store.resolve(key, High)
       → Match on resolution:
          Available { content_hash, .. }:
            a. Update INode's FileContent to SingleHash/Chunked (promote)
            b. Fetch from S3 CAS via normal flow
            c. Cache locally
            d. Return data
          Pending { .. }:
            a. Start polling loop (configurable interval, default 30s)
            b. Each iteration: relaxed_store.poll(key)
            c. On Available: promote and return data
            d. On timeout (configurable, default 30min): return EIO
          Failed { reason }:
            → Return EIO with logged reason
```

### Promotion: Relaxed → Strongly Consistent

Once a relaxed file is resolved, its INode is "promoted" to strongly consistent. This means:
- Subsequent reads use the normal CAS fetch path (no more polling)
- The file behaves identically to a pre-uploaded file
- The promotion is in-memory only (not persisted to the manifest)

```rust
impl INodeFile {
    /// Promote a relaxed file to strongly consistent after resolution.
    ///
    /// # Arguments
    /// * `resolution` - The resolved CAS location from the upload agent.
    pub fn promote_from_relaxed(&mut self, resolution: &RelaxedResolution) {
        if let RelaxedResolution::Available {
            ref content_hash,
            hash_algorithm,
            size,
            ref chunk_hashes,
        } = resolution
        {
            self.size = *size;
            self.hash_algorithm = *hash_algorithm;
            self.content = match chunk_hashes {
                Some(chunks) if !chunks.is_empty() => FileContent::Chunked(chunks.clone()),
                _ => FileContent::SingleHash(content_hash.clone()),
            };
        }
    }
}
```


---

## Message Queue Design (SQS)

### Queue Topology

Two SQS queues per farm/queue combination, created during Deadline queue setup:

```
deadline-{farm_id}-{queue_id}-file-requests-high
deadline-{farm_id}-{queue_id}-file-requests-async
```

**Why two queues instead of priority on a single queue?**
- SQS doesn't support message priority natively
- Separate queues let the upload agent process high-priority requests first
- The agent can poll high-priority with short polling and async with long polling
- Simpler to monitor and alarm on queue depth independently

### Message Format

```rust
/// SQS message body for a file upload request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadRequest {
    /// Version of the message format.
    pub version: String,  // "2026-03-21"
    /// The relaxed root this file belongs to.
    pub root_id: String,
    /// The source path on the submitter's machine (for the upload agent to locate the file).
    pub source_root_path: String,
    /// Relative path within the root.
    pub relative_path: String,
    /// The path key (XXH128 of relative_path) — where to write the completion marker.
    pub path_key: String,
    /// S3 bucket for CAS upload and completion marker.
    pub bucket: String,
    /// S3 root prefix (e.g., "DeadlineCloud").
    pub root_prefix: String,
    /// Job ID requesting this file (for auditing/cancellation).
    pub job_id: String,
    /// Timestamp of the request (epoch seconds).
    pub requested_at: f64,
    /// Priority hint (informational — queue selection is the real priority mechanism).
    pub priority: String,  // "high" | "async_eventual"
}
```

### Message Deduplication

Multiple VFS instances (multiple workers running the same job) may request the same file simultaneously. We use SQS content-based deduplication (FIFO queues) or application-level dedup:

**Option A: FIFO queues with content-based dedup**
- Message group ID: `root_id`
- Dedup ID: `path_key`
- SQS automatically deduplicates within 5-minute window
- Downside: FIFO queues have lower throughput (300 msg/s per group, 3000 msg/s with batching)

**Option B: Standard queues with application-level dedup**
- Upload agent tracks in-flight requests in a local set
- If a file is already being uploaded, skip duplicate messages
- Higher throughput, but duplicate work is possible during agent restarts

**Recommendation for V1:** Standard queues with application-level dedup. The upload agent is the bottleneck (disk I/O + network upload), not message throughput. Duplicate messages just result in a redundant S3 HEAD check.

### Scaling Considerations

The design must handle 10,000+ pending file requests:

- SQS standard queues support unlimited in-flight messages
- The VFS tracks pending requests in a `DashMap<RelaxedFileKey, PendingState>` (lock-free)
- Polling is batched: a single background task polls S3 for all pending keys periodically, rather than one poll per file
- The polling task uses `list_objects` with prefix to check multiple completion markers in one API call

```rust
/// State tracking for pending relaxed file requests.
#[derive(Debug)]
pub struct PendingFileTracker {
    /// Map of pending file keys to their request state.
    pending: DashMap<String, PendingState>,  // path_key → state
}

/// State of a pending file request.
#[derive(Debug, Clone)]
pub struct PendingState {
    /// When the request was first made.
    pub requested_at: Instant,
    /// Number of poll attempts so far.
    pub poll_count: u32,
    /// Waiters: tasks blocked on this file.
    pub waiters: Arc<tokio::sync::Notify>,
}

impl PendingFileTracker {
    /// Register a new pending file request.
    ///
    /// # Arguments
    /// * `path_key` - The path key of the file.
    ///
    /// # Returns
    /// A `Notify` handle that will be signaled when the file becomes available.
    pub fn register(&self, path_key: &str) -> Arc<tokio::sync::Notify> {
        // ...
    }

    /// Mark a file as resolved. Wakes all waiters.
    ///
    /// # Arguments
    /// * `path_key` - The path key of the resolved file.
    pub fn resolve(&self, path_key: &str) {
        if let Some((_, state)) = self.pending.remove(path_key) {
            state.waiters.notify_waiters();
        }
    }

    /// Get all pending path keys for batch polling.
    ///
    /// # Returns
    /// A snapshot of all currently pending path keys.
    pub fn pending_keys(&self) -> Vec<String> {
        self.pending.iter().map(|e| e.key().clone()).collect()
    }
}
```


---

## S3 Layout for Pending Uploads

### Key Structure

```
{root_prefix}/PendingUploads/{root_id}/{path_key}.xxh128    ← completion marker (JSON)
{root_prefix}/Data/{content_hash}.xxh128                     ← actual file content (CAS)
```

The completion marker is a small JSON file (~200 bytes) that acts as a pointer from the path-based key to the content-addressed storage. This two-level indirection is necessary because:

1. The VFS knows the path key but not the content hash
2. The upload agent knows both (it reads the file, hashes it, uploads to CAS, then writes the marker)
3. Once the VFS reads the marker, it has the content hash and can fetch from CAS normally

### Why Not Upload Directly to the Path Key?

Storing file content at the path key would work for V1 (full file upload), but breaks the CAS deduplication model. If two different relaxed roots reference the same file content, CAS stores it once. Path-keyed storage would store it twice.

### Cleanup

Completion markers are ephemeral. They can be cleaned up:
- After the job completes (Deadline lifecycle hook)
- Via S3 lifecycle rules (e.g., expire after 7 days)
- By the upload agent after confirming the job is done

---

## Upload Agent (On-Premises Waiter Service)

The upload agent runs on the customer's on-premises infrastructure. It polls SQS for file requests, reads files from network storage, and uploads them to S3.

### Architecture

```rust
/// Configuration for the upload agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadAgentConfig {
    /// AWS region for SQS and S3.
    pub region: String,
    /// S3 bucket for CAS and pending uploads.
    pub bucket: String,
    /// S3 root prefix.
    pub root_prefix: String,
    /// Farm ID (determines which SQS queues to poll).
    pub farm_id: String,
    /// Queue ID.
    pub queue_id: String,
    /// Map of root_id → local filesystem path.
    /// The agent uses this to locate files on the network storage.
    pub root_mappings: HashMap<String, PathBuf>,
    /// Maximum concurrent uploads.
    pub max_concurrent_uploads: usize,  // Default: 8
    /// High-priority queue poll interval.
    pub high_priority_poll_interval: Duration,  // Default: 1s
    /// Async queue poll interval.
    pub async_poll_interval: Duration,  // Default: 5s
}
```

### Agent Flow

```
loop {
    1. Poll high-priority queue (short poll, up to 10 messages)
    2. For each message:
       a. Parse FileUploadRequest
       b. Resolve local path: root_mappings[root_id] / relative_path
       c. Check if file exists locally
          - If not: write a Failed marker to S3, delete SQS message
       d. Check if completion marker already exists in S3 (idempotency)
          - If yes: delete SQS message, skip
       e. Hash the file (XXH128)
       f. Check if CAS object exists (HEAD request / S3CheckCache)
          - If not: upload file to CAS ({root_prefix}/Data/{hash}.xxh128)
       g. Write completion marker to S3 PendingUploads
       h. Delete SQS message
    3. If high-priority queue was empty, poll async queue
    4. Process async messages with same flow
}
```

### V1 Scope: Full File Upload Only

For V1, the upload agent always uploads the complete file. Chunked upload (partial blocks) is deferred to V2:

- V1: File is hashed as a whole, uploaded as a single CAS object (or multipart for >256MB)
- V2 (future): File is chunked into 256MB blocks, each hashed and uploaded independently. The completion marker includes `chunk_hashes` for the VFS to fetch individual chunks.

The data structures already include `chunk_hashes: Option<Vec<String>>` in `UploadCompletionMarker`, so V2 is a backward-compatible extension. The VFS promotion logic already handles both `SingleHash` and `Chunked` via the `chunk_hashes` field.

---

## VFS Module Extension

### New Module: `relaxed/`

This is a new module within the VFS crate that extends the existing S3 CRT network module.

```
crates/vfs/src/
├── ...existing modules...
├── relaxed/                    # New: Relaxed consistency support
│   ├── mod.rs                  # Public API exports
│   ├── types.rs                # RelaxedFileKey, RelaxedResolution, RequestPriority
│   ├── store.rs                # RelaxedFileStore trait
│   ├── s3_resolver.rs          # S3-based implementation (check markers, fetch from CAS)
│   ├── sqs_requester.rs        # SQS message publisher (enqueue upload requests)
│   ├── pending_tracker.rs      # PendingFileTracker (DashMap + Notify)
│   └── poller.rs               # Background polling task
```

### Integration with Existing VFS

The `DeadlineVfs` (or `WritableDeadlineVfs`) gains an optional `RelaxedFileStore`:

```rust
pub struct DeadlineVfs {
    inodes: INodeManager,
    store: Arc<dyn FileStore>,                          // Existing: CAS content
    relaxed_store: Option<Arc<dyn RelaxedFileStore>>,   // New: relaxed resolution
    pending_tracker: Arc<PendingFileTracker>,            // New: pending state
    handles: RwLock<HashMap<u64, OpenHandle>>,
    next_handle: AtomicU64,
    pool: Arc<MemoryPool>,
    executor: Arc<AsyncExecutor>,
    options: VfsOptions,
}
```

### VfsOptions Extension

```rust
/// Configuration for relaxed consistency behavior.
#[derive(Debug, Clone)]
pub struct RelaxedConsistencyOptions {
    /// Interval between polls for pending files.
    pub poll_interval: Duration,          // Default: 30s
    /// Maximum time to wait for a file before returning EIO.
    pub max_wait_timeout: Duration,       // Default: 30min
    /// Number of S3 keys to check per batch poll.
    pub batch_poll_size: usize,           // Default: 1000
    /// Whether to prefetch relaxed files on directory listing.
    pub prefetch_on_readdir: bool,        // Default: false
}

impl VfsOptions {
    // Existing fields...
    
    /// Relaxed consistency options (None if no relaxed roots).
    pub relaxed: Option<RelaxedConsistencyOptions>,
}
```

### Background Poller

A dedicated background task periodically checks S3 for completion markers of all pending files:

```rust
/// Background task that polls S3 for completed uploads.
///
/// # Arguments
/// * `tracker` - The pending file tracker to check and resolve.
/// * `storage_client` - S3 client for checking completion markers.
/// * `bucket` - S3 bucket name.
/// * `root_prefix` - S3 root prefix.
/// * `options` - Polling configuration.
async fn poll_pending_uploads(
    tracker: Arc<PendingFileTracker>,
    storage_client: Arc<dyn StorageClient>,
    bucket: String,
    root_prefix: String,
    options: RelaxedConsistencyOptions,
) {
    loop {
        let pending_keys: Vec<String> = tracker.pending_keys();
        
        if !pending_keys.is_empty() {
            // Batch check: list_objects with prefix for each root_id
            // Group keys by root_id for efficient S3 listing
            for (root_id, keys) in group_by_root(&pending_keys) {
                let prefix: String = format!(
                    "{}/PendingUploads/{}/",
                    root_prefix, root_id
                );
                let objects: Vec<ObjectInfo> = storage_client
                    .list_objects(&bucket, &prefix)
                    .await
                    .unwrap_or_default();
                
                let existing_keys: HashSet<&str> = objects
                    .iter()
                    .map(|o| o.key.as_str())
                    .collect();
                
                for key in keys {
                    let s3_key: String = format!(
                        "{}/PendingUploads/{}/{}.xxh128",
                        root_prefix, root_id, key
                    );
                    if existing_keys.contains(s3_key.as_str()) {
                        // File uploaded! Resolve and wake waiters.
                        tracker.resolve(&key);
                    }
                }
            }
        }
        
        tokio::time::sleep(options.poll_interval).await;
    }
}
```


---

## Building the INode Tree for Relaxed Roots

At mount time, the VFS receives both strongly consistent manifests and relaxed root declarations. For relaxed roots, we don't have a file listing — we only know the root directory paths.

### The Path Traversal Problem

Consider: the submitter declares `source_path: "/mnt/shared"` as a relaxed root. Path mapping translates this to the worker's VFS mount at `/mnt/vfs/shared/`. A render app then reads `/mnt/vfs/shared/project/file.png`.

FUSE resolves paths one component at a time via `lookup()`:

```
1. lookup(root_ino, "shared")    → finds the relaxed root directory INode (created at mount)
2. lookup(shared_ino, "project") → ??? "project" doesn't exist in the INode tree
3. lookup(project_ino, "file.png") → ??? can't even get here if step 2 fails
```

The VFS has no manifest listing directories or files under a relaxed root. It must handle every intermediate path component, not just the final file.

### Approach: Auto-Vivifying Directories + Lazy File INodes

The VFS treats any `lookup` under a relaxed root as potentially valid. It cannot distinguish between a directory traversal (`project/`) and a file access (`file.png`) at lookup time — FUSE doesn't tell us. The strategy:

1. **Mount time**: Create directory INodes for each relaxed root path (after path mapping). Mark these directories with a `relaxed_root_id` so descendants can be identified.
2. **Intermediate lookups**: Any `lookup` for an unknown name under a relaxed directory creates an **ambiguous INode** — initially a directory (to allow further traversal). If a `read()` or `open()` is later called on it, it's re-typed as a file.
3. **File access**: When `open()` is called on an auto-vivified directory, the VFS knows it's actually a file. It converts the INode to a file with `FileContent::Relaxed(key)`.
4. **Directory listing**: `readdir` returns only previously auto-vivified children (best-effort).

### Concrete Walk-Through

```
App reads: /mnt/vfs/shared/project/file.png

Step 1: lookup(root_ino, "shared")
  → "shared" exists (created at mount as relaxed root dir, relaxed_root_id = "a1b2...")
  → Return INodeDir { id: 2, relaxed_root_id: Some("a1b2...") }

Step 2: lookup(shared_ino=2, "project")
  → "project" NOT in shared's children map
  → Parent has relaxed_root_id → auto-vivify
  → Create INodeDir { id: 3, name: "project", relaxed_root_id: Some("a1b2...") }
  → Add to shared's children: "project" → 3
  → Return FileAttr { ino: 3, kind: Directory, ... }

Step 3: lookup(project_ino=3, "file.png")
  → "file.png" NOT in project's children map
  → Parent has relaxed_root_id → auto-vivify
  → Compute relative_path: "project/file.png" (relative to relaxed root "/mnt/shared")
  → Create INodeDir { id: 4, name: "file.png", relaxed_root_id: Some("a1b2...") }
     (yes, a directory — we don't know it's a file yet)
  → Add to project's children: "file.png" → 4
  → Return FileAttr { ino: 4, kind: Directory, ... }

Step 4: open(ino=4, flags=O_RDONLY)
  → INode 4 is a directory, but open() with O_RDONLY means the app wants to read it as a file
  → Convert INode 4: Directory → File with FileContent::Relaxed
  → relative_path = "project/file.png"
  → key = relaxed_file_key("a1b2...", "project/file.png")
  → Return file handle

Step 5: read(ino=4, fh, offset=0, size=4096)
  → FileContent::Relaxed(key) → resolve/poll/promote flow
  → Eventually returns file data
```

### Alternative: Assume Leaf = File

A simpler heuristic: if the name contains a file extension (has a `.`), treat it as a file at `lookup` time. This avoids the directory→file conversion but is fragile (directories can have dots, files can lack extensions).

**Recommendation for V1**: Use the auto-vivify approach. It's more robust and handles edge cases like extensionless files or directories with dots in their names.

### Relaxed Root Tracking on INodeDir

```rust
pub struct INodeDir {
    id: INodeId,
    parent_id: INodeId,
    name: String,
    path: String,
    children: RwLock<HashMap<String, INodeId>>,
    /// If this directory is (or is under) a relaxed consistency root,
    /// this holds the root_id. Used to determine if auto-vivification
    /// should occur on lookup misses.
    relaxed_root_id: Option<String>,
    /// The source path of the relaxed root (for computing relative paths).
    /// Only set on the root directory itself, not descendants.
    relaxed_source_path: Option<String>,
}
```

### INode Creation for Relaxed Files

```rust
impl INodeManager {
    /// Auto-vivify a directory under a relaxed root on lookup miss.
    /// The directory inherits the relaxed_root_id from its parent.
    ///
    /// # Arguments
    /// * `parent` - Parent directory (must have relaxed_root_id set).
    /// * `name` - Name of the child to create.
    ///
    /// # Returns
    /// The INode ID of the newly created directory.
    pub fn auto_vivify_relaxed_dir(
        &self,
        parent: &INodeDir,
        name: &str,
    ) -> INodeId {
        let id: INodeId = self.next_id.fetch_add(1, Ordering::Relaxed);
        let path: String = format!("{}/{}", parent.path, name);
        let dir = INodeDir {
            id,
            parent_id: parent.id,
            name: name.to_string(),
            path,
            children: RwLock::new(HashMap::new()),
            relaxed_root_id: parent.relaxed_root_id.clone(),
            relaxed_source_path: None,  // Only the root has this
        };
        // Insert into inode map and parent's children
        // ...
        id
    }

    /// Convert an auto-vivified directory INode into a relaxed file INode.
    /// Called when open() is invoked on what turns out to be a file.
    ///
    /// # Arguments
    /// * `ino` - The INode ID to convert.
    /// * `key` - The relaxed file key for on-demand resolution.
    ///
    /// # Returns
    /// Ok if conversion succeeded, Err if the INode has children (it's a real directory).
    pub fn convert_to_relaxed_file(
        &self,
        ino: INodeId,
        key: RelaxedFileKey,
    ) -> Result<(), VfsError> {
        // Fail if the directory has children — it's been used as a real directory
        // and converting it would orphan the children.
        // ...
        let file = INodeFile {
            id: ino,
            parent_id: /* from old dir */,
            name: /* from old dir */,
            path: /* from old dir */,
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
            content: FileContent::Relaxed(key),
            hash_algorithm: HashAlgorithm::Xxh128,
            executable: false,
        };
        // Replace in inode map
        // ...
        Ok(())
    }
}
```

### Computing the Relative Path

When converting an auto-vivified INode to a relaxed file, we need the relative path from the relaxed root. This is computed by walking up the INode tree:

```rust
/// Compute the relative path of an INode within its relaxed root.
///
/// # Arguments
/// * `inode_path` - The full VFS path of the INode (e.g., "/shared/project/file.png").
/// * `root_mount_path` - The VFS mount path of the relaxed root (e.g., "/shared").
///
/// # Returns
/// The relative path (e.g., "project/file.png"), posix-normalized.
pub fn relaxed_relative_path(inode_path: &str, root_mount_path: &str) -> String {
    let relative: &str = inode_path
        .strip_prefix(root_mount_path)
        .unwrap_or(inode_path)
        .trim_start_matches('/');
    normalize_for_manifest(relative)
}
```

The SQS message then includes both the `relative_path` ("project/file.png") and the `source_root_path` ("/mnt/shared"), so the upload agent resolves the local file as `/mnt/shared/project/file.png`.

### FUSE `lookup` Implementation

```rust
fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
    let name_str: &str = match name.to_str() {
        Some(s) => s,
        None => { reply.error(libc::ENOENT); return; }
    };

    // Try existing lookup first
    if let Some(child_id) = self.inodes.lookup_child(parent, name_str) {
        let child = self.inodes.get(child_id).unwrap();
        reply.entry(&TTL, &child.to_fuser_attr(), 0);
        return;
    }

    // Check if parent is under a relaxed root
    let parent_dir: &INodeDir = match self.inodes.get_dir(parent) {
        Some(d) => d,
        None => { reply.error(libc::ENOENT); return; }
    };

    if let Some(ref _root_id) = parent_dir.relaxed_root_id {
        // Auto-vivify: create a directory INode for this unknown name.
        // It may later be converted to a file on open().
        let child_id: INodeId = self.inodes.auto_vivify_relaxed_dir(parent_dir, name_str);
        let child = self.inodes.get(child_id).unwrap();
        reply.entry(&TTL, &child.to_fuser_attr(), 0);
    } else {
        // Not under a relaxed root — normal ENOENT
        reply.error(libc::ENOENT);
    }
}
```

### Handling `getattr` and `stat` for Unresolved Files

After conversion from directory to file, the INode reports `size=0` until the file is resolved. After promotion, the correct size from the `UploadCompletionMarker` is set.

**Important**: Applications that check `stat()` size before reading (e.g., memory-mapped I/O) may behave unexpectedly with `size=0`. After resolution/promotion, the correct size is set. This is an acceptable trade-off for V1 — most render applications open files directly without pre-checking size.

### INode Lifecycle for Relaxed Paths

```
                    lookup miss under relaxed root
                              │
                              ▼
                    ┌─────────────────┐
                    │  Auto-vivified  │
                    │  Directory      │
                    │  (relaxed_root  │
                    │   _id set)      │
                    └────────┬────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
         further lookup   open(O_RDONLY)   readdir
         on children      or read()        │
              │              │              ▼
              ▼              ▼         Return known
         Auto-vivify    Convert to     children only
         more dirs      INodeFile      (best-effort)
                        with Relaxed
                        content
                             │
                             ▼
                    ┌─────────────────┐
                    │  Relaxed File   │
                    │  (size=0,       │
                    │   no hash)      │
                    └────────┬────────┘
                             │
                          read()
                             │
                             ▼
                    resolve → poll → promote
                             │
                             ▼
                    ┌─────────────────┐
                    │  Promoted File  │
                    │  (size=N,       │
                    │   SingleHash    │
                    │   or Chunked)   │
                    └─────────────────┘
```

---

## Sequence Diagram: End-to-End Flow

```
Submitter                  Deadline API              Worker VFS              SQS              Upload Agent           S3
    │                          │                        │                    │                    │                  │
    │  CreateJob(attachments   │                        │                    │                    │                  │
    │  + relaxedRoots)         │                        │                    │                    │                  │
    │─────────────────────────►│                        │                    │                    │                  │
    │                          │                        │                    │                    │                  │
    │                          │  Schedule job          │                    │                    │                  │
    │                          │───────────────────────►│                    │                    │                  │
    │                          │                        │                    │                    │                  │
    │                          │                        │ Mount VFS          │                    │                  │
    │                          │                        │ (manifests +       │                    │                  │
    │                          │                        │  relaxed roots)    │                    │                  │
    │                          │                        │                    │                    │                  │
    │                          │                        │ App reads          │                    │                  │
    │                          │                        │ /assets/tex.png    │                    │                  │
    │                          │                        │                    │                    │                  │
    │                          │                        │ Check S3 marker ──────────────────────────────────────────►│
    │                          │                        │◄──────────────────────────────────── 404 Not Found        │
    │                          │                        │                    │                    │                  │
    │                          │                        │ Enqueue request ──►│                    │                  │
    │                          │                        │                    │                    │                  │
    │                          │                        │                    │  Poll high queue   │                  │
    │                          │                        │                    │◄───────────────────│                  │
    │                          │                        │                    │                    │                  │
    │                          │                        │                    │                    │ Read local file  │
    │                          │                        │                    │                    │ Hash (XXH128)    │
    │                          │                        │                    │                    │ Upload to CAS ──►│
    │                          │                        │                    │                    │                  │
    │                          │                        │                    │                    │ Write marker ───►│
    │                          │                        │                    │                    │                  │
    │                          │                        │ Poll (30s) ───────────────────────────────────────────────►│
    │                          │                        │◄──────────────────────────────────── Marker found!        │
    │                          │                        │                    │                    │                  │
    │                          │                        │ Read marker JSON   │                    │                  │
    │                          │                        │ Promote INode      │                    │                  │
    │                          │                        │ Fetch from CAS ──────────────────────────────────────────►│
    │                          │                        │◄──────────────────────────────────── File content         │
    │                          │                        │                    │                    │                  │
    │                          │                        │ Return data to app │                    │                  │
    │                          │                        │                    │                    │                  │
```


---

## V1 vs V2 Scope

### V1 (This Design)

| Feature | Scope |
|---------|-------|
| Full file upload only | ✅ Upload agent uploads entire files |
| Completion marker with CAS hash | ✅ JSON pointer to CAS |
| Two SQS priority queues | ✅ High + async eventual |
| VFS polling with configurable interval | ✅ Default 30s |
| INode promotion after resolution | ✅ Relaxed → SingleHash |
| 10,000+ pending file tracking | ✅ DashMap + batch polling |
| Standard SQS queues + app-level dedup | ✅ Simpler, higher throughput |
| Lazy INode creation for relaxed paths | ✅ On-demand via lookup |

### V2 (Future)

| Feature | Notes |
|---------|-------|
| Chunked upload (partial blocks) | Upload agent chunks >256MB files, writes chunk_hashes in marker |
| Streaming upload | Start serving partial content while upload is in progress |
| SNS fan-out | Notify multiple VFS instances when a file is uploaded (push vs poll) |
| Directory enumeration | Upload agent sends directory listings for relaxed roots |
| File change detection | Detect on-prem file changes and invalidate CAS entries |
| Bandwidth throttling | Rate-limit upload agent to avoid saturating on-prem network |

### V1 → V2 Migration Path

The data structures are designed for forward compatibility:
- `UploadCompletionMarker.chunk_hashes` is `Option<Vec<String>>` — V1 writes `None`, V2 writes chunk hashes
- `FileContent::Relaxed` promotion handles both `SingleHash` and `Chunked` via the same code path
- SQS message format includes a `version` field for future schema evolution
- The `RelaxedFileStore` trait can be extended with new methods without breaking existing implementations

---

## Error Handling and Edge Cases

### File Not Found On-Prem

The upload agent writes a failure marker:

```rust
/// Failure marker written when the upload agent cannot find the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFailureMarker {
    /// Reason for failure.
    pub reason: String,
    /// Timestamp of the failure.
    pub failed_at: f64,
    /// The original relative path.
    pub relative_path: String,
}
```

Stored at the same S3 key as the completion marker, but with a different content type or a `"status": "failed"` field. The VFS reads this and returns `EIO` to the application.

### Upload Agent Offline

If the upload agent is offline, SQS messages accumulate. When the agent comes back online, it processes the backlog. The VFS continues polling and will eventually get the files. The `max_wait_timeout` (default 30min) prevents indefinite blocking.

### Duplicate Requests from Multiple Workers

Multiple workers running the same job may request the same file. This is handled at two levels:
1. SQS: Duplicate messages are harmless — the upload agent checks for existing CAS objects before uploading
2. S3: The completion marker is written idempotently (same content hash → same marker)

### Race Condition: File Changes During Upload

If a file on the on-prem storage changes while the upload agent is reading it:
- The uploaded content may be a mix of old and new data (torn read)
- This is acceptable under relaxed consistency — the studio opted into this trade-off
- For V2, we could add optional checksum verification on the upload agent side

### VFS Unmount with Pending Files

On unmount, the VFS:
1. Cancels all pending poll tasks
2. Returns `EIO` to any blocked reads
3. Does not attempt to cancel SQS messages (they'll expire via visibility timeout)

---

## Alternatives Considered

### 1. SNS Instead of SQS

SNS could push notifications to the VFS when files are uploaded, eliminating polling. However:
- VFS instances are ephemeral (worker lifetime) — managing SNS subscriptions adds complexity
- SNS → SQS fan-out is possible but adds another queue anyway
- Polling is simpler for V1 and sufficient for 30s latency tolerance

**Verdict:** Defer to V2. SNS fan-out can be added as an optimization without changing the core design.

### 2. S3 Event Notifications

S3 can trigger Lambda or SQS on `PutObject` events for the PendingUploads prefix. The VFS could subscribe to these events instead of polling.

**Verdict:** Good optimization for V2. Requires additional infrastructure setup per farm/queue.

### 3. Direct S3 Transfer (No SQS)

The VFS could write a "request" object to S3, and the upload agent could poll S3 for requests.

**Verdict:** SQS is better for this pattern — it provides exactly-once delivery semantics, visibility timeouts, and dead-letter queues. S3 polling is more expensive and less reliable.

### 4. AWS Transfer Family / DataSync

Use managed AWS services for on-prem → S3 transfer.

**Verdict:** These are batch-oriented services, not suitable for on-demand single-file transfers with sub-minute latency requirements.

---

## Open Questions for Discussion

1. **Directory enumeration**: Should the upload agent periodically send directory listings for relaxed roots? This would allow `readdir` to return actual file names instead of empty directories. Trade-off: more SQS traffic and S3 storage for directory metadata.

2. **File size reporting**: The VFS reports `size=0` for unresolved relaxed files. Some applications (e.g., memory-mapped I/O) may fail. Should we require the submitter to provide a size hint in the relaxed root declaration? Or should the upload agent pre-scan and upload a lightweight "directory manifest" with sizes?

3. **Authentication**: The upload agent needs AWS credentials to access SQS and S3. Should it use IAM roles (if on EC2), Deadline Cloud credentials, or a separate credential mechanism for on-prem?

4. **Queue lifecycle**: Who creates/deletes the SQS queues? Options:
   - Deadline service creates them when a queue is configured for relaxed consistency
   - The upload agent creates them on first run
   - CloudFormation/CDK as part of farm setup

5. **Cancellation**: If a job is cancelled, should pending SQS messages be purged? Or let them expire naturally? Purging saves upload bandwidth but requires additional API calls.

6. **Multi-region**: If the on-prem storage is closer to a different AWS region than the Deadline farm, should the upload agent upload to a regional S3 bucket with cross-region replication?

7. **Monitoring**: What CloudWatch metrics should the upload agent publish? Suggestions:
   - Queue depth (high and async)
   - Upload latency (time from SQS receive to completion marker written)
   - Files uploaded per minute
   - Failed uploads (file not found, permission denied)
   - Bytes uploaded per minute

---

## Related Documents

- [vfs.md](vfs.md) — VFS core design (FileStore, MemoryPool, FUSE)
- [job-submission.md](job-submission.md) — Attachments struct being extended
- [storage-design.md](storage-design.md) — StorageClient trait, CAS operations
- [path-mapping.md](path-mapping.md) — Path mapping rules, root ID generation
- [model-design.md](model-design.md) — Manifest data structures
- [storage-profiles.md](storage-profiles.md) — FileSystemLocation types
- [hash-cache.md](hash-cache.md) — S3CheckCache for dedup in upload agent
