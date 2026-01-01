# VFS Write Performance Trace Analysis

Date: 2026-01-01

## Test Configuration
- Command: `sudo perf record -g -F 999 -- ./target/release/examples/mount_vfs job_bundles_manifest_v2.json ./vfs --writable --cache-dir /tmp/vfs-cache`
- Samples: 3,887 (with kernel symbols)
- Event: cpu-clock:ppp

## Top-Level Breakdown (with kernel symbols)

| Overhead | Component | Symbol | Description |
|----------|-----------|--------|-------------|
| 20.98% | vfs-io-worker | `_raw_spin_unlock_irqrestore` | Kernel spinlock release |
| 10.84% | vfs-async-executor | `_raw_spin_unlock_irqrestore` | Kernel spinlock release |
| 8.32% | mount_vfs | `_raw_spin_unlock_irqrestore` | Kernel spinlock release |
| 7.60% | mount_vfs | `libc::read` | FUSE device read |
| 3.27% | mount_vfs | `internal_get_user_pages_fast` | Page table walks |
| 2.19% | vfs-io-worker | `libc::memmove` | Memory copy |
| 1.83% | vfs-io-worker | `libc::syscall` | Syscall overhead |
| 1.70% | vfs-io-worker | `libc::memset` | Memory zeroing |
| 1.60% | vfs-io-worker | `do_user_addr_fault` | Page faults |
| 1.00% | mount_vfs | `__iov_iter_get_pages_alloc` | FUSE buffer allocation |
| 0.98% | vfs-io-worker | `clear_page_rep` | Page clearing |
| 0.88% | mount_vfs | `fuse_dev_write` | FUSE write to kernel |
| 0.85% | mount_vfs | `fuse_dev_do_write` | FUSE write processing |

## Userspace VFS Code

| Overhead | Function |
|----------|----------|
| 0.72% | `MemoryPool::modify_dirty_in_place` |
| 0.54% | `AsyncExecutor::block_on_internal` |
| 0.54% | `MemoryPool::has_dirty` |
| 0.36% | `DirtyFileManager::is_dirty` |
| 0.33% | `tokio::Unparker::unpark` |

## Key Findings

### 1. Futex Contention is the #1 Issue (~40% of CPU)

The `_raw_spin_unlock_irqrestore` calls are dominated by **futex operations**:

```
vfs-io-worker syscalls:
├── 81.87% __x64_sys_futex
│   └── 100% do_futex
│       ├── 95.88% futex_wake
│       │   └── 98.24% wake_up_q → try_to_wake_up → _raw_spin_unlock_irqrestore
│       └── 4.12% futex_wait
```

This indicates heavy thread synchronization - likely from:
- Mutex locks in `MemoryPool` (std::sync::Mutex uses futex)
- RwLock in `DirtyFileManager` 
- Tokio runtime thread coordination

### 2. Memory Allocation Pressure (~12% of vfs-io-worker)

```
ksys_write → vfs_write → generic_perform_write:
├── 70.54% shmem_write_begin → shmem_get_folio_gfp
│   ├── 40.70% shmem_add_to_page_cache (spinlocks)
│   ├── 34.88% shmem_alloc_and_acct_folio (page allocation)
│   └── 13.95% folio_add_lru (LRU list management)
└── 23.26% copy_page_from_iter_atomic → copy_user_generic_string
```

The kernel is allocating pages for shmem (tmpfs) writes - this is the COW cache.

### 3. Page Faults (~1.6%)

`do_user_addr_fault` and `do_anonymous_page` indicate page faults when accessing newly allocated memory. This is expected for large allocations but could be reduced with pre-allocation.

### 4. FUSE Protocol Overhead (~3%)

```
fuse_dev_write (0.88%) + fuse_dev_do_write (0.85%) + fuse_copy_* (~1%)
```

The FUSE kernel module is processing requests. This is unavoidable but relatively small.

## Detailed Analysis

### Lock Contention in `modify_dirty_in_place`

The current implementation has a problematic pattern:
```rust
pub fn modify_dirty_in_place<F>(...) {
    let inner = self.inner.lock().unwrap();  // Lock #1
    // ... lookup block ...
    drop(inner);                              // Release
    let mut inner = self.inner.lock().unwrap(); // Lock #2 - re-acquire!
    // ... update size tracking ...
}
```

This double-lock pattern is inefficient and creates a race window.

### HashMap Key Hashing

`BlockKey` uses `ContentId` which is an enum:
```rust
pub enum ContentId {
    Hash(u64),    // Folded hash
    Inode(u64),   // Inode ID
}
```

The `BlockKey` hash involves:
1. Hashing the enum discriminant
2. Hashing the u64 value
3. Hashing the chunk_index (u32)

Using SipHash for this is overkill - these are trusted internal keys.

### Redundant Dirty Checks

The write path does:
1. `DirtyFileManager::is_dirty()` - HashMap lookup with RwLock
2. `MemoryPool::has_dirty()` - HashMap lookup with Mutex
3. `MemoryPool::modify_dirty_in_place()` - Another HashMap lookup

Three separate lock acquisitions and HashMap lookups for a single write.

---

## Optimization Recommendations (Updated with Kernel Analysis)

### 1. CRITICAL: Reduce Futex/Lock Contention (~40% potential improvement)

**Impact**: Very High
**Effort**: Medium

The biggest issue is futex contention from Mutex/RwLock operations. Options:

**Option A: Use parking_lot instead of std::sync**
```rust
// In Cargo.toml
parking_lot = "0.12"

// Replace std::sync::Mutex with parking_lot::Mutex
use parking_lot::Mutex;
```
`parking_lot` uses spinning before falling back to futex, reducing syscalls.

**Option B: Use lock-free data structures for hot paths**
```rust
// For dirty block lookup, use a concurrent HashMap
use dashmap::DashMap;

struct MemoryPoolInner {
    // Lock-free concurrent map for hot path
    key_index: DashMap<BlockKey, PoolBlockId>,
    // Keep mutex only for cold paths (eviction, stats)
    blocks: Mutex<HashMap<PoolBlockId, Arc<PoolBlock>>>,
}
```

**Option C: Batch operations to reduce lock acquisitions**
```rust
/// Write multiple chunks in a single lock acquisition.
pub fn batch_modify_dirty<F>(
    &self,
    operations: &[(u64, u32, F)],  // (inode_id, chunk_index, modifier)
) -> Result<Vec<usize>, MemoryPoolError>
```

### 2. Eliminate Double-Lock in `modify_dirty_in_place`

**Impact**: Medium (reduces futex calls)
**Effort**: Low

Current code acquires the mutex twice:
```rust
let inner = self.inner.lock().unwrap();  // Lock #1
// ... lookup ...
drop(inner);
let mut inner = self.inner.lock().unwrap(); // Lock #2
```

Fix: Keep the lock for the entire operation.

### 3. Pre-allocate Memory to Reduce Page Faults

**Impact**: ~2% CPU reduction
**Effort**: Low

The kernel is spending time in `do_anonymous_page` and `clear_page_rep`. Pre-allocate buffers:

```rust
impl PoolBlock {
    fn new_mutable(key: BlockKey, data: Vec<u8>, capacity: Option<usize>) -> Self {
        let mut data = data;
        if let Some(cap) = capacity {
            // Pre-allocate and touch pages to avoid faults during writes
            data.reserve(cap);
            // Optionally: data.resize(cap, 0); data.truncate(original_len);
        }
        // ...
    }
}
```

### 4. Use FxHashMap for Internal HashMaps

**Impact**: ~0.4% CPU reduction
**Effort**: Low

```rust
use rustc_hash::FxHashMap;

struct MemoryPoolInner {
    blocks: FxHashMap<PoolBlockId, Arc<PoolBlock>>,
    key_index: FxHashMap<BlockKey, PoolBlockId>,
}
```

### 5. Reduce Memory Copies

**Impact**: ~2% CPU reduction (memmove/memset)
**Effort**: Medium

Add a direct write method that avoids the closure pattern:

```rust
/// Write data directly to a dirty block at the specified offset.
pub fn write_dirty_at(
    &self,
    inode_id: u64,
    chunk_index: u32,
    offset: usize,
    data: &[u8],
) -> Result<(), MemoryPoolError> {
    let key = BlockKey::from_inode(inode_id, chunk_index);
    let inner = self.inner.lock().unwrap();
    
    let block_id = *inner.key_index.get(&key)
        .ok_or(MemoryPoolError::BlockNotFound(0))?;
    let block = inner.blocks.get(&block_id)
        .ok_or(MemoryPoolError::BlockNotFound(block_id))?;
    
    match &block.data {
        BlockData::Mutable(rw_data) => {
            let mut guard = rw_data.write().unwrap();
            let end = offset + data.len();
            if end > guard.len() {
                guard.resize(end, 0);
            }
            guard[offset..end].copy_from_slice(data);
            block.mark_needs_flush();
            Ok(())
        }
        BlockData::Immutable(_) => Err(MemoryPoolError::BlockNotMutable(block_id)),
    }
}
```

### 6. Consider io_uring for Disk Cache Writes

**Impact**: Reduces syscall overhead for disk I/O
**Effort**: High

For the disk cache flush operations, `io_uring` can batch multiple writes into a single syscall.

---

## Priority Order (Updated)

1. **parking_lot or DashMap** - Biggest win, addresses 40% of CPU time
2. **Fix double-lock** - Easy fix, reduces futex calls
3. **Pre-allocate buffers** - Reduces page faults
4. **FxHashMap** - Easy win, low risk
5. **Direct write method** - Reduces memory copies
6. **io_uring** - Future optimization for disk I/O

## Summary

The trace reveals that **lock contention is the dominant bottleneck**, not the userspace VFS code itself. The `_raw_spin_unlock_irqrestore` calls (40% of CPU) are from futex operations triggered by Rust's `std::sync::Mutex` and `RwLock`. 

Switching to `parking_lot` or using lock-free structures like `DashMap` for the hot paths would have the biggest impact. The userspace VFS code (`modify_dirty_in_place`, `has_dirty`, `is_dirty`) is only ~1.6% of total CPU time.
