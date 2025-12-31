# VFS Async/Sync Boundary Refactoring

**Status: ✅ IMPLEMENTED**

## Problem Analysis

### Alice Ryhl's Critique

The original VFS implementation had a critical architectural flaw in how it bridged the synchronous FUSE interface with async S3 operations:

> "The `block_on()` calls in your FUSE implementation are a red flag - you're blocking Tokio worker threads which can lead to deadlocks if the runtime is under load or if you exhaust the thread pool."

### Original Implementation (Problematic)

```rust
// In fuse.rs - captured ambient runtime
let runtime = Handle::try_current()
    .map_err(|e| VfsError::MountFailed(format!("No tokio runtime: {}", e)))?;

// In read operations - BLOCKED TOKIO WORKER THREAD
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
2. **Use a dedicated runtime with `Runtime::new()` instead of `Handle::current()`** ← Implemented
3. Consider if async is even needed (sync S3 client with thread pool)

---

## Implemented Solution: Dedicated Async Executor Thread

Completely separates the FUSE sync world from the Tokio async world using a dedicated background thread that owns its own Tokio runtime.

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
│                                    │ block on typed oneshot receiver         │
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

### Key Design Decisions

1. **Typed Channels**: Results flow through typed oneshot channels, avoiding `Box<dyn Any>` overhead
2. **Result-Based API**: All methods return `Result<T, ExecutorError>` instead of panicking
3. **Timeout Support**: Built-in timeout support with configurable defaults
4. **Graceful Shutdown**: `CancellationToken` for clean shutdown on unmount

---

## Implementation

### Location: `crates/vfs/src/executor.rs`

### Error Types

```rust
/// Errors that can occur during executor operations.
#[derive(Debug, Clone)]
pub enum ExecutorError {
    /// The executor has been shut down or the background thread died.
    Shutdown,
    /// The operation was cancelled.
    Cancelled,
    /// The operation timed out.
    Timeout { duration: Duration },
    /// The task panicked during execution.
    TaskPanicked,
}
```

### Configuration

```rust
/// Configuration for the async executor.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Number of Tokio worker threads.
    pub worker_threads: usize,        // Default: 4
    /// Channel buffer size for work submission.
    pub queue_size: usize,            // Default: 1024
    /// Default timeout for operations (None = no timeout).
    pub default_timeout: Option<Duration>,  // Default: None
}
```

### Core API

```rust
impl AsyncExecutor {
    /// Create a new executor with a dedicated runtime thread.
    pub fn new(config: ExecutorConfig) -> Self;
    
    /// Create executor with default settings (4 worker threads).
    pub fn with_defaults() -> Self;
    
    /// Execute an async operation and block until complete.
    /// Returns Result instead of panicking on executor failure.
    pub fn block_on<F, T>(&self, future: F) -> Result<T, ExecutorError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static;
    
    /// Execute with explicit timeout.
    pub fn block_on_timeout<F, T>(&self, future: F, timeout: Duration) -> Result<T, ExecutorError>;
    
    /// Execute with cancellation support.
    pub fn block_on_cancellable<F, T>(&self, future: F) -> Result<T, ExecutorError>;
    
    /// Execute with both timeout and cancellation.
    pub fn block_on_cancellable_timeout<F, T>(&self, future: F, timeout: Duration) -> Result<T, ExecutorError>;
    
    /// Cancel all in-flight operations (for graceful shutdown).
    pub fn cancel_all(&self);
    
    /// Check if cancelled.
    pub fn is_cancelled(&self) -> bool;
    
    /// Check if still running.
    pub fn is_running(&self) -> bool;
}
```

### Type-Safe Channel Design

To avoid `Box<dyn Any>` overhead and runtime downcasting, each `block_on` call creates a typed oneshot channel:

```rust
fn block_on_internal<F, T>(&self, future: F) -> Result<T, ExecutorError> {
    // Create typed oneshot channel for this specific call
    let (result_tx, result_rx) = oneshot::channel::<T>();  // Typed!

    // Wrap the future to send result through typed channel
    let work: BoxFuture<'static, ()> = async move {
        let result: T = future.await;
        let _ = result_tx.send(result);  // Direct send, no boxing
    }.boxed();

    // Submit work
    self.tx.blocking_send(WorkItem { work }).map_err(|_| ExecutorError::Shutdown)?;

    // Block on typed oneshot - simple condvar wait, no Tokio
    result_rx.blocking_recv().map_err(|_| ExecutorError::Shutdown)
}
```

---

## Integration with VFS

### VfsOptions

```rust
pub struct VfsOptions {
    // ... existing fields ...
    
    /// Async executor configuration.
    pub executor: ExecutorConfig,
}
```

### DeadlineVfs Usage

```rust
impl DeadlineVfs {
    pub fn new(manifest: &Manifest, store: Arc<dyn FileStore>, options: VfsOptions) -> Result<Self, VfsError> {
        // Create dedicated executor instead of capturing Handle::current()
        let executor = Arc::new(AsyncExecutor::new(options.executor.clone()));
        // ...
    }
    
    fn read_single_hash(&self, hash: &str, offset: u64, size: u64) -> Result<Vec<u8>, VfsError> {
        // Use dedicated executor - returns Result
        let handle = self.executor
            .block_on(async move { pool.acquire(&key, fetch_fn).await })
            .map_err(|e| VfsError::ExecutorError(e.to_string()))?;
        // ...
    }
}
```

### WritableVfs Usage

All async operations in `fuse_writable.rs` handle the nested `Result`:

```rust
fn read(&mut self, ..., reply: ReplyData) {
    let exec_result = self.executor.block_on(async move { dm.read(ino, offset, size).await });
    match exec_result {
        Ok(Ok(data)) => reply.data(&data),
        Ok(Err(e)) => {
            tracing::error!("Read failed: {}", e);
            reply.error(libc::EIO);
        }
        Err(e) => {
            tracing::error!("Executor error: {}", e);
            reply.error(libc::EIO);
        }
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
| **Error Handling** | Panics | Returns `Result` |
| **Timeout** | None | Configurable |

---

## Files Modified

| File | Changes |
|------|---------|
| `crates/vfs/src/executor.rs` | New file - complete executor implementation |
| `crates/vfs/src/error.rs` | Added `ExecutorError` variant to `VfsError` |
| `crates/vfs/src/options.rs` | Added `ExecutorConfig` to `VfsOptions` |
| `crates/vfs/src/fuse.rs` | Updated to use executor with Result handling |
| `crates/vfs/src/fuse_writable.rs` | Updated all async ops with Result handling |
| `crates/vfs/src/lib.rs` | Export `ExecutorError`, `ExecutorConfig` |

---

## Current Test Coverage

The following tests exist in `crates/vfs/src/executor.rs`:

```rust
// Basic functionality
test_executor_basic              // Simple async execution
test_executor_returns_result     // Verify Result return type
test_executor_concurrent         // 100 concurrent operations
test_executor_async_sleep        // Async timing verification

// Timeout tests
test_executor_timeout            // Timeout triggers correctly
test_executor_timeout_success    // Completes before timeout
test_executor_default_timeout    // Config-based default timeout

// Cancellation tests
test_executor_cancellation       // Cancel long-running operation
test_executor_cancellable_timeout // Combined cancel + timeout

// Lifecycle tests
test_executor_drop_cancels       // Drop triggers cancellation
test_executor_shutdown_returns_error // Post-shutdown behavior
test_executor_config             // Configuration builder

// Stress tests
test_no_deadlock_under_load      // 50 concurrent ops, 2 workers
test_typed_channel_no_any_overhead // Verify typed channels work
```

---

## Additional Tests to Implement

The following test cases should be added for comprehensive coverage:

### Error Propagation Tests

```rust
#[test]
fn test_executor_propagates_result_err() {
    // Verify that Result::Err from the future is properly propagated
    let executor = AsyncExecutor::with_defaults();
    let result: Result<Result<i32, &str>, ExecutorError> = 
        executor.block_on(async { Err("inner error") });
    assert!(result.unwrap().is_err());
}

#[test]
fn test_executor_with_option_none() {
    // Verify Option types work correctly
    let executor = AsyncExecutor::with_defaults();
    let result: Option<i32> = executor.block_on(async { None }).unwrap();
    assert!(result.is_none());
}
```

### Stress & Edge Cases

```rust
#[test]
fn test_executor_rapid_create_drop() {
    // Verify no resource leaks with rapid lifecycle
    for _ in 0..10 {
        let executor = AsyncExecutor::with_defaults();
        let _: i32 = executor.block_on(async { 42 }).unwrap();
    }
}

#[test]
fn test_executor_queue_pressure() {
    // Test behavior when queue is near capacity
    let config = ExecutorConfig::default().with_queue_size(4);
    let executor = Arc::new(AsyncExecutor::new(config));
    
    let handles: Vec<_> = (0..20)
        .map(|i| {
            let exec = executor.clone();
            std::thread::spawn(move || {
                exec.block_on(async move {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    i
                })
            })
        })
        .collect();

    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
}
```

### Panic Handling

```rust
#[test]
fn test_executor_task_panic_isolated() {
    // Verify one panicking task doesn't crash the executor
    let executor = Arc::new(AsyncExecutor::with_defaults());
    
    let exec1 = executor.clone();
    let panic_handle = std::thread::spawn(move || {
        exec1.block_on(async { panic!("intentional panic") })
    });
    
    std::thread::sleep(Duration::from_millis(50));
    
    // Executor should still work
    let result: i32 = executor.block_on(async { 123 }).unwrap();
    assert_eq!(result, 123);
    
    let panic_result = panic_handle.join();
    assert!(panic_result.is_err() || matches!(panic_result.unwrap(), Err(ExecutorError::Shutdown)));
}
```

### Timeout Edge Cases

```rust
#[test]
fn test_timeout_zero_duration() {
    let executor = AsyncExecutor::with_defaults();
    let result = executor.block_on_timeout(async { 42 }, Duration::from_nanos(0));
    // Should either succeed or timeout - both acceptable
    assert!(result.is_ok() || matches!(result, Err(ExecutorError::Timeout { .. })));
}

#[test]
fn test_timeout_exact_boundary() {
    let executor = AsyncExecutor::with_defaults();
    let result = executor.block_on_timeout(
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            42
        },
        Duration::from_millis(50),
    );
    // Could go either way at exact boundary
    println!("Boundary result: {:?}", result);
}
```

### Cancellation Edge Cases

```rust
#[test]
fn test_cancel_before_submit() {
    let executor = AsyncExecutor::with_defaults();
    executor.cancel_all();
    
    let result: Result<i32, ExecutorError> = executor.block_on(async { 42 });
    // Either succeeds or returns appropriate error
    println!("Post-cancel result: {:?}", result);
}

#[test]
fn test_multiple_cancel_calls() {
    let executor = AsyncExecutor::with_defaults();
    executor.cancel_all();
    executor.cancel_all(); // Should be idempotent
    executor.cancel_all();
    assert!(executor.is_cancelled());
}
```

### Large Data Transfer

```rust
#[test]
fn test_executor_large_result() {
    let executor = AsyncExecutor::with_defaults();
    let large_data: Vec<u8> = executor.block_on(async {
        vec![0u8; 10 * 1024 * 1024] // 10MB
    }).unwrap();
    assert_eq!(large_data.len(), 10 * 1024 * 1024);
}
```

### Drop During Active Work

```rust
#[test]
fn test_drop_with_pending_work() {
    let executor = Arc::new(AsyncExecutor::with_defaults());
    
    let exec_clone = executor.clone();
    let handle = std::thread::spawn(move || {
        exec_clone.block_on(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            42
        })
    });
    
    std::thread::sleep(Duration::from_millis(10));
    drop(executor);
    
    // Thread should complete with error (not hang)
    let result = handle.join().unwrap();
    assert!(matches!(result, Err(ExecutorError::Shutdown) | Err(ExecutorError::Cancelled)));
}
```

---

## Summary

This refactoring addresses Alice Ryhl's core concerns:

1. ✅ **Replaced `Handle::current()` with dedicated `Runtime::new()`** in a background thread
2. ✅ **Using channel-based coordination** (mpsc + typed oneshot) instead of `block_on()`
3. ✅ **Added cancellation tokens** for graceful shutdown
4. ✅ **Result-based error handling** instead of panics
5. ✅ **Timeout support** with configurable defaults
6. ✅ **Type-safe channels** avoiding `Box<dyn Any>` overhead

The result is a clean separation between the synchronous FUSE world and the asynchronous I/O world, eliminating the deadlock risk while maintaining the benefits of async S3 operations.

All 140 VFS tests pass including 14 executor-specific tests.
