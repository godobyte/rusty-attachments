# VFS Read Path Optimization Plan

Date: 2026-01-02
Based on: `perf/vfs-read-perf-analysis.md`

## Executive Summary

The read path is **memory-bound**, not CPU-bound. VFS userspace code is <0.1% of CPU time.
The dominant costs are:
- memmove (data copying): 22%
- Page faults: 20%
- Kernel page management: ~20%

**Realistic improvement target: 7-10% CPU reduction** by eliminating unnecessary copies.

## Constraint: FUSE Interface is Copy-Only

The `fuser` crate's `ReplyData::data(&[u8])` method requires a copy to kernel space via `writev()`.
This copy is unavoidable without kernel-level changes (FUSE splice support).

**We can only optimize copies BEFORE the FUSE boundary.**

## Current Data Flow Analysis

### Dirty File Read Path (Single Chunk)

Location: `crates/vfs/src/write/dirty.rs` - `read_single_chunk()`

```rust
// COPY #1: Clone entire chunk from RwLock
let pool_data: Vec<u8> = handle.data();  // calls self.data.read().clone()

// COPY #2: Byte-by-byte copy (inefficient!)
for i in start..end {
    if i < pool_data.len() {
        result.push(pool_data[i]);
    } else {
        result.push(0);
    }
}

// COPY #3: To kernel (unavoidable)
reply.data(&result);
```

**Problems:**
1. `handle.data()` clones the entire chunk even if we only need a slice
2. Byte-by-byte push is O(n) with potential reallocations
3. Two unnecessary copies before FUSE

### Dirty File Read Path (Multi-Chunk)

Location: `crates/vfs/src/write/dirty.rs` - `read()`

```rust
// COPY #1: Slice copy into result Vec
handle.with_data(|chunk_data: &[u8]| {
    result.extend_from_slice(&chunk_data[read_start..actual_end]);
});

// COPY #2: To kernel (unavoidable)
reply.data(&result);
```

**This path is already optimized** - uses `with_data()` to avoid clone.

### CAS Read Path (S3 Files)

Location: `crates/vfs/src/fuse_writable.rs` - `read()`

```rust
// COPY #1: S3 data into Vec (in store.retrieve())
let data: Vec<u8> = store.retrieve(&hash, hash_alg).await?;

// COPY #2: Slice for offset/size
reply.data(&data[off..end]);

// COPY #3: To kernel (unavoidable)
```

**Reasonably efficient** - only one extra copy for slicing.

## Optimization Tasks

### Task 1: Fix `read_single_chunk()` - HIGH PRIORITY

**File:** `crates/vfs/src/write/dirty.rs`
**Function:** `read_single_chunk()`
**Expected improvement:** ~5% CPU reduction

**Current code (lines ~570-600):**
```rust
// Bad: clones entire chunk
let pool_data: Vec<u8> = if let Some(handle) = self.pool.get_dirty(inode_id, 0) {
    handle.data()  // This clones!
} else {
    Vec::new()
};

// Bad: byte-by-byte copy
for i in start..end {
    if i < pool_data.len() {
        result.push(pool_data[i]);
    } else {
        result.push(0);
    }
}
```

**Optimized code:**
```rust
let result: Vec<u8> = if let Some(handle) = self.pool.get_dirty(inode_id, 0) {
    handle.with_data(|chunk_data: &[u8]| {
        let start: usize = offset as usize;
        let end: usize = (offset + size as u64).min(file_size) as usize;
        
        if start >= end {
            return Vec::new();
        }
        
        let mut result: Vec<u8> = Vec::with_capacity(end - start);
        
        if end <= chunk_data.len() {
            // Fast path: all data in chunk
            result.extend_from_slice(&chunk_data[start..end]);
        } else {
            // Slow path: chunk is smaller than file size (truncate extended)
            let chunk_end: usize = chunk_data.len().min(end);
            if start < chunk_end {
                result.extend_from_slice(&chunk_data[start..chunk_end]);
            }
            // Fill remaining with zeros
            let zeros_needed: usize = end - result.len() - start;
            result.resize(result.len() + zeros_needed, 0);
        }
        
        result
    })
} else {
    // New file with no content - return zeros
    vec![0u8; (end - start)]
};
```

**Key changes:**
1. Use `with_data()` instead of `data()` to avoid clone
2. Use `extend_from_slice()` instead of byte-by-byte push
3. Pre-allocate result Vec with correct capacity

### Task 2: Add Sync Read Path for Dirty Files - MEDIUM PRIORITY

**File:** `crates/vfs/src/write/dirty.rs`
**Expected improvement:** ~2% CPU reduction (reduces async overhead)

Similar to `write_sync()`, add a `read_sync()` for already-loaded chunks:

```rust
/// Synchronous read for dirty files with data already in pool.
///
/// # Arguments
/// * `inode_id` - Inode ID
/// * `offset` - Byte offset
/// * `size` - Bytes to read
///
/// # Returns
/// Some(data) if read succeeded synchronously, None if async load needed.
pub fn read_sync(&self, inode_id: INodeId, offset: u64, size: u32) -> Option<Vec<u8>> {
    // Check if file is dirty
    let (file_size, chunk_size, chunk_count): (u64, u64, u32) = {
        let guard = self.dirty_metadata.read().ok()?;
        let meta: &DirtyFileMetadata = guard.get(&inode_id)?;
        (meta.size(), meta.chunk_size(), meta.chunk_count())
    };

    if offset >= file_size {
        return Some(Vec::new());
    }

    let read_end: u64 = (offset + size as u64).min(file_size);
    
    // Single chunk case
    if chunk_count <= 1 {
        // Check if chunk 0 is in pool
        if !self.pool.has_dirty(inode_id, 0) {
            return None;  // Need async load
        }
        
        let handle = self.pool.get_dirty(inode_id, 0)?;
        return Some(handle.with_data(|data: &[u8]| {
            let start: usize = offset as usize;
            let end: usize = read_end as usize;
            // ... same logic as Task 1
        }));
    }

    // Multi-chunk: check all needed chunks are in pool
    let start_chunk: u32 = (offset / chunk_size) as u32;
    let end_chunk: u32 = ((read_end - 1) / chunk_size) as u32;
    
    for chunk_idx in start_chunk..=end_chunk {
        if !self.pool.has_dirty(inode_id, chunk_idx) {
            return None;  // Need async load
        }
    }

    // All chunks in pool - read synchronously
    // ... same logic as async read()
}
```

**Update FUSE read handler:**
```rust
fn read(&mut self, ..., reply: ReplyData) {
    if self.dirty_manager.is_dirty(ino) {
        // Try sync path first
        if let Some(data) = self.dirty_manager.read_sync(ino, offset as u64, size) {
            reply.data(&data);
            return;
        }
        
        // Fall back to async
        let exec_result = self.executor.block_on(async move { ... });
        // ...
    }
    // ...
}
```

### Task 3: Pre-allocate Result Buffers - LOW PRIORITY

**Expected improvement:** ~1-2% (reduces page faults)

For known file sizes, pre-allocate and pre-fault the result buffer:

```rust
/// Pre-allocate a buffer and touch pages to avoid faults during copy.
fn prefault_buffer(size: usize) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::with_capacity(size);
    // Touch every page to ensure it's mapped
    unsafe {
        buf.set_len(size);
        for i in (0..size).step_by(4096) {
            std::ptr::write_volatile(&mut buf[i], 0);
        }
        buf.set_len(0);
    }
    buf
}
```

This is a micro-optimization with diminishing returns.

## Implementation Order

1. **Task 1** (High priority, easy): Fix `read_single_chunk()` - 1 hour
2. **Task 2** (Medium priority): Add `read_sync()` - 2-3 hours
3. **Task 3** (Low priority): Pre-fault buffers - optional

## Testing

After implementation, re-run the perf test:

```bash
# Start VFS with perf recording
sudo perf record -g -F 999 -o perf/vfs-read-perf-v2.data -- \
    ./target/release/examples/mount_vfs job_bundles_manifest_v2.json ./vfs \
    --writable --cache-dir /tmp/vfs-cache

# Run test workload
sudo bash perf/vfs-read-test.sh

# Stop perf (Ctrl+C) and analyze
sudo perf report -i perf/vfs-read-perf-v2.data --stdio --no-children > perf/vfs-read-perf-v2-analysis.txt
```

## Expected Results

| Metric | Before | After Task 1 | After Task 2 |
|--------|--------|--------------|--------------|
| memmove | 22% | ~17% | ~15% |
| Async executor | 0.2% | 0.2% | ~0.1% |
| **Total improvement** | - | ~5% | ~7-8% |

## Why Not More?

- **Page faults (20%)**: Unavoidable - kernel allocates pages when data is written
- **FUSE copy (part of 22%)**: Unavoidable - protocol requires userspace→kernel copy
- **Kernel page management (10%)**: Unavoidable - LRU, memcg accounting

The VFS userspace code is already highly optimized. Further gains require:
- FUSE splice support (kernel-level)
- Memory-mapped I/O (architectural change)
- Huge pages (system configuration)

## Files to Modify

1. `crates/vfs/src/write/dirty.rs`
   - `read_single_chunk()` - Task 1
   - Add `read_sync()` - Task 2

2. `crates/vfs/src/fuse_writable.rs`
   - `fn read()` - Update to try sync path first (Task 2)
