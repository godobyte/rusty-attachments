# Implementation Plan: VFS Write Performance Improvements

## Overview

This document outlines the implementation plan for two high-impact optimizations identified in the DashMap analysis:

1. **Sync write path for dirty files** (10-15% expected improvement)
2. **Zero-copy writes** (5-8% expected improvement)

---

## Optimization 1: Synchronous Write Path for Dirty Files

### Problem Statement

Currently, every write operation goes through `AsyncExecutor::block_on()`:

```rust
// fuse_writable.rs - current implementation
fn write(&mut self, ..., data: &[u8], reply: ReplyWrite) {
    let dm: Arc<DirtyFileManager> = self.dirty_manager.clone();
    let data_vec: Vec<u8> = data.to_vec();  // Copy #1
    let offset_u64: u64 = offset as u64;

    let exec_result = self.executor.block_on(async move { 
        dm.write(ino, offset_u64, &data_vec).await  // Async bridge
    });
    // ...
}
```

This causes ~22% CPU overhead in futex operations for thread coordination, even though the actual write operations for already-dirty files are entirely synchronous (DashMap lookups and modifications).

### Analysis of Write Path

The `DirtyFileManager::write()` method does the following:

1. `cow_copy(inode_id)` - Only async if file needs COW (fetches from S3)
2. `ensure_chunk_in_pool(inode_id, chunk_idx)` - Only async if chunk not in pool
3. `pool.modify_dirty_in_place()` - **Synchronous** (DashMap operation)
4. Metadata updates - **Synchronous** (RwLock on HashMap)

For files that are **already dirty** with **chunks already in pool**, the entire path is synchronous.

### Implementation Plan

#### Step 1: Add `write_sync()` method to DirtyFileManager

File: `crates/vfs/src/write/dirty.rs`

```rust
/// Write to an already-dirty file synchronously.
///
/// This is a fast path for files that are already dirty and have their
/// chunks loaded in the pool. Returns `None` if async path is needed.
///
/// # Arguments
/// * `inode_id` - Inode ID of file
/// * `offset` - Byte offset to write at
/// * `data` - Data to write
///
/// # Returns
/// `Some(bytes_written)` if sync write succeeded, `None` if async path needed.
pub fn write_sync(
    &self,
    inode_id: INodeId,
    offset: u64,
    data: &[u8],
) -> Option<Result<usize, VfsError>> {
    // Fast path: check if file is dirty
    if !self.is_dirty(inode_id) {
        return None;  // Need async COW
    }

    // Get metadata
    let (chunk_size, chunk_count, file_size): (u64, u32, u64) = {
        let guard = self.dirty_metadata.read().unwrap();
        let meta = guard.get(&inode_id)?;
        (meta.chunk_size(), meta.chunk_count(), meta.size())
    };

    let write_end: u64 = offset + data.len() as u64;

    // Small file optimization: single chunk
    if chunk_count <= 1 && write_end <= chunk_size {
        return Some(self.write_single_chunk_sync(inode_id, offset, data));
    }

    // Multi-chunk: check if all chunks are in pool
    let start_chunk: u32 = (offset / chunk_size) as u32;
    let end_chunk: u32 = if data.is_empty() {
        start_chunk
    } else {
        ((write_end - 1) / chunk_size) as u32
    };

    // Check all required chunks are in pool
    for chunk_idx in start_chunk..=end_chunk {
        if !self.pool.has_dirty(inode_id, chunk_idx) {
            return None;  // Need async to load chunk
        }
    }

    // All chunks in pool - do sync write
    Some(self.write_multi_chunk_sync(inode_id, offset, data, chunk_size))
}

/// Synchronous single-chunk write (no async, no Vec clone).
fn write_single_chunk_sync(
    &self,
    inode_id: INodeId,
    offset: u64,
    data: &[u8],
) -> Result<usize, VfsError> {
    // Check chunk is in pool
    if !self.pool.has_dirty(inode_id, 0) {
        return Err(VfsError::MemoryPoolError("Chunk not in pool".to_string()));
    }

    let write_end: u64 = offset + data.len() as u64;
    let offset_usize: usize = offset as usize;
    let end: usize = offset_usize + data.len();

    // Modify in place - pass slice directly, no Vec clone
    self.pool
        .modify_dirty_in_place_with_slice(inode_id, 0, offset_usize, data)
        .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

    // Update metadata
    {
        let mut guard = self.dirty_metadata.write().unwrap();
        if let Some(meta) = guard.get_mut(&inode_id) {
            meta.mark_chunk_dirty(0);
            if write_end > meta.size() {
                meta.set_size(write_end);
            }
            meta.touch();
        }
    }

    Ok(data.len())
}

/// Synchronous multi-chunk write.
fn write_multi_chunk_sync(
    &self,
    inode_id: INodeId,
    offset: u64,
    data: &[u8],
    chunk_size: u64,
) -> Result<usize, VfsError> {
    let write_end: u64 = offset + data.len() as u64;
    let start_chunk: u32 = (offset / chunk_size) as u32;
    let end_chunk: u32 = ((write_end - 1) / chunk_size) as u32;

    let mut data_offset: usize = 0;

    for chunk_idx in start_chunk..=end_chunk {
        let chunk_start_offset: u64 = chunk_idx as u64 * chunk_size;
        let write_start_in_chunk: usize = if chunk_idx == start_chunk {
            (offset - chunk_start_offset) as usize
        } else {
            0
        };
        let write_end_in_chunk: usize = if chunk_idx == end_chunk {
            (write_end - chunk_start_offset) as usize
        } else {
            chunk_size as usize
        };
        let write_len: usize = write_end_in_chunk - write_start_in_chunk;

        // Get slice of data for this chunk
        let chunk_data: &[u8] = &data[data_offset..data_offset + write_len];

        // Modify chunk in place with slice
        self.pool
            .modify_dirty_in_place_with_slice(inode_id, chunk_idx, write_start_in_chunk, chunk_data)
            .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

        // Mark chunk as dirty
        {
            let mut guard = self.dirty_metadata.write().unwrap();
            if let Some(meta) = guard.get_mut(&inode_id) {
                meta.mark_chunk_dirty(chunk_idx);
            }
        }

        data_offset += write_len;
    }

    // Update file size
    {
        let mut guard = self.dirty_metadata.write().unwrap();
        if let Some(meta) = guard.get_mut(&inode_id) {
            if write_end > meta.size() {
                meta.set_size(write_end);
            }
            meta.touch();
        }
    }

    Ok(data.len())
}
```

#### Step 2: Add `modify_dirty_in_place_with_slice()` to MemoryPool

File: `crates/vfs/src/memory_pool_v2.rs`

```rust
/// Modify dirty block data in place using a slice (zero-copy).
///
/// This is an optimized version that takes a slice directly instead of
/// a closure, avoiding the need to clone data into a Vec.
///
/// # Arguments
/// * `inode_id` - Inode ID
/// * `chunk_index` - Chunk index within the file
/// * `offset` - Byte offset within the chunk to start writing
/// * `data` - Data slice to write
///
/// # Returns
/// Ok(()) on success, error if block not found or not dirty.
pub fn modify_dirty_in_place_with_slice(
    &self,
    inode_id: INodeId,
    chunk_index: u32,
    offset: usize,
    data: &[u8],
) -> Result<(), MemoryPoolError> {
    let key: BlockKey = match self.key_index.get(&(inode_id, chunk_index)) {
        Some(entry) => entry.value().clone(),
        None => return Err(MemoryPoolError::BlockNotFound),
    };

    let block_ref = self.blocks.get(&key)
        .ok_or(MemoryPoolError::BlockNotFound)?;
    
    let block: &Arc<RwLock<Block>> = block_ref.value();
    let mut block_guard = block.write();
    
    if !block_guard.is_dirty {
        return Err(MemoryPoolError::NotDirty);
    }

    let end: usize = offset + data.len();
    
    // Extend if needed
    if end > block_guard.data.len() {
        block_guard.data.resize(end, 0);
    }
    
    // Direct copy from slice - no intermediate Vec
    block_guard.data[offset..end].copy_from_slice(data);
    block_guard.needs_flush = true;

    Ok(())
}
```

#### Step 3: Update FUSE write handler

File: `crates/vfs/src/fuse_writable.rs`

```rust
fn write(
    &mut self,
    _req: &Request,
    ino: u64,
    _fh: u64,
    offset: i64,
    data: &[u8],
    _write_flags: u32,
    _flags: i32,
    _lock: Option<u64>,
    reply: ReplyWrite,
) {
    let offset_u64: u64 = offset as u64;

    // Fast path: try synchronous write for already-dirty files
    if let Some(result) = self.dirty_manager.write_sync(ino, offset_u64, data) {
        match result {
            Ok(written) => {
                reply.written(written as u32);
                return;
            }
            Err(e) => {
                tracing::error!("Sync write failed for inode {}: {}", ino, e);
                reply.error(libc::EIO);
                return;
            }
        }
    }

    // Slow path: need async (COW or chunk loading)
    let dm: Arc<DirtyFileManager> = self.dirty_manager.clone();
    let data_vec: Vec<u8> = data.to_vec();  // Only clone when async needed

    let exec_result = self.executor.block_on(async move { 
        dm.write(ino, offset_u64, &data_vec).await 
    });
    
    match exec_result {
        Ok(Ok(written)) => reply.written(written as u32),
        Ok(Err(e)) => {
            tracing::error!("Write failed for inode {}: {}", ino, e);
            reply.error(libc::EIO);
        }
        Err(e) => {
            tracing::error!("Executor error during write for inode {}: {}", ino, e);
            reply.error(libc::EIO);
        }
    }
}
```

---

## Optimization 2: Zero-Copy Writes

### Problem Statement

The current write path clones data multiple times:

```rust
// fuse_writable.rs
let data_vec: Vec<u8> = data.to_vec();  // Copy #1

// dirty.rs write_single_chunk()
let data_to_write: Vec<u8> = data.to_vec();  // Copy #2
file_data[offset..end].copy_from_slice(&data_to_write);  // Copy #3
```

This causes 7.77% CPU overhead in `memmove` operations.

### Implementation Plan

This is already addressed in Optimization 1 by:

1. **Sync path**: Passes `&[u8]` slice directly to `modify_dirty_in_place_with_slice()`
2. **Async path**: Only clones when async is actually needed (COW or chunk loading)

#### Additional Change: Update existing async write methods

File: `crates/vfs/src/write/dirty.rs`

Update `write_single_chunk()` to avoid the intermediate Vec:

```rust
async fn write_single_chunk(
    &self,
    inode_id: INodeId,
    offset: u64,
    data: &[u8],
) -> Result<usize, VfsError> {
    // Ensure chunk 0 is loaded
    self.ensure_chunk_in_pool(inode_id, 0).await?;

    let write_end: u64 = offset + data.len() as u64;
    let offset_usize: usize = offset as usize;
    let end: usize = offset_usize + data.len();

    // Use the new slice-based method - no Vec clone needed
    self.pool
        .modify_dirty_in_place_with_slice(inode_id, 0, offset_usize, data)
        .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

    // Update metadata
    {
        let mut guard = self.dirty_metadata.write().unwrap();
        if let Some(meta) = guard.get_mut(&inode_id) {
            meta.mark_chunk_dirty(0);
            if write_end > meta.size() {
                meta.set_size(write_end);
            }
            meta.touch();
        }
    }

    Ok(data.len())
}
```

Similarly update `write_multi_chunk()`:

```rust
async fn write_multi_chunk(
    &self,
    inode_id: INodeId,
    offset: u64,
    data: &[u8],
) -> Result<usize, VfsError> {
    // ... (chunk calculation same as before)

    for chunk_idx in start_chunk..=end_chunk {
        self.ensure_chunk_in_pool(inode_id, chunk_idx).await?;

        // ... (offset calculation same as before)

        // Use slice directly - no Vec clone
        let chunk_data: &[u8] = &data[data_offset..data_offset + write_len];

        self.pool
            .modify_dirty_in_place_with_slice(inode_id, chunk_idx, write_start_in_chunk, chunk_data)
            .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

        // ... (metadata update same as before)
    }

    // ... (file size update same as before)
}
```

---

## Implementation Order

### Phase 1: Zero-Copy Foundation (Low Risk)

1. Add `modify_dirty_in_place_with_slice()` to `memory_pool_v2.rs`
2. Update `write_single_chunk()` to use slice method
3. Update `write_multi_chunk()` to use slice method
4. Run tests: `cargo test -p rusty-attachments-vfs`

### Phase 2: Sync Write Path (Medium Risk)

1. Add `write_sync()` to `DirtyFileManager`
2. Add `write_single_chunk_sync()` helper
3. Add `write_multi_chunk_sync()` helper
4. Update FUSE `write()` handler to try sync path first
5. Run tests: `cargo test -p rusty-attachments-vfs`

### Phase 3: Validation

1. Build with debug symbols: `RUSTFLAGS="-C debuginfo=2" cargo build --release --features fuse -p rusty-attachments-vfs --examples`
2. Run perf benchmark with same workload
3. Compare futex overhead (target: <20% total)
4. Compare memmove overhead (target: <3%)

---

## Files to Modify

| File | Changes |
|------|---------|
| `crates/vfs/src/memory_pool_v2.rs` | Add `modify_dirty_in_place_with_slice()` |
| `crates/vfs/src/write/dirty.rs` | Add `write_sync()`, update async write methods |
| `crates/vfs/src/fuse_writable.rs` | Update `write()` to try sync path first |

---

## Expected Results

| Metric | Before | After | Improvement |
|--------|--------|-------|-------------|
| vfs-io-worker futex | 17.15% | ~8% | -9% |
| vfs-async-execu futex | 6.82% | ~3% | -4% |
| memmove | 7.77% | ~2% | -5% |
| **Total CPU reduction** | - | - | **~18%** |

---

## Rollback Plan

If issues are found:
1. The sync path returns `None` to fall back to async - safe by design
2. The slice method is additive - original closure method still works
3. Can revert FUSE handler to always use async path

---

## Testing Checklist

- [ ] Unit tests pass: `cargo test -p rusty-attachments-vfs`
- [ ] Integration tests pass: `cargo test -p rusty-attachments-vfs --test '*'`
- [ ] Manual test: mount VFS, write files, verify content
- [ ] Perf test: compare before/after with same workload
- [ ] Stress test: concurrent writes from multiple processes
