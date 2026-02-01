# 3-Stage Pipeline Architecture Design

## Overview

An advanced pipeline that separates file processing into three concurrent stages connected by async channels. This allows better memory utilization and higher throughput than the basic pipelined approach.

## Problem Statement

The basic pipeline processes each file through all stages sequentially within a single task. While concurrent across files, this limits throughput because:

1. Fast readers wait for slow hashers
2. Fast hashers wait for slow uploaders
3. Memory is held for entire file lifecycle

## Goals

1. Decouple read, hash, and upload stages
2. Allow each stage to run at its own pace
3. Better memory utilization through stage-specific buffering
4. Higher throughput via independent concurrency per stage
5. Reuse building blocks from basic pipeline

## Architecture

### Pyramid Structure

```
┌─────────────────────────────────────────────────────────────┐
│              hash_upload_abs_manifest_staged()              │
│                    (Top-level API)                          │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                      StagedPipeline                         │
│              (Orchestrates 3-stage processing)              │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌───────────────┐     ┌───────────────┐     ┌───────────────┐
│  Read Stage   │────▶│  Hash Stage   │────▶│ Upload Stage  │
│ (16 workers)  │     │ (16 workers)  │     │ (32 workers)  │
└───────────────┘     └───────────────┘     └───────────────┘
        │                     │                     │
        └─────────────────────┴─────────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
┌─────────────────┐ ┌─────────────────┐ ┌─────────────────────┐
│   MemoryPool    │ │UploadDeduplicator│ │  ProgressTracker   │
│   (Shared)      │ │    (Shared)      │ │     (Shared)       │
└─────────────────┘ └─────────────────┘ └─────────────────────┘
```

### Component Reuse

From basic pipeline:
- `MemoryPool` - Same backpressure mechanism
- `UploadDeduplicator` - Same deduplication logic
- `ProgressTracker` - Same progress tracking
- `WorkItem` - Same input structure
- `HashUploadOptions` - Same configuration

New components:
- `StagedPipeline` - Orchestrator for 3-stage processing
- `StagedPipelineConfig` - Stage-specific concurrency settings
- Inter-stage channel types

## Data Flow

```
┌──────────┐    mpsc(256)    ┌──────────┐    mpsc(256)    ┌──────────┐
│   Read   │ ──────────────▶ │   Hash   │ ──────────────▶ │  Upload  │
│  Stage   │  ReadOutput     │  Stage   │  HashOutput     │  Stage   │
└──────────┘                 └──────────┘                 └──────────┘
     │                            │                            │
     ▼                            ▼                            ▼
 Read file                  Compute hash                 Upload to S3
 from disk                  (spawn_blocking)             (async I/O)
```

### Inter-Stage Messages

```rust
/// Output from read stage to hash stage.
struct ReadOutputOwned {
    item: WorkItem,
    data: Option<Bytes>,      // File contents (None if skipping)
    cached_hash: Option<String>,
    skip_entirely: bool,      // True if hash cached + S3 exists
}

/// Output from hash stage to upload stage.
struct HashOutputOwned {
    item: WorkItem,
    data: Option<Bytes>,      // File contents for upload
    hash: String,
    hash_cached: bool,
    skip_upload: bool,        // True if S3 already has this hash
}
```

## Implementation

### Module Structure

```
crates/storage/src/hash_upload/
├── mod.rs                  # Exports both pipelines
├── pipeline.rs             # Basic HashUploadPipeline
├── staged_pipeline.rs      # NEW: StagedPipeline
├── options.rs              # Shared options
├── deduplication.rs        # Shared deduplicator
├── memory_pool.rs          # Shared memory pool
└── progress.rs             # Shared progress tracker
```

### StagedPipelineConfig

```rust
/// Configuration for the staged pipeline.
#[derive(Debug, Clone)]
pub struct StagedPipelineConfig {
    /// Maximum concurrent file reads.
    pub read_concurrency: usize,
    /// Maximum concurrent hash computations.
    pub hash_concurrency: usize,
    /// Maximum concurrent uploads.
    pub upload_concurrency: usize,
    /// Maximum memory for in-flight data.
    pub max_memory_bytes: u64,
    /// Memory permit granularity.
    pub permit_size: u64,
}

impl Default for StagedPipelineConfig {
    fn default() -> Self {
        Self {
            read_concurrency: 16,
            hash_concurrency: 16,
            upload_concurrency: 32,
            max_memory_bytes: 5 * 1024 * 1024 * 1024, // 5GB
            permit_size: 64 * 1024 * 1024,            // 64MB
        }
    }
}
```

### Stage Implementations

#### Read Stage

```rust
async fn run_read_stage(
    &self,
    items: Vec<WorkItem>,
    tx: mpsc::Sender<ReadOutputOwned>,
) -> Result<(), StorageError> {
    stream::iter(items)
        .map(|item| {
            let tx = tx.clone();
            async move {
                let output: ReadOutputOwned = self.read_file(item).await?;
                tx.send(output).await.map_err(|_| StorageError::Other {
                    message: "Read stage channel closed".to_string(),
                })?;
                Ok(())
            }
        })
        .buffer_unordered(self.config.read_concurrency)
        .try_collect()
        .await
}
```

#### Hash Stage

```rust
async fn run_hash_stage(
    &self,
    mut rx: mpsc::Receiver<ReadOutputOwned>,
    tx: mpsc::Sender<HashOutputOwned>,
) -> Result<(), StorageError> {
    let mut pending: Vec<ReadOutputOwned> = Vec::new();

    while let Some(input) = rx.recv().await {
        pending.push(input);

        // Process in batches when we have enough or channel is empty
        if pending.len() >= self.config.hash_concurrency || rx.is_empty() {
            let batch: Vec<ReadOutputOwned> = std::mem::take(&mut pending);
            let results = stream::iter(batch)
                .map(|input| self.hash_data(input))
                .buffer_unordered(self.config.hash_concurrency)
                .collect::<Vec<_>>()
                .await;

            for result in results {
                tx.send(result?).await.map_err(|_| ...)?;
            }
        }
    }
    Ok(())
}
```

#### Upload Stage

```rust
async fn run_upload_stage(
    &self,
    mut rx: mpsc::Receiver<HashOutputOwned>,
    tx: mpsc::Sender<StagedProcessedItem>,
) -> Result<(), StorageError> {
    let mut pending: Vec<HashOutputOwned> = Vec::new();

    while let Some(input) = rx.recv().await {
        pending.push(input);

        if pending.len() >= self.config.upload_concurrency || rx.is_empty() {
            let batch: Vec<HashOutputOwned> = std::mem::take(&mut pending);
            let results = stream::iter(batch)
                .map(|input| self.upload_data(input))
                .buffer_unordered(self.config.upload_concurrency)
                .collect::<Vec<_>>()
                .await;

            for result in results {
                tx.send(result?).await.map_err(|_| ...)?;
            }
        }
    }
    Ok(())
}
```

### Pipeline Orchestration

```rust
pub async fn process(
    self: Arc<Self>,
    items: Vec<WorkItem>,
) -> Result<Vec<StagedProcessedItem>, StorageError> {
    let total_items: usize = items.len();

    // Create channels between stages
    let (read_tx, read_rx) = mpsc::channel::<ReadOutputOwned>(CHANNEL_CAPACITY);
    let (hash_tx, hash_rx) = mpsc::channel::<HashOutputOwned>(CHANNEL_CAPACITY);
    let (result_tx, mut result_rx) = mpsc::channel::<StagedProcessedItem>(CHANNEL_CAPACITY);

    // Spawn all stages concurrently
    let read_handle = tokio::spawn({
        let pipeline = Arc::clone(&self);
        async move { pipeline.run_read_stage(items, read_tx).await }
    });

    let hash_handle = tokio::spawn({
        let pipeline = Arc::clone(&self);
        async move { pipeline.run_hash_stage(read_rx, hash_tx).await }
    });

    let upload_handle = tokio::spawn({
        let pipeline = Arc::clone(&self);
        async move { pipeline.run_upload_stage(hash_rx, result_tx).await }
    });

    // Collect results
    let mut results: Vec<StagedProcessedItem> = Vec::with_capacity(total_items);
    while let Some(item) = result_rx.recv().await {
        results.push(item);
    }

    // Wait for all stages
    read_handle.await??;
    hash_handle.await??;
    upload_handle.await??;

    Ok(results)
}
```

## Public API

```rust
/// Hash and upload manifest contents using 3-stage pipeline.
///
/// # Arguments
/// * `manifest` - Manifest with files to process
/// * `source_root` - Root directory where files are located
/// * `data_cache` - Content-addressable storage (Arc-wrapped)
/// * `hash_cache` - Optional hash cache (Arc-wrapped)
/// * `options` - Pipeline configuration
///
/// # Returns
/// Result containing updated manifest and statistics.
pub async fn hash_upload_abs_manifest_staged<C: ContentAddressedDataCache + Send + Sync + 'static>(
    manifest: Manifest,
    source_root: &str,
    data_cache: Arc<C>,
    hash_cache: Option<Arc<HashCache>>,
    options: HashUploadOptions,
) -> Result<HashUploadResult, StorageError>
```

## Performance Characteristics

| Metric | Basic Pipeline | Staged Pipeline |
|--------|----------------|-----------------|
| Stage coupling | Tight (per-file) | Loose (channels) |
| Memory efficiency | Good | Better |
| Throughput | Good | Higher |
| Complexity | Lower | Higher |
| Debugging | Easier | Harder |

### When to Use Each

- **Basic Pipeline**: Simpler workloads, debugging, smaller file counts
- **Staged Pipeline**: Large uploads, maximum throughput needed

## Channel Sizing

```rust
const CHANNEL_CAPACITY: usize = 256;
```

Rationale:
- Large enough to absorb burst differences between stages
- Small enough to provide backpressure
- 256 items × ~64MB average = ~16GB theoretical max in-flight
- Actual memory bounded by `MemoryPool`

## Error Handling

Errors in any stage propagate through:
1. Stage returns `Err` from its async function
2. `JoinHandle` captures the error
3. Main orchestrator awaits all handles and returns first error

Channel closure is handled gracefully - downstream stages drain remaining items.

## Testing Strategy

1. Unit tests for each stage in isolation
2. Integration tests for full pipeline
3. Tests for stage failure scenarios
4. Tests for channel backpressure
5. Benchmark: basic vs staged pipeline

## References

- [Basic Pipeline](./02-pipelined-hash-upload.md)
- [Supporting Infrastructure](./05-supporting-infrastructure.md)
- [tokio::sync::mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/)
