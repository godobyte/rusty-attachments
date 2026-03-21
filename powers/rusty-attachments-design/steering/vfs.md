# VFS Design Summary

**Full doc:** `design/vfs.md`  
**Status:** ✅ READ-ONLY COMPLETE | ✅ WRITE SUPPORT COMPLETE in `crates/vfs/`

## Purpose
FUSE-based virtual filesystem that mounts Deadline Cloud job attachment manifests. Files appear as local files but content is fetched on-demand from S3 CAS.

## Architecture

```
Layer 3: FUSE Interface (fuser::Filesystem impl)
         Platform VFS: FSKit (macOS), ProjFS (Windows)
Layer 2: VFS Operations (lookup, read, readdir)
         AsyncExecutor (sync FUSE → async tokio bridge)
Layer 1: Primitives (INodeManager, FileStore, MemoryPool v2, DiskCache)
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

### Memory Pool (v2)
- Lock-free hot path via DashMap (concurrent reads without contention)
- LRU eviction on cold path (Mutex-protected)
- Fetch coordination via SharedFetch (prevents duplicate S3 requests)
- Mutable block support for writable VFS
- Hit/allocation counters for stats

```rust
struct MemoryPoolConfig {
    max_size: u64,    // Default: 8GB
    block_size: u64,  // Default: 256MB
}

impl MemoryPool {
    async fn acquire<F, Fut>(&self, key: &BlockKey, fetch: F) -> Result<BlockHandle, MemoryPoolError>;
    fn try_get(&self, key: &BlockKey) -> Option<BlockHandle>;
    fn acquire_mutable(&self, key: &BlockKey, data: Vec<u8>) -> Result<MutableBlockHandle, MemoryPoolError>;
    fn stats(&self) -> MemoryPoolStats;
    fn hit_count(&self) -> u64;
    fn allocation_count(&self) -> u64;
    fn hit_rate(&self) -> f64;
}
```

### AsyncExecutor
Bridges synchronous FUSE callbacks to async tokio runtime:

```rust
struct ExecutorConfig {
    worker_threads: usize,     // Default: 4
    queue_size: usize,         // Default: 256
    default_timeout: Option<Duration>,  // Default: 30s
}

impl AsyncExecutor {
    fn block_on<F, T>(&self, future: F) -> Result<T, ExecutorError>;
    fn block_on_timeout<F, T>(&self, future: F, timeout: Duration) -> Result<T, ExecutorError>;
    fn block_on_cancellable<F, T>(&self, future: F) -> Result<T, ExecutorError>;
    fn cancel_all(&self);
}
```

### Disk Cache (ReadCache)
Persistent on-disk cache for CAS content blocks:

```rust
struct ReadCacheOptions {
    cache_dir: PathBuf,
    max_size: u64,           // Default: 50GB
    write_through: bool,     // Default: true
}

impl ReadCache {
    fn get(&self, hash: &str) -> Result<Option<Vec<u8>>, DiskCacheError>;
    fn put(&self, hash: &str, data: &[u8]) -> Result<(), DiskCacheError>;
    fn contains(&self, hash: &str) -> bool;
    fn contains_with_size(&self, hash: &str, expected_size: u64) -> bool;
    fn current_size(&self) -> u64;
}
```

Scans existing cache on startup, tracks size atomically, uses temp files for atomic writes.

### VFS Options
```rust
struct VfsOptions {
    pool: MemoryPoolConfig,
    prefetch: PrefetchStrategy,  // None, OnOpen, Sequential
    kernel_cache: KernelCacheOptions,
    read_ahead: ReadAheadOptions,
    timeouts: TimeoutOptions,
    read_cache: ReadCacheConfig,     // Disk cache (enabled/disabled)
    executor: ExecutorConfig,        // Async executor settings
}
```

## Platform VFS Crates

### vfs-fskit (macOS 15.4+)
Apple FSKit-based VFS in `crates/vfs-fskit/`:
```
FSKitBridge (Swift appex) ←→ TCP + Protobuf ←→ fskit-rs ←→ WritableFsKit
```
Reuses shared VFS primitives (INodeManager, MemoryPool, DirtyFileManager).

### vfs-projfs (Windows)
Microsoft ProjFS-based VFS in `crates/vfs-projfs/`:
```
Layer 3: ProjFsVirtualizer (ProjFS callbacks)
Layer 2: VfsCallbacks (coordination & dirty state)
Layer 1: ManifestProjection (in-memory manifest tree)
Layer 0: Shared VFS primitives
```
Influenced by VFSForGit design. Includes PathRegistry, ModifiedPathsDatabase, background prefetch.

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
- Understanding memory pool v2 design (lock-free hot path)
- Adding prefetch strategies
- Stats collection implementation
- AsyncExecutor bridge pattern
- Disk cache (ReadCache) implementation
- Platform VFS (FSKit/ProjFS) integration
