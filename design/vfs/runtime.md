# VFS Runtime Architecture

## Overview

The VFS uses a **unified memory pool** (`MemoryPool`) that handles both:
1. **Immutable files** (from manifest) - identified by content hash
2. **Mutable files** (new/modified) - identified by inode ID

## Immutable Files (Manifest Files)

**Read Flow:**
1. `WritableVfs::read()` checks if file is dirty first
2. If not dirty, falls back to `FileStore::retrieve()` via the read-only path
3. `DeadlineVfs::read_file_data()` → `read_single_hash()` or `read_chunked()`
4. Uses `MemoryPool::acquire()` with a fetch closure that calls S3

**Caching:**
- Blocks keyed by `BlockKey::from_hash(hash, chunk_index)`
- LRU eviction when pool is full
- Clean blocks (no `needs_flush` flag) can be evicted freely
- Re-fetched from S3 on cache miss

## Mutable Files (New/Modified)

**Write Flow:**
1. `WritableVfs::write()` → `DirtyFileManager::write()`
2. `cow_copy()` creates `DirtyFileMetadata` (sparse - no data fetched yet)
3. `ensure_chunk_in_pool()` fetches original chunk from S3 if needed
4. Data modified in pool via `pool.update_dirty()`
5. `flush_to_disk()` writes assembled file to `MaterializedCache`

**Key Data Structures:**

```
DirtyFileMetadata (in DirtyFileManager)
├── inode_id, rel_path, state (New/Modified/Deleted)
├── original_hashes (for COW invalidation)
├── dirty_chunks: HashSet<u32> (which chunks modified)
└── size, mtime, chunk_count

MemoryPool
├── blocks: HashMap<PoolBlockId, Arc<PoolBlock>>
├── key_index: HashMap<BlockKey, PoolBlockId>
│   ├── BlockKey::Hash(hash, chunk) - read-only
│   └── BlockKey::Inode(id, chunk) - dirty
└── lru_order: VecDeque (front=oldest)
```

**Disk Cache (`MaterializedCache`):**
```
cache_dir/
├── .deleted/           # Tombstones for deleted files
├── .meta/              # Chunked file metadata JSON
└── path/to/file.txt    # Full file content (assembled from chunks)
```

## COW (Copy-on-Write) Flow

1. First write to manifest file triggers `cow_copy()`
2. Creates `DirtyFileMetadata` with `state: Modified`
3. Invalidates any cached read-only blocks for original hashes
4. On read/write, `ensure_chunk_in_pool()` lazily fetches original chunks from S3
5. Modified chunks stored with `BlockKey::from_inode(inode_id, chunk_index)`
6. Dirty blocks marked `needs_flush: true` - won't be evicted until flushed

## Memory Pool Eviction Rules

1. Only evicts blocks with `ref_count == 0`
2. Prefers clean blocks (`needs_flush == false`)
3. Dirty blocks protected until `mark_flushed()` called after disk write
4. Hash-based blocks can always be re-fetched; inode blocks cannot
