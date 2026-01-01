//! Memory pool for managing fixed-size blocks with LRU eviction.
//!
//! This module provides a memory pool optimized for V2 manifest chunk handling.
//! Blocks are 256MB (matching `CHUNK_SIZE_V2`) and are managed with LRU eviction
//! when the pool reaches its configured maximum size.
//!
//! # Architecture (v2 - DashMap based)
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      MemoryPool                              │
//! │  ┌─────────────────────────────────────────────────────┐    │
//! │  │  blocks: DashMap<PoolBlockId, Arc<PoolBlock>>       │    │
//! │  │  key_index: DashMap<BlockKey, PoolBlockId>          │    │
//! │  │  pending_fetches: DashMap<BlockKey, SharedFetch>    │    │
//! │  │  lru_state: Mutex<LruState>  (cold path only)       │    │
//! │  └─────────────────────────────────────────────────────┘    │
//! └─────────────────────────────────────────────────────────────┘
//!
//! PoolBlock:
//!   - data: Arc<Vec<u8>> or Arc<RwLock<Vec<u8>>>
//!   - key: BlockKey
//!   - ref_count: AtomicUsize
//!   - needs_flush: AtomicBool (for dirty blocks)
//! ```
//!
//! # Thread Safety (v2)
//!
//! - Hot path uses DashMap for lock-free concurrent access
//! - Block data stored in `Arc<Vec<u8>>` for lock-free reads
//! - Mutable blocks use `parking_lot::RwLock` for efficient writes
//! - LRU state protected by `parking_lot::Mutex` (cold path only)

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use futures::future::{BoxFuture, Shared};
use futures::FutureExt;
use parking_lot::{Mutex, RwLock};
use tokio::sync::oneshot;

use rusty_attachments_common::CHUNK_SIZE_V2;

/// Unique identifier for a block in the pool's internal storage.
pub type PoolBlockId = u64;

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
    BlockNotFound(PoolBlockId),

    /// Block is not mutable (tried to modify an immutable block).
    BlockNotMutable(PoolBlockId),

    /// Block is not dirty (tried to modify a clean block).
    NotDirty,

    /// Pool is at capacity and all blocks are in use (cannot evict).
    PoolExhausted {
        current_blocks: usize,
        in_use_blocks: usize,
    },

    /// Content retrieval failed.
    RetrievalFailed(String),

    /// Flush operation failed during eviction.
    FlushFailed(String),
}

impl fmt::Display for MemoryPoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryPoolError::BlockNotFound(id) => write!(f, "Block not found: {}", id),
            MemoryPoolError::BlockNotMutable(id) => {
                write!(f, "Block {} is not mutable", id)
            }
            MemoryPoolError::NotDirty => write!(f, "Block is not dirty"),
            MemoryPoolError::PoolExhausted {
                current_blocks,
                in_use_blocks,
            } => write!(
                f,
                "Pool exhausted: {} blocks allocated, {} in use",
                current_blocks, in_use_blocks
            ),
            MemoryPoolError::RetrievalFailed(msg) => write!(f, "Content retrieval failed: {}", msg),
            MemoryPoolError::FlushFailed(msg) => write!(f, "Flush failed during eviction: {}", msg),
        }
    }
}

impl std::error::Error for MemoryPoolError {}

// ============================================================================
// Hash Cache for Reverse Lookups
// ============================================================================

/// Cache for reverse hash lookups (folded u64 -> original hex string).
#[derive(Debug, Default)]
pub struct HashCache {
    folded_to_hex: RwLock<std::collections::HashMap<u64, String>>,
}

impl HashCache {
    /// Create a new hash cache.
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Insert a hash mapping.
    pub fn insert(&self, hash_hex: String, folded: u64) {
        let mut cache = self.folded_to_hex.write();
        cache.insert(folded, hash_hex);
    }
    
    /// Get original hash string from folded value.
    pub fn get(&self, folded: u64) -> Option<String> {
        let cache = self.folded_to_hex.read();
        cache.get(&folded).cloned()
    }
    
    /// Remove a hash mapping.
    pub fn remove(&self, folded: u64) -> Option<String> {
        let mut cache = self.folded_to_hex.write();
        cache.remove(&folded)
    }
    
    /// Clear all cached mappings.
    pub fn clear(&self) {
        let mut cache = self.folded_to_hex.write();
        cache.clear();
    }
    
    /// Get the number of cached mappings.
    pub fn len(&self) -> usize {
        let cache = self.folded_to_hex.read();
        cache.len()
    }
    
    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        let cache = self.folded_to_hex.read();
        cache.is_empty()
    }
}

// ============================================================================
// Content ID
// ============================================================================

/// Identifier for block content - either hash-based or inode-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentId {
    /// Folded hash for read-only content (64-bit for efficiency).
    Hash(u64),
    /// Inode-based ID for dirty content (must be flushed before eviction).
    Inode(u64),
}

impl ContentId {
    /// Create a hash-based content ID from hex string.
    pub fn from_hash_hex(hash_hex: &str) -> Self {
        ContentId::Hash(rusty_attachments_common::hash::fold_hash_to_u64(hash_hex))
    }
    
    /// Create a hash-based content ID from raw bytes.
    pub fn from_bytes(data: &[u8]) -> Self {
        ContentId::Hash(rusty_attachments_common::hash::hash_bytes_folded(data))
    }
    
    /// Check if this is a hash-based (read-only) content ID.
    pub fn is_hash(&self) -> bool {
        matches!(self, ContentId::Hash(_))
    }

    /// Check if this is an inode-based (dirty) content ID.
    pub fn is_inode(&self) -> bool {
        matches!(self, ContentId::Inode(_))
    }

    /// Get the folded hash if this is a hash-based ID.
    pub fn as_hash_folded(&self) -> Option<u64> {
        match self {
            ContentId::Hash(h) => Some(*h),
            ContentId::Inode(_) => None,
        }
    }

    /// Get the inode ID if this is an inode-based ID.
    pub fn as_inode(&self) -> Option<u64> {
        match self {
            ContentId::Hash(_) => None,
            ContentId::Inode(id) => Some(*id),
        }
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentId::Hash(h) => write!(f, "hash_folded:{:016x}", h),
            ContentId::Inode(id) => write!(f, "inode:{}", id),
        }
    }
}

// ============================================================================
// Block Key
// ============================================================================

/// Key identifying a unique block of content.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockKey {
    /// Content identifier (hash or inode ID).
    pub id: ContentId,
    /// Chunk index within the file (0 for non-chunked files).
    pub chunk_index: u32,
}

impl BlockKey {
    /// Create a new block key from a content hash hex string.
    pub fn from_hash_hex(hash_hex: &str, chunk_index: u32) -> Self {
        Self {
            id: ContentId::from_hash_hex(hash_hex),
            chunk_index,
        }
    }
    
    /// Create a new block key from raw bytes.
    pub fn from_bytes(data: &[u8], chunk_index: u32) -> Self {
        Self {
            id: ContentId::from_bytes(data),
            chunk_index,
        }
    }

    /// Create a new block key from an inode ID (for dirty blocks).
    pub fn from_inode(inode_id: u64, chunk_index: u32) -> Self {
        Self {
            id: ContentId::Inode(inode_id),
            chunk_index,
        }
    }

    /// Check if this is a read-only (hash-based) block key.
    pub fn is_read_only(&self) -> bool {
        self.id.is_hash()
    }

    /// Check if this is a dirty (inode-based) block key.
    pub fn is_dirty(&self) -> bool {
        self.id.is_inode()
    }

    /// Get the folded hash if this is a hash-based key.
    pub fn hash_folded(&self) -> Option<u64> {
        self.id.as_hash_folded()
    }

    /// Get the inode ID if this is an inode-based key.
    pub fn inode_id(&self) -> Option<u64> {
        self.id.as_inode()
    }
}

impl fmt::Display for BlockKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.id, self.chunk_index)
    }
}

// ============================================================================
// Block Data Storage
// ============================================================================

/// Block data storage - either immutable (read-only) or mutable (dirty).
enum BlockData {
    /// Immutable data for read-only blocks (fetched from S3).
    Immutable(Arc<Vec<u8>>),
    /// Mutable data for dirty blocks (modified files).
    Mutable(Arc<RwLock<Vec<u8>>>),
}

impl BlockData {
    /// Get the size of the data in bytes.
    fn len(&self) -> usize {
        match self {
            BlockData::Immutable(data) => data.len(),
            BlockData::Mutable(data) => data.read().len(),
        }
    }

    /// Get the immutable Arc reference if this is an immutable block.
    fn as_immutable(&self) -> Option<Arc<Vec<u8>>> {
        match self {
            BlockData::Immutable(data) => Some(data.clone()),
            BlockData::Mutable(_) => None,
        }
    }

    /// Get the mutable RwLock reference if this is a mutable block.
    fn as_mutable(&self) -> Option<Arc<RwLock<Vec<u8>>>> {
        match self {
            BlockData::Immutable(_) => None,
            BlockData::Mutable(data) => Some(data.clone()),
        }
    }
}

// ============================================================================
// Pool Block
// ============================================================================

/// A single block of memory in the pool.
pub(crate) struct PoolBlock {
    /// Key identifying the content stored in this block.
    pub(crate) key: BlockKey,
    /// The actual data buffer.
    data: BlockData,
    /// Number of active references to this block.
    ref_count: AtomicUsize,
    /// True if block has unflushed modifications.
    needs_flush: AtomicBool,
}

impl PoolBlock {
    /// Create a new pool block with immutable data.
    fn new(key: BlockKey, data: Arc<Vec<u8>>, needs_flush: bool) -> Self {
        Self {
            key,
            data: BlockData::Immutable(data),
            ref_count: AtomicUsize::new(0),
            needs_flush: AtomicBool::new(needs_flush),
        }
    }

    /// Create a new pool block with mutable data.
    fn new_mutable(key: BlockKey, mut data: Vec<u8>, capacity: Option<usize>) -> Self {
        if let Some(cap) = capacity {
            if data.capacity() < cap {
                data.reserve(cap - data.len());
            }
        }
        Self {
            key,
            data: BlockData::Mutable(Arc::new(RwLock::new(data))),
            ref_count: AtomicUsize::new(0),
            needs_flush: AtomicBool::new(true),
        }
    }

    /// Check if this block can be evicted without flushing.
    fn can_evict_without_flush(&self) -> bool {
        self.ref_count.load(Ordering::Acquire) == 0
            && !self.needs_flush.load(Ordering::Acquire)
    }

    /// Check if this block can be evicted (possibly after flushing).
    fn can_evict(&self) -> bool {
        self.ref_count.load(Ordering::Acquire) == 0
    }

    /// Check if this block needs to be flushed before eviction.
    fn needs_flush(&self) -> bool {
        self.needs_flush.load(Ordering::Acquire)
    }

    /// Mark this block as flushed.
    fn mark_flushed(&self) {
        self.needs_flush.store(false, Ordering::Release);
    }

    /// Mark this block as needing flush.
    fn mark_needs_flush(&self) {
        self.needs_flush.store(true, Ordering::Release);
    }

    /// Get the size of this block in bytes.
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    /// Check if this block is mutable.
    fn is_mutable(&self) -> bool {
        matches!(self.data, BlockData::Mutable(_))
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
            .field("mutable", &self.is_mutable())
            .field("ref_count", &self.ref_count.load(Ordering::Relaxed))
            .field("needs_flush", &self.needs_flush.load(Ordering::Relaxed))
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
// Block Handles
// ============================================================================

/// RAII handle to an immutable block in the pool.
pub struct BlockHandle {
    /// Direct reference to block data.
    data: Arc<Vec<u8>>,
    /// Reference to the block for ref_count management.
    block: Arc<PoolBlock>,
}

impl BlockHandle {
    /// Get a reference to the block's data.
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

/// RAII handle to a mutable (dirty) block in the pool.
pub struct MutableBlockHandle {
    /// Reference to mutable block data.
    data: Arc<RwLock<Vec<u8>>>,
    /// Reference to the block for ref_count management.
    block: Arc<PoolBlock>,
}

impl MutableBlockHandle {
    /// Get a copy of the block's data.
    pub fn data(&self) -> Vec<u8> {
        self.data.read().clone()
    }

    /// Access the data with a read lock.
    pub fn with_data<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        let guard = self.data.read();
        f(&guard)
    }

    /// Get the size of the block data in bytes.
    pub fn len(&self) -> usize {
        self.data.read().len()
    }

    /// Check if the block is empty.
    pub fn is_empty(&self) -> bool {
        self.data.read().is_empty()
    }
}

impl Drop for MutableBlockHandle {
    fn drop(&mut self) {
        self.block.release();
    }
}

// ============================================================================
// Pending Fetch Types
// ============================================================================

/// Result of a pending fetch operation.
type FetchResult = Result<Arc<Vec<u8>>, String>;

/// Shared future for coordinating concurrent fetches of the same key.
type SharedFetch = Shared<BoxFuture<'static, FetchResult>>;

// ============================================================================
// LRU State (Cold Path)
// ============================================================================

/// Cold path state protected by mutex (eviction only).
struct LruState {
    /// LRU order: front = oldest (evict first), back = newest.
    lru_order: VecDeque<PoolBlockId>,
    /// Current total size of all blocks in bytes.
    current_size: u64,
    /// Next block ID to allocate.
    next_id: PoolBlockId,
    /// Pool configuration.
    config: MemoryPoolConfig,
}

impl LruState {
    /// Create a new LRU state with the given configuration.
    fn new(config: MemoryPoolConfig) -> Self {
        Self {
            lru_order: VecDeque::new(),
            current_size: 0,
            next_id: 1,
            config,
        }
    }

    /// Allocate a new block ID and update tracking.
    fn allocate_block(&mut self, size: u64) -> PoolBlockId {
        let id: PoolBlockId = self.next_id;
        self.next_id += 1;
        self.current_size += size;
        self.lru_order.push_back(id);
        id
    }

    /// Move a block to the back of the LRU queue (most recently used).
    fn touch(&mut self, block_id: PoolBlockId) {
        self.lru_order.retain(|&id| id != block_id);
        self.lru_order.push_back(block_id);
    }

    /// Remove a block from LRU tracking.
    fn remove(&mut self, block_id: PoolBlockId, size: u64) {
        self.lru_order.retain(|&id| id != block_id);
        self.current_size = self.current_size.saturating_sub(size);
    }

    /// Check if eviction is needed for the given size.
    fn needs_eviction(&self, needed_size: u64) -> bool {
        self.current_size + needed_size > self.config.max_size
    }

    /// Update size tracking after a block modification.
    fn update_size(&mut self, old_size: u64, new_size: u64) {
        if new_size > old_size {
            self.current_size += new_size - old_size;
        } else if old_size > new_size {
            self.current_size = self.current_size.saturating_sub(old_size - new_size);
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
    /// Number of blocks that need flushing before eviction.
    pub dirty_blocks: usize,
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

    /// Number of clean blocks (don't need flush).
    pub fn clean_blocks(&self) -> usize {
        self.total_blocks - self.dirty_blocks
    }
}


// ============================================================================
// Memory Pool (Public API) - DashMap based
// ============================================================================

/// Thread-safe memory pool for managing fixed-size blocks with LRU eviction.
///
/// This v2 implementation uses DashMap for lock-free concurrent access on hot paths.
/// LRU eviction state is protected by a separate mutex (cold path only).
pub struct MemoryPool {
    /// Block storage - concurrent read/write via DashMap.
    blocks: DashMap<PoolBlockId, Arc<PoolBlock>>,
    /// Key to block ID mapping - concurrent read/write.
    key_index: DashMap<BlockKey, PoolBlockId>,
    /// In-flight fetches to prevent duplicate requests.
    pending_fetches: DashMap<BlockKey, SharedFetch>,
    /// Cold path state - exclusive access for eviction.
    lru_state: Mutex<LruState>,
    /// Total allocations counter.
    allocation_count: AtomicU64,
    /// Cache hit counter.
    hit_count: AtomicU64,
}

impl MemoryPool {
    /// Create a new memory pool with the given configuration.
    ///
    /// # Arguments
    /// * `config` - Pool configuration specifying max size and block size
    pub fn new(config: MemoryPoolConfig) -> Self {
        Self {
            blocks: DashMap::new(),
            key_index: DashMap::new(),
            pending_fetches: DashMap::new(),
            lru_state: Mutex::new(LruState::new(config)),
            allocation_count: AtomicU64::new(0),
            hit_count: AtomicU64::new(0),
        }
    }

    /// Create a memory pool with default configuration (8GB max, 256MB blocks).
    pub fn with_defaults() -> Self {
        Self::new(MemoryPoolConfig::default())
    }

    // ========================================================================
    // Hot Path Methods - Lock-free via DashMap
    // ========================================================================

    /// Check if a dirty block exists (lock-free).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `chunk_index` - Chunk index
    ///
    /// # Returns
    /// true if block exists, false otherwise.
    pub fn has_dirty(&self, inode_id: u64, chunk_index: u32) -> bool {
        let key = BlockKey::from_inode(inode_id, chunk_index);
        self.key_index.contains_key(&key)
    }

    /// Modify a dirty block in place using a closure (optimized - single lock acquisition).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the dirty file
    /// * `chunk_index` - Chunk index within the file
    /// * `modifier` - Closure that modifies the data in place
    ///
    /// # Returns
    /// Ok(new_size) on success, Err if block not found or not mutable.
    pub fn modify_dirty_in_place<F>(
        &self,
        inode_id: u64,
        chunk_index: u32,
        modifier: F,
    ) -> Result<usize, MemoryPoolError>
    where
        F: FnOnce(&mut Vec<u8>),
    {
        let key = BlockKey::from_inode(inode_id, chunk_index);
        
        // Lock-free lookup via DashMap
        let block_id: PoolBlockId = *self.key_index
            .get(&key)
            .ok_or(MemoryPoolError::BlockNotFound(0))?;
        
        let block: Arc<PoolBlock> = self.blocks
            .get(&block_id)
            .ok_or(MemoryPoolError::BlockNotFound(block_id))?
            .clone();
        
        // Modify data using parking_lot RwLock (no futex for uncontended case)
        let (old_size, new_size): (u64, u64) = match &block.data {
            BlockData::Mutable(data) => {
                let mut guard = data.write();
                let old_size: u64 = guard.len() as u64;
                modifier(&mut guard);
                let new_size: u64 = guard.len() as u64;
                (old_size, new_size)
            }
            BlockData::Immutable(_) => {
                return Err(MemoryPoolError::BlockNotMutable(block_id));
            }
        };
        
        // Update size tracking (cold path - only if size changed)
        if new_size != old_size {
            let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
            lru.update_size(old_size, new_size);
        }
        
        // Mark needs flush (atomic operation)
        block.mark_needs_flush();
        
        Ok(new_size as usize)
    }

    /// Modify a dirty block in place using a slice (zero-copy).
    ///
    /// This is an optimized version that takes a slice directly instead of
    /// a closure, avoiding the need to clone data into a Vec.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the dirty file
    /// * `chunk_index` - Chunk index within the file
    /// * `offset` - Byte offset within the chunk to start writing
    /// * `data` - Data slice to write
    ///
    /// # Returns
    /// Ok(()) on success, Err if block not found or not mutable.
    pub fn modify_dirty_in_place_with_slice(
        &self,
        inode_id: u64,
        chunk_index: u32,
        offset: usize,
        data: &[u8],
    ) -> Result<(), MemoryPoolError> {
        let key = BlockKey::from_inode(inode_id, chunk_index);

        // Lock-free lookup via DashMap
        let block_id: PoolBlockId = *self
            .key_index
            .get(&key)
            .ok_or(MemoryPoolError::BlockNotFound(0))?;

        let block: Arc<PoolBlock> = self
            .blocks
            .get(&block_id)
            .ok_or(MemoryPoolError::BlockNotFound(block_id))?
            .clone();

        // Modify data using parking_lot RwLock
        let (old_size, new_size): (u64, u64) = match &block.data {
            BlockData::Mutable(block_data) => {
                let mut guard = block_data.write();
                let old_size: u64 = guard.len() as u64;
                let end: usize = offset + data.len();

                // Extend if needed
                if end > guard.len() {
                    guard.resize(end, 0);
                }

                // Direct copy from slice - no intermediate Vec
                guard[offset..end].copy_from_slice(data);
                let new_size: u64 = guard.len() as u64;
                (old_size, new_size)
            }
            BlockData::Immutable(_) => {
                return Err(MemoryPoolError::BlockNotMutable(block_id));
            }
        };

        // Update size tracking (cold path - only if size changed)
        if new_size != old_size {
            let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
            lru.update_size(old_size, new_size);
        }

        // Mark needs flush (atomic operation)
        block.mark_needs_flush();

        Ok(())
    }

    /// Get a dirty block if it exists (lock-free lookup).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `chunk_index` - Chunk index
    ///
    /// # Returns
    /// Handle to the mutable block if found, None otherwise.
    pub fn get_dirty(&self, inode_id: u64, chunk_index: u32) -> Option<MutableBlockHandle> {
        let key = BlockKey::from_inode(inode_id, chunk_index);
        
        // Lock-free lookup
        let block_id: PoolBlockId = *self.key_index.get(&key)?;
        let block: Arc<PoolBlock> = self.blocks.get(&block_id)?.clone();
        
        block.acquire();
        self.hit_count.fetch_add(1, Ordering::Relaxed);
        
        // Touch LRU (cold path)
        {
            let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
            lru.touch(block_id);
        }
        
        // Extract mutable data reference
        let data_ref: Arc<RwLock<Vec<u8>>> = match &block.data {
            BlockData::Mutable(d) => d.clone(),
            BlockData::Immutable(_) => {
                block.release();
                return None;
            }
        };
        
        Some(MutableBlockHandle {
            data: data_ref,
            block,
        })
    }

    /// Mark a dirty block as flushed (lock-free).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `chunk_index` - Chunk index
    ///
    /// # Returns
    /// true if block was found and marked, false if not found.
    pub fn mark_flushed(&self, inode_id: u64, chunk_index: u32) -> bool {
        let key = BlockKey::from_inode(inode_id, chunk_index);
        
        if let Some(block_id) = self.key_index.get(&key) {
            if let Some(block) = self.blocks.get(&*block_id) {
                block.mark_flushed();
                return true;
            }
        }
        false
    }

    /// Mark a dirty block as needing flush (lock-free).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `chunk_index` - Chunk index
    ///
    /// # Returns
    /// true if block was found and marked, false if not found.
    pub fn mark_needs_flush(&self, inode_id: u64, chunk_index: u32) -> bool {
        let key = BlockKey::from_inode(inode_id, chunk_index);
        
        if let Some(block_id) = self.key_index.get(&key) {
            if let Some(block) = self.blocks.get(&*block_id) {
                block.mark_needs_flush();
                return true;
            }
        }
        false
    }

    // ========================================================================
    // Insert/Update Methods
    // ========================================================================

    /// Insert or update a dirty block with mutable storage.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the dirty file
    /// * `chunk_index` - Chunk index within the file
    /// * `data` - The dirty chunk data
    ///
    /// # Returns
    /// Handle to the dirty block.
    pub fn insert_dirty(
        &self,
        inode_id: u64,
        chunk_index: u32,
        data: Vec<u8>,
    ) -> Result<MutableBlockHandle, MemoryPoolError> {
        let capacity: usize = CHUNK_SIZE_V2 as usize;
        self.insert_dirty_with_capacity(inode_id, chunk_index, data, Some(capacity))
    }

    /// Insert a dirty block with custom capacity.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the dirty file
    /// * `chunk_index` - Chunk index within the file
    /// * `data` - The dirty chunk data
    /// * `capacity` - Optional pre-allocation capacity
    ///
    /// # Returns
    /// Handle to the dirty block.
    pub fn insert_dirty_with_capacity(
        &self,
        inode_id: u64,
        chunk_index: u32,
        data: Vec<u8>,
        capacity: Option<usize>,
    ) -> Result<MutableBlockHandle, MemoryPoolError> {
        let key = BlockKey::from_inode(inode_id, chunk_index);
        let data_size: u64 = data.len() as u64;
        
        // Remove existing block if present
        if let Some((_, old_block_id)) = self.key_index.remove(&key) {
            if let Some((_, old_block)) = self.blocks.remove(&old_block_id) {
                let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
                lru.remove(old_block_id, old_block.size());
            }
        }
        
        // Evict if necessary
        self.evict_for_space(data_size)?;
        
        // Allocate block ID and update LRU
        let block_id: PoolBlockId = {
            let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
            lru.allocate_block(data_size)
        };
        
        // Create and insert block
        let block = Arc::new(PoolBlock::new_mutable(key.clone(), data, capacity));
        block.acquire();
        
        self.blocks.insert(block_id, block.clone());
        self.key_index.insert(key, block_id);
        self.allocation_count.fetch_add(1, Ordering::Relaxed);
        
        // Extract mutable data reference
        let data_ref: Arc<RwLock<Vec<u8>>> = match &block.data {
            BlockData::Mutable(d) => d.clone(),
            BlockData::Immutable(_) => unreachable!("new_mutable always creates Mutable blocks"),
        };
        
        Ok(MutableBlockHandle {
            data: data_ref,
            block,
        })
    }

    /// Update an existing dirty block's data.
    pub fn update_dirty(
        &self,
        inode_id: u64,
        chunk_index: u32,
        data: Vec<u8>,
    ) -> Result<MutableBlockHandle, MemoryPoolError> {
        self.insert_dirty(inode_id, chunk_index, data)
    }


    // ========================================================================
    // Eviction Methods (Cold Path)
    // ========================================================================

    /// Evict blocks until we have room for a new block.
    fn evict_for_space(&self, needed_size: u64) -> Result<(), MemoryPoolError> {
        loop {
            let should_evict: bool = {
                let lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
                lru.needs_eviction(needed_size)
            };
            
            if !should_evict {
                break;
            }
            
            let evicted: bool = self.evict_one_clean()?;
            if !evicted {
                break;
            }
        }
        Ok(())
    }

    /// Evict the least recently used block that doesn't need flushing.
    fn evict_one_clean(&self) -> Result<bool, MemoryPoolError> {
        // Find evictable block
        let evict_id: Option<PoolBlockId> = {
            let lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
            lru.lru_order.iter().find_map(|&id| {
                self.blocks.get(&id).and_then(|entry| {
                    if entry.can_evict_without_flush() {
                        Some(id)
                    } else {
                        None
                    }
                })
            })
        };
        
        match evict_id {
            Some(id) => {
                self.remove_block(id);
                Ok(true)
            }
            None => {
                if self.blocks.is_empty() {
                    Ok(false)
                } else {
                    let in_use: usize = self.blocks.iter()
                        .filter(|entry| !entry.value().can_evict())
                        .count();
                    Err(MemoryPoolError::PoolExhausted {
                        current_blocks: self.blocks.len(),
                        in_use_blocks: in_use,
                    })
                }
            }
        }
    }

    /// Remove a block from the pool.
    fn remove_block(&self, block_id: PoolBlockId) {
        if let Some((_, block)) = self.blocks.remove(&block_id) {
            self.key_index.remove(&block.key);
            let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
            lru.remove(block_id, block.size());
        }
    }

    /// Remove all blocks for an inode (clean up on file delete).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to remove
    ///
    /// # Returns
    /// Number of blocks removed.
    pub fn remove_inode_blocks(&self, inode_id: u64) -> usize {
        let to_remove: Vec<PoolBlockId> = self.key_index
            .iter()
            .filter_map(|entry| {
                if entry.key().id.as_inode() == Some(inode_id) {
                    Some(*entry.value())
                } else {
                    None
                }
            })
            .collect();
        
        let count: usize = to_remove.len();
        for block_id in to_remove {
            self.remove_block(block_id);
        }
        count
    }

    /// Invalidate all read-only blocks for a given hash.
    ///
    /// # Arguments
    /// * `hash_hex` - Content hash hex string to invalidate
    ///
    /// # Returns
    /// Number of blocks invalidated.
    pub fn invalidate_hash(&self, hash_hex: &str) -> usize {
        let folded: u64 = rusty_attachments_common::hash::fold_hash_to_u64(hash_hex);
        let to_remove: Vec<PoolBlockId> = self.key_index
            .iter()
            .filter_map(|entry| {
                if entry.key().id.as_hash_folded() == Some(folded) {
                    let block_id: PoolBlockId = *entry.value();
                    // Only remove if not in use
                    if let Some(block) = self.blocks.get(&block_id) {
                        if block.can_evict() {
                            return Some(block_id);
                        }
                    }
                }
                None
            })
            .collect();
        
        let count: usize = to_remove.len();
        for block_id in to_remove {
            self.remove_block(block_id);
        }
        count
    }

    // ========================================================================
    // Read-Only Block Methods
    // ========================================================================

    /// Acquire a block for the given key, fetching content if not cached.
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
        // Fast path: check cache (lock-free)
        if let Some(block_id) = self.key_index.get(key) {
            if let Some(block) = self.blocks.get(&*block_id) {
                let block: Arc<PoolBlock> = block.clone();
                block.acquire();
                self.hit_count.fetch_add(1, Ordering::Relaxed);
                
                // Touch LRU
                {
                    let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
                    lru.touch(*block_id);
                }
                
                let data: Arc<Vec<u8>> = block.data.as_immutable()
                    .expect("Read-only block should be immutable");
                return Ok(BlockHandle { data, block });
            }
        }
        
        // Check for pending fetch
        if let Some(pending) = self.pending_fetches.get(key) {
            let shared: SharedFetch = pending.clone();
            drop(pending);
            let result: FetchResult = shared.await;
            return self.handle_fetch_result(key, result);
        }
        
        // Start new fetch
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
        let (tx, rx) = oneshot::channel::<FetchResult>();
        
        let shared_future: SharedFetch = async move {
            rx.await.unwrap_or_else(|_| Err("Fetch cancelled".to_string()))
        }
        .boxed()
        .shared();
        
        // Double-check and register pending fetch
        {
            // Check cache again
            if let Some(block_id) = self.key_index.get(key) {
                if let Some(block) = self.blocks.get(&*block_id) {
                    let block: Arc<PoolBlock> = block.clone();
                    block.acquire();
                    self.hit_count.fetch_add(1, Ordering::Relaxed);
                    let data: Arc<Vec<u8>> = block.data.as_immutable()
                        .expect("Read-only block should be immutable");
                    return Ok(BlockHandle { data, block });
                }
            }
            
            // Check for existing pending fetch
            if let Some(existing) = self.pending_fetches.get(key) {
                let shared: SharedFetch = existing.clone();
                drop(existing);
                let result: FetchResult = shared.await;
                return self.handle_fetch_result(key, result);
            }
            
            // Register our fetch
            self.pending_fetches.insert(key.clone(), shared_future.clone());
        }
        
        // Perform fetch outside any lock
        let fetch_result: Result<Vec<u8>, MemoryPoolError> = fetch().await;
        
        let result: FetchResult = match fetch_result {
            Ok(data) => Ok(Arc::new(data)),
            Err(e) => Err(e.to_string()),
        };
        
        // Send result to waiters
        let _ = tx.send(result.clone());
        
        // Complete fetch
        self.complete_fetch(key, result)
    }

    /// Complete a fetch operation and insert the block.
    fn complete_fetch(
        &self,
        key: &BlockKey,
        result: FetchResult,
    ) -> Result<BlockHandle, MemoryPoolError> {
        // Remove pending fetch
        self.pending_fetches.remove(key);
        
        match result {
            Ok(data) => {
                // Check if block was inserted by another path
                if let Some(block_id) = self.key_index.get(key) {
                    if let Some(block) = self.blocks.get(&*block_id) {
                        let block: Arc<PoolBlock> = block.clone();
                        block.acquire();
                        self.hit_count.fetch_add(1, Ordering::Relaxed);
                        let data_ref: Arc<Vec<u8>> = block.data.as_immutable()
                            .expect("Read-only block should be immutable");
                        return Ok(BlockHandle { data: data_ref, block });
                    }
                }
                
                // Evict if necessary
                let data_size: u64 = data.len() as u64;
                self.evict_for_space(data_size)?;
                
                // Allocate and insert
                let block_id: PoolBlockId = {
                    let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
                    lru.allocate_block(data_size)
                };
                
                let block = Arc::new(PoolBlock::new(key.clone(), data.clone(), false));
                block.acquire();
                
                self.blocks.insert(block_id, block.clone());
                self.key_index.insert(key.clone(), block_id);
                self.allocation_count.fetch_add(1, Ordering::Relaxed);
                
                Ok(BlockHandle { data, block })
            }
            Err(msg) => Err(MemoryPoolError::RetrievalFailed(msg)),
        }
    }

    /// Handle the result of a shared fetch.
    fn handle_fetch_result(
        &self,
        key: &BlockKey,
        result: FetchResult,
    ) -> Result<BlockHandle, MemoryPoolError> {
        match result {
            Ok(data) => {
                // Block should now be in cache
                if let Some(block_id) = self.key_index.get(key) {
                    if let Some(block) = self.blocks.get(&*block_id) {
                        let block: Arc<PoolBlock> = block.clone();
                        block.acquire();
                        self.hit_count.fetch_add(1, Ordering::Relaxed);
                        let data_ref: Arc<Vec<u8>> = block.data.as_immutable()
                            .expect("Read-only block should be immutable");
                        return Ok(BlockHandle { data: data_ref, block });
                    }
                }
                
                // Block was evicted - create handle from shared data
                let block = Arc::new(PoolBlock::new(key.clone(), data.clone(), false));
                block.acquire();
                Ok(BlockHandle { data, block })
            }
            Err(msg) => Err(MemoryPoolError::RetrievalFailed(msg)),
        }
    }

    /// Try to get a block if it's already cached (no fetch).
    pub fn try_get(&self, key: &BlockKey) -> Option<BlockHandle> {
        let block_id: PoolBlockId = *self.key_index.get(key)?;
        let block: Arc<PoolBlock> = self.blocks.get(&block_id)?.clone();
        
        block.acquire();
        self.hit_count.fetch_add(1, Ordering::Relaxed);
        
        // Touch LRU
        {
            let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
            lru.touch(block_id);
        }
        
        let data: Arc<Vec<u8>> = block.data.as_immutable()?;
        Some(BlockHandle { data, block })
    }


    // ========================================================================
    // Stats and Utility Methods
    // ========================================================================

    /// Get current pool statistics.
    pub fn stats(&self) -> MemoryPoolStats {
        let lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
        let in_use_blocks: usize = self.blocks.iter()
            .filter(|entry| !entry.value().can_evict())
            .count();
        let dirty_blocks: usize = self.blocks.iter()
            .filter(|entry| entry.value().needs_flush())
            .count();
        
        MemoryPoolStats {
            total_blocks: self.blocks.len(),
            in_use_blocks,
            dirty_blocks,
            current_size: lru.current_size,
            max_size: lru.config.max_size,
            pending_fetches: self.pending_fetches.len(),
        }
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
    pub fn clear(&self) -> Result<(), MemoryPoolError> {
        let in_use: usize = self.blocks.iter()
            .filter(|entry| !entry.value().can_evict())
            .count();
        
        if in_use > 0 {
            return Err(MemoryPoolError::PoolExhausted {
                current_blocks: self.blocks.len(),
                in_use_blocks: in_use,
            });
        }
        
        self.blocks.clear();
        self.key_index.clear();
        
        let mut lru: parking_lot::MutexGuard<'_, LruState> = self.lru_state.lock();
        lru.lru_order.clear();
        lru.current_size = 0;
        
        Ok(())
    }

    /// Shrink a dirty block's allocation after flush to save memory.
    pub fn shrink_dirty_block(&self, inode_id: u64, chunk_index: u32) -> bool {
        let key = BlockKey::from_inode(inode_id, chunk_index);
        
        if let Some(block_id) = self.key_index.get(&key) {
            if let Some(block) = self.blocks.get(&*block_id) {
                if let BlockData::Mutable(data) = &block.data {
                    let mut guard = data.write();
                    guard.shrink_to_fit();
                    return true;
                }
            }
        }
        false
    }

    /// Get the number of dirty blocks for an inode.
    pub fn dirty_block_count(&self, inode_id: u64) -> usize {
        self.key_index
            .iter()
            .filter(|entry| entry.key().id.as_inode() == Some(inode_id))
            .count()
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
#[async_trait::async_trait]
pub trait BlockContentProvider: Send + Sync {
    /// Fetch the content for a block.
    async fn fetch(&self, key: &BlockKey) -> Result<Vec<u8>, MemoryPoolError>;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn small_config() -> MemoryPoolConfig {
        MemoryPoolConfig {
            max_size: 1024 * 1024,
            block_size: 256 * 1024,
        }
    }

    #[test]
    fn test_content_id_hash() {
        let id: ContentId = ContentId::from_hash_hex("abcdef1234567890abcdef1234567890");
        assert!(id.is_hash());
        assert!(!id.is_inode());
        assert!(id.as_hash_folded().is_some());
        assert_eq!(id.as_inode(), None);
    }

    #[test]
    fn test_content_id_inode() {
        let id = ContentId::Inode(42);
        assert!(!id.is_hash());
        assert!(id.is_inode());
        assert_eq!(id.as_hash_folded(), None);
        assert_eq!(id.as_inode(), Some(42));
    }

    #[test]
    fn test_block_key_from_inode() {
        let key = BlockKey::from_inode(42, 3);
        assert!(!key.is_read_only());
        assert!(key.is_dirty());
        assert_eq!(key.inode_id(), Some(42));
        assert_eq!(key.chunk_index, 3);
    }

    #[test]
    fn test_has_dirty_empty() {
        let pool = MemoryPool::new(small_config());
        assert!(!pool.has_dirty(42, 0));
    }

    #[test]
    fn test_insert_and_has_dirty() {
        let pool = MemoryPool::new(small_config());
        let data: Vec<u8> = vec![1, 2, 3, 4];
        
        pool.insert_dirty(42, 0, data).unwrap();
        
        assert!(pool.has_dirty(42, 0));
        assert!(!pool.has_dirty(42, 1));
        assert!(!pool.has_dirty(43, 0));
    }

    #[test]
    fn test_modify_dirty_in_place() {
        let pool = MemoryPool::new(small_config());
        let data: Vec<u8> = vec![1, 2, 3, 4];
        
        pool.insert_dirty(42, 0, data).unwrap();
        
        let new_size: usize = pool.modify_dirty_in_place(42, 0, |d| {
            d.push(5);
            d.push(6);
        }).unwrap();
        
        assert_eq!(new_size, 6);
        
        // Verify data was modified
        let handle = pool.get_dirty(42, 0).unwrap();
        assert_eq!(handle.len(), 6);
    }

    #[test]
    fn test_mark_flushed() {
        let pool = MemoryPool::new(small_config());
        let data: Vec<u8> = vec![1, 2, 3, 4];
        
        pool.insert_dirty(42, 0, data).unwrap();
        
        assert!(pool.mark_flushed(42, 0));
        assert!(!pool.mark_flushed(42, 1)); // Non-existent
    }

    #[test]
    fn test_remove_inode_blocks() {
        let pool = MemoryPool::new(small_config());
        
        pool.insert_dirty(42, 0, vec![1, 2]).unwrap();
        pool.insert_dirty(42, 1, vec![3, 4]).unwrap();
        pool.insert_dirty(43, 0, vec![5, 6]).unwrap();
        
        let removed: usize = pool.remove_inode_blocks(42);
        
        assert_eq!(removed, 2);
        assert!(!pool.has_dirty(42, 0));
        assert!(!pool.has_dirty(42, 1));
        assert!(pool.has_dirty(43, 0));
    }

    #[test]
    fn test_concurrent_has_dirty() {
        use std::thread;
        
        let pool = Arc::new(MemoryPool::new(small_config()));
        
        // Insert some blocks
        for i in 0..10 {
            pool.insert_dirty(i, 0, vec![i as u8]).unwrap();
        }
        
        // Concurrent reads
        let handles: Vec<_> = (0..4).map(|_| {
            let pool: Arc<MemoryPool> = pool.clone();
            thread::spawn(move || {
                for _ in 0..1000 {
                    for i in 0..10 {
                        assert!(pool.has_dirty(i, 0));
                    }
                }
            })
        }).collect();
        
        for h in handles {
            h.join().unwrap();
        }
    }
}
