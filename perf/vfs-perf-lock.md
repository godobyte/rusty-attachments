# VFS Performance: Lock Contention Fix

## Problem Summary

Perf trace shows ~40% of CPU time in futex operations due to lock contention in `MemoryPool`.
The hot path (`modify_dirty_in_place`, `has_dirty`) serializes all access through a single `Mutex<MemoryPoolInner>`.

## Solution Overview

1. Replace single `Mutex<MemoryPoolInner>` with `DashMap` for concurrent key lookups
2. Fix double-lock pattern in `modify_dirty_in_place`
3. Keep a separate `Mutex` only for LRU eviction state (cold path)

## Implementation Status

### ✅ Step 1: Add Dependencies (DONE)

Added to `crates/vfs/Cargo.toml`:
```toml
dashmap = "6"
parking_lot = "0.12"
```

### ✅ Step 2: Create memory_pool_v2.rs (DONE)

Created `crates/vfs/src/memory_pool_v2.rs` with:
- DashMap-based `blocks` and `key_index` for lock-free hot path
- `parking_lot::Mutex<LruState>` for cold path (eviction only)
- `parking_lot::RwLock` for mutable block data
- Fixed double-lock in `modify_dirty_in_place`

### 🔲 Step 3: Update lib.rs Re-exports

File: `crates/vfs/src/lib.rs`

Current:
```rust
pub use memory_pool::{
    BlockContentProvider, BlockHandle, BlockKey, MemoryPool, MemoryPoolConfig, MemoryPoolError,
    MemoryPoolStats,
};
```

Change to:
```rust
pub use memory_pool_v2::{
    BlockContentProvider, BlockHandle, BlockKey, MemoryPool, MemoryPoolConfig, MemoryPoolError,
    MemoryPoolStats, MutableBlockHandle,
};
```

### 🔲 Step 4: Update Read Path (fuse.rs)

File: `crates/vfs/src/fuse.rs`

This file handles read-only FUSE operations. Changes needed:

1. Update import:
```rust
// Change from:
use crate::memory_pool::{BlockKey, MemoryPool, MemoryPoolError, MemoryPoolStats};
// To:
use crate::memory_pool_v2::{BlockKey, MemoryPool, MemoryPoolError, MemoryPoolStats};
```

2. The `pool.acquire()` calls in `read()` method remain unchanged - API is compatible.

3. `VfsStatsCollector` uses `pool.stats()`, `pool.hit_count()`, `pool.allocation_count()`, `pool.hit_rate()` - all compatible.

### 🔲 Step 5: Update Write Path (dirty.rs)

File: `crates/vfs/src/write/dirty.rs`

This file handles dirty file tracking for COW writes. Changes needed:

1. Update import:
```rust
// Change from:
use crate::memory_pool::MemoryPool;
// To:
use crate::memory_pool_v2::MemoryPool;
```

2. Also update test imports:
```rust
// Change from:
use crate::memory_pool::MemoryPoolConfig;
// To:
use crate::memory_pool_v2::MemoryPoolConfig;
```

3. Methods used by DirtyFileManager (all compatible with v2):
   - `pool.has_dirty(inode_id, chunk_index)` - lock-free in v2
   - `pool.modify_dirty_in_place(inode_id, chunk_index, modifier)` - optimized in v2
   - `pool.get_dirty(inode_id, chunk_index)` - lock-free lookup in v2
   - `pool.insert_dirty(inode_id, chunk_index, data)` - compatible
   - `pool.mark_flushed(inode_id, chunk_index)` - lock-free in v2
   - `pool.invalidate_hash(hash)` - compatible
   - `pool.remove_inode_blocks(inode_id)` - compatible

### 🔲 Step 6: Update Writable FUSE (fuse_writable.rs)

File: `crates/vfs/src/fuse_writable.rs`

This file handles writable FUSE operations. Changes needed:

1. Update import:
```rust
// Change from:
use crate::memory_pool::MemoryPool;
// To:
use crate::memory_pool_v2::MemoryPool;
```

2. The `WritableVfs` struct holds `Arc<MemoryPool>` - type compatible.

3. Methods used (all compatible):
   - Delegates to `DirtyFileManager` which uses the pool

### 🔲 Step 7: Update Options (options.rs)

File: `crates/vfs/src/options.rs`

1. Update import:
```rust
// Change from:
use crate::memory_pool::MemoryPoolConfig;
// To:
use crate::memory_pool_v2::MemoryPoolConfig;
```

### 🔲 Step 8: Update Stats (write/stats.rs)

File: `crates/vfs/src/write/stats.rs`

1. Update import:
```rust
// Change from:
use crate::memory_pool::{MemoryPool, MemoryPoolStats};
// To:
use crate::memory_pool_v2::{MemoryPool, MemoryPoolStats};
```

### 🔲 Step 9: Update Integration Tests

File: `crates/vfs/tests/read_cache_integration.rs`

1. Update import:
```rust
// Change from:
use rusty_attachments_vfs::memory_pool::{MemoryPool, MemoryPoolConfig};
// To:
use rusty_attachments_vfs::memory_pool_v2::{MemoryPool, MemoryPoolConfig};
```

Or if using re-exports from lib.rs, no change needed.

### 🔲 Step 10: Run Tests and Benchmark

1. Run unit tests:
```bash
cargo test -p rusty-attachments-vfs
```

2. Build release:
```bash
cargo build --release --features fuse -p rusty-attachments-vfs
```

3. Run perf benchmark:
```bash
sudo perf record -g -F 999 -- ./target/release/examples/mount_vfs ...
```

4. Compare futex syscall counts before/after

## API Compatibility

The `memory_pool_v2` module exports the same public API as `memory_pool`:

| Type/Function | v1 | v2 | Notes |
|---------------|----|----|-------|
| `MemoryPool::new()` | ✅ | ✅ | Same signature |
| `MemoryPool::with_defaults()` | ✅ | ✅ | Same signature |
| `MemoryPool::acquire()` | ✅ | ✅ | Same signature |
| `MemoryPool::try_get()` | ✅ | ✅ | Same signature |
| `MemoryPool::has_dirty()` | ✅ | ✅ | Lock-free in v2 |
| `MemoryPool::get_dirty()` | ✅ | ✅ | Lock-free lookup in v2 |
| `MemoryPool::insert_dirty()` | ✅ | ✅ | Same signature |
| `MemoryPool::modify_dirty_in_place()` | ✅ | ✅ | No double-lock in v2 |
| `MemoryPool::mark_flushed()` | ✅ | ✅ | Lock-free in v2 |
| `MemoryPool::mark_needs_flush()` | ✅ | ✅ | Lock-free in v2 |
| `MemoryPool::invalidate_hash()` | ✅ | ✅ | Same signature |
| `MemoryPool::remove_inode_blocks()` | ✅ | ✅ | Same signature |
| `MemoryPool::stats()` | ✅ | ✅ | Same return type |
| `MemoryPool::hit_count()` | ✅ | ✅ | Same signature |
| `MemoryPool::allocation_count()` | ✅ | ✅ | Same signature |
| `MemoryPool::hit_rate()` | ✅ | ✅ | Same signature |
| `MemoryPool::clear()` | ✅ | ✅ | Same signature |
| `BlockHandle` | ✅ | ✅ | Same API |
| `MutableBlockHandle` | ✅ | ✅ | Same API |
| `BlockKey` | ✅ | ✅ | Same API |
| `MemoryPoolConfig` | ✅ | ✅ | Same fields |
| `MemoryPoolStats` | ✅ | ✅ | Same fields |
| `MemoryPoolError` | ✅ | ✅ | Same variants |

## Files to Modify Summary

| File | Change Type |
|------|-------------|
| `crates/vfs/Cargo.toml` | ✅ Add dependencies |
| `crates/vfs/src/memory_pool_v2.rs` | ✅ New file |
| `crates/vfs/src/lib.rs` | 🔲 Update re-exports |
| `crates/vfs/src/fuse.rs` | 🔲 Update import |
| `crates/vfs/src/fuse_writable.rs` | 🔲 Update import |
| `crates/vfs/src/write/dirty.rs` | 🔲 Update imports |
| `crates/vfs/src/write/stats.rs` | 🔲 Update import |
| `crates/vfs/src/options.rs` | 🔲 Update import |
| `crates/vfs/tests/read_cache_integration.rs` | 🔲 Update import (if needed) |

## Expected Results

- Reduce futex syscalls by ~80% on hot path
- Multiple threads can read/write different files concurrently
- LRU eviction remains serialized (acceptable - it's infrequent)
- No API changes required for consumers
