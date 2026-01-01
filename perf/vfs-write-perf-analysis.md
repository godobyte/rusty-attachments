# VFS Write Performance Trace Analysis

Date: 2026-01-01

## Test Configuration
- Command: `sudo perf record -g -F 999 -o /tmp/vfs-write-perf.data -- ./target/release/examples/mount_vfs job_bundles_manifest_v2.json ./vfs --writable --cache-dir /tmp/vfs-cache`
- Samples: 2,562
- Event: cpu-clock:ppp

## Top Userspace Hotspots (VFS Code)

| Overhead | Function |
|----------|----------|
| 0.94% | `MemoryPool::modify_dirty_in_place` |
| 0.74% | `MemoryPool::has_dirty` |
| 0.59% | `DirtyFileManager::is_dirty` |
| 0.47% | `AsyncExecutor::block_on_internal` |
| 0.23% | `core::hash::BuildHasher::hash_one` |
| 0.20% | `<Hasher>::write` (SipHash) |
| 0.12% | `DirtyFileManager::ensure_chunk_in_pool` |

## System-Level Overhead

| Overhead | Component | Symbol |
|----------|-----------|--------|
| 20.80% | vfs-io-worker | kernel (unknown) |
| 8.98% | mount_vfs | kernel (unknown) |
| 8.74% | mount_vfs | `libc::read` |
| 8.20% | vfs-async-executor | kernel (unknown) |
| 2.07% | vfs-io-worker | `libc::memmove` |
| 1.83% | vfs-io-worker | `libc::memset` |
| 1.13% | vfs-io-worker | `libc::syscall` |

## Key Observations

1. **Kernel dominates**: ~38% of time in kernel space (FUSE I/O, memory operations)

2. **Dirty tracking overhead**: `MemoryPool` and `DirtyFileManager` functions together ~2.3%
   - These involve HashMap lookups (SipHash)
   - `has_dirty` / `is_dirty` checks appear frequently

3. **Memory operations significant**: `memmove` (2.07%) + `memset` (1.83%) = ~4%

4. **Hashing overhead**: SipHash operations visible in profile (~0.4%)

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

## Optimization Recommendations

### 1. Use FxHashMap (rustc-hash) for Internal HashMaps

**Impact**: ~0.4% CPU reduction
**Effort**: Low

Replace `HashMap` with `FxHashMap` for:
- `MemoryPoolInner::blocks`
- `MemoryPoolInner::key_index`
- `DirtyFileManager::dirty_metadata`

```rust
use rustc_hash::FxHashMap;

struct MemoryPoolInner {
    blocks: FxHashMap<PoolBlockId, Arc<PoolBlock>>,
    key_index: FxHashMap<BlockKey, PoolBlockId>,
    // ...
}
```

FxHash is ~2-3x faster than SipHash for integer keys.

### 2. Eliminate Double-Lock in `modify_dirty_in_place`

**Impact**: Reduces lock contention
**Effort**: Low

```rust
pub fn modify_dirty_in_place<F>(...) -> Result<usize, MemoryPoolError> {
    let mut inner = self.inner.lock().unwrap();
    
    let block_id = *inner.key_index.get(&key)
        .ok_or(MemoryPoolError::BlockNotFound(0))?;
    
    let block = inner.blocks.get(&block_id)
        .ok_or(MemoryPoolError::BlockNotFound(block_id))?;
    
    let (old_size, new_size) = match &block.data {
        BlockData::Mutable(data) => {
            let mut guard = data.write().unwrap();
            let old = guard.len() as u64;
            modifier(&mut guard);
            (old, guard.len() as u64)
        }
        BlockData::Immutable(_) => return Err(MemoryPoolError::BlockNotMutable(block_id)),
    };
    
    // Update size while still holding lock
    if new_size != old_size {
        inner.current_size = inner.current_size
            .saturating_add(new_size)
            .saturating_sub(old_size);
    }
    
    block.mark_needs_flush();
    Ok(new_size as usize)
}
```

### 3. Combined Dirty Check + Modify Operation

**Impact**: ~1.3% CPU reduction (eliminates redundant lookups)
**Effort**: Medium

Add a method that combines `has_dirty` check with modification:

```rust
/// Get or create a dirty block, returning a handle for modification.
pub fn get_or_ensure_dirty(
    &self,
    inode_id: u64,
    chunk_index: u32,
    create_fn: impl FnOnce() -> Vec<u8>,
) -> Result<MutableBlockHandle, MemoryPoolError> {
    let key = BlockKey::from_inode(inode_id, chunk_index);
    let mut inner = self.inner.lock().unwrap();
    
    // Single lookup - either get existing or create
    if let Some(&block_id) = inner.key_index.get(&key) {
        // Existing block
        let block = inner.blocks.get(&block_id).unwrap().clone();
        block.acquire();
        // ... return handle
    } else {
        // Create new block
        let data = create_fn();
        let block = inner.insert_mutable(key, data, None);
        block.acquire();
        // ... return handle
    }
}
```

### 4. Per-Inode Dirty Block Cache

**Impact**: Reduces HashMap lookups for sequential writes
**Effort**: Medium

For files being actively written, cache the block handle:

```rust
struct DirtyFileMetadata {
    // ... existing fields ...
    
    /// Cached handle to chunk 0 for small files (most common case)
    cached_chunk0: Option<Arc<PoolBlock>>,
}
```

### 5. Batch Write Coalescing

**Impact**: Reduces syscall overhead
**Effort**: High

FUSE sends many small writes (4KB-128KB). Coalesce them:

```rust
struct WriteBuffer {
    inode_id: INodeId,
    pending_writes: Vec<(u64, Vec<u8>)>,  // (offset, data)
    last_flush: Instant,
}

impl WriteBuffer {
    fn should_flush(&self) -> bool {
        self.pending_writes.len() > 16 
            || self.last_flush.elapsed() > Duration::from_millis(10)
    }
}
```

### 6. Reduce Memory Copies in Write Path

**Impact**: ~2% CPU reduction (memmove/memset)
**Effort**: Medium

Current write path copies data multiple times:
1. `data.to_vec()` in `write_single_chunk`
2. `copy_from_slice` into pool buffer

Consider using `bytes::Bytes` or direct slice writes:

```rust
// Instead of:
let data_to_write: Vec<u8> = data.to_vec();
self.pool.modify_dirty_in_place(inode_id, 0, |file_data| {
    file_data[offset..end].copy_from_slice(&data_to_write);
});

// Use:
self.pool.write_at(inode_id, 0, offset, data)?;
```

---

## Priority Order

1. **FxHashMap** - Easy win, low risk
2. **Fix double-lock** - Bug fix + perf improvement
3. **Combined dirty check** - Moderate effort, good payoff
4. **Write coalescing** - Higher effort, significant impact for write-heavy workloads
