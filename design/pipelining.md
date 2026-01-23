# Hash+Upload Pipelining: Python vs Rust

**Date:** 2026-01-23

## Executive Summary

Python's snapshots library implements a **pipelined** hash+upload architecture where hashing and uploading happen concurrently for different files. Rust currently uses a **sequential** approach where all hashes are computed first, then uploads happen.

---

## Python Pipeline Architecture

### Two-Pool Design

```
┌─────────────────────────────────────────────────────────────────┐
│                    Python Hash+Upload Pipeline                   │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────────────┐      ┌──────────────────────┐         │
│  │  READ+HASH Pool      │      │  UPLOAD Pool         │         │
│  │  (ThreadPoolExecutor)│ ───► │  (ThreadPoolExecutor)│         │
│  │  max_workers=10      │      │  max_workers=10      │         │
│  └──────────────────────┘      └──────────────────────┘         │
│           │                            │                         │
│           ▼                            ▼                         │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    _MemoryPool                            │   │
│  │  - Backpressure control (max_memory_bytes)               │   │
│  │  - Blocks READ+HASH when memory exhausted                │   │
│  │  - Released after UPLOAD completes                       │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Data Flow

```
File A: [READ+HASH] ──────────────────► [UPLOAD] ──────────────────► Done
File B:              [READ+HASH] ──────────────────► [UPLOAD] ──────► Done
File C:                           [READ+HASH] ──────────────────► [UPLOAD]
                                                                      │
Time ─────────────────────────────────────────────────────────────────►
```

### Key Components

**`_MemoryPool`** (`_hash_upload_abs_manifest_pipeline.py`):
```python
class _MemoryPool:
    def __init__(self, max_bytes: int) -> None:
        self._max_bytes = max_bytes
        self._allocated_bytes = 0
        self._lock = threading.Lock()
        self._space_available = threading.Condition(self._lock)

    def allocate(self, size: int) -> None:
        """Blocks if memory limit reached."""
        with self._space_available:
            while self._allocated_bytes + size > self._max_bytes:
                self._space_available.wait()  # BACKPRESSURE
            self._allocated_bytes += size

    def release(self, size: int) -> None:
        """Releases memory after upload completes."""
        with self._space_available:
            self._allocated_bytes -= size
            self._space_available.notify_all()
```

**Hash Deduplication**:
```python
# In HashUploadPipelineBase._do_upload():
with self._uploading_hashes_lock:
    if item.chunk_hash in self._uploading_hashes:
        # Another thread is uploading this hash, wait for it
        wait_event = self._uploading_hashes[item.chunk_hash]
    else:
        # Register this hash as being uploaded
        self._uploading_hashes[item.chunk_hash] = threading.Event()

if wait_event is not None:
    wait_event.wait()  # Skip duplicate upload
    item.skipped = True
```

### Work Item Types

| Type | Use Case | Memory Behavior |
|------|----------|-----------------|
| `_ChunkWorkItem` | Files that fit in memory, or chunks of large files | Data held in memory until upload completes |
| `_StreamingWorkItem` | Files larger than `max_memory_bytes` | Two-pass: hash streaming, then upload streaming |
| `_MultipartPartWorkItem` | Parts of multipart S3 uploads | Each part held in memory during upload |

### Memory Limits

```python
MIN_MEMORY_BYTES = 256 * 1024 * 1024   # 256MB minimum
MAX_MEMORY_BYTES = 16 * 1024 * 1024 * 1024  # 16GB maximum

def _get_default_max_memory_bytes() -> int:
    mem = psutil.virtual_memory()
    quarter_of_total = mem.total // 4
    available_minus_1gb = mem.available - (1024 * 1024 * 1024)
    return min(MAX_MEMORY_BYTES, max(MIN_MEMORY_BYTES, quarter_of_total, available_minus_1gb))
```

---

## Rust Architecture (Current)

### Sequential Design

```
┌─────────────────────────────────────────────────────────────────┐
│                    Rust Upload Architecture                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Phase 1: FileSystemScanner::snapshot()                         │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  - Walk directory tree                                    │   │
│  │  - Compute hashes for all files                          │   │
│  │  - Build manifest with hashes                            │   │
│  └──────────────────────────────────────────────────────────┘   │
│                              │                                   │
│                              ▼                                   │
│  Phase 2: UploadOrchestrator::upload_manifest_contents()        │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  - Manifest already has all hashes                       │   │
│  │  - Check S3 existence for each file                      │   │
│  │  - Upload missing files with buffer_unordered(10)        │   │
│  │  - CRT handles multipart internally                      │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Data Flow

```
ALL Files: [HASH ALL] ─────────────────────────────────────────────► 
                                                                     │
           Then:                                                     │
                                                                     ▼
File A:                                              [UPLOAD] ──────► Done
File B:                                              [UPLOAD] ──────► Done
File C:                                              [UPLOAD] ──────► Done
                                                                      │
Time ─────────────────────────────────────────────────────────────────►
```

### Key Code (`upload.rs`)

```rust
pub async fn upload_manifest_contents(
    &self,
    manifest: &Manifest,  // ASSUMES hashes already computed
    source_root: &str,
    progress: Option<&dyn ProgressCallback>,
) -> Result<TransferStatistics, StorageError> {
    // Collect entries (hashes already in manifest)
    let entries: Vec<UploadEntry> = self.collect_upload_entries(manifest, source_root);
    
    // Separate small (<80MB) and large files
    let (small_files, large_files) = entries.into_iter()
        .partition(|e| e.size < self.options.small_file_threshold);
    
    // Upload in parallel (but NO concurrent hashing)
    let small_stats = self.upload_files_parallel(small_files, ...).await?;
    let large_stats = self.upload_files_parallel(large_files, ...).await?;
    
    Ok(stats)
}
```

---

## Comparison

| Aspect | Python | Rust |
|--------|--------|------|
| **Pipeline** | Yes - hash and upload concurrent | No - sequential |
| **File reads** | Once (data kept for upload) | Twice (hash, then upload) |
| **Memory management** | `_MemoryPool` with backpressure | None - CRT handles |
| **Hash deduplication** | Yes - waits for duplicates | No |
| **Combined operation** | `hash_upload_abs_manifest()` | Separate `snapshot()` + `upload()` |
| **Large file handling** | Streaming mode | CRT multipart |
| **Progress granularity** | Per-part for multipart | Per-file |

---

## Why Python Pipelines

1. **GIL (Global Interpreter Lock)**: Python's GIL means CPU-bound hashing blocks I/O. Separate thread pools escape the GIL.

2. **Memory efficiency**: Read file once → hash → upload → release. No double-read.

3. **Throughput**: While File A uploads, File B can be hashing. Better utilization of both CPU and network.

4. **Backpressure**: Memory pool prevents OOM when hashing faster than uploading.

---

## Why Rust Doesn't Need Explicit Pools

From `design/manifestv2/2026-01-10-update.md`:

> **Do NOT implement Python's explicit two-pool pipeline.** Instead:
> - Use `tokio::task::spawn_blocking` for CPU-bound hashing
> - Use `buffer_unordered` for concurrent uploads
> - Use `Semaphore` for memory backpressure
> - This is simpler, more idiomatic, and equally efficient

**Reasons:**
- No GIL - true parallelism without explicit pools
- `tokio` work-stealing runtime handles scheduling
- `spawn_blocking` moves CPU work off async threads
- `rayon` available for parallel iterators

---

## Current Gap in Rust

The current Rust implementation doesn't implement the recommended approach either. It:
1. Hashes everything first (in `FileSystemScanner`)
2. Then uploads (in `UploadOrchestrator`)

To match Python's efficiency, Rust would need:
```rust
pub async fn hash_upload_manifest(
    manifest: AbsManifest,  // With hash=None for unhashed files
    data_cache: &impl ContentAddressedDataCache,
    options: HashUploadOptions,
) -> Result<(AbsManifest, UploadStatistics), StorageError> {
    let semaphore = Arc::new(Semaphore::new(options.max_memory_permits));
    
    stream::iter(manifest.files())
        .map(|file| {
            let sem = semaphore.clone();
            async move {
                let _permit = sem.acquire().await?;  // Backpressure
                
                // Hash on blocking thread
                let (hash, data) = spawn_blocking(|| hash_file(&file.path)).await??;
                
                // Upload (async I/O)
                data_cache.put_object(&hash, &data).await?;
                
                Ok(file.with_hash(hash))
            }
        })
        .buffer_unordered(options.concurrency)
        .collect()
        .await
}
```

---

## Test Ideas

### 1. Throughput Comparison
- Same dataset, measure total time
- Python `hash_upload_abs_manifest()` vs Rust sequential

### 2. Memory Usage
- Monitor peak memory during upload
- Python should be bounded by `max_memory_bytes`
- Rust may spike during hash phase

### 3. Network Utilization
- Measure network throughput over time
- Python should show steady utilization
- Rust may show gaps between hash and upload phases

### 4. Large File Handling
- Files > 1GB
- Compare streaming vs multipart behavior

### 5. Deduplication Efficiency
- Dataset with duplicate files
- Python should upload once, Rust uploads each

---

## Concrete Test Plan

### Test Dataset
Create a test dataset with:
- 1000 small files (1KB-1MB each)
- 100 medium files (10MB-50MB each)
- 10 large files (100MB-500MB each)
- 5 duplicate files (same content, different names)

### Metrics to Capture

| Metric | How to Measure |
|--------|----------------|
| Total time | `time.perf_counter()` / `std::time::Instant` |
| Peak memory | `psutil.Process().memory_info().rss` / `/proc/self/status` |
| Network throughput | S3 transfer bytes / time |
| CPU utilization | `psutil.cpu_percent()` / `top` |
| File reads | `strace -e read` count |

### Test Script Outline

```python
# Python test
import time
import psutil
from deadline.job_attachments._snapshots._operations import hash_upload_abs_manifest

def test_python_pipeline(manifest, data_cache):
    process = psutil.Process()
    start_mem = process.memory_info().rss
    start_time = time.perf_counter()
    
    result = hash_upload_abs_manifest(manifest, data_cache)
    
    end_time = time.perf_counter()
    peak_mem = process.memory_info().rss
    
    return {
        "time": end_time - start_time,
        "peak_memory": peak_mem - start_mem,
        "bytes_transferred": result.statistics.uploaded_bytes,
    }
```

```rust
// Rust test
use std::time::Instant;

async fn test_rust_sequential(options: &SnapshotOptions, orchestrator: &UploadOrchestrator) {
    let start = Instant::now();
    
    // Phase 1: Hash
    let scanner = FileSystemScanner::new();
    let manifest = scanner.snapshot(options, None)?;
    let hash_time = start.elapsed();
    
    // Phase 2: Upload
    let stats = orchestrator.upload_manifest_contents(&manifest, source_root, None).await?;
    let total_time = start.elapsed();
    
    println!("Hash time: {:?}", hash_time);
    println!("Upload time: {:?}", total_time - hash_time);
    println!("Total time: {:?}", total_time);
}
```

---

## Implementation Options for Rust

### Option A: Pipelined Hash+Upload (Match Python)

```rust
pub async fn hash_upload_manifest(
    manifest: AbsManifest,  // With hash=None for unhashed files
    data_cache: &impl ContentAddressedDataCache,
    options: HashUploadOptions,
) -> Result<(AbsManifest, UploadStatistics), StorageError> {
    let semaphore = Arc::new(Semaphore::new(options.max_memory_permits));
    
    stream::iter(manifest.files())
        .map(|file| {
            let sem = semaphore.clone();
            async move {
                let _permit = sem.acquire().await?;  // Backpressure
                
                // Read file into memory
                let data = tokio::fs::read(&file.path).await?;
                
                // Hash on blocking thread (CPU-bound)
                let hash = spawn_blocking(move || hash_bytes(&data)).await??;
                
                // Upload (async I/O) - data still in memory
                data_cache.put_object(&hash, &data).await?;
                
                Ok(file.with_hash(hash))
            }
        })
        .buffer_unordered(options.concurrency)
        .collect()
        .await
}
```

**Pros:**
- Single file read (matches Python efficiency)
- Concurrent hash + upload for different files
- Memory bounded by semaphore

**Cons:**
- More complex than current approach
- Requires refactoring scanner/upload separation

### Option B: Parallel Hashing with Deduplication

Keep current architecture but add:
1. Parallel hashing with `rayon`
2. Hash deduplication before upload

```rust
// In FileSystemScanner::snapshot()
let hashes: Vec<(PathBuf, String)> = files
    .par_iter()
    .map(|f| (f.path.clone(), hash_file(&f.path)?))
    .collect::<Result<Vec<_>, _>>()?;

// In UploadOrchestrator
let unique_hashes: HashSet<&str> = entries.iter()
    .filter_map(|e| e.hash.as_deref())
    .collect();

// Only upload unique hashes
```

**Pros:**
- Simpler change to existing code
- Parallel hashing improves CPU utilization
- Deduplication reduces uploads

**Cons:**
- Still reads files twice
- No concurrent hash+upload

### Option C: Streaming with CRT

Use CRT's streaming upload with hash computation:

```rust
// Custom body that hashes while streaming
struct HashingBody {
    inner: SdkBody,
    hasher: Xxh3Hasher,
}

impl Body for HashingBody {
    fn poll_data(...) -> Poll<Option<Result<Bytes>>> {
        match self.inner.poll_data(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                self.hasher.update(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            other => other,
        }
    }
}
```

**Pros:**
- Single read, hash computed during upload
- Works with CRT's multipart

**Cons:**
- Complex integration with CRT
- Hash only known after upload completes

---

## Recommendation

**Short term (Option B):** Add parallel hashing and deduplication to existing code. Low risk, immediate benefit.

**Medium term (Option A):** Implement pipelined hash+upload for new `hash_upload_abs_manifest()` function. Matches Python behavior.

**Long term:** Evaluate Option C for very large files where memory is a concern.

---

## Files to Study

**Python:**
- `_snapshots/_operations/_hash_upload_abs_manifest.py` - Main entry point
- `_snapshots/_operations/_hash_upload_abs_manifest_pipeline.py` - Base pipeline
- `_snapshots/_operations/_hash_upload_abs_manifest_s3_pipeline.py` - S3 specifics
- `_snapshots/_operations/_hash_upload_abs_manifest_file_system_pipeline.py` - FS specifics

**Rust:**
- `crates/storage/src/upload.rs` - Upload orchestrator
- `crates/filesystem/src/scanner.rs` - Hashing (separate from upload)
- `crates/storage/src/traits.rs` - StorageClient trait
