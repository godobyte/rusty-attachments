# VFS Write Performance Analysis: DashMap Migration (v3)

## Executive Summary

After migrating from `memory_pool` (single Mutex) to `memory_pool_v2` (DashMap-based), the VFS write performance shows measurable improvement in lock contention, but significant optimization opportunities remain.

**Key Finding:** The DashMap migration reduced futex contention from ~40% to ~32.6% (-18% relative improvement). However, the remaining bottlenecks are now in different areas that can yield >5% gains.

## Performance Breakdown by Thread

### 1. vfs-io-worker (FUSE I/O Thread) - 47% of samples

| Category | Overhead | Notes |
|----------|----------|-------|
| Futex wake (thread coordination) | 17.15% | Waking async executor threads |
| Memory operations (memmove) | 7.77% | Data copying in write path |
| Page faults | 6.56% | Memory allocation on write |
| Disk I/O (MaterializedCache) | 4.62% | Writing to disk cache |
| DashMap operations | 1.31% | Lock-free lookups |
| memory_pool_v2 operations | 0.65% | modify_dirty_in_place |

### 2. mount_vfs (Main FUSE Thread) - 30% of samples

| Category | Overhead | Notes |
|----------|----------|-------|
| FUSE kernel communication | 12.60% | fuse_dev_do_write, fuse_dev_write |
| libc read() | 9.19% | Reading FUSE requests |
| Futex operations | 4.67% | Thread synchronization |
| Page operations | 3.46% | fuse_copy_fill, iov_iter |

### 3. vfs-async-execu (Tokio Executor) - 15% of samples

| Category | Overhead | Notes |
|----------|----------|-------|
| Futex wake | 6.82% | Waking worker threads |
| Tokio scheduler | 2.51% | worker_to_notify, push_remote_task |
| syscall overhead | 1.39% | syscall_enter_from_user_mode |

---

## Optimization Opportunities (>5% Impact)

### 🔴 Opportunity 1: Eliminate Async Executor Overhead (~15-20% potential)

**Current Problem:**
The write path uses `AsyncExecutor::block_on()` to bridge sync FUSE callbacks to async operations. This causes:
- 15.55% in `futex_wake` from vfs-io-worker waking async executor
- 6.82% in `futex_wake` from async executor coordination
- ~2.5% in Tokio scheduler overhead

**Call Stack:**
```
vfs-io-worker -> syscall -> futex_wake -> wake_up_q -> try_to_wake_up
```

**Solution: Make write path fully synchronous**

The dirty file write path doesn't actually need async:
1. `has_dirty()` - DashMap lookup (already sync)
2. `get_dirty()` - DashMap lookup (already sync)  
3. `modify_dirty_in_place()` - DashMap modify (already sync)
4. `insert_dirty()` - DashMap insert (already sync)

Only `ensure_chunk_in_pool()` needs async (for S3 fetch on COW), but for writes to already-dirty files, this is a no-op.

**Implementation:**
```rust
// In fuse_writable.rs write() handler
fn write(&mut self, ...) {
    // Fast path: file already dirty, no async needed
    if self.dirty_manager.is_dirty(ino) {
        // Synchronous write - no executor needed
        let written = self.dirty_manager.write_sync(ino, offset, data)?;
        reply.written(written as u32);
        return;
    }
    
    // Slow path: COW needed, use async
    let result = self.executor.block_on(async { ... });
}
```

**Expected Impact:** 10-15% reduction in CPU time

---

### 🔴 Opportunity 2: Reduce Memory Copy Overhead (~8% potential)

**Current Problem:**
`__memmove_avx_unaligned_erms` at 7.77% indicates excessive data copying.

**Call Stack Analysis:**
```
memmove -> page_fault -> do_anonymous_page -> vma_alloc_folio -> clear_page_rep
```

This shows:
1. Data is being copied into newly allocated pages
2. Pages are being zeroed before use

**Root Cause:** Each write creates a new `Vec<u8>` copy of the data:
```rust
// In dirty.rs write_single_chunk()
let data_to_write: Vec<u8> = data.to_vec();  // COPY HERE
self.pool.modify_dirty_in_place(inode_id, 0, |file_data| {
    file_data[offset..end].copy_from_slice(&data_to_write);  // COPY AGAIN
});
```

**Solution: Zero-copy write path**
```rust
// Pass slice directly, avoid intermediate Vec
self.pool.modify_dirty_in_place(inode_id, 0, |file_data| {
    if end > file_data.len() {
        file_data.resize(end, 0);
    }
    file_data[offset..end].copy_from_slice(data);  // Direct from FUSE buffer
});
```

**Expected Impact:** 5-8% reduction in CPU time

---

### 🟡 Opportunity 3: Batch Disk Cache Writes (~5% potential)

**Current Problem:**
`MaterializedCache::write_file` shows 4.62% overhead with this call stack:
```
write_file -> __GI___libc_write -> ksys_write -> generic_perform_write -> copy_user_generic_string
```

Each dirty chunk is written to disk individually on flush.

**Solution: Batch writes with write-behind**
1. Accumulate dirty chunks in memory
2. Write to disk in batches (e.g., every 100 chunks or 100MB)
3. Use `O_DIRECT` + aligned buffers to bypass page cache

**Implementation:**
```rust
struct BatchedWriteCache {
    pending: Vec<(String, Vec<u8>)>,  // (path, data)
    pending_size: usize,
    max_batch_size: usize,  // 100MB default
}

impl BatchedWriteCache {
    fn queue_write(&mut self, path: String, data: Vec<u8>) {
        self.pending.push((path, data));
        self.pending_size += data.len();
        if self.pending_size >= self.max_batch_size {
            self.flush_batch();
        }
    }
}
```

**Expected Impact:** 3-5% reduction in CPU time

---

### 🟡 Opportunity 4: Optimize DashMap Hash Function (~1-2% potential)

**Current Problem:**
DashMap operations show:
- `<dashmap::DashMap>::_get`: 1.31%
- `dashmap::DashMap::hash_u64`: 0.18%
- `<core::hash::sip::Hasher>::write`: 0.21%

SipHash is cryptographically secure but slow for this use case.

**Solution: Use FxHash or AHash**
```rust
use dashmap::DashMap;
use rustc_hash::FxHasher;
use std::hash::BuildHasherDefault;

type FastDashMap<K, V> = DashMap<K, V, BuildHasherDefault<FxHasher>>;

// In memory_pool_v2.rs
pub struct MemoryPool {
    blocks: FastDashMap<BlockKey, Arc<RwLock<Block>>>,
    key_index: FastDashMap<(INodeId, u32), BlockKey>,
}
```

**Expected Impact:** 1-2% reduction in CPU time

---

### 🟢 Opportunity 5: Reduce Tokio Task Spawn Overhead (~2% potential)

**Current Problem:**
Tokio scheduler functions show ~2.5% overhead:
- `worker_to_notify`: 1.03%
- `Unparker::unpark`: 0.96%
- `push_remote_task`: 0.52%

**Solution: Use `spawn_local` for single-threaded operations**

For operations that don't need cross-thread scheduling:
```rust
// Instead of spawning to multi-thread executor
tokio::spawn(async { ... });

// Use local task for same-thread execution
tokio::task::spawn_local(async { ... });
```

**Expected Impact:** 1-2% reduction in CPU time

---

## Summary of Recommendations

| Priority | Optimization | Expected Impact | Complexity |
|----------|-------------|-----------------|------------|
| 🔴 High | Sync write path for dirty files | 10-15% | Medium |
| 🔴 High | Zero-copy write (eliminate Vec clone) | 5-8% | Low |
| 🟡 Medium | Batch disk cache writes | 3-5% | Medium |
| 🟡 Medium | FxHash for DashMap | 1-2% | Low |
| 🟢 Low | spawn_local for local tasks | 1-2% | Low |

**Total Potential Improvement: 20-32%**

---

## Comparison: Before vs After DashMap Migration

| Metric | v2 (Mutex) | v3 (DashMap) | Change |
|--------|------------|--------------|--------|
| vfs-io-worker futex | 20.98% | 17.15% | -3.83% ✅ |
| vfs-async-execu futex | 10.84% | 6.82% | -4.02% ✅ |
| mount_vfs futex | 8.32% | 8.65% | +0.33% |
| Total futex overhead | ~40% | ~32.6% | -7.4% ✅ |
| DashMap visible | N/A | 1.31% | New |
| memmove | 2.19% | 7.77% | +5.58% ⚠️ |

**Note:** The increase in memmove is expected - threads are now doing actual work instead of waiting on locks. This is a positive sign that the lock contention was the bottleneck.

---

## Next Steps

1. **Implement sync write path** (Opportunity 1) - Highest impact, medium effort
2. **Remove Vec clone in write** (Opportunity 2) - High impact, low effort
3. **Profile again** after changes to validate improvements
4. **Consider batch writes** if disk I/O becomes the new bottleneck
