# VFS Design Summary

**Full doc:** `design/vfs.md`  
**Status:** ✅ READ-ONLY COMPLETE | ✅ WRITE SUPPORT COMPLETE in `crates/vfs/`

## Purpose
FUSE-based virtual filesystem that mounts Deadline Cloud job attachment manifests. Files appear as local files but content is fetched on-demand from S3 CAS.

## Architecture

```
Layer 3: FUSE Interface (fuser::Filesystem impl)
Layer 2: VFS Operations (lookup, read, readdir)
Layer 1: Primitives (INodeManager, FileStore, MemoryPool)
```

## Key Types

### INode Types
```rust
type INodeId = u64;
const ROOT_INODE: INodeId = 1;

enum FileContent {
    SingleHash(String),      // V1 and small V2 files
    Chunked(Vec<String>),    // V2 large files (>256MB)
}

struct INodeFile { id, parent_id, name, path, size, mtime, content, hash_algorithm, executable }
struct INodeDir { id, parent_id, name, path, children: RwLock<HashMap<String, INodeId>> }
struct INodeSymlink { id, parent_id, name, path, target }
```

### FileStore Trait
```rust
#[async_trait]
pub trait FileStore: Send + Sync {
    async fn retrieve(&self, hash: &str, algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError>;
    async fn retrieve_range(&self, hash: &str, algorithm: HashAlgorithm, 
                            offset: u64, size: u64) -> Result<Vec<u8>, VfsError>;
}
```

### Memory Pool
- Fixed-size blocks (256MB, matching V2 chunk size)
- LRU eviction
- Fetch coordination (prevents duplicate fetches)
- Lock-free data access via `Arc<Vec<u8>>`

```rust
struct MemoryPoolConfig {
    max_size: u64,    // Default: 8GB
    block_size: u64,  // Default: 256MB
}

impl MemoryPool {
    async fn acquire<F, Fut>(&self, key: &BlockKey, fetch: F) -> Result<BlockHandle, Error>;
    fn try_get(&self, key: &BlockKey) -> Option<BlockHandle>;
    fn stats(&self) -> MemoryPoolStats;
}
```

### VFS Options
```rust
struct VfsOptions {
    pool: MemoryPoolConfig,
    prefetch: PrefetchStrategy,  // None, OnOpen, Sequential, Eager
    kernel_cache: KernelCacheOptions,
    read_ahead: ReadAheadOptions,
    timeouts: TimeoutOptions,
}
```

## StorageClientAdapter

Bridges `StorageClient` (storage crate) to `FileStore` (VFS crate):
```rust
use rusty_attachments_vfs::StorageClientAdapter;

let client = CrtStorageClient::new(settings).await?;
let location = S3Location::new("bucket", "root", "Data", "Manifests");
let store: Arc<dyn FileStore> = Arc::new(StorageClientAdapter::new(client, location));
```

## CLI Usage

```bash
# Basic mount with S3 backend
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
    <manifest.json> <mountpoint> --bucket my-bucket --root-prefix DeadlineCloud

# With stats dashboard
cargo run ... -- <manifest.json> <mountpoint> --stats

# Writable mode with COW
cargo run ... -- <manifest.json> <mountpoint> --writable --cache-dir /tmp/vfs-cow

# Mock file store (testing)
cargo run ... -- <manifest.json> <mountpoint> --mock
```

## Stats Dashboard

Live statistics when `--stats` is provided:
- Uptime, inode count, open files
- Memory pool: blocks, memory usage, pending fetches
- Cache: hits, allocations, hit rate
- Open file list with sizes

## Manifest Version Handling

| Feature | V1 | V2 | VFS Handling |
|---------|----|----|--------------|
| Files | hash, size, mtime | + chunkhashes | FileContent enum |
| Directories | Implicit | Explicit dirs[] | Build from paths (V1) or use dirs (V2) |
| Symlinks | Not supported | symlink_target | Skip in V1, INodeSymlink in V2 |
| Executable | Not supported | runnable | Default false for V1 |
| Chunking | Not supported | >256MB | Single hash for V1, chunk-aware for V2 |

## When to Read Full Doc
- Implementing FUSE operations
- Understanding memory pool design
- Adding prefetch strategies
- Stats collection implementation
