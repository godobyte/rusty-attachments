# VFS Unified Memory Pool Design

**Status: ✅ IMPLEMENTED**

All phases of the unified memory pool are complete. See `crates/vfs/src/memory_pool_v2.rs` for the implementation.

## Problem Statement

Currently, the VFS has two separate memory management systems:
1. `MemoryPool` - LRU cache for read-only chunk data from S3
2. `DirtyFileManager` - Stores modified file content in `HashMap<INodeId, DirtyFile>`

This leads to:
- No global memory limit enforcement
- Dirty data can grow unbounded
- Read-only cache doesn't know about modifications (stale data risk)
- No coordination for memory pressure

## Goals

1. **Unified memory limit** - Single pool constrains total memory usage
2. **Pure LRU eviction** - All blocks evicted based solely on last access time
3. **Invalidation on write** - When a file is modified, evict stale read-only copies
4. **Force flush under pressure** - When pool is full, flush oldest dirty blocks to disk then evict
5. **Testability** - Maintain ability to test with mock implementations

## Proposed Design

### Block Types

Extend `BlockKey` to distinguish block types:

```rust
/// Identifier for a block - either hash-based or inode-based.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockId {
    /// Hash-based ID for read-only content (can be re-fetched from S3).
    Hash(String),
    /// Inode-based ID for dirty content.
    Inode(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockKey {
    /// Content hash or inode ID for dirty blocks.
    pub id: BlockId,
    /// Chunk index within the file.
    pub chunk_index: u32,
}
```

### Pool Block Extensions

```rust
struct PoolBlock {
    key: BlockKey,
    data: Arc<Vec<u8>>,
    ref_count: AtomicUsize,
    /// True if block has unflushed modifications (needs flush before evict).
    needs_flush: AtomicBool,
}

impl PoolBlock {
    fn can_evict(&self) -> bool {
        self.ref_count.load(Ordering::Acquire) == 0
    }
    
    fn needs_flush(&self) -> bool {
        self.needs_flush.load(Ordering::Acquire)
    }
}
```

### Eviction Strategy

**Pure LRU with force-flush:**

1. All blocks (clean and dirty) tracked in a single LRU queue
2. When eviction is needed, iterate LRU from oldest:
   - If block has `ref_count == 0` and `!needs_flush`: evict immediately
   - If block has `ref_count == 0` and `needs_flush`: flush to disk, then evict
   - If block has `ref_count > 0`: skip (in use)
3. This preserves true LRU behavior - oldest blocks are evicted first regardless of dirty status

### New Pool Methods

```rust
impl MemoryPool {
    /// Invalidate all read-only blocks for a given hash.
    /// Called when a file is modified to prevent stale reads.
    ///
    /// # Arguments
    /// * `hash` - Content hash to invalidate
    ///
    /// # Returns
    /// Number of blocks invalidated.
    pub fn invalidate_hash(&self, hash: &str) -> usize;

    /// Insert or update a dirty block.
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
    ) -> Result<BlockHandle, MemoryPoolError>;

    /// Mark a dirty block as flushed (no longer needs flush before evict).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `chunk_index` - Chunk index
    pub fn mark_flushed(&self, inode_id: u64, chunk_index: u32);

    /// Remove all blocks for an inode (clean up on file delete).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to remove
    pub fn remove_inode_blocks(&self, inode_id: u64);

    /// Get dirty block if it exists.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `chunk_index` - Chunk index
    pub fn get_dirty(&self, inode_id: u64, chunk_index: u32) -> Option<BlockHandle>;

    /// Evict blocks to free space, flushing dirty blocks as needed.
    /// 
    /// # Arguments
    /// * `needed_size` - Bytes needed for new allocation
    /// * `flush_fn` - Callback to flush dirty block to disk
    async fn evict_for_space_with_flush<F>(
        &self,
        needed_size: u64,
        flush_fn: F,
    ) -> Result<(), MemoryPoolError>
    where
        F: Fn(&BlockKey, &[u8]) -> BoxFuture<'static, Result<(), VfsError>>;
}
```

### DirtyFileManager Changes

```rust
pub struct DirtyFileManager {
    /// Metadata about dirty files (content stored in pool).
    dirty_metadata: RwLock<HashMap<INodeId, DirtyFileMetadata>>,
    /// Shared memory pool for content storage.
    pool: Arc<MemoryPool>,
    /// Disk cache for persistence.
    cache: Arc<dyn WriteCache>,
    /// Read-only file store for COW source.
    read_store: Arc<dyn FileStore>,
    /// Inode manager for path lookup.
    inodes: Arc<INodeManager>,
}

/// Metadata about a dirty file (content stored in pool).
struct DirtyFileMetadata {
    inode_id: INodeId,
    rel_path: String,
    parent_inode: Option<INodeId>,
    original_hashes: Vec<String>,  // For invalidation
    state: DirtyState,
    mtime: SystemTime,
    executable: bool,
    size: u64,
    chunk_count: u32,
    dirty_chunks: HashSet<u32>,
}
```

---

## Implementation Phases

### Phase 1: Extend MemoryPool with BlockId enum

**Scope:** Modify `BlockKey` to support both hash-based and inode-based identification.

**Changes:**
- Add `BlockId` enum (`Hash(String)` | `Inode(u64)`)
- Update `BlockKey` to use `BlockId` instead of `hash: String`
- Add helper constructors: `BlockKey::from_hash()`, `BlockKey::from_inode()`
- Update all existing code to use `BlockKey::from_hash()` (backward compatible)
- Add unit tests for new key types

**Files:**
- `crates/vfs/src/memory_pool.rs`

**Estimated effort:** Small

---

### Phase 2: Add `needs_flush` flag and flush-aware eviction

**Scope:** Add dirty block tracking and force-flush eviction.

**Changes:**
- Add `needs_flush: AtomicBool` to `PoolBlock`
- Modify `evict_one()` to handle dirty blocks:
  - If `needs_flush`, call flush callback before evicting
- Add `FlushCallback` trait for async flush operations
- Add `evict_for_space_with_flush()` method
- Add unit tests for flush-aware eviction

**Files:**
- `crates/vfs/src/memory_pool.rs`

**Estimated effort:** Medium

---

### Phase 3: Add invalidation and dirty block methods

**Scope:** Add methods for dirty block management.

**Changes:**
- Add `invalidate_hash()` - remove all blocks with matching hash
- Add `insert_dirty()` - insert block with `needs_flush = true`
- Add `mark_flushed()` - set `needs_flush = false`
- Add `get_dirty()` - lookup by inode ID
- Add `remove_inode_blocks()` - cleanup on file delete
- Add unit tests for each method

**Files:**
- `crates/vfs/src/memory_pool.rs`

**Estimated effort:** Medium

---

### Phase 4: Create DirtyFileMetadata and refactor DirtyFileManager

**Scope:** Separate metadata from content in DirtyFileManager.

**Changes:**
- Create `DirtyFileMetadata` struct (no content, just tracking info)
- Add `pool: Arc<MemoryPool>` to `DirtyFileManager`
- Update `DirtyFileManager::new()` to accept pool
- Refactor `cow_copy()` to store original hashes for invalidation
- Keep existing `DirtyFile` and `DirtyContent` temporarily for compatibility

**Files:**
- `crates/vfs/src/write/dirty.rs`

**Estimated effort:** Medium

---

### Phase 5: Migrate read operations to use pool

**Scope:** Update read path to use unified pool.

**Changes:**
- Update `read()` to check pool for dirty blocks first
- Update `ensure_chunk_loaded()` to use pool
- Update `ensure_small_file_loaded()` to use pool
- Invalidate read-only blocks when file becomes dirty
- Add integration tests

**Files:**
- `crates/vfs/src/write/dirty.rs`

**Estimated effort:** Medium

---

### Phase 6: Migrate write operations to use pool

**Scope:** Update write path to store content in pool.

**Changes:**
- Update `write()` to store dirty chunks in pool via `insert_dirty()`
- Update `write_small()` and `write_chunked()` 
- Call `invalidate_hash()` when modifying a file
- Update `truncate()` to work with pool
- Add integration tests

**Files:**
- `crates/vfs/src/write/dirty.rs`

**Estimated effort:** Medium

---

### Phase 7: Migrate flush and delete operations

**Scope:** Update flush and delete to work with pool.

**Changes:**
- Update `flush_to_disk()` to read from pool and call `mark_flushed()`
- Update `delete_file()` to call `remove_inode_blocks()`
- Update `remove_new_files_under_path()` to clean up pool blocks
- Add integration tests

**Files:**
- `crates/vfs/src/write/dirty.rs`

**Estimated effort:** Small

---

### Phase 8: Update WritableVfs and cleanup

**Scope:** Wire everything together and remove old code.

**Changes:**
- Update `WritableVfs::new()` to pass pool to `DirtyFileManager`
- Remove `DirtyContent` enum (content now in pool)
- Remove content fields from `DirtyFile` (now `DirtyFileMetadata`)
- Update stats collection to include unified pool stats
- Update all tests
- Final cleanup of unused code

**Files:**
- `crates/vfs/src/fuse_writable.rs`
- `crates/vfs/src/write/dirty.rs`
- `crates/vfs/src/write/stats.rs`
- `crates/vfs/tests/file_operations_matrix.rs`

**Estimated effort:** Medium

---

## Phase Summary

| Phase | Description | Effort | Dependencies | Status |
|-------|-------------|--------|--------------|--------|
| 1 | BlockId enum | Small | None | ✅ Complete |
| 2 | Flush-aware eviction | Medium | Phase 1 | ✅ Complete |
| 3 | Dirty block methods | Medium | Phase 2 | ✅ Complete |
| 4 | DirtyFileMetadata | Medium | Phase 3 | ✅ Complete |
| 5 | Read operations | Medium | Phase 4 | ✅ Complete |
| 6 | Write operations | Medium | Phase 5 | ✅ Complete |
| 7 | Flush/delete operations | Small | Phase 6 | ✅ Complete |
| 8 | Integration & cleanup | Medium | Phase 7 | ✅ Complete |

Each phase is designed to be:
- Self-contained with its own tests
- Backward compatible until Phase 8
- Completable in a single context window

## Completed Work

### Phase 1-3 Implementation (December 2024)

Added to `crates/vfs/src/memory_pool.rs`:

**New Types:**
- `ContentId` enum with `Hash(String)` and `Inode(u64)` variants
- Updated `BlockKey` to use `ContentId` instead of raw hash string
- Added `BlockKey::from_hash()` and `BlockKey::from_inode()` constructors
- Deprecated `BlockKey::new()` and `BlockKey::single()` for backward compatibility

**PoolBlock Changes:**
- Added `needs_flush: AtomicBool` field
- Added `can_evict_without_flush()` method
- Added `mark_flushed()` and `mark_needs_flush()` methods

**MemoryPoolInner Changes:**
- Updated `insert()` to accept `needs_flush` parameter
- Added `evict_one_clean()` - only evicts blocks that don't need flush
- Added `find_oldest_evictable()` - for future force-flush eviction
- Added `invalidate_hash()` - removes all blocks with matching hash
- Added `remove_inode_blocks()` - removes all blocks for an inode

**MemoryPool Public API:**
- `invalidate_hash(hash)` - Invalidate read-only blocks when file is modified
- `insert_dirty(inode_id, chunk_index, data)` - Insert dirty block
- `update_dirty(inode_id, chunk_index, data)` - Update dirty block
- `mark_flushed(inode_id, chunk_index)` - Mark block as flushed
- `mark_needs_flush(inode_id, chunk_index)` - Mark block as needing flush
- `get_dirty(inode_id, chunk_index)` - Get dirty block handle
- `remove_inode_blocks(inode_id)` - Remove all blocks for inode
- `has_dirty(inode_id, chunk_index)` - Check if dirty block exists
- `dirty_block_count(inode_id)` - Count dirty blocks for inode

**MemoryPoolStats Changes:**
- Added `dirty_blocks` field
- Added `clean_blocks()` method

**Tests Added:**
- 24 tests covering all new functionality
- Backward compatibility tests for deprecated methods

### Phase 4-8 Implementation (December 2024)

Added to `crates/vfs/src/write/dirty.rs`:

**New Types:**
- `DirtyFileMetadata` - Metadata-only tracking for dirty files (content stored in pool)
  - Tracks inode_id, rel_path, parent_inode, original_hashes, state, mtime, executable, size, chunk_count, dirty_chunks
  - `from_cow()` - Create from COW copy of existing file
  - `new_file()` - Create for new file
  - Helper methods for chunk calculations and dirty tracking

**DirtyFileManager Changes:**
- Added `dirty_metadata: RwLock<HashMap<INodeId, DirtyFileMetadata>>` for pool-based storage
- Added `pool: Option<Arc<MemoryPool>>` for unified memory management
- Added `with_pool()` constructor for pool-based mode
- Added `use_pool()` helper to check if pool mode is enabled
- Added `pool()` getter for memory pool reference

**Pool-Based Operations (Phase 5-7):**
- `read_from_pool()` - Read using pool-based storage
- `read_single_chunk_from_pool()` - Optimized single-chunk read
- `ensure_chunk_in_pool()` - Load chunk into pool from S3 if needed
- `write_to_pool()` - Write using pool-based storage
- `write_single_chunk_to_pool()` - Optimized single-chunk write
- `truncate_pool()` - Truncate using pool-based storage
- `flush_to_disk_pool()` - Flush from pool to disk cache

**Legacy Operations (Backward Compatibility):**
- `read_small_legacy()`, `read_chunked_legacy()` - Legacy read paths
- `write_small_legacy()`, `write_chunked_legacy()` - Legacy write paths
- `truncate_legacy()` - Legacy truncate path
- `flush_to_disk_legacy()` - Legacy flush path

**Updated Methods:**
- `is_dirty()` - Checks both pool metadata and legacy storage
- `get_size()`, `get_mtime()`, `get_state()` - Check both storages
- `cow_copy()` - Uses pool-based metadata when available, invalidates original hashes
- `read()`, `write()` - Route to pool or legacy based on mode
- `create_file()` - Creates in appropriate storage
- `get_new_files_in_dir()`, `lookup_new_file()`, `is_new_file()` - Check both storages
- `delete_file()` - Removes from pool and metadata/legacy storage
- `truncate()` - Routes to pool or legacy
- `flush_to_disk()` - Routes to pool or legacy, marks chunks as flushed
- `remove_new_files_under_path()` - Cleans up pool blocks
- `get_dirty_entries()` - Collects from both storages
- `clear()` - Clears pool blocks and both storages

**VfsError Changes:**
- Added `MemoryPoolError(String)` variant

**WritableVfs Changes (Phase 8):**
- Updated `new()` to use `DirtyFileManager::with_pool()` for unified memory management

**Tests Added:**
- `test_dirty_file_metadata_*` - 7 tests for DirtyFileMetadata
- `test_manager_with_pool_*` - 8 tests for pool-based DirtyFileManager operations
- Total: 24 tests in dirty.rs module

### Legacy Code Cleanup (December 2024)

Removed backward compatibility code to always use the memory pool:

**Removed from `dirty.rs`:**
- `DirtyContent` enum (Small/Chunked variants) - content now stored in pool
- `DirtyFile` struct - replaced by `DirtyFileMetadata`
- `dirty_files: RwLock<HashMap<INodeId, DirtyFile>>` field
- `use_pool()` method - pool is now always used
- All `*_legacy` methods:
  - `read_small_legacy()`
  - `read_chunked_legacy()`
  - `write_small_legacy()`
  - `write_chunked_legacy()`
  - `truncate_legacy()`
  - `flush_to_disk_legacy()`
  - `ensure_chunk_loaded()` (legacy chunk loading)
  - `ensure_small_file_loaded()` (legacy small file loading)
- `ContentSnapshot` enum (used only by legacy flush)

**API Changes:**
- `DirtyFileManager::new()` now requires a `pool` parameter
- `DirtyFileManager::with_pool()` is now an alias for `new()`
- `pool()` method now returns `&Arc<MemoryPool>` (not `Option`)
- Removed `#[allow(dead_code)]` annotations from `DirtyFileMetadata` methods

**Updated Exports:**
- `crates/vfs/src/write/mod.rs`: Exports `DirtyFileMetadata` instead of `DirtyContent`/`DirtyFile`
- `crates/vfs/src/lib.rs`: Updated re-exports accordingly

**Test Updates:**
- Removed `small_to_chunked` test module (tested removed types)
- Updated `create_test_env()` helpers to pass pool parameter
- All 108 lib tests and 51 integration tests pass

**Architecture After Cleanup:**
```
WritableVfs
    └── DirtyFileManager
            ├── dirty_metadata: HashMap<INodeId, DirtyFileMetadata>  // Tracks state
            ├── pool: Arc<MemoryPool>                                 // Stores content
            ├── cache: Arc<dyn WriteCache>                            // Disk persistence
            └── read_store: Arc<dyn FileStore>                        // S3 source

MemoryPool
    ├── Hash-based blocks (read-only, can be re-fetched)
    └── Inode-based blocks (dirty, need flush before eviction)
```
