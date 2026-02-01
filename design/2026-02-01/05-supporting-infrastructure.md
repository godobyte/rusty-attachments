# Supporting Infrastructure Design

## Overview

Building blocks for the pipelined hash+upload system: memory pool, upload deduplication, progress tracking, and configuration options.

## Components

### 1. MemoryPool

Semaphore-based memory allocation with backpressure for controlling in-flight data.

### 2. UploadDeduplicator

Broadcast channel coordination to prevent duplicate uploads of identical content.

### 3. ProgressTracker

Thread-safe atomic progress tracking with detailed metrics.

### 4. HashUploadOptions

Builder-pattern configuration with auto-memory detection.

---

## 1. MemoryPool

### Purpose

Control memory usage by limiting concurrent in-flight data. When the pool is exhausted, new allocations block until memory is released.

### Data Structure

```rust
/// Memory pool with backpressure for pipelined operations.
pub struct MemoryPool {
    /// Maximum bytes allowed.
    max_bytes: u64,
    /// Currently allocated bytes (for monitoring).
    allocated: AtomicU64,
    /// Semaphore for blocking when full.
    semaphore: Semaphore,
    /// Permit size (granularity of allocation).
    permit_size: u64,
}

/// RAII guard for allocated memory.
pub struct MemoryPermit<'a> {
    pool: &'a MemoryPool,
    size: u64,
    _permit: SemaphorePermit<'a>,
}
```

### Implementation

```rust
impl MemoryPool {
    /// Create a new memory pool.
    ///
    /// # Arguments
    /// * `max_bytes` - Maximum memory to allow
    /// * `permit_size` - Size of each permit (e.g., 64MB)
    pub fn new(max_bytes: u64, permit_size: u64) -> Self {
        let permits: usize = (max_bytes / permit_size).max(1) as usize;
        Self {
            max_bytes,
            allocated: AtomicU64::new(0),
            semaphore: Semaphore::new(permits),
            permit_size,
        }
    }

    /// Allocate memory from the pool.
    ///
    /// Blocks if the pool is exhausted until memory is released.
    ///
    /// # Arguments
    /// * `size` - Number of bytes to allocate
    ///
    /// # Returns
    /// A permit that releases memory when dropped.
    pub async fn allocate(&self, size: u64) -> MemoryPermit<'_> {
        // Calculate permits needed (round up)
        let permits_needed: u32 = ((size + self.permit_size - 1) / self.permit_size).max(1) as u32;

        // Acquire permits (blocks if not available)
        let permit: SemaphorePermit<'_> = self
            .semaphore
            .acquire_many(permits_needed)
            .await
            .expect("semaphore closed");

        self.allocated.fetch_add(size, Ordering::Relaxed);

        MemoryPermit {
            pool: self,
            size,
            _permit: permit,
        }
    }

    /// Try to allocate without blocking.
    pub fn try_allocate(&self, size: u64) -> Option<MemoryPermit<'_>> {
        let permits_needed: u32 = ((size + self.permit_size - 1) / self.permit_size).max(1) as u32;

        match self.semaphore.try_acquire_many(permits_needed) {
            Ok(permit) => {
                self.allocated.fetch_add(size, Ordering::Relaxed);
                Some(MemoryPermit { pool: self, size, _permit: permit })
            }
            Err(_) => None,
        }
    }
}

impl Drop for MemoryPermit<'_> {
    fn drop(&mut self) {
        self.pool.allocated.fetch_sub(self.size, Ordering::Relaxed);
        // SemaphorePermit is automatically released when dropped
    }
}
```

### Usage

```rust
let pool = Arc::new(MemoryPool::new(
    5 * 1024 * 1024 * 1024,  // 5GB max
    64 * 1024 * 1024,        // 64MB permit size
));

// Blocks if pool is exhausted
let permit = pool.allocate(file_size).await;
let data: Vec<u8> = read_file(&path).await?;
// ... use data ...
drop(permit);  // Memory released
```

---

## 2. UploadDeduplicator

### Purpose

Track in-flight uploads to prevent duplicate uploads of the same hash. When multiple files have identical content, only one upload is performed.

### Data Structure

```rust
/// Tracks in-flight uploads to prevent duplicate uploads.
pub struct UploadDeduplicator {
    /// Map of hash -> broadcast sender for completion notification.
    in_flight: Mutex<HashMap<String, broadcast::Sender<UploadResult>>>,
}

/// Result of a deduplicated upload operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadResult {
    Success,
    Failed,
}

/// Intent returned when registering an upload.
pub enum UploadIntent {
    /// Proceed with upload (first uploader).
    Proceed,
    /// Wait for existing upload to complete.
    Wait(broadcast::Receiver<UploadResult>),
}
```

### Implementation

```rust
impl UploadDeduplicator {
    pub fn new() -> Self {
        Self { in_flight: Mutex::new(HashMap::new()) }
    }

    /// Register intent to upload a hash.
    ///
    /// # Returns
    /// - `Proceed` if this is the first uploader
    /// - `Wait(receiver)` if another upload is in progress
    pub fn register(&self, hash: &str) -> UploadIntent {
        let mut in_flight = self.in_flight.lock().unwrap();

        if let Some(sender) = in_flight.get(hash) {
            // Another upload in progress, subscribe to completion
            UploadIntent::Wait(sender.subscribe())
        } else {
            // First uploader, register and proceed
            let (sender, _) = broadcast::channel(1);
            in_flight.insert(hash.to_string(), sender);
            UploadIntent::Proceed
        }
    }

    /// Mark upload as complete.
    pub fn complete(&self, hash: &str) {
        let mut in_flight = self.in_flight.lock().unwrap();
        if let Some(sender) = in_flight.remove(hash) {
            let _ = sender.send(UploadResult::Success);
        }
    }

    /// Mark upload as failed.
    pub fn failed(&self, hash: &str) {
        let mut in_flight = self.in_flight.lock().unwrap();
        if let Some(sender) = in_flight.remove(hash) {
            let _ = sender.send(UploadResult::Failed);
        }
    }
}
```

### Usage

```rust
let dedup = Arc::new(UploadDeduplicator::new());

match dedup.register(&hash) {
    UploadIntent::Proceed => {
        // We're the first uploader
        match upload_to_s3(&hash, &data).await {
            Ok(_) => dedup.complete(&hash),
            Err(_) => dedup.failed(&hash),
        }
    }
    UploadIntent::Wait(mut receiver) => {
        // Wait for other upload
        match receiver.recv().await {
            Ok(UploadResult::Success) => { /* Already uploaded */ }
            Ok(UploadResult::Failed) | Err(_) => {
                // Other failed, try ourselves
                upload_to_s3(&hash, &data).await?;
            }
        }
    }
}
```

---

## 3. ProgressTracker

### Purpose

Thread-safe progress tracking with detailed metrics for hash and upload operations.

### Data Structure

```rust
/// Progress snapshot for hash+upload operations.
#[derive(Debug, Clone)]
pub struct HashUploadProgress {
    // Totals
    pub total_files: u64,
    pub total_bytes: u64,

    // Hashing progress
    pub hashed_files: u64,
    pub hashed_bytes: u64,
    pub hash_skipped_files: u64,
    pub hash_skipped_bytes: u64,

    // Upload progress
    pub uploaded_files: u64,
    pub uploaded_bytes: u64,
    pub upload_skipped_files: u64,
    pub upload_skipped_bytes: u64,

    // Timing
    pub elapsed_secs: f64,
    pub transfer_rate_bytes_per_sec: f64,

    // Overall
    pub progress_percent: f64,
    pub message: String,
}

/// Thread-safe progress tracker.
pub struct ProgressTracker {
    start_time: Instant,
    total_files: u64,
    total_bytes: u64,

    hashed_files: AtomicU64,
    hashed_bytes: AtomicU64,
    hash_skipped_files: AtomicU64,
    hash_skipped_bytes: AtomicU64,

    uploaded_files: AtomicU64,
    uploaded_bytes: AtomicU64,
    upload_skipped_files: AtomicU64,
    upload_skipped_bytes: AtomicU64,
}
```

### Implementation

```rust
impl ProgressTracker {
    pub fn new(total_files: u64, total_bytes: u64) -> Self {
        Self {
            start_time: Instant::now(),
            total_files,
            total_bytes,
            hashed_files: AtomicU64::new(0),
            // ... other fields initialized to 0
        }
    }

    /// Record hash completion for a file.
    pub fn record_hash_complete(&self, bytes: u64, from_cache: bool) {
        if from_cache {
            self.hash_skipped_files.fetch_add(1, Ordering::Relaxed);
            self.hash_skipped_bytes.fetch_add(bytes, Ordering::Relaxed);
        } else {
            self.hashed_files.fetch_add(1, Ordering::Relaxed);
            self.hashed_bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// Record upload completion for a file.
    pub fn record_upload_complete(&self, bytes: u64, skipped: bool) {
        if skipped {
            self.upload_skipped_files.fetch_add(1, Ordering::Relaxed);
            self.upload_skipped_bytes.fetch_add(bytes, Ordering::Relaxed);
        } else {
            self.uploaded_files.fetch_add(1, Ordering::Relaxed);
            self.uploaded_bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// Get a consistent snapshot of current progress.
    pub fn snapshot(&self) -> HashUploadProgress {
        let elapsed: f64 = self.start_time.elapsed().as_secs_f64();
        let uploaded_bytes: u64 = self.uploaded_bytes.load(Ordering::Relaxed);
        let upload_skipped_bytes: u64 = self.upload_skipped_bytes.load(Ordering::Relaxed);

        let total_processed: u64 = uploaded_bytes + upload_skipped_bytes;
        let progress_percent: f64 = if self.total_bytes > 0 {
            (total_processed as f64 / self.total_bytes as f64) * 100.0
        } else {
            100.0
        };

        HashUploadProgress {
            total_files: self.total_files,
            total_bytes: self.total_bytes,
            // ... load all atomic values
            elapsed_secs: elapsed,
            transfer_rate_bytes_per_sec: uploaded_bytes as f64 / elapsed.max(0.001),
            progress_percent,
            message: format!("Uploaded {:.1} MB / {:.1} MB ({:.1}%)", ...),
        }
    }
}
```

---

## 4. HashUploadOptions

### Purpose

Configuration for the hash+upload pipeline with builder pattern and auto-detection.

### Data Structure

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

### Implementation

```rust
impl Default for HashUploadOptions {
    fn default() -> Self {
        Self {
            max_memory_bytes: 5 * 1024 * 1024 * 1024, // 5GB
            max_concurrency: 32,
            chunk_size: 256 * 1024 * 1024, // 256MB
            use_hash_cache: true,
            use_s3_check_cache: true,
            force_rehash: false,
        }
    }
}

impl HashUploadOptions {
    pub fn new() -> Self { Self::default() }

    pub fn with_max_memory(mut self, bytes: u64) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    pub fn with_max_concurrency(mut self, concurrency: usize) -> Self {
        self.max_concurrency = concurrency;
        self
    }

    // ... other builder methods

    /// Calculate memory based on system resources.
    ///
    /// Uses heuristic: min(16GB, max(256MB, quarter_of_total, available - 1GB))
    pub fn auto_memory() -> u64 {
        #[cfg(target_os = "linux")]
        {
            if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
                let mut total_kb: u64 = 0;
                let mut available_kb: u64 = 0;

                for line in meminfo.lines() {
                    if line.starts_with("MemTotal:") {
                        total_kb = parse_meminfo_value(line);
                    } else if line.starts_with("MemAvailable:") {
                        available_kb = parse_meminfo_value(line);
                    }
                }

                let quarter_total: u64 = total_kb * 1024 / 4;
                let available_minus_1gb: u64 = available_kb.saturating_sub(1024 * 1024) * 1024;

                let min_bytes: u64 = 256 * 1024 * 1024;  // 256MB
                let max_bytes: u64 = 16 * 1024 * 1024 * 1024;  // 16GB

                return max_bytes.min(min_bytes.max(quarter_total).max(available_minus_1gb));
            }
        }

        // Default fallback
        1024 * 1024 * 1024  // 1GB
    }
}
```

### Usage

```rust
// Default options
let options = HashUploadOptions::default();

// Custom options with builder
let options = HashUploadOptions::new()
    .with_max_memory(HashUploadOptions::auto_memory())
    .with_max_concurrency(64)
    .with_hash_cache(true)
    .with_force_rehash(false);
```

---

## Module Structure

```
crates/storage/src/hash_upload/
├── mod.rs              # Public API
├── pipeline.rs         # HashUploadPipeline
├── staged_pipeline.rs  # StagedPipeline
├── options.rs          # HashUploadOptions
├── deduplication.rs    # UploadDeduplicator
├── memory_pool.rs      # MemoryPool
└── progress.rs         # ProgressTracker
```

## Exports

```rust
// crates/storage/src/hash_upload/mod.rs
mod deduplication;
mod memory_pool;
mod options;
mod pipeline;
mod progress;
mod staged_pipeline;

pub use options::HashUploadOptions;
pub use pipeline::{ProcessedItem, WorkItem};
pub use progress::{HashUploadProgress, ProgressTracker};
pub use staged_pipeline::{StagedPipeline, StagedPipelineConfig, StagedProcessedItem};

// Internal use only (not pub)
use deduplication::{UploadDeduplicator, UploadIntent, UploadResult};
use memory_pool::MemoryPool;
```

## Testing Strategy

### MemoryPool
- Test allocation and release
- Test blocking when exhausted
- Test try_allocate failure

### UploadDeduplicator
- Test first uploader proceeds
- Test second uploader waits
- Test completion notification
- Test failure allows retry

### ProgressTracker
- Test initial state
- Test recording hash/upload
- Test snapshot consistency
- Test progress percentage

### HashUploadOptions
- Test default values
- Test builder pattern
- Test auto_memory returns reasonable value

## References

- [Pipelined Hash+Upload](./02-pipelined-hash-upload.md)
- [Staged Pipeline](./03-staged-pipeline.md)
- [tokio::sync::Semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html)
- [tokio::sync::broadcast](https://docs.rs/tokio/latest/tokio/sync/broadcast/)
