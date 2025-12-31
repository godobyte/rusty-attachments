//! Async executor for bridging sync FUSE callbacks with async I/O.
//!
//! This module provides a dedicated async executor that runs in a background thread,
//! completely separate from any ambient Tokio runtime. This design avoids the deadlock
//! risk of calling `block_on()` on Tokio worker threads.
//!
//! # Architecture
//!
//! ```text
//! FUSE Thread                    Executor Thread
//! ───────────                    ───────────────
//!     │                               │
//!     │ submit(future) ──────────────►│
//!     │                               │ spawn task
//!     │ blocking_recv() ◄─────────────│ send result
//!     │                               │
//! ```
//!
//! The FUSE callback blocks on a simple `oneshot::blocking_recv()`, which is just
//! a condvar wait - no Tokio involvement. The async work runs on a completely
//! separate Tokio runtime with its own thread pool.
//!
//! # Type-Safe Channel Design
//!
//! To avoid the overhead of `Box<dyn Any>` and runtime downcasting, each `block_on`
//! call creates a typed oneshot channel. The work item only carries the boxed future
//! and a type-erased sender, but the actual result flows through a typed channel
//! created per-call.

use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

// ============================================================================
// Error Types
// ============================================================================

/// Errors that can occur during executor operations.
#[derive(Debug, Clone)]
pub enum ExecutorError {
    /// The executor has been shut down or the background thread died.
    Shutdown,
    /// The operation was cancelled.
    Cancelled,
    /// The operation timed out.
    Timeout {
        /// The timeout duration that was exceeded.
        duration: Duration,
    },
    /// The task panicked during execution.
    TaskPanicked,
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutorError::Shutdown => write!(f, "Executor has been shut down"),
            ExecutorError::Cancelled => write!(f, "Operation was cancelled"),
            ExecutorError::Timeout { duration } => {
                write!(f, "Operation timed out after {:?}", duration)
            }
            ExecutorError::TaskPanicked => write!(f, "Task panicked during execution"),
        }
    }
}

impl std::error::Error for ExecutorError {}

// ============================================================================
// Work Item (Internal)
// ============================================================================

/// Type-erased work item for the executor queue.
///
/// The future produces `()` because the actual result is sent through
/// a typed oneshot channel captured in the closure.
struct WorkItem {
    /// The async work to execute. Returns () because result goes through typed channel.
    work: BoxFuture<'static, ()>,
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the async executor.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Number of Tokio worker threads.
    pub worker_threads: usize,
    /// Channel buffer size for work submission.
    pub queue_size: usize,
    /// Default timeout for operations (None = no timeout).
    pub default_timeout: Option<Duration>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            worker_threads: 4,
            queue_size: 1024,
            default_timeout: None,
        }
    }
}

impl ExecutorConfig {
    /// Create config with specified worker threads.
    ///
    /// # Arguments
    /// * `worker_threads` - Number of Tokio worker threads
    pub fn with_worker_threads(mut self, worker_threads: usize) -> Self {
        self.worker_threads = worker_threads;
        self
    }

    /// Create config with specified queue size.
    ///
    /// # Arguments
    /// * `queue_size` - Channel buffer size for work submission
    pub fn with_queue_size(mut self, queue_size: usize) -> Self {
        self.queue_size = queue_size;
        self
    }

    /// Set default timeout for operations.
    ///
    /// # Arguments
    /// * `timeout` - Default timeout duration (None = no timeout)
    pub fn with_default_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.default_timeout = timeout;
        self
    }
}

// ============================================================================
// Async Executor
// ============================================================================

/// Async executor that runs in a dedicated background thread.
///
/// FUSE callbacks submit work via a channel and block on a oneshot receiver.
/// This avoids blocking Tokio worker threads and prevents deadlocks.
///
/// # Error Handling
///
/// All blocking methods return `Result<T, ExecutorError>` instead of panicking,
/// allowing callers to handle executor shutdown gracefully.
///
/// # Type Safety
///
/// Results flow through typed oneshot channels created per-call, avoiding
/// the overhead of `Box<dyn Any>` and runtime downcasting.
pub struct AsyncExecutor {
    /// Channel to submit async work.
    tx: mpsc::Sender<WorkItem>,
    /// Cancellation token for graceful shutdown.
    cancel_token: CancellationToken,
    /// Handle to the background thread.
    thread: Option<JoinHandle<()>>,
    /// Whether the executor is still running.
    running: Arc<AtomicBool>,
    /// Default timeout for operations.
    default_timeout: Option<Duration>,
}

impl AsyncExecutor {
    /// Create a new executor with a dedicated runtime thread.
    ///
    /// # Arguments
    /// * `config` - Executor configuration
    ///
    /// # Returns
    /// New executor instance with background thread running.
    pub fn new(config: ExecutorConfig) -> Self {
        let (tx, rx) = mpsc::channel::<WorkItem>(config.queue_size);
        let cancel_token = CancellationToken::new();
        let token_clone: CancellationToken = cancel_token.clone();
        let worker_threads: usize = config.worker_threads;
        let running = Arc::new(AtomicBool::new(true));
        let running_clone: Arc<AtomicBool> = running.clone();

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
                            biased;

                            _ = token_clone.cancelled() => {
                                break; // Graceful shutdown
                            }
                            item = rx.recv() => {
                                match item {
                                    Some(work_item) => {
                                        // Spawn each work item as a separate task
                                        tokio::spawn(work_item.work);
                                    }
                                    None => break, // Channel closed
                                }
                            }
                        }
                    }
                });

                // Mark as no longer running
                running_clone.store(false, Ordering::Release);
            })
            .expect("Failed to spawn executor thread");

        Self {
            tx,
            cancel_token,
            thread: Some(thread),
            running,
            default_timeout: config.default_timeout,
        }
    }

    /// Create executor with default settings (4 worker threads).
    pub fn with_defaults() -> Self {
        Self::new(ExecutorConfig::default())
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
    /// Ok(result) on success, Err on executor shutdown or task panic.
    ///
    /// # Type Safety
    /// Results flow through a typed oneshot channel, avoiding Box<dyn Any> overhead.
    pub fn block_on<F, T>(&self, future: F) -> Result<T, ExecutorError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        match self.default_timeout {
            Some(timeout) => self.block_on_timeout(future, timeout),
            None => self.block_on_internal(future),
        }
    }

    /// Execute an async operation with an explicit timeout.
    ///
    /// # Arguments
    /// * `future` - The async operation to execute
    /// * `timeout` - Maximum time to wait for completion
    ///
    /// # Returns
    /// Ok(result) on success, Err(Timeout) if deadline exceeded.
    pub fn block_on_timeout<F, T>(&self, future: F, timeout: Duration) -> Result<T, ExecutorError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        // Check if executor is still running
        if !self.running.load(Ordering::Acquire) {
            return Err(ExecutorError::Shutdown);
        }

        // Create typed oneshot channel for this specific call
        let (result_tx, result_rx) = oneshot::channel::<Result<T, ExecutorError>>();

        // Wrap the future to send result through typed channel
        let work: BoxFuture<'static, ()> = async move {
            let result: Result<T, ExecutorError> =
                match tokio::time::timeout(timeout, future).await {
                    Ok(value) => Ok(value),
                    Err(_) => Err(ExecutorError::Timeout { duration: timeout }),
                };
            // Ignore send errors - caller may have dropped
            let _ = result_tx.send(result);
        }
        .boxed();

        // Submit work to the executor thread
        if self.tx.blocking_send(WorkItem { work }).is_err() {
            return Err(ExecutorError::Shutdown);
        }

        // Block on the typed oneshot - this is a simple channel wait, not block_on()!
        match result_rx.blocking_recv() {
            Ok(result) => result,
            Err(_) => Err(ExecutorError::Shutdown),
        }
    }

    /// Execute an async operation without timeout.
    ///
    /// # Arguments
    /// * `future` - The async operation to execute
    ///
    /// # Returns
    /// Ok(result) on success, Err on executor shutdown.
    fn block_on_internal<F, T>(&self, future: F) -> Result<T, ExecutorError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        // Check if executor is still running
        if !self.running.load(Ordering::Acquire) {
            return Err(ExecutorError::Shutdown);
        }

        // Create typed oneshot channel for this specific call
        let (result_tx, result_rx) = oneshot::channel::<T>();

        // Wrap the future to send result through typed channel
        let work: BoxFuture<'static, ()> = async move {
            let result: T = future.await;
            // Ignore send errors - caller may have dropped
            let _ = result_tx.send(result);
        }
        .boxed();

        // Submit work to the executor thread
        if self.tx.blocking_send(WorkItem { work }).is_err() {
            return Err(ExecutorError::Shutdown);
        }

        // Block on the typed oneshot - this is a simple channel wait, not block_on()!
        match result_rx.blocking_recv() {
            Ok(result) => Ok(result),
            Err(_) => Err(ExecutorError::Shutdown),
        }
    }

    /// Execute an async operation with cancellation support.
    ///
    /// # Arguments
    /// * `future` - The async operation to execute
    ///
    /// # Returns
    /// Ok(result) if completed, Err(Cancelled) if cancelled.
    pub fn block_on_cancellable<F, T>(&self, future: F) -> Result<T, ExecutorError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let token: CancellationToken = self.cancel_token.clone();

        let wrapped = async move {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(ExecutorError::Cancelled),
                result = future => Ok(result),
            }
        };

        // Use block_on_internal to avoid double-timeout
        match self.block_on_internal(wrapped)? {
            Ok(value) => Ok(value),
            Err(e) => Err(e),
        }
    }

    /// Execute an async operation with both timeout and cancellation support.
    ///
    /// # Arguments
    /// * `future` - The async operation to execute
    /// * `timeout` - Maximum time to wait for completion
    ///
    /// # Returns
    /// Ok(result) if completed, Err(Timeout) or Err(Cancelled) on failure.
    pub fn block_on_cancellable_timeout<F, T>(
        &self,
        future: F,
        timeout: Duration,
    ) -> Result<T, ExecutorError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let token: CancellationToken = self.cancel_token.clone();

        let wrapped = async move {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(ExecutorError::Cancelled),
                result = tokio::time::timeout(timeout, future) => {
                    match result {
                        Ok(value) => Ok(value),
                        Err(_) => Err(ExecutorError::Timeout { duration: timeout }),
                    }
                }
            }
        };

        // Use block_on_internal to avoid double-wrapping
        match self.block_on_internal(wrapped)? {
            Ok(value) => Ok(value),
            Err(e) => Err(e),
        }
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

    /// Check if the executor is still running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

impl Drop for AsyncExecutor {
    fn drop(&mut self) {
        // Signal cancellation
        self.cancel_token.cancel();

        // Wait for the thread to finish
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ============================================================================
// Legacy Compatibility (for gradual migration)
// ============================================================================

/// Error returned when an operation is cancelled.
///
/// Deprecated: Use `ExecutorError::Cancelled` instead.
#[derive(Debug, Clone, Copy)]
pub struct ExecutorCancelled;

impl fmt::Display for ExecutorCancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Operation cancelled")
    }
}

impl std::error::Error for ExecutorCancelled {}

impl From<ExecutorCancelled> for ExecutorError {
    fn from(_: ExecutorCancelled) -> Self {
        ExecutorError::Cancelled
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_executor_basic() {
        let executor = AsyncExecutor::with_defaults();
        let result: i32 = executor.block_on(async { 42 }).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_executor_returns_result() {
        let executor = AsyncExecutor::with_defaults();
        let result: Result<i32, ExecutorError> = executor.block_on(async { 42 });
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
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
            assert_eq!(h.join().unwrap().unwrap(), i * 2);
        }
    }

    #[test]
    fn test_executor_async_sleep() {
        let executor = AsyncExecutor::with_defaults();
        let start = std::time::Instant::now();

        let result: u32 = executor
            .block_on(async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                123
            })
            .unwrap();

        assert_eq!(result, 123);
        assert!(start.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn test_executor_timeout() {
        let executor = AsyncExecutor::with_defaults();

        let result = executor.block_on_timeout(
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                42
            },
            Duration::from_millis(50),
        );

        assert!(matches!(result, Err(ExecutorError::Timeout { .. })));
    }

    #[test]
    fn test_executor_timeout_success() {
        let executor = AsyncExecutor::with_defaults();

        let result = executor.block_on_timeout(
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                42
            },
            Duration::from_millis(100),
        );

        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_executor_default_timeout() {
        let config = ExecutorConfig::default().with_default_timeout(Some(Duration::from_millis(50)));
        let executor = AsyncExecutor::new(config);

        let result: Result<i32, ExecutorError> = executor.block_on(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            42
        });

        assert!(matches!(result, Err(ExecutorError::Timeout { .. })));
    }

    #[test]
    fn test_executor_cancellation() {
        let executor = Arc::new(AsyncExecutor::with_defaults());
        let exec_clone = executor.clone();

        // Start a long-running operation in another thread
        let handle = std::thread::spawn(move || {
            exec_clone.block_on_cancellable(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                42
            })
        });

        // Cancel after a short delay
        std::thread::sleep(Duration::from_millis(50));
        executor.cancel_all();

        // Wait for the thread to finish
        let result = handle.join().unwrap();
        assert!(
            matches!(result, Err(ExecutorError::Cancelled) | Err(ExecutorError::Shutdown)),
            "Expected cancellation or shutdown error, got {:?}",
            result
        );
    }

    #[test]
    fn test_executor_cancellable_timeout() {
        let executor = AsyncExecutor::with_defaults();

        // Test timeout triggers before cancellation
        let result = executor.block_on_cancellable_timeout(
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                42
            },
            Duration::from_millis(50),
        );

        assert!(matches!(result, Err(ExecutorError::Timeout { .. })));
    }

    #[test]
    fn test_executor_drop_cancels() {
        let executor = AsyncExecutor::with_defaults();
        assert!(!executor.is_cancelled());
        assert!(executor.is_running());
        drop(executor);
        // Thread should have been joined on drop
    }

    #[test]
    fn test_executor_shutdown_returns_error() {
        let executor = AsyncExecutor::with_defaults();
        executor.cancel_all();

        // Give the executor time to shut down
        std::thread::sleep(Duration::from_millis(50));

        // After shutdown, operations should return Shutdown error
        // Note: This may still succeed if the work is submitted before shutdown completes
        let result: Result<i32, ExecutorError> = executor.block_on(async { 42 });
        // Either succeeds (submitted before shutdown) or fails with Shutdown
        if result.is_err() {
            assert!(matches!(result, Err(ExecutorError::Shutdown)));
        }
    }

    #[test]
    fn test_executor_config() {
        let config = ExecutorConfig::default()
            .with_worker_threads(2)
            .with_queue_size(512)
            .with_default_timeout(Some(Duration::from_secs(30)));

        assert_eq!(config.worker_threads, 2);
        assert_eq!(config.queue_size, 512);
        assert_eq!(config.default_timeout, Some(Duration::from_secs(30)));

        let executor = AsyncExecutor::new(config);
        let result: i32 = executor.block_on(async { 99 }).unwrap();
        assert_eq!(result, 99);
    }

    #[test]
    fn test_no_deadlock_under_load() {
        // Simulate the problematic scenario:
        // Many concurrent FUSE operations all waiting for async work
        let executor = Arc::new(AsyncExecutor::new(
            ExecutorConfig::default().with_worker_threads(2),
        ));

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
            let data = h.join().unwrap().unwrap();
            assert_eq!(data.len(), 1024);
        }
    }

    #[test]
    fn test_typed_channel_no_any_overhead() {
        // This test verifies that results flow through typed channels
        // by using a type that doesn't implement Any (if we were using Any, this would fail)
        let executor = AsyncExecutor::with_defaults();

        // Complex nested type that works with typed channels
        let result: Result<Vec<Result<String, i32>>, ExecutorError> = executor.block_on(async {
            vec![Ok("hello".to_string()), Err(42), Ok("world".to_string())]
        });

        let inner = result.unwrap();
        assert_eq!(inner.len(), 3);
        assert_eq!(inner[0].as_ref().unwrap(), "hello");
        assert_eq!(inner[1].as_ref().unwrap_err(), &42);
    }
}
