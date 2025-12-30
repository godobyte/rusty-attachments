# VFS Trace-Based Prefetching Design

**Status: DRAFT**

## Overview

This design introduces a trace-based prefetching system for the VFS. The system operates in two phases:

1. **Trace Collection**: Instrument the FUSE layer to capture file access patterns during a workload run
2. **Trace Analysis**: Analyze traces offline to generate an optimal prefetch plan
3. **Prefetch Execution**: Load the prefetch plan at VFS startup to warm the cache

## Goals

- Capture access patterns with minimal runtime overhead
- Generate prefetch plans optimized for time budget (N minutes) or memory budget (M MB)
- Support V2 manifest 256MB block scheme
- Decouple trace collection from analysis (offline processing)

---

## Data Structures

### Trace Event

```rust
/// A single trace event captured from the FUSE layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// Monotonic timestamp in microseconds since trace start.
    pub timestamp_us: u64,
    /// Type of event.
    pub event_type: TraceEventType,
    /// Inode ID of the file.
    pub inode: u64,
    /// File path (captured on open, referenced by inode thereafter).
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TraceEventType {
    /// File opened.
    Open,
    /// File read: offset and size in bytes.
    Read { offset: u64, size: u32 },
    /// File closed.
    Close,
}
```

### Trace File Format

```rust
/// Header for the trace file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceHeader {
    /// Format version.
    pub version: u32,
    /// Manifest hash (to validate trace matches manifest).
    pub manifest_hash: String,
    /// Block size used (256MB for V2, 0 for V1 unbounded blocks).
    pub block_size: u64,
    /// Trace start time (wall clock, for reference).
    pub start_time_unix_ms: u64,
}

/// Complete trace file structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceFile {
    pub header: TraceHeader,
    /// Path table: inode -> path (deduplicated).
    pub paths: HashMap<u64, String>,
    /// Events in chronological order.
    pub events: Vec<TraceEvent>,
}
```

### Prefetch Plan

```rust
/// A prefetch plan generated from trace analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefetchPlan {
    /// Format version.
    pub version: u32,
    /// Manifest hash this plan was generated for.
    pub manifest_hash: String,
    /// Ordered list of blocks to prefetch.
    pub blocks: Vec<PrefetchBlock>,
    /// Total size of all blocks in bytes.
    pub total_size: u64,
    /// Estimated time to prefetch all blocks (based on trace timing).
    pub estimated_time_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefetchBlock {
    /// Content hash of the block.
    pub hash: String,
    /// Chunk index (0 for non-chunked files).
    pub chunk_index: u32,
    /// Priority score (higher = prefetch earlier).
    pub priority: f64,
    /// File path (for debugging/logging).
    pub path: String,
}
```

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Phase 1: Trace Collection                            │
│                                                                              │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐                   │
│  │  FUSE Layer  │───▶│ TraceBuffer  │───▶│ FlushThread  │───▶ trace.json    │
│  │  (instrumented)   │ (lock-free)  │    │ (low priority)│                   │
│  └──────────────┘    └──────────────┘    └──────────────┘                   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                         Phase 2: Trace Analysis                              │
│                                                                              │
│  trace.json ───▶ analyze_trace.py ───▶ prefetch_plan.json                   │
│                                                                              │
│  Options:                                                                    │
│    --time-budget 5      # Prefetch blocks accessed in first 5 minutes       │
│    --memory-budget 2048 # Prefetch up to 2GB of blocks                      │
│    --strategy first-access | frequency | weighted                           │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                         Phase 3: Prefetch Execution                          │
│                                                                              │
│  prefetch_plan.json ───▶ VFS Startup ───▶ Background Prefetch Thread        │
│                                                                              │
│  mount_vfs --prefetch-plan prefetch_plan.json                               │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Phase 1: Trace Collection

### Instrumentation Points

Instrument the following FUSE methods in `fuse.rs`:

| Method | Event | Data Captured |
|--------|-------|---------------|
| `open()` | `Open` | inode, path |
| `read()` | `Read` | inode, offset, size |
| `release()` | `Close` | inode |

### TraceCollector

```rust
/// Configuration for trace collection.
pub struct TraceConfig {
    /// Path to write trace file.
    pub output_path: PathBuf,
    /// Buffer size before flush (default: 10,000 events).
    pub buffer_size: usize,
    /// Flush interval in milliseconds (default: 1000).
    pub flush_interval_ms: u64,
}

/// Thread-safe trace collector with async buffer flush.
pub struct TraceCollector {
    /// Lock-free event buffer (SPSC or MPSC ring buffer).
    buffer: Arc<TraceBuffer>,
    /// Path table for deduplication.
    paths: Arc<RwLock<HashMap<u64, String>>>,
    /// Trace start time.
    start_time: Instant,
    /// Flush thread handle.
    flush_handle: Option<JoinHandle<()>>,
}

impl TraceCollector {
    /// Create a new trace collector.
    pub fn new(config: TraceConfig) -> Self;
    
    /// Record a trace event (non-blocking).
    /// Returns immediately, event is buffered for async flush.
    #[inline]
    pub fn record(&self, event_type: TraceEventType, inode: u64, path: Option<&str>);
    
    /// Stop collection and flush remaining events.
    pub fn finish(self) -> Result<TraceFile, TraceError>;
}
```

### Lock-Free Buffer Design

```rust
/// Ring buffer for trace events.
/// Uses atomic indices for lock-free producer (FUSE thread).
struct TraceBuffer {
    /// Pre-allocated event slots.
    events: Box<[UnsafeCell<Option<TraceEvent>>]>,
    /// Write index (producer).
    write_idx: AtomicUsize,
    /// Read index (consumer/flush thread).
    read_idx: AtomicUsize,
    /// Capacity (power of 2 for fast modulo).
    capacity: usize,
}

impl TraceBuffer {
    /// Push event (producer side, non-blocking).
    /// Returns false if buffer is full (event dropped).
    #[inline]
    pub fn push(&self, event: TraceEvent) -> bool;
    
    /// Drain available events (consumer side).
    pub fn drain(&self) -> Vec<TraceEvent>;
}
```

### Flush Thread

```rust
/// Background thread for flushing trace events to disk.
fn flush_thread(
    buffer: Arc<TraceBuffer>,
    paths: Arc<RwLock<HashMap<u64, String>>>,
    output_path: PathBuf,
    flush_interval: Duration,
) {
    // Set low priority (nice value)
    #[cfg(unix)]
    unsafe { libc::nice(10); }
    
    let mut file = BufWriter::new(File::create(&output_path)?);
    
    loop {
        std::thread::sleep(flush_interval);
        
        let events = buffer.drain();
        if events.is_empty() {
            continue;
        }
        
        // Write events as newline-delimited JSON for streaming
        for event in events {
            serde_json::to_writer(&mut file, &event)?;
            writeln!(file)?;
        }
        file.flush()?;
    }
}
```

### VFS Integration

```rust
// In VfsOptions
pub struct VfsOptions {
    // ... existing fields ...
    
    /// Trace collection configuration (None = disabled).
    pub trace: Option<TraceConfig>,
}

// In DeadlineVfs
impl DeadlineVfs {
    pub fn new(manifest: &Manifest, store: Arc<dyn FileStore>, options: VfsOptions) -> Result<Self, VfsError> {
        let trace_collector: Option<Arc<TraceCollector>> = options.trace
            .map(|config| Arc::new(TraceCollector::new(config)));
        
        // ... rest of initialization ...
    }
}

// In FUSE open()
fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
    // ... existing logic ...
    
    if let Some(ref collector) = self.trace_collector {
        collector.record(TraceEventType::Open, ino, Some(&path));
    }
    
    // ... rest of method ...
}

// In FUSE read()
fn read(&mut self, _req: &Request, ino: u64, fh: u64, offset: i64, size: u32, ...) {
    if let Some(ref collector) = self.trace_collector {
        collector.record(TraceEventType::Read { offset: offset as u64, size }, ino, None);
    }
    
    // ... existing logic ...
}
```

---

## Phase 2: Trace Analysis

### Analysis Script: `analyze_trace.py`

```python
#!/usr/bin/env python3
"""
Analyze VFS trace files and generate prefetch plans.

Usage:
    analyze_trace.py trace.json manifest.json -o prefetch_plan.json [OPTIONS]

Options:
    --time-budget MINUTES    Include blocks accessed within first N minutes (default: 5)
    --memory-budget MB       Limit total prefetch size to M megabytes
    --strategy STRATEGY      Prioritization strategy: first-access, frequency, weighted
    --min-block-accesses N   Minimum accesses to include a block (default: 1)
"""
```

### Analysis Strategies

#### 1. First-Access Strategy (Default)

Prioritize blocks by when they were first accessed. Good for startup optimization.

```python
def analyze_first_access(events: List[TraceEvent], time_budget_us: int) -> List[PrefetchBlock]:
    """
    Include all blocks first accessed within the time budget.
    Priority = inverse of first access time (earlier = higher priority).
    """
    first_access: Dict[BlockKey, int] = {}  # block -> first access timestamp
    
    for event in events:
        if event.event_type == "Read":
            block_key = compute_block_key(event.inode, event.offset)
            if block_key not in first_access:
                first_access[block_key] = event.timestamp_us
    
    # Filter by time budget and sort by first access
    blocks = [
        (key, ts) for key, ts in first_access.items()
        if ts <= time_budget_us
    ]
    blocks.sort(key=lambda x: x[1])
    
    return [
        PrefetchBlock(
            hash=key.hash,
            chunk_index=key.chunk_index,
            priority=1.0 / (1 + ts / 1_000_000),  # Higher priority for earlier access
            path=key.path,
        )
        for key, ts in blocks
    ]
```

#### 2. Frequency Strategy

Prioritize blocks by access count. Good for repeated access patterns.

```python
def analyze_frequency(events: List[TraceEvent], time_budget_us: int) -> List[PrefetchBlock]:
    """
    Prioritize blocks by number of accesses within time budget.
    """
    access_count: Dict[BlockKey, int] = defaultdict(int)
    first_access: Dict[BlockKey, int] = {}
    
    for event in events:
        if event.event_type == "Read" and event.timestamp_us <= time_budget_us:
            block_key = compute_block_key(event.inode, event.offset)
            access_count[block_key] += 1
            if block_key not in first_access:
                first_access[block_key] = event.timestamp_us
    
    # Sort by frequency (descending), then by first access (ascending)
    blocks = list(access_count.items())
    blocks.sort(key=lambda x: (-x[1], first_access[x[0]]))
    
    return [
        PrefetchBlock(
            hash=key.hash,
            chunk_index=key.chunk_index,
            priority=count,
            path=key.path,
        )
        for key, count in blocks
    ]
```

#### 3. Weighted Strategy

Combine first-access time and frequency with configurable weights.

```python
def analyze_weighted(
    events: List[TraceEvent],
    time_budget_us: int,
    time_weight: float = 0.7,
    freq_weight: float = 0.3,
) -> List[PrefetchBlock]:
    """
    Weighted combination of first-access time and frequency.
    
    score = time_weight * (1 - normalized_time) + freq_weight * normalized_freq
    """
    # ... implementation ...
```

### Block Key Computation

```python
CHUNK_SIZE_V2 = 256 * 1024 * 1024  # 256 MB

@dataclass
class BlockKey:
    hash: str
    chunk_index: int
    path: str

def compute_block_key(
    inode: int,
    offset: int,
    manifest: Manifest,
    path_table: Dict[int, str],
    block_size: int,
) -> BlockKey:
    """
    Map (inode, offset) to a block key using manifest metadata.
    
    Args:
        inode: File inode ID
        offset: Read offset in bytes
        manifest: Manifest containing file metadata
        path_table: Mapping from inode to path
        block_size: Block size from trace header (0 = V1 unbounded)
    """
    path = path_table[inode]
    file_entry = manifest.get_file(path)
    
    if block_size == 0:
        # V1 manifest: unbounded blocks, single hash per file
        chunk_index = 0
        hash = file_entry.hash
    elif file_entry.chunkhashes:
        # V2 chunked file: compute chunk index
        chunk_index = offset // block_size
        hash = file_entry.chunkhashes[chunk_index]
    else:
        # V2 non-chunked file: single hash
        chunk_index = 0
        hash = file_entry.hash
    
    return BlockKey(hash=hash, chunk_index=chunk_index, path=path)
```

### Memory Budget Enforcement

```python
def apply_memory_budget(blocks: List[PrefetchBlock], budget_bytes: int) -> List[PrefetchBlock]:
    """
    Truncate block list to fit within memory budget.
    Assumes blocks are already sorted by priority.
    """
    result = []
    total_size = 0
    
    for block in blocks:
        block_size = CHUNK_SIZE_V2  # Assume max block size
        if total_size + block_size > budget_bytes:
            break
        result.append(block)
        total_size += block_size
    
    return result
```

---

## Phase 3: Prefetch Execution

### VFS Startup with Prefetch Plan

```rust
// CLI option
mount_vfs manifest.json /mnt/vfs --prefetch-plan prefetch_plan.json

// In VfsOptions
pub struct VfsOptions {
    // ... existing fields ...
    
    /// Path to prefetch plan file.
    pub prefetch_plan: Option<PathBuf>,
}
```

### Prefetch Executor

```rust
/// Executes a prefetch plan in the background.
pub struct PrefetchExecutor {
    plan: PrefetchPlan,
    pool: Arc<MemoryPool>,
    store: Arc<dyn FileStore>,
    hash_algorithm: HashAlgorithm,
    /// Progress tracking.
    completed: AtomicUsize,
    /// Cancellation flag.
    cancelled: AtomicBool,
}

impl PrefetchExecutor {
    /// Start prefetching in a background task.
    /// Returns a handle to monitor progress or cancel.
    pub fn start(
        plan: PrefetchPlan,
        pool: Arc<MemoryPool>,
        store: Arc<dyn FileStore>,
        hash_algorithm: HashAlgorithm,
        concurrency: usize,
    ) -> PrefetchHandle {
        let executor = Arc::new(Self {
            plan,
            pool,
            store,
            hash_algorithm,
            completed: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
        });
        
        let handle = tokio::spawn(executor.clone().run(concurrency));
        
        PrefetchHandle { executor, handle }
    }
    
    async fn run(self: Arc<Self>, concurrency: usize) {
        use futures::stream::{self, StreamExt};
        
        stream::iter(self.plan.blocks.iter())
            .map(|block| self.prefetch_block(block))
            .buffer_unordered(concurrency)
            .for_each(|result| async {
                if result.is_ok() {
                    self.completed.fetch_add(1, Ordering::Relaxed);
                }
            })
            .await;
    }
    
    async fn prefetch_block(&self, block: &PrefetchBlock) -> Result<(), MemoryPoolError> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(());
        }
        
        let key = BlockKey::from_hash_hex(&block.hash, block.chunk_index);
        let hash = block.hash.clone();
        let store = self.store.clone();
        let alg = self.hash_algorithm;
        
        // Acquire will fetch if not cached, or return immediately if cached
        let _ = self.pool.acquire(&key, move || async move {
            store.retrieve(&hash, alg)
                .await
                .map_err(|e| MemoryPoolError::RetrievalFailed(e.to_string()))
        }).await?;
        
        Ok(())
    }
}

/// Handle for monitoring/controlling prefetch execution.
pub struct PrefetchHandle {
    executor: Arc<PrefetchExecutor>,
    handle: tokio::task::JoinHandle<()>,
}

impl PrefetchHandle {
    /// Get prefetch progress (completed / total).
    pub fn progress(&self) -> (usize, usize) {
        (
            self.executor.completed.load(Ordering::Relaxed),
            self.executor.plan.blocks.len(),
        )
    }
    
    /// Cancel prefetching.
    pub fn cancel(&self) {
        self.executor.cancelled.store(true, Ordering::Relaxed);
    }
    
    /// Wait for prefetching to complete.
    pub async fn wait(self) {
        let _ = self.handle.await;
    }
}
```

---

## CLI Interface

### Trace Collection

```bash
# Mount with trace collection enabled
cargo run --example mount_vfs -- \
    manifest.json /mnt/vfs \
    --trace-output /tmp/trace.json \
    --bucket my-bucket --root-prefix MyPrefix

# Run workload...

# Unmount (trace is finalized on unmount)
fusermount -u /mnt/vfs
```

### Trace Analysis

```bash
# Analyze trace with 5-minute time budget (default)
python3 utils/analyze_trace.py \
    /tmp/trace.json manifest.json \
    -o prefetch_plan.json

# Analyze with memory budget
python3 utils/analyze_trace.py \
    /tmp/trace.json manifest.json \
    -o prefetch_plan.json \
    --memory-budget 2048  # 2GB

# Analyze with custom time budget and strategy
python3 utils/analyze_trace.py \
    /tmp/trace.json manifest.json \
    -o prefetch_plan.json \
    --time-budget 10 \
    --strategy weighted
```

### Prefetch Execution

```bash
# Mount with prefetch plan
cargo run --example mount_vfs -- \
    manifest.json /mnt/vfs \
    --prefetch-plan prefetch_plan.json \
    --bucket my-bucket --root-prefix MyPrefix
```

---

## Performance Considerations

### Trace Collection Overhead

| Aspect | Target | Approach |
|--------|--------|----------|
| Latency per event | < 100ns | Lock-free ring buffer, no allocation |
| Memory overhead | < 10MB | Fixed-size buffer, streaming flush |
| CPU overhead | < 1% | Low-priority flush thread |
| Disk I/O | Batched | Buffered writes, periodic flush |

### Prefetch Execution

| Aspect | Target | Approach |
|--------|--------|----------|
| Concurrency | Configurable | Default 4 concurrent fetches |
| Priority | Background | Lower priority than user reads |
| Cancellation | Immediate | Atomic flag checked per block |
| Memory pressure | Adaptive | Respect pool limits, LRU eviction |

---

## Future Enhancements

1. **Adaptive Prefetching**: Adjust prefetch plan at runtime based on actual access patterns
2. **Trace Compression**: Use binary format or compression for large traces
3. **Multi-Workload Analysis**: Combine traces from multiple runs for better coverage
4. **Prefetch Scheduling**: Time-based prefetch scheduling (prefetch block X at time T)
5. **Integration with ReadCache**: Prefetch to disk cache for persistence across mounts

---

## File Locations

| Component | Location |
|-----------|----------|
| TraceCollector | `crates/vfs/src/trace/collector.rs` |
| TraceBuffer | `crates/vfs/src/trace/buffer.rs` |
| PrefetchExecutor | `crates/vfs/src/prefetch/executor.rs` |
| PrefetchPlan types | `crates/vfs/src/prefetch/plan.rs` |
| Analysis script | `utils/analyze_trace.py` |
| Design doc | `design/vfs-trace-precaching.md` |
