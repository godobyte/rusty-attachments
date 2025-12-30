# VFS Async/Sync Boundary Refactoring

**Status: PROPOSED**

## Problem Analysis

### Alice Ryhl's Critique

The current VFS implementation has a critical architectural flaw in how it bridges the synchronous FUSE interface with async S3 operations:

> "The `block_on()` calls in your FUSE implementation are a red flag - you're blocking Tokio worker threads which can lead to deadlocks if the runtime is under load or if you exhaust the thread pool."

### Current Implementation (Problematic)

```rust
// In fuse.rs - captures ambient runtime
let runtime = Handle::try_current()
    .map_err(|e| VfsError::MountFailed(format!("No tokio runtime: {}", e)))?;

// In read operations - BLOCKS TOKIO WORKER THREAD
let handle = self.runtime.block_on(async move {
    pool.acquire(&key, move || { ... }).await
});
```

**The Deadlock Scenario:**

1. FUSE callbacks run on `fuser`'s internal thread pool
2. `Handle::try_current()` captures the ambient Tokio runtime
3. `block_on()` blocks a Tokio worker thread waiting for async work
4. If all worker threads get blocked waiting for async operations that themselves need worker threads to complete → **DEADLOCK**

### Alice's Recommended Solutions

1. Use `spawn_blocking()` to move FUSE callbacks to a blocking thread pool
2. **Use a dedicated runtime with `Runtime::new()` instead of `Handle::current()`** ← Recommended
3. Consider if async is even needed (sync S3 client with thread pool)

---

## Proposed Solution: Dedicated Async Executor Thread

Completely separate the FUSE sync world from the Tokio async world by using a dedicated background thread that owns its own Tokio runtime.

### Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         FUSE Thread Pool                                     │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  lookup(), read(), write(), etc.                                    │    │
│  │  (synchronous callbacks from fuser)                                 │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                    │                                         │
│                                    │ submit work via mpsc channel            │
│                                    │ block on oneshot receiver               │
│                                    ▼                                         │
└─────────────────────────────────────────────────────────────────────────────┘
                                     │
┌─────────────────────────────────────────────────────────────────────────────┐
│                      AsyncExecutor (Dedicated Thread)                        │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  Own Tokio Runtime (Runtime::new(), NOT Handle::current())          │    │
│  │  - Dedicated worker threads for VFS I/O                             │    │
│  │  - Isolated from application runtime                                │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                    │                                         │
│                                    │ async S3 operations                     │
│                                    ▼                                         │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  MemoryPool, FileStore, ReadCache                                   │    │
│  │  (pure async code)                                                  │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Key Insight

The FUSE callback blocks on a simple `oneshot::blocking_recv()`, which is just a condvar wait - **no Tokio involvement**. The async work runs on a completely separate Tokio runtime with its own thread pool.

---

## Implementation

### Core Types

```rust
use std::any::Any;
use std::future::Future;
use std::sync::Arc;
use std::thread::JoinHandle;

use futures::future::BoxFuture;
use futures::FutureExt;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Work item submitted to the async executor.
struct WorkItem {
    /// The async work to execute (boxed future).
    work: BoxFuture<'static, Box<dyn Any + Send>>,
    /// Channel to send the result back to the caller.
    result_tx: oneshot::Sender<Box<dyn Any + Send>>,
}

/// Async executor that runs in a dedicated background thread.
///
/// FUSE callbacks submit work via a channel and block on a oneshot receiver.
/// This avoids blocking Tokio worker threads and prevents deadlocks.
///
/// # Architecture
///
/// ```text
/// FUSE Thread                    Executor Thread
/// ───────────                    ───────────────
///     │                               │
///     │ submit(future) ──────────────►│
///     │                               │ spawn task
///     │ blocking_recv() ◄─────────────│ send result
///     │                               │
/// ```
pub struct AsyncExecutor {
    /// Channel to submit async work.
    tx: mpsc::Sender<WorkItem>,
    /// Cancellation token for graceful shutdown.
    cancel_token: CancellationToken,
    /// Handle to the background thread.
    _thread: JoinHandle<()>,
}
```

### AsyncExecutor Implementation

```rust
impl AsyncExecutor {
    /// Create a new executor with a dedicated runtime thread.
    ///
    /// # Arguments
    /// * `worker_threads` - Number of Tokio worker threads (default: 4)
    ///
    /// # Returns
    /// New executor instance with background thread running.
    pub fn new(worker_threads: usize) -> Self {
        let (tx, rx) = mpsc::channel::<WorkItem>(1024);
        let cancel_token = CancellationToken::new();
        let token_clone: CancellationToken = cancel_token.clone();

        let thread: JoinHandle<()> = std::thread::Builder::new()
            .name("vfs-async-executor".to_string())
            .spawn(move || {
                // Create a NEW runtime - not Handle::current()!
                let rt: tokio::runtime::Runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(worker_threads)
                    .thread_name("vfs-io-worker")
                    .enable_all()
                    .build()
                    .expect("Failed to create VFS runtime");

                rt.block_on(async move {
                    let mut rx: mpsc::Receiver<WorkItem> = rx;
                    
                    loop {
                        tokio::select! {
                            item = rx.recv() => {
                                match item {
                                    Some(work_item) => {
                                        // Spawn each work item as a separate task
                                        tokio::spawn(async move {
                                            let result: Box<dyn Any + Send> = work_item.work.await;
                                            let _ = work_item.result_tx.send(result);
                                        });
                                    }
                                    None => break, // Channel closed
                                }
                            }
                            _ = token_clone.cancelled() => {
                                break; // Graceful shutdown
                            }
                        }
                    }
                });
            })
            .expect("Failed to spawn executor thread");

        Self {
            tx,
            cancel_token,
            _thread: thread,
        }
    }

    /// Create executor with default settings (4 worker threads).
    pub fn with_defaults() -> Self {
        Self::new(4)
    }

    /// Execute an async operation and block until complete.
    ///
    /// This is safe to call from FUSE callbacks because it blocks
    /// on a oneshot channel, not on the Tokio runtime.
    ///
    /// # Arguments
    /// * `future` - The async operation to execute
    ///
    /// # Returns
    /// The result of the future.
    ///
    /// # Panics
    /// Panics if the executor thread has died.
    pub fn block_on<F, T>(&self, future: F) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (result_tx, result_rx) = oneshot::channel::<Box<dyn Any + Send>>();

        // Wrap the future to box the result
        let work: BoxFuture<'static, Box<dyn Any + Send>> = async move {
            let result: T = future.await;
            Box::new(result) as Box<dyn Any + Send>
        }
        .boxed();

        // Submit work to the executor thread
        self.tx
            .blocking_send(WorkItem { work, result_tx })
            .expect("Executor thread died");

        // Block on the oneshot - this is a simple channel wait, not block_on()!
        let boxed_result: Box<dyn Any + Send> = result_rx
            .blocking_recv()
            .expect("Executor dropped result");

        *boxed_result.downcast::<T>().expect("Type mismatch")
    }

    /// Execute an async operation with cancellation support.
    ///
    /// # Arguments
    /// * `future` - The async operation to execute
    ///
    /// # Returns
    /// Ok(result) if completed, Err(Cancelled) if cancelled.
    pub fn block_on_cancellable<F, T>(&self, future: F) -> Result<T, ExecutorCancelled>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let token: CancellationToken = self.cancel_token.clone();
        
        self.block_on(async move {
            tokio::select! {
                result = future => Ok(result),
                _ = token.cancelled() => Err(ExecutorCancelled),
            }
        })
    }

    /// Cancel all in-flight operations.
    ///
    /// Called on unmount or SIGINT for graceful shutdown.
    pub fn cancel_all(&self) {
        self.cancel_token.cancel();
    }

    /// Check if the executor has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }
}

/// Error returned when an operation is cancelled.
#[derive(Debug, Clone, Copy)]
pub struct ExecutorCancelled;

impl std::fmt::Display for ExecutorCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Operation cancelled")
    }
}

impl std::error::Error for ExecutorCancelled {}
```

### Updated DeadlineVfs

```rust
pub struct DeadlineVfs {
    /// Inode manager for file metadata.
    inodes: INodeManager,
    /// File store for fetching content from S3.
    store: Arc<dyn FileStore>,
    /// Memory pool for caching content.
    pool: Arc<MemoryPool>,
    /// Optional disk cache for persistent caching.
    read_cache: Option<Arc<ReadCache>>,
    /// Hash algorithm used by the manifest.
    hash_algorithm: HashAlgorithm,
    /// Open file handles.
    handles: Arc<RwLock<HashMap<u64, OpenHandle>>>,
    /// Next file handle ID.
    next_handle: AtomicU64,
    /// VFS options.
    options: VfsOptions,
    /// Dedicated async executor (REPLACES runtime: Handle).
    executor: Arc<AsyncExecutor>,
    /// VFS creation time.
    start_time: Instant,
}

impl DeadlineVfs {
    /// Create a new VFS from a manifest.
    ///
    /// # Arguments
    /// * `manifest` - Manifest describing the filesystem
    /// * `store` - File store for fetching content
    /// * `options` - VFS configuration options
    pub fn new(
        manifest: &Manifest,
        store: Arc<dyn FileStore>,
        options: VfsOptions,
    ) -> Result<Self, VfsError> {
        let inodes = build_from_manifest(manifest);
        let hash_algorithm: HashAlgorithm = manifest.hash_alg();
        let pool = Arc::new(MemoryPool::new(options.pool.clone()));

        // Initialize read cache if enabled
        let read_cache: Option<Arc<ReadCache>> = if options.read_cache.enabled {
            let cache_options = ReadCacheOptions {
                cache_dir: options.read_cache.cache_dir.clone(),
                write_through: options.read_cache.write_through,
            };
            Some(Arc::new(
                ReadCache::new(cache_options)
                    .map_err(|e| VfsError::MountFailed(format!("Failed to init read cache: {}", e)))?,
            ))
        } else {
            None
        };

        // Create dedicated executor instead of capturing Handle::current()
        let executor = Arc::new(AsyncExecutor::new(options.executor_threads.unwrap_or(4)));

        Ok(Self {
            inodes,
            store,
            pool,
            read_cache,
            hash_algorithm,
            handles: Arc::new(RwLock::new(HashMap::new())),
            next_handle: AtomicU64::new(1),
            options,
            executor,
            start_time: Instant::now(),
        })
    }
}
```

### Updated Read Operations

```rust
impl DeadlineVfs {
    /// Read content for a single-hash (non-chunked) file.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `offset` - Byte offset to start reading
    /// * `size` - Number of bytes to read
    fn read_single_hash(&self, hash: &str, offset: u64, size: u64) -> Result<Vec<u8>, VfsError> {
        let key = BlockKey::from_hash_hex(hash, 0);
        let hash_owned: String = hash.to_string();
        let store: Arc<dyn FileStore> = self.store.clone();
        let alg: HashAlgorithm = self.hash_algorithm;
        let pool: Arc<MemoryPool> = self.pool.clone();
        let read_cache: Option<Arc<ReadCache>> = self.read_cache.clone();

        // Use dedicated executor instead of runtime.block_on()
        let handle = self.executor.block_on(async move {
            pool.acquire(&key, move || {
                let s: Arc<dyn FileStore> = store;
                let h: String = hash_owned;
                let rc: Option<Arc<ReadCache>> = read_cache;
                async move { fetch_with_cache(&h, &s, alg, rc.as_deref()).await }
            })
            .await
        });

        match handle {
            Ok(blk) => {
                let data: &[u8] = blk.data();
                let start: usize = offset as usize;
                let end: usize = (offset + size).min(data.len() as u64) as usize;
                Ok(data[start..end].to_vec())
            }
            Err(e) => Err(VfsError::ContentRetrievalFailed {
                hash: hash.to_string(),
                source: e.to_string().into(),
            }),
        }
    }

    /// Read content for a chunked file.
    ///
    /// # Arguments
    /// * `hashes` - Chunk hashes
    /// * `offset` - Byte offset to start reading
    /// * `size` - Number of bytes to read
    fn read_chunked(&self, hashes: &[String], offset: u64, size: u64) -> Result<Vec<u8>, VfsError> {
        let chunk_size: u64 = CHUNK_SIZE_V2;
        let start_chunk: usize = (offset / chunk_size) as usize;
        let end_off: u64 = offset + size;
        let end_chunk: usize = ((end_off.saturating_sub(1)) / chunk_size) as usize;

        let mut result: Vec<u8> = Vec::with_capacity(size as usize);

        for idx in start_chunk..=end_chunk {
            if idx >= hashes.len() {
                break;
            }

            let hash: String = hashes[idx].clone();
            let key = BlockKey::from_hash_hex(&hash, idx as u32);
            let hash_owned: String = hash.clone();
            let store: Arc<dyn FileStore> = self.store.clone();
            let alg: HashAlgorithm = self.hash_algorithm;
            let pool: Arc<MemoryPool> = self.pool.clone();
            let read_cache: Option<Arc<ReadCache>> = self.read_cache.clone();

            // Use dedicated executor
            let handle = self.executor.block_on(async move {
                pool.acquire(&key, move || {
                    let s: Arc<dyn FileStore> = store;
                    let h: String = hash_owned;
                    let rc: Option<Arc<ReadCache>> = read_cache;
                    async move { fetch_with_cache(&h, &s, alg, rc.as_deref()).await }
                })
                .await
            });

            match handle {
                Ok(blk) => {
                    let data: &[u8] = blk.data();
                    let chunk_start: u64 = idx as u64 * chunk_size;
                    let rs: usize = if idx == start_chunk {
                        (offset - chunk_start) as usize
                    } else {
                        0
                    };
                    let re: usize = if idx == end_chunk {
                        ((end_off - chunk_start) as usize).min(data.len())
                    } else {
                        data.len()
                    };
                    if rs < data.len() {
                        result.extend_from_slice(&data[rs..re]);
                    }
                }
                Err(e) => {
                    return Err(VfsError::ContentRetrievalFailed {
                        hash,
                        source: e.to_string().into(),
                    })
                }
            }
        }

        Ok(result)
    }
}
```

---

## Why This Resolves the Deadlock

| Aspect | Before (Problematic) | After (Safe) |
|--------|---------------------|--------------|
| **Runtime** | `Handle::current()` (shared) | `Runtime::new()` (dedicated) |
| **Blocking** | `block_on()` on Tokio worker | `blocking_recv()` on oneshot |
| **Thread Pool** | Shared with application | Isolated VFS workers |
| **Deadlock Risk** | High under load | None |

### Detailed Explanation

1. **No `block_on()` on Tokio worker threads**: The FUSE callback blocks on a simple `oneshot::blocking_recv()`, which is just a condvar wait - no Tokio involvement.

2. **Dedicated runtime**: The async work runs on a completely separate Tokio runtime with its own thread pool. Even if the main application's runtime is under load, the VFS has dedicated workers.

3. **Work isolation**: Each FUSE operation spawns as a separate Tokio task, so slow S3 fetches don't block other operations.

4. **Clean async/sync boundary**: The sync boundary is pushed to the very edge (FUSE interface only), with pure async code inside.

---

## Addressing Alice's Other Concerns

### Cancellation Handling

> "Your biggest risk is the lack of cancellation handling - if a FUSE operation is interrupted (user hits Ctrl+C, process killed), your in-flight S3 requests will continue running and potentially corrupt state."

The `AsyncExecutor` includes a `CancellationToken` that can be triggered on unmount:

```rust
impl Drop for DeadlineVfs {
    fn drop(&mut self) {
        // Cancel all in-flight operations on unmount
        self.executor.cancel_all();
    }
}
```

### Memory Pool Fetch Cancellation

> "Your memory pool's `Shared<BoxFuture>` pattern for fetch coordination is correct... but you need to be careful about cancellation - if all waiters drop their handles, does the fetch get cancelled?"

The current implementation handles this reasonably - if all waiters drop, the fetch continues but the result is discarded. For explicit abort-on-drop semantics, we could track `JoinHandle`s:

```rust
struct PendingFetch {
    shared: SharedFetch,
    handle: JoinHandle<()>,
}

impl Drop for PendingFetch {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
```

### Error Types

> "Your error handling with `Box<dyn Error>` loses the ability to match on specific error types... use concrete error types or at least `anyhow::Error` with context."

This is a valid concern for retry logic. Consider adding:

```rust
#[derive(Debug)]
pub enum VfsIoError {
    /// Transient S3 error (retry-able)
    S3Transient { source: Box<dyn std::error::Error + Send + Sync>, retries: u32 },
    /// Permanent S3 error (not retry-able)
    S3Permanent { source: Box<dyn std::error::Error + Send + Sync> },
    /// Content hash mismatch
    HashMismatch { expected: String, actual: String },
    /// Operation cancelled
    Cancelled,
}
```

---

## Configuration Options

Add to `VfsOptions`:

```rust
pub struct VfsOptions {
    // ... existing fields ...
    
    /// Number of worker threads for the async executor.
    /// Default: 4
    pub executor_threads: Option<usize>,
    
    /// Channel buffer size for work submission.
    /// Default: 1024
    pub executor_queue_size: Option<usize>,
}
```

---

## Migration Plan

### Phase 1: Add AsyncExecutor
1. Create `src/executor.rs` with `AsyncExecutor` implementation
2. Add unit tests for executor behavior
3. Add cancellation tests

### Phase 2: Update DeadlineVfs (Read-Only)
1. Replace `runtime: Handle` with `executor: Arc<AsyncExecutor>`
2. Update `read_single_hash()` and `read_chunked()`
3. Add executor shutdown in `Drop`
4. Update tests

### Phase 3: Update WritableVfs
1. Apply same pattern to `fuse_writable.rs`
2. Update `read()`, `write()`, `truncate()` operations
3. Ensure dirty file manager uses executor

### Phase 4: Testing & Validation
1. Stress test with concurrent reads
2. Test cancellation behavior
3. Verify no deadlocks under load
4. Memory leak testing with valgrind

---

## Testing Requirements

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_executor_basic() {
        let executor = AsyncExecutor::with_defaults();
        let result: i32 = executor.block_on(async { 42 });
        assert_eq!(result, 42);
    }

    #[test]
    fn test_executor_concurrent() {
        let executor = Arc::new(AsyncExecutor::with_defaults());
        let handles: Vec<_> = (0..100)
            .map(|i| {
                let exec = executor.clone();
                std::thread::spawn(move || exec.block_on(async move { i * 2 }))
            })
            .collect();

        for (i, h) in handles.into_iter().enumerate() {
            assert_eq!(h.join().unwrap(), i * 2);
        }
    }

    #[test]
    fn test_executor_cancellation() {
        let executor = AsyncExecutor::with_defaults();
        
        // Start a long-running operation
        let handle = std::thread::spawn({
            let exec = executor.clone();
            move || {
                exec.block_on_cancellable(async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    42
                })
            }
        });

        // Cancel after a short delay
        std::thread::sleep(Duration::from_millis(100));
        executor.cancel_all();

        // Should return Cancelled error
        let result = handle.join().unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_no_deadlock_under_load() {
        // Simulate the problematic scenario:
        // Many concurrent FUSE operations all waiting for async work
        let executor = Arc::new(AsyncExecutor::new(2)); // Limited workers
        
        let handles: Vec<_> = (0..50)
            .map(|_| {
                let exec = executor.clone();
                std::thread::spawn(move || {
                    exec.block_on(async {
                        // Simulate S3 fetch
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        vec![0u8; 1024]
                    })
                })
            })
            .collect();

        // All should complete without deadlock
        for h in handles {
            let data = h.join().unwrap();
            assert_eq!(data.len(), 1024);
        }
    }
}
```

---

## Summary

This refactoring addresses Alice Ryhl's core concern by:

1. **Replacing `Handle::current()` with a dedicated `Runtime::new()`** in a background thread
2. **Using channel-based coordination** (mpsc + oneshot) instead of `block_on()`
3. **Adding cancellation tokens** for graceful shutdown
4. **Keeping async code pure** - only the FUSE boundary is sync

The result is a clean separation between the synchronous FUSE world and the asynchronous I/O world, eliminating the deadlock risk while maintaining the benefits of async S3 operations.
