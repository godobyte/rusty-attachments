//! Memory pool for managing fixed-size blocks with LRU eviction.
//!
//! This module provides a memory pool optimized for V2 manifest chunk handling.
//! Blocks are 256MB (matching `CHUNK_SIZE_V2`) and are managed with LRU eviction
//! when the pool reaches its configured maximum size.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      MemoryPool                              │
//! │  ┌─────────────────────────────────────────────────────┐    │
//! │  │  blocks: HashMap<BlockId, Arc<PoolBlock>>           │    │
//! │  │  pending_fetches: HashMap<BlockKey, Shared<Future>> │    │
//! │  │  lru_order: VecDeque<BlockId>  (front=oldest)       │    │
//! │  │  current_size / max_size                            │    │
//! │  └─────────────────────────────────────────────────────┘    │
//! └─────────────────────────────────────────────────────────────┘
//!
//! PoolBlock:
//!   - data: Arc<Vec<u8>> (lock-free reads)
//!   - key: BlockKey
//!   - ref_count: AtomicUsize
//! ```
//!
//! # Thread Safety
//!
//! - Block data stored in `Arc<Vec<u8>>` for lock-free reads
//! - `ref_count` uses `AtomicUsize` for lock-free increment/decrement
//! - Fetch coordination prevents duplicate S3 requests (thundering herd)
//! - Pool metadata protected by `Mutex` (held only for quick HashMap ops)
//!
//! # Usage
//!
//! ```ignore
//! let pool = MemoryPool::new(MemoryPoolConfig::default());
//! 
//! // Acquire a block (fetches from S3 if not cached)
//! let handle = pool.acquire(&key, || async { fetch_from_s3(&key).await }).await?;
//! 
//! // Read data directly - no lock needed
//! let data: &[u8] = handle.data();
//! 
//! // Handle automatically releases on drop
//! ```

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, Shared};
use futures::FutureExt;
use tokio::sync::oneshot;

use rusty_attachments_common::CHUNK_SIZE_V2;

/// Unique identifier for a block in the pool.
pub type BlockId = u64;

/// Default maximum pool size (8GB).
pub const DEFAULT_MAX_POOL_SIZE: u64 = 8 * 1024 * 1024 * 1024;

/// Default block size (256MB, matching V2 chunk size).
pub const DEFAULT_BLOCK_SIZE: u64 = CHUNK_SIZE_V2;


// ============================================================================
// Error Types
// ============================================================================

/// Errors that can occur during memory pool operations.
#[derive(Debug, Clone)]
pub enum MemoryPoolError {
    /// Block not found in pool.
    BlockNotFound(BlockId),

    /// Pool is at capacity and all blocks are in use (cannot evict).
    PoolExhausted {
        current_blocks: usize,
        in_use_blocks: usize,
    },

    /// Content retrieval failed.
    RetrievalFailed(String),
}

impl fmt::Display for MemoryPoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryPoolError::BlockNotFound(id) => write!(f, "Block not found: {}", id),
            MemoryPoolError::PoolExhausted {
                current_blocks,
                in_use_blocks,
            } => write!(
                f,
                "Pool exhausted: {} blocks allocated, {} in use",
                current_blocks, in_use_blocks
            ),
            MemoryPoolError::RetrievalFailed(msg) => write!(f, "Content retrieval failed: {}", msg),
        }
    }
}

impl std::error::Error for MemoryPoolError {}

// ============================================================================
// Block Key
// ============================================================================

/// Key identifying a unique block of content.
///
/// Combines a content hash with a chunk index to uniquely identify
/// a specific 256MB chunk within a file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockKey {
    /// Content hash (e.g., xxh128 hash of the chunk).
    pub hash: String,
    /// Chunk index within the file (0 for non-chunked files).
    pub chunk_index: u32,
}

impl BlockKey {
    /// Create a new block key.
    ///
    /// # Arguments
    /// * `hash` - Content hash identifying the chunk
    /// * `chunk_index` - Zero-based chunk index within the file
    pub fn new(hash: impl Into<String>, chunk_index: u32) -> Self {
        Self {
            hash: hash.into(),
            chunk_index,
        }
    }

    /// Create a block key for a non-chunked file (chunk_index = 0).
    ///
    /// # Arguments
    /// * `hash` - Content hash of the entire file
    pub fn single(hash: impl Into<String>) -> Self {
        Self::new(hash, 0)
    }
}

impl fmt::Display for BlockKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.hash, self.chunk_index)
    }
}

// ============================================================================
// Pool Block
// ============================================================================

/// A single block of memory in the pool.
struct PoolBlock {
    /// Key identifying the content stored in this block.
    key: BlockKey,
    /// The actual data buffer (Arc for lock-free sharing).
    data: Arc<Vec<u8>>,
    /// Number of active references to this block (atomic for lock-free updates).
    ref_count: AtomicUsize,
}

impl PoolBlock {
    /// Check if this block can be evicted (no active references).
    fn can_evict(&self) -> bool {
        self.ref_count.load(Ordering::Acquire) == 0
    }

    /// Get the size of this block in bytes.
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    /// Increment reference count.
    fn acquire(&self) {
        self.ref_count.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement reference count.
    fn release(&self) {
        self.ref_count.fetch_sub(1, Ordering::AcqRel);
    }
}

impl fmt::Debug for PoolBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PoolBlock")
            .field("key", &self.key)
            .field("size", &self.data.len())
            .field("ref_count", &self.ref_count.load(Ordering::Relaxed))
            .finish()
    }
}


// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the memory pool.
#[derive(Debug, Clone)]
pub struct MemoryPoolConfig {
    /// Maximum total size of the pool in bytes.
    pub max_size: u64,
    /// Size of each block in bytes.
    pub block_size: u64,
}

impl Default for MemoryPoolConfig {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_MAX_POOL_SIZE,
            block_size: DEFAULT_BLOCK_SIZE,
        }
    }
}

impl MemoryPoolConfig {
    /// Create a new configuration with custom max size.
    ///
    /// # Arguments
    /// * `max_size` - Maximum pool size in bytes
    pub fn with_max_size(max_size: u64) -> Self {
        Self {
            max_size,
            ..Default::default()
        }
    }

    /// Calculate the maximum number of blocks this pool can hold.
    pub fn max_blocks(&self) -> usize {
        (self.max_size / self.block_size) as usize
    }
}

// ============================================================================
// Block Handle
// ============================================================================

/// RAII handle to a block in the pool.
///
/// Provides direct access to block data without locks and automatically
/// decrements the reference count when dropped.
pub struct BlockHandle {
    /// Direct reference to block data - no lock needed for reads.
    data: Arc<Vec<u8>>,
    /// Reference to the block for ref_count management.
    block: Arc<PoolBlock>,
}

impl BlockHandle {
    /// Get a reference to the block's data.
    ///
    /// This is lock-free - data is accessed directly via Arc.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Get the size of the block data in bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the block is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl Drop for BlockHandle {
    fn drop(&mut self) {
        self.block.release();
    }
}

impl AsRef<[u8]> for BlockHandle {
    fn as_ref(&self) -> &[u8] {
        self.data()
    }
}

// ============================================================================
// Pending Fetch
// ============================================================================

/// Result of a pending fetch operation.
type FetchResult = Result<Arc<Vec<u8>>, String>;

/// Shared future for coordinating concurrent fetches of the same key.
type SharedFetch = Shared<BoxFuture<'static, FetchResult>>;


// ============================================================================
// Memory Pool Inner
// ============================================================================

/// Internal state of the memory pool.
struct MemoryPoolInner {
    /// Configuration for this pool.
    config: MemoryPoolConfig,
    /// All blocks currently in the pool.
    blocks: HashMap<BlockId, Arc<PoolBlock>>,
    /// Map from content key to block ID for fast lookup.
    key_index: HashMap<BlockKey, BlockId>,
    /// In-flight fetches to prevent duplicate requests.
    pending_fetches: HashMap<BlockKey, SharedFetch>,
    /// LRU order: front = oldest (evict first), back = newest.
    lru_order: VecDeque<BlockId>,
    /// Current total size of all blocks in bytes.
    current_size: u64,
    /// Next block ID to allocate.
    next_id: BlockId,
}

impl MemoryPoolInner {
    /// Create a new pool inner with the given configuration.
    ///
    /// # Arguments
    /// * `config` - Pool configuration
    fn new(config: MemoryPoolConfig) -> Self {
        Self {
            config,
            blocks: HashMap::new(),
            key_index: HashMap::new(),
            pending_fetches: HashMap::new(),
            lru_order: VecDeque::new(),
            current_size: 0,
            next_id: 1,
        }
    }

    /// Look up a block by its content key.
    ///
    /// # Arguments
    /// * `key` - The content key to look up
    ///
    /// # Returns
    /// The block if found, None otherwise.
    fn lookup(&mut self, key: &BlockKey) -> Option<Arc<PoolBlock>> {
        let block_id: BlockId = *self.key_index.get(key)?;
        let block: Arc<PoolBlock> = self.blocks.get(&block_id)?.clone();
        self.touch_lru(block_id);
        Some(block)
    }

    /// Move a block to the back of the LRU queue (most recently used).
    ///
    /// # Arguments
    /// * `block_id` - ID of the block to touch
    fn touch_lru(&mut self, block_id: BlockId) {
        self.lru_order.retain(|&id| id != block_id);
        self.lru_order.push_back(block_id);
    }

    /// Insert a new block into the pool.
    ///
    /// # Arguments
    /// * `key` - Content key for the block
    /// * `data` - The chunk data wrapped in Arc
    ///
    /// # Returns
    /// The newly created block.
    fn insert(&mut self, key: BlockKey, data: Arc<Vec<u8>>) -> Arc<PoolBlock> {
        let block_id: BlockId = self.next_id;
        self.next_id += 1;

        let size: u64 = data.len() as u64;
        let block: Arc<PoolBlock> = Arc::new(PoolBlock {
            key: key.clone(),
            data,
            ref_count: AtomicUsize::new(0),
        });

        self.blocks.insert(block_id, block.clone());
        self.key_index.insert(key, block_id);
        self.lru_order.push_back(block_id);
        self.current_size += size;

        block
    }

    /// Evict blocks until we have room for a new block.
    ///
    /// # Arguments
    /// * `needed_size` - Size in bytes needed for the new block
    fn evict_for_space(&mut self, needed_size: u64) -> Result<(), MemoryPoolError> {
        while self.current_size + needed_size > self.config.max_size {
            let evicted: bool = self.evict_one()?;
            if !evicted {
                break;
            }
        }
        Ok(())
    }

    /// Evict the least recently used block that has no active references.
    ///
    /// # Returns
    /// Ok(true) if a block was evicted, Ok(false) if no evictable blocks,
    /// Err if pool is exhausted (all blocks in use).
    fn evict_one(&mut self) -> Result<bool, MemoryPoolError> {
        let evict_id: Option<BlockId> = self
            .lru_order
            .iter()
            .find_map(|&id| self.blocks.get(&id).filter(|b| b.can_evict()).map(|_| id));

        match evict_id {
            Some(id) => {
                self.remove_block(id);
                Ok(true)
            }
            None => {
                if self.blocks.is_empty() {
                    Ok(false)
                } else {
                    let in_use: usize = self.blocks.values().filter(|b| !b.can_evict()).count();
                    Err(MemoryPoolError::PoolExhausted {
                        current_blocks: self.blocks.len(),
                        in_use_blocks: in_use,
                    })
                }
            }
        }
    }

    /// Remove a block from the pool.
    ///
    /// # Arguments
    /// * `block_id` - ID of the block to remove
    fn remove_block(&mut self, block_id: BlockId) {
        if let Some(block) = self.blocks.remove(&block_id) {
            self.key_index.remove(&block.key);
            self.lru_order.retain(|&id| id != block_id);
            self.current_size -= block.size();
        }
    }

    /// Get pool statistics.
    fn stats(&self) -> MemoryPoolStats {
        let in_use_blocks: usize = self.blocks.values().filter(|b| !b.can_evict()).count();
        MemoryPoolStats {
            total_blocks: self.blocks.len(),
            in_use_blocks,
            current_size: self.current_size,
            max_size: self.config.max_size,
            pending_fetches: self.pending_fetches.len(),
        }
    }
}


// ============================================================================
// Memory Pool Stats
// ============================================================================

/// Statistics about the memory pool state.
#[derive(Debug, Clone, Default)]
pub struct MemoryPoolStats {
    /// Total number of blocks in the pool.
    pub total_blocks: usize,
    /// Number of blocks currently in use (ref_count > 0).
    pub in_use_blocks: usize,
    /// Current total size of all blocks in bytes.
    pub current_size: u64,
    /// Maximum pool size in bytes.
    pub max_size: u64,
    /// Number of in-flight fetch operations.
    pub pending_fetches: usize,
}

impl MemoryPoolStats {
    /// Calculate pool utilization as a percentage.
    pub fn utilization(&self) -> f64 {
        if self.max_size == 0 {
            0.0
        } else {
            (self.current_size as f64 / self.max_size as f64) * 100.0
        }
    }

    /// Number of free blocks (not in use).
    pub fn free_blocks(&self) -> usize {
        self.total_blocks - self.in_use_blocks
    }
}

// ============================================================================
// Memory Pool (Public API)
// ============================================================================

/// Thread-safe memory pool for managing fixed-size blocks with LRU eviction.
///
/// Designed for V2 manifest chunk handling where each chunk is 256MB.
/// The pool maintains blocks in memory and evicts least-recently-used
/// blocks when capacity is reached.
///
/// # Thread Safety
///
/// - Block data is stored in `Arc<Vec<u8>>` for lock-free reads
/// - Reference counting uses atomics
/// - Fetch coordination prevents duplicate S3 requests
pub struct MemoryPool {
    inner: Arc<Mutex<MemoryPoolInner>>,
    allocation_count: AtomicU64,
    hit_count: AtomicU64,
}

impl MemoryPool {
    /// Create a new memory pool with the given configuration.
    ///
    /// # Arguments
    /// * `config` - Pool configuration specifying max size and block size
    pub fn new(config: MemoryPoolConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryPoolInner::new(config))),
            allocation_count: AtomicU64::new(0),
            hit_count: AtomicU64::new(0),
        }
    }

    /// Create a memory pool with default configuration (8GB max, 256MB blocks).
    pub fn with_defaults() -> Self {
        Self::new(MemoryPoolConfig::default())
    }

    /// Acquire a block for the given key, fetching content if not cached.
    ///
    /// If the block is already in the pool, returns a handle to it.
    /// If another thread is already fetching this key, waits for that fetch.
    /// Otherwise, calls the fetch function to retrieve the data.
    ///
    /// # Arguments
    /// * `key` - Content key identifying the block
    /// * `fetch` - Async function to fetch the data if not cached
    ///
    /// # Returns
    /// A handle providing direct access to the block data.
    pub async fn acquire<F, Fut>(
        &self,
        key: &BlockKey,
        fetch: F,
    ) -> Result<BlockHandle, MemoryPoolError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Vec<u8>, MemoryPoolError>> + Send + 'static,
    {
        // Check cache and pending fetches
        let pending: Option<SharedFetch> = {
            let mut inner: std::sync::MutexGuard<'_, MemoryPoolInner> = self.inner.lock().unwrap();

            // Fast path: block already cached
            if let Some(block) = inner.lookup(key) {
                block.acquire();
                self.hit_count.fetch_add(1, Ordering::Relaxed);
                return Ok(BlockHandle {
                    data: block.data.clone(),
                    block,
                });
            }

            // Check if another thread is already fetching this key
            inner.pending_fetches.get(key).cloned()
        };

        // If there's a pending fetch, wait for it
        if let Some(shared_future) = pending {
            let result: FetchResult = shared_future.await;
            return self.handle_fetch_result(key, result);
        }

        // We need to start a new fetch
        self.start_fetch(key, fetch).await
    }

    /// Start a new fetch operation with coordination.
    async fn start_fetch<F, Fut>(
        &self,
        key: &BlockKey,
        fetch: F,
    ) -> Result<BlockHandle, MemoryPoolError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Vec<u8>, MemoryPoolError>> + Send + 'static,
    {
        // Create a channel to broadcast the result
        let (tx, rx) = oneshot::channel::<FetchResult>();

        // Create a shared future that all waiters can clone and await
        let shared_future: SharedFetch = async move {
            rx.await.unwrap_or_else(|_| Err("Fetch cancelled".to_string()))
        }
        .boxed()
        .shared();

        // Register the pending fetch - check for existing fetch first
        let existing_fetch: Option<SharedFetch> = {
            let mut inner: std::sync::MutexGuard<'_, MemoryPoolInner> = self.inner.lock().unwrap();

            // Double-check: another thread may have inserted while we were setting up
            if let Some(block) = inner.lookup(key) {
                block.acquire();
                self.hit_count.fetch_add(1, Ordering::Relaxed);
                return Ok(BlockHandle {
                    data: block.data.clone(),
                    block,
                });
            }

            // Check again for pending fetch (race condition)
            if let Some(existing) = inner.pending_fetches.get(key) {
                Some(existing.clone())
            } else {
                inner.pending_fetches.insert(key.clone(), shared_future.clone());
                None
            }
        }; // Lock released here

        // If there was an existing fetch, wait for it
        if let Some(shared) = existing_fetch {
            let result: FetchResult = shared.await;
            return self.handle_fetch_result(key, result);
        }

        // Perform the fetch outside the lock
        let fetch_result: Result<Vec<u8>, MemoryPoolError> = fetch().await;

        // Process the result
        let result: FetchResult = match fetch_result {
            Ok(data) => Ok(Arc::new(data)),
            Err(e) => Err(e.to_string()),
        };

        // Send result to all waiters
        let _ = tx.send(result.clone());

        // Remove pending fetch and insert block if successful
        self.complete_fetch(key, result)
    }

    /// Complete a fetch operation and insert the block.
    fn complete_fetch(
        &self,
        key: &BlockKey,
        result: FetchResult,
    ) -> Result<BlockHandle, MemoryPoolError> {
        let mut inner: std::sync::MutexGuard<'_, MemoryPoolInner> = self.inner.lock().unwrap();

        // Remove the pending fetch
        inner.pending_fetches.remove(key);

        match result {
            Ok(data) => {
                // Check if block was inserted by another path
                if let Some(block) = inner.lookup(key) {
                    block.acquire();
                    self.hit_count.fetch_add(1, Ordering::Relaxed);
                    return Ok(BlockHandle {
                        data: block.data.clone(),
                        block,
                    });
                }

                // Evict if necessary
                let data_size: u64 = data.len() as u64;
                inner.evict_for_space(data_size)?;

                // Insert new block
                let block: Arc<PoolBlock> = inner.insert(key.clone(), data);
                block.acquire();
                self.allocation_count.fetch_add(1, Ordering::Relaxed);

                Ok(BlockHandle {
                    data: block.data.clone(),
                    block,
                })
            }
            Err(msg) => Err(MemoryPoolError::RetrievalFailed(msg)),
        }
    }

    /// Handle the result of a shared fetch (for waiters).
    fn handle_fetch_result(
        &self,
        key: &BlockKey,
        result: FetchResult,
    ) -> Result<BlockHandle, MemoryPoolError> {
        match result {
            Ok(data) => {
                // The block should now be in the cache
                let mut inner: std::sync::MutexGuard<'_, MemoryPoolInner> =
                    self.inner.lock().unwrap();
                if let Some(block) = inner.lookup(key) {
                    block.acquire();
                    self.hit_count.fetch_add(1, Ordering::Relaxed);
                    Ok(BlockHandle {
                        data: block.data.clone(),
                        block,
                    })
                } else {
                    // Block was evicted before we could get it - create handle from shared data
                    // This is a rare edge case
                    let block: Arc<PoolBlock> = Arc::new(PoolBlock {
                        key: key.clone(),
                        data,
                        ref_count: AtomicUsize::new(1),
                    });
                    Ok(BlockHandle {
                        data: block.data.clone(),
                        block,
                    })
                }
            }
            Err(msg) => Err(MemoryPoolError::RetrievalFailed(msg)),
        }
    }

    /// Try to get a block if it's already cached (no fetch).
    ///
    /// # Arguments
    /// * `key` - Content key to look up
    ///
    /// # Returns
    /// Some(handle) if cached, None if not in pool.
    pub fn try_get(&self, key: &BlockKey) -> Option<BlockHandle> {
        let mut inner: std::sync::MutexGuard<'_, MemoryPoolInner> = self.inner.lock().unwrap();
        let block: Arc<PoolBlock> = inner.lookup(key)?;
        block.acquire();
        self.hit_count.fetch_add(1, Ordering::Relaxed);
        Some(BlockHandle {
            data: block.data.clone(),
            block,
        })
    }

    /// Get current pool statistics.
    pub fn stats(&self) -> MemoryPoolStats {
        let inner: std::sync::MutexGuard<'_, MemoryPoolInner> = self.inner.lock().unwrap();
        inner.stats()
    }

    /// Get the total number of allocations since pool creation.
    pub fn allocation_count(&self) -> u64 {
        self.allocation_count.load(Ordering::Relaxed)
    }

    /// Get the total number of cache hits since pool creation.
    pub fn hit_count(&self) -> u64 {
        self.hit_count.load(Ordering::Relaxed)
    }

    /// Calculate the cache hit rate as a percentage.
    pub fn hit_rate(&self) -> f64 {
        let hits: u64 = self.hit_count.load(Ordering::Relaxed);
        let allocs: u64 = self.allocation_count.load(Ordering::Relaxed);
        let total: u64 = hits + allocs;
        if total == 0 {
            0.0
        } else {
            (hits as f64 / total as f64) * 100.0
        }
    }

    /// Clear all blocks from the pool.
    ///
    /// # Returns
    /// Err if any blocks are still in use.
    pub fn clear(&self) -> Result<(), MemoryPoolError> {
        let mut inner: std::sync::MutexGuard<'_, MemoryPoolInner> = self.inner.lock().unwrap();
        let in_use: usize = inner.blocks.values().filter(|b| !b.can_evict()).count();
        if in_use > 0 {
            return Err(MemoryPoolError::PoolExhausted {
                current_blocks: inner.blocks.len(),
                in_use_blocks: in_use,
            });
        }
        inner.blocks.clear();
        inner.key_index.clear();
        inner.lru_order.clear();
        inner.current_size = 0;
        Ok(())
    }
}

impl Default for MemoryPool {
    fn default() -> Self {
        Self::with_defaults()
    }
}


// ============================================================================
// Content Provider Trait
// ============================================================================

/// Trait for types that can provide block content.
///
/// Implement this trait to integrate with different storage backends
/// (S3, local disk, etc.) for fetching chunk data.
#[async_trait::async_trait]
pub trait BlockContentProvider: Send + Sync {
    /// Fetch the content for a block.
    ///
    /// # Arguments
    /// * `key` - The block key identifying the content to fetch
    ///
    /// # Returns
    /// The raw bytes of the block content.
    async fn fetch(&self, key: &BlockKey) -> Result<Vec<u8>, MemoryPoolError>;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn small_config() -> MemoryPoolConfig {
        MemoryPoolConfig {
            max_size: 1024 * 1024,
            block_size: 256 * 1024,
        }
    }

    #[tokio::test]
    async fn test_acquire_and_release() {
        let pool: MemoryPool = MemoryPool::new(small_config());
        let key: BlockKey = BlockKey::new("hash123", 0);
        let data: Vec<u8> = vec![1, 2, 3, 4];

        let handle: BlockHandle = pool
            .acquire(&key, move || async move { Ok(data.clone()) })
            .await
            .unwrap();

        assert_eq!(handle.data(), &[1, 2, 3, 4]);

        let stats: MemoryPoolStats = pool.stats();
        assert_eq!(stats.total_blocks, 1);
        assert_eq!(stats.in_use_blocks, 1);

        drop(handle);

        let stats: MemoryPoolStats = pool.stats();
        assert_eq!(stats.in_use_blocks, 0);
    }

    #[tokio::test]
    async fn test_cache_hit() {
        let pool: MemoryPool = MemoryPool::new(small_config());
        let key: BlockKey = BlockKey::new("hash123", 0);

        let _h1: BlockHandle = pool
            .acquire(&key, || async move { Ok(vec![1, 2, 3]) })
            .await
            .unwrap();

        assert_eq!(pool.allocation_count(), 1);
        assert_eq!(pool.hit_count(), 0);

        let _h2: BlockHandle = pool
            .acquire(&key, || async move { panic!("should not fetch") })
            .await
            .unwrap();

        assert_eq!(pool.allocation_count(), 1);
        assert_eq!(pool.hit_count(), 1);
    }

    #[tokio::test]
    async fn test_lru_eviction() {
        let config: MemoryPoolConfig = MemoryPoolConfig {
            max_size: 300,
            block_size: 100,
        };
        let pool: MemoryPool = MemoryPool::new(config);

        let k1: BlockKey = BlockKey::new("h1", 0);
        let k2: BlockKey = BlockKey::new("h2", 0);
        let k3: BlockKey = BlockKey::new("h3", 0);

        let h1: BlockHandle = pool.acquire(&k1, || async move { Ok(vec![0; 100]) }).await.unwrap();
        drop(h1);

        let h2: BlockHandle = pool.acquire(&k2, || async move { Ok(vec![0; 100]) }).await.unwrap();
        drop(h2);

        let h3: BlockHandle = pool.acquire(&k3, || async move { Ok(vec![0; 100]) }).await.unwrap();
        drop(h3);

        assert_eq!(pool.stats().total_blocks, 3);

        let k4: BlockKey = BlockKey::new("h4", 0);
        let _h4: BlockHandle = pool.acquire(&k4, || async move { Ok(vec![0; 100]) }).await.unwrap();

        assert_eq!(pool.stats().total_blocks, 3);
        assert!(pool.try_get(&k1).is_none());
        assert!(pool.try_get(&k2).is_some());
    }

    #[tokio::test]
    async fn test_pool_exhausted() {
        let config: MemoryPoolConfig = MemoryPoolConfig {
            max_size: 200,
            block_size: 100,
        };
        let pool: MemoryPool = MemoryPool::new(config);

        let k1: BlockKey = BlockKey::new("h1", 0);
        let k2: BlockKey = BlockKey::new("h2", 0);
        let k3: BlockKey = BlockKey::new("h3", 0);

        let _h1: BlockHandle = pool.acquire(&k1, || async move { Ok(vec![0; 100]) }).await.unwrap();
        let _h2: BlockHandle = pool.acquire(&k2, || async move { Ok(vec![0; 100]) }).await.unwrap();

        let result: Result<BlockHandle, MemoryPoolError> =
            pool.acquire(&k3, || async move { Ok(vec![0; 100]) }).await;
        assert!(matches!(result, Err(MemoryPoolError::PoolExhausted { .. })));
    }

    #[tokio::test]
    async fn test_fetch_coordination() {
        // Test that concurrent requests for the same key only fetch once
        let pool: Arc<MemoryPool> = Arc::new(MemoryPool::new(small_config()));
        let key: BlockKey = BlockKey::new("shared_key", 0);
        let fetch_count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));

        let mut handles: Vec<tokio::task::JoinHandle<BlockHandle>> = Vec::new();

        for _ in 0..5 {
            let pool_clone: Arc<MemoryPool> = pool.clone();
            let key_clone: BlockKey = key.clone();
            let fetch_count_clone: Arc<AtomicUsize> = fetch_count.clone();

            let handle: tokio::task::JoinHandle<BlockHandle> = tokio::spawn(async move {
                pool_clone
                    .acquire(&key_clone, move || {
                        let fc: Arc<AtomicUsize> = fetch_count_clone.clone();
                        async move {
                            fc.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            Ok(vec![42; 100])
                        }
                    })
                    .await
                    .unwrap()
            });
            handles.push(handle);
        }

        let results: Vec<BlockHandle> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // All handles should have the same data
        for h in &results {
            assert_eq!(h.data(), &vec![42; 100]);
        }

        // Fetch should have been called only once (or at most twice due to race)
        let count: usize = fetch_count.load(Ordering::SeqCst);
        assert!(count <= 2, "Expected at most 2 fetches, got {}", count);
    }

    #[tokio::test]
    async fn test_lock_free_reads() {
        // Test that multiple readers can access data simultaneously
        let pool: Arc<MemoryPool> = Arc::new(MemoryPool::new(small_config()));
        let key: BlockKey = BlockKey::new("read_test", 0);

        // Insert a block
        let _: BlockHandle = pool
            .acquire(&key, || async { Ok(vec![1, 2, 3, 4, 5]) })
            .await
            .unwrap();

        // Spawn multiple readers
        let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        for _ in 0..10 {
            let pool_clone: Arc<MemoryPool> = pool.clone();
            let key_clone: BlockKey = key.clone();

            let handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
                let h: BlockHandle = pool_clone.try_get(&key_clone).unwrap();
                // Simulate reading data
                assert_eq!(h.data(), &[1, 2, 3, 4, 5]);
                tokio::time::sleep(Duration::from_millis(10)).await;
                assert_eq!(h.data(), &[1, 2, 3, 4, 5]);
            });
            handles.push(handle);
        }

        futures::future::join_all(handles).await;
    }

    #[test]
    fn test_block_key_display() {
        let key: BlockKey = BlockKey::new("abc123", 5);
        assert_eq!(format!("{}", key), "abc123:5");
    }

    #[test]
    fn test_stats_utilization() {
        let stats: MemoryPoolStats = MemoryPoolStats {
            total_blocks: 4,
            in_use_blocks: 2,
            current_size: 512,
            max_size: 1024,
            pending_fetches: 0,
        };
        assert_eq!(stats.utilization(), 50.0);
        assert_eq!(stats.free_blocks(), 2);
    }
}
