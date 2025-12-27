# VFS Write Support Design Summary

**Full doc:** `design/vfs-writes.md`  
**Status:** ✅ IMPLEMENTED in `crates/vfs/src/write/`

## Purpose
Copy-on-write (COW) support for the VFS, enabling file modifications with disk-based caching and diff manifest generation.

## Architecture

```
Layer 3: WritableVfs (fuser::Filesystem)
         - write() → COW to dirty layer
         - fsync() → flush to disk cache
         - release() → finalize file state

Layer 2: Write Layer
         - DirtyFileManager: COW file tracking
         - MaterializedCache: Disk persistence

Layer 1: Read Layer (existing)
         - MemoryPool, INodeManager, FileStore
```

## Key Types

### WriteCache Trait
```rust
#[async_trait]
pub trait WriteCache: Send + Sync {
    async fn write_file(&self, rel_path: &str, data: &[u8]) -> Result<(), WriteCacheError>;
    async fn read_file(&self, rel_path: &str) -> Result<Option<Vec<u8>>, WriteCacheError>;
    async fn delete_file(&self, rel_path: &str) -> Result<(), WriteCacheError>;
    fn is_deleted(&self, rel_path: &str) -> bool;
    async fn list_files(&self) -> Result<Vec<String>, WriteCacheError>;
    fn cache_dir(&self) -> &Path;
}
```

Implementations:
- `MaterializedCache`: Disk-based with real file paths
- `MemoryWriteCache`: In-memory for testing

### DirtyFile
```rust
enum DirtyState { Modified, New, Deleted }

struct DirtyFile {
    inode_id: INodeId,
    rel_path: String,
    data: Vec<u8>,
    original_hash: Option<String>,
    state: DirtyState,
    mtime: SystemTime,
    executable: bool,
}
```

### DirtyContent (for chunked files)
```rust
enum DirtyContent {
    Small { data: Vec<u8> },
    Chunked {
        original_chunks: Vec<String>,
        dirty_chunks: HashSet<u32>,
        loaded_chunks: HashMap<u32, Vec<u8>>,
        total_size: u64,
        chunk_size: u64,
    },
}
```

### DirtyFileManager
```rust
impl DirtyFileManager {
    fn is_dirty(&self, inode_id: INodeId) -> bool;
    async fn cow_copy(&self, inode_id: INodeId) -> Result<(), VfsError>;
    async fn write(&self, inode_id: INodeId, offset: u64, data: &[u8]) -> Result<usize, VfsError>;
    fn create_file(&self, inode_id: INodeId, rel_path: String) -> Result<(), VfsError>;
    async fn delete_file(&self, inode_id: INodeId) -> Result<(), VfsError>;
    fn get_dirty_entries(&self) -> Vec<DirtyEntry>;
}
```

## MaterializedCache Disk Layout

```
cache_dir/
├── .deleted/                    # Tombstones for deleted files
│   └── path/to/file
├── .meta/                       # Metadata for chunked files
│   └── path/to/large_video.mp4.json
└── path/to/
    ├── small_file.txt           # Full content
    ├── large_video.mp4.part4    # Only dirty chunk 4
    └── large_video.mp4.part7    # Only dirty chunk 7
```

For chunked files, only dirty chunks are stored (e.g., 512MB for 2 modified chunks of a 2GB file).

## Diff Manifest Export

```rust
pub trait DiffManifestExporter {
    async fn export_diff_manifest(
        &self,
        parent_manifest: &Manifest,
        parent_encoded: &str,
    ) -> Result<Manifest, VfsError>;
    
    fn clear_dirty(&self) -> Result<(), VfsError>;
    fn dirty_summary(&self) -> DirtySummary;
}

struct DirtySummary {
    new_count: usize,
    modified_count: usize,
    deleted_count: usize,
    new_dir_count: usize,
    deleted_dir_count: usize,
}
```

## COW Flow

1. `write()` called on file
2. If not dirty: `cow_copy()` fetches original content from S3
3. Write to in-memory `DirtyFile`
4. `flush_to_disk()` persists to `MaterializedCache`
5. On export: generate diff manifest with changes

## When to Read Full Doc
- Implementing COW operations
- Understanding chunked file handling
- Disk cache layout details
- Diff manifest generation
