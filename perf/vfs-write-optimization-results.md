# VFS Write Path Optimization Results

## Summary

Implemented sync write path and zero-copy writes as specified in `dashmap-improvements.md`. The optimizations exceeded the 18% target, achieving ~31% CPU reduction for write operations.

## Implementation

### Phase 1: Zero-Copy Foundation
- Added `modify_dirty_in_place_with_slice()` to `MemoryPool` - passes data slices directly instead of cloning into Vec
- Updated `write_single_chunk()` and `write_multi_chunk()` to use slice-based method

### Phase 2: Sync Write Path
- Added `write_sync()`, `write_single_chunk_sync()`, `write_multi_chunk_sync()` to `DirtyFileManager`
- Updated FUSE `write()` handler to try sync path first, falling back to async only when COW or chunk loading is needed

## Performance Results

### V3 → V4 Comparison

| Metric | V3 | V4 | Improvement |
|--------|----|----|-------------|
| vfs-io-worker futex | 17.15% | 0% | -17.15% |
| vfs-async-execu futex | 6.82% | 0% | -6.82% |
| mount_vfs futex | 3.51% | 0% | -3.51% |
| memmove overhead | 7.77% | 4.2% | -3.57% |
| **Total CPU reduction** | - | - | **~31%** |

### V4 Userspace Code Overhead

| Function | CPU % |
|----------|-------|
| `fuser::request::Request::dispatch` | 0.45% |
| `DashMap::_get` | 0.61% |
| `WritableVfs::write` | 0.30% |
| `DirtyFileManager::is_dirty` | 0.30% |
| `DirtyFileManager::write_sync` | 0.15% |
| `MemoryPool::modify_dirty_in_place_with_slice` | 0.08% |
| **Total VFS code** | **~2%** |

## Why This Is Optimal

The V4 profile shows that our application-level code now consumes only ~2% of CPU time. The remaining overhead is dominated by:

1. **Kernel FUSE protocol overhead (17%+)**: `_raw_spin_unlock_irqrestore` in `fuse_request_end` - this is the kernel waking up the FUSE daemon after completing each request. This is inherent to the FUSE architecture.

2. **FUSE device I/O (14%)**: `read()` syscalls to receive FUSE requests from `/dev/fuse`.

3. **Memory operations (4%)**: Unavoidable data copying between kernel and userspace.

The sync write path completely eliminates async executor overhead by:
- Bypassing `block_on()` for already-dirty files with chunks in pool
- Avoiding futex syscalls for thread coordination
- Passing data as slices directly (no intermediate Vec allocation)

## Remaining Micro-Optimizations (Diminishing Returns)

These would yield <1% total improvement:

1. **Replace SipHash with faster hash** (~0.3% → ~0.1%): Use `ahash` or `fxhash` for inode lookups
2. **Cache dirty state in file handle**: Avoid DashMap lookup on every write
3. **Batch metadata updates**: Defer `dirty_metadata` updates to fsync/release

Not recommended - the effort exceeds the benefit.

## Future Optimization: FUSE_WRITEBACK_CACHE

For further improvement beyond app-level optimizations, consider enabling `FUSE_WRITEBACK_CACHE`:

### What It Does
- Kernel buffers writes in page cache instead of sending each write to userspace immediately
- Coalesces multiple small writes into larger batches
- Writes are flushed on fsync, close, or memory pressure

### Expected Improvement
- **Small sequential writes**: 2-10x throughput improvement
- **Random small writes**: 3-5x improvement  
- **Large writes**: Minimal improvement (already efficient)

### Trade-offs
- Data not immediately visible to userspace daemon until flush
- Slightly higher memory usage (kernel page cache)
- fsync becomes more expensive (must flush all buffered data)
- Requires `FUSE_CAP_WRITEBACK_CACHE` capability negotiation in `fuser`

### Implementation
Requires changes to FUSE mount options and capability negotiation. The `fuser` crate may need configuration to enable this.

## Files Modified

- `crates/vfs/src/memory_pool_v2.rs` - Added `modify_dirty_in_place_with_slice()`, `NotDirty` error
- `crates/vfs/src/write/dirty.rs` - Added sync write methods, updated async methods to use slice API
- `crates/vfs/src/fuse_writable.rs` - Updated `write()` to try sync path first

## Trace Files

- `perf/vfs-write-perf-v3.data` - Before optimization
- `perf/vfs-write-perf-v4.data` - After optimization
- `perf/vfs-write-perf-v4-analysis.txt` - Full perf report
