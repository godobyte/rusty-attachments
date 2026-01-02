# Rusty Attachments: Virtual File System (VFS) Module Design

**Status: ✅ READ-ONLY COMPLETE | ✅ WRITE SUPPORT COMPLETE**

## Implementation Status

| Component | Status | Location |
|-----------|--------|----------|
| INode Primitives | ✅ | `src/inode/{file,dir,symlink,types}.rs` |
| INodeManager | ✅ | `src/inode/manager.rs` |
| FileStore Trait | ✅ | `src/content/store.rs` |
| MemoryFileStore (test) | ✅ | `src/content/store.rs` |
| StorageClientAdapter | ✅ | `src/content/s3_adapter.rs` |
| Manifest Builder (V1/V2) | ✅ | `src/builder.rs` |
| MemoryPool v2 (DashMap, LRU, unified) | ✅ | `src/memory_pool_v2.rs` |
| AsyncExecutor (deadlock-free) | ✅ | `src/executor.rs` |
| VfsOptions | ✅ | `src/options.rs` |
| FUSE Implementation (read-only) | ✅ | `src/fuse.rs` |
| FUSE Implementation (writable) | ✅ | `src/fuse_writable.rs` |
| Error Types | ✅ | `src/error.rs` |
| Example Binary | ✅ | `examples/mount_vfs.rs` |
| Stats Dashboard | ✅ | `src/fuse.rs`, `src/write/stats.rs` |
| WriteCache Trait | ✅ | `src/diskcache/traits.rs` |
| MaterializedCache (disk) | ✅ | `src/diskcache/materialized.rs` |
| MemoryWriteCache (test) | ✅ | `src/diskcache/memory_cache.rs` |
| DirtyFileManager (COW) | ✅ | `src/write/dirty.rs` |
| DirtyDirManager | ✅ | `src/write/dirty_dir.rs` |
| DiffManifestExporter | ✅ | `src/write/export.rs` |
| ReadCache (disk cache) | ✅ | `src/diskcache/read_cache.rs` |
| Hash Verification | ❌ | Future work |

---

## Integration with Storage Crates

The VFS reuses existing storage infrastructure rather than implementing S3 access directly.

### Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         VFS Crate                                            │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  FileStore trait                                                     │    │
│  │    - retrieve(hash, algorithm) -> Vec<u8>                           │    │
│  │    - retrieve_range(hash, algorithm, offset, size) -> Vec<u8>       │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  StorageClientAdapter<C: StorageClient>  (src/content/s3_adapter.rs)│    │
│  │    - Bridges StorageClient → FileStore                              │    │
│  │    - Now part of VFS crate for reuse                                │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ wraps
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Storage Crates                                       │
│  ┌────────────────────────────┐  ┌────────────────────────────────────┐    │
│  │  storage crate             │  │  storage-crt crate                 │    │
│  │  - StorageClient trait     │  │  - CrtStorageClient                │    │
│  │  - S3Location              │  │    (AWS SDK implementation)        │    │
│  │  - StorageSettings         │  │                                    │    │
│  │  - DownloadOrchestrator    │  │                                    │    │
│  └────────────────────────────┘  └────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Reused Components

| Component | Crate | Purpose in VFS |
|-----------|-------|----------------|
| `StorageClient` trait | `storage` | S3 operations interface |
| `CrtStorageClient` | `storage-crt` | AWS SDK S3 implementation |
| `S3Location` | `storage` | CAS key generation (`cas_key()`) |
| `StorageSettings` | `storage` | Region, credentials config |
| `HashAlgorithm` | `model` | Hash algorithm for key extension |

### StorageClientAdapter Pattern

The adapter bridges the `StorageClient` trait (storage crate) to the `FileStore` trait (VFS crate).
**Now located in `src/content/s3_adapter.rs` for reuse across applications:**

```rust
use rusty_attachments_vfs::{StorageClientAdapter, FileStore};
use rusty_attachments_storage::{S3Location, StorageSettings};
use rusty_attachments_storage_crt::CrtStorageClient;

// Create storage client
let settings = StorageSettings::default();
let client = CrtStorageClient::new(settings).await?;
let location = S3Location::new("bucket", "root", "Data", "Manifests");

// Wrap in adapter - now implements FileStore
let store: Arc<dyn FileStore> = Arc::new(StorageClientAdapter::new(client, location));
```

### Why This Design?

1. **Pyramid Architecture**: VFS builds on storage primitives rather than duplicating S3 code
2. **Testability**: Can inject mock `StorageClient` for testing without S3
3. **Backend Flexibility**: Same VFS works with CRT, WASM, or future backends
4. **Single Source of Truth**: S3 key format (`cas_key()`) defined once in storage crate
5. **Reusability**: `StorageClientAdapter` is now part of the VFS crate public API

### Usage

```bash
# Basic mount with S3 backend (uses default AWS credentials)
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
    <manifest.json> <mountpoint> \
    --bucket adeadlineja --root-prefix DeadlineCloud

# Mount with live stats dashboard
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
    <manifest.json> <mountpoint> --stats \
    --bucket adeadlineja --root-prefix DeadlineCloud

# Mount with mock file store (for testing without S3)
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
    <manifest.json> <mountpoint> --mock

# Writable mode with COW support
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
    <manifest.json> <mountpoint> --writable --cache-dir /tmp/vfs-cow

# Full example with all options
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
    /tmp/manifest.json ~/vfs --stats --writable \
    --bucket my-bucket --root-prefix MyPrefix --region us-east-1
```

### CLI Options

| Option | Default | Description |
|--------|---------|-------------|
| `<manifest.json>` | (required) | Path to manifest JSON file |
| `<mountpoint>` | (required) | Directory to mount VFS (created if needed) |
| `--stats` | off | Show live statistics dashboard |
| `--mock` | off | Use mock file store instead of S3 |
| `--writable` | off | Mount in read-write mode with COW support |
| `--cache-dir` | `/tmp/vfs-cache` | Directory for COW cache (writable mode) |
| `--bucket` | `adeadlineja` | S3 bucket name |
| `--root-prefix` | `DeadlineCloud` | S3 root prefix |
| `--region` | `us-west-2` | AWS region |

### Stats Dashboard

When `--stats` is provided, a background thread displays a live dashboard:

```
╔══════════════════════════════════════════════════════════════════╗
║                    VFS Statistics Dashboard                       ║
╠══════════════════════════════════════════════════════════════════╣
║ Uptime:    45s                                                   ║
╠══════════════════════════════════════════════════════════════════╣
║ FILESYSTEM                                                        ║
║   Inodes:        649                                              ║
║   Open files:      3                                              ║
╠══════════════════════════════════════════════════════════════════╣
║ MEMORY POOL                                                       ║
║   Blocks:      5 total,      2 in use                            ║
║   Memory:     1.25 GB /     8.00 GB (15.6%)                      ║
║   Pending fetches:    1                                          ║
╠══════════════════════════════════════════════════════════════════╣
║ CACHE                                                             ║
║   Hits:         42  Allocations:         12                       ║
║   Hit rate:  77.78%                                              ║
╠══════════════════════════════════════════════════════════════════╣
║ OPEN FILES                                                        ║
║    1. scene/textures/diffuse.png                         2.50 MB ║
║    2. models/character.fbx                              15.30 MB ║
║    3. audio/background.wav                               4.20 MB ║
╚══════════════════════════════════════════════════════════════════╝
```

#### Stats API

The stats can also be accessed programmatically:

```rust
// Get a thread-safe stats collector before mounting
let stats_collector: VfsStatsCollector = vfs.stats_collector();

// Collect stats from any thread
let stats: VfsStats = stats_collector.collect();
println!("Open files: {}", stats.open_files);
println!("Cache hit rate: {:.2}%", stats.cache_hit_rate);
println!("Pool memory: {} / {}", stats.pool_stats.current_size, stats.pool_stats.max_size);
```

#### VfsStats Fields

| Field | Type | Description |
|-------|------|-------------|
| `inode_count` | `usize` | Total inodes in filesystem |
| `open_files` | `usize` | Currently open file handles |
| `open_file_list` | `Vec<OpenFileInfo>` | Details of each open file |
| `pool_stats` | `MemoryPoolStats` | Memory pool statistics |
| `cache_hits` | `u64` | Total cache hits |
| `cache_allocations` | `u64` | Total cache misses (fetches) |
| `cache_hit_rate` | `f64` | Hit rate percentage |
| `uptime_secs` | `u64` | Seconds since VFS creation |

---

## Background Summary

This module ports the C++ Fus3 project to Rust, creating a FUSE-based virtual filesystem that mounts Deadline Cloud job attachment manifests. Files appear as local files but content is fetched on-demand from S3 CAS (Content-Addressable Storage).

### Core Concepts

| Concept | Description |
|---------|-------------|
| **Manifest** | JSON file listing files with paths, sizes, mtimes, and content hashes |
| **CAS** | Content-Addressable Storage - files stored by hash (e.g., `Data/{hash}.xxh128`) |
| **INode** | In-memory representation of a file/directory/symlink with metadata |
| **INodeManager** | Allocates inode IDs and maintains ID→INode and path→INode maps |
| **FileStore** | Trait for content retrieval (S3, disk cache, or memory) |

### Key Operations Flow

```
Mount:
  1. Parse manifest JSON
  2. Build INode tree (INodeManager::AddPathINodes for each entry)
  3. Store content hash in each file INode's FileStorageInfo
  4. Start FUSE session

File Access (e.g., `cat /mnt/vfs/scene.blend`):
  1. lookup("/", "scene.blend") → find INode by name in parent's children
  2. getattr(ino) → return FileAttr from INode
  3. open(ino) → create OpenFileInfo, return file handle
  4. read(ino, fh, offset, size):
     a. Get content hash from INode
     b. Build S3 key: "{CASPrefix}/{hash}.{hashAlg}"
     c. Fetch from cache or S3
     d. Verify xxhash integrity
     e. Return data slice
  5. release(fh) → cleanup OpenFileInfo
```

### S3 CAS Key Format

```
{CASPrefix}/{hash}.{hashAlg}
Example: Data/b9957a169638056faef9f7c45721db91.xxh128
```

### Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `ROOT_INODE` | 1 | Root directory inode ID |
| `ATTR_TIMEOUT` | 86400s | Kernel cache TTL (24 hours) |
| `DEFAULT_FILE_PERMS` | 0o644 | rw-r--r-- |
| `DEFAULT_DIR_PERMS` | 0o755 | rwxr-xr-x |

---

## Fuser Library Summary

The `fuser` crate provides the Rust FUSE interface. Key points:

### FileAttr Structure
```rust
pub struct FileAttr {
    pub ino: u64,           // Inode number
    pub size: u64,          // Size in bytes
    pub blocks: u64,        // Size in blocks
    pub atime: SystemTime,  // Access time
    pub mtime: SystemTime,  // Modification time
    pub ctime: SystemTime,  // Change time
    pub kind: FileType,     // Directory, RegularFile, Symlink
    pub perm: u16,          // Permissions (0o755, etc.)
    pub nlink: u32,         // Hard link count
    pub uid: u32, pub gid: u32,
    pub blksize: u32,       // Block size (512)
}
```

### Required Filesystem Trait Methods (Read-Only)

| Method | Purpose | Reply |
|--------|---------|-------|
| `lookup(parent, name)` | Resolve path component | `ReplyEntry` with FileAttr |
| `getattr(ino)` | Get file attributes | `ReplyAttr` with FileAttr |
| `readdir(ino, offset)` | List directory | `ReplyDirectory` with entries |
| `open(ino, flags)` | Open file | `ReplyOpen` with file handle |
| `read(ino, fh, offset, size)` | Read file data | `ReplyData` with bytes |
| `release(ino, fh)` | Close file | `ReplyEmpty` |
| `readlink(ino)` | Read symlink target | `ReplyData` with path |

### Mount API
```rust
// Blocking mount (returns on unmount)
fuser::mount2(filesystem, "/mnt/vfs", &[MountOption::RO])?;

// Background mount (returns immediately)
let session = fuser::spawn_mount2(filesystem, "/mnt/vfs", &[MountOption::RO])?;
// session.join() or drop to unmount
```

---

## Goals

1. **Mount manifests as filesystems** - Read-only mounted directories from V1/V2 manifests
2. **Lazy content retrieval** - Fetch from S3 CAS on-demand when files are read
3. **Pyramid architecture** - Single-responsibility primitives → composition → FUSE interface
4. **Cross-platform** - Linux and macOS (FUSE), potential Windows (WinFSP)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Layer 3: FUSE Interface                                   │
│                    (fuser::Filesystem impl)                                  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Layer 2: VFS Operations                                   │
│                    (lookup, read, readdir)                                   │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Layer 1: Primitives                                       │
│         INodeManager          │           FileStore                          │
│    (INode, INodeDir, etc.)    │    (trait for content retrieval)            │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Existing Crates (REUSED)                                  │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  model crate                                                         │    │
│  │    - Manifest (V1/V2 parsing)                                       │    │
│  │    - HashAlgorithm                                                  │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  storage crate                                                       │    │
│  │    - StorageClient trait (S3 operations interface)                  │    │
│  │    - S3Location (cas_key() for CAS key generation)                  │    │
│  │    - StorageSettings (region, credentials)                          │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  storage-crt crate                                                   │    │
│  │    - CrtStorageClient (AWS SDK implementation of StorageClient)     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Project Structure

```
crates/vfs/src/
├── lib.rs                # Public API exports
├── error.rs              # VfsError enum
├── options.rs            # VfsOptions, PrefetchStrategy, ReadCacheConfig, ExecutorConfig
├── memory_pool.rs        # Legacy memory pool (deprecated)
├── memory_pool_v2.rs     # Layer 1: DashMap-based unified memory pool (current)
├── executor.rs           # Layer 1: Dedicated async executor (deadlock-free)
├── builder.rs            # Layer 2: Build INode tree from Manifest
│
├── inode/                # Layer 1: INode primitives
│   ├── mod.rs
│   ├── types.rs          # INodeId, INodeType, INode trait
│   ├── file.rs           # INodeFile (hash, size, mtime, FileContent)
│   ├── dir.rs            # INodeDir (children map)
│   ├── symlink.rs        # INodeSymlink (target path)
│   └── manager.rs        # INodeManager (allocation, lookup)
│
├── content/              # Layer 1: Content retrieval
│   ├── mod.rs
│   ├── store.rs          # FileStore trait, MemoryFileStore
│   └── s3_adapter.rs     # StorageClientAdapter<C: StorageClient>
│
├── diskcache/            # Layer 1: Disk cache implementations
│   ├── mod.rs
│   ├── error.rs          # DiskCacheError
│   ├── traits.rs         # WriteCache trait
│   ├── materialized.rs   # MaterializedCache (dirty files), ChunkedFileMeta
│   ├── memory_cache.rs   # MemoryWriteCache (test impl)
│   └── read_cache.rs     # ReadCache (immutable CAS content)
│
├── write/                # Layer 2: Write support (COW)
│   ├── mod.rs
│   ├── dirty.rs          # DirtyFileMetadata, DirtyFileManager (content in MemoryPool)
│   ├── dirty_dir.rs      # DirtyDir, DirtyDirManager
│   ├── export.rs         # DiffManifestExporter trait, DirtySummary
│   └── stats.rs          # WritableVfsStats, WritableVfsStatsCollector
│
├── fuse.rs               # Layer 3: Read-only fuser::Filesystem
└── fuse_writable.rs      # Layer 3: Writable fuser::Filesystem (COW)

examples/
└── mount_vfs.rs          # CLI example with S3 and mock backends
```

---

## Core Types

### INode Types

```rust
pub type INodeId = u64;
pub const ROOT_INODE: INodeId = 1;pub enum INodeType { File, Directory, Symlink }

/// File content source - handles both V1 (single hash) and V2 (chunked) files.
pub enum FileContent {
    /// Single hash for entire file (V1 and small V2 files).
    SingleHash(String),
    /// Chunk hashes for large V2 files (>256MB).
    Chunked(Vec<String>),
}

pub struct INodeFile {
    id: INodeId,
    parent_id: INodeId,
    name: String,
    path: String,
    size: u64,
    mtime: SystemTime,
    content: FileContent,      // Single hash or chunk hashes
    hash_algorithm: HashAlgorithm,
    executable: bool,          // V2 only (runnable flag)
}

pub struct INodeDir {
    id: INodeId,
    parent_id: INodeId,
    name: String,
    path: String,
    children: RwLock<HashMap<String, INodeId>>,  // name → child inode
}

pub struct INodeSymlink {
    id: INodeId,
    parent_id: INodeId,
    name: String,
    path: String,
    target: String,            // Symlink target path (V2 only)
}
```

### Manifest Version Compatibility

The VFS must handle both V1 (v2023-03-03) and V2 (v2025-12-04-beta) manifests gracefully.

| Feature | V1 (v2023-03-03) | V2 (v2025-12-04-beta) | VFS Handling |
|---------|------------------|----------------------|--------------|
| **Files** | `hash`, `size`, `mtime` | Same + optional `chunkhashes` | `FileContent` enum |
| **Directories** | Implicit (from paths) | Explicit `dirs` array | Build from paths (V1) or use dirs (V2) |
| **Symlinks** | Not supported | `symlink_target` field | Skip in V1, create `INodeSymlink` in V2 |
| **Executable** | Not supported | `runnable` field | Default `false` for V1 |
| **Chunking** | Not supported | Files >256MB use `chunkhashes` | Single hash for V1, chunk-aware reads for V2 |
| **Deleted entries** | Not supported | `deleted` flag (diff manifests) | Filter out deleted entries |

#### Building INodes from Manifest

```rust
impl INodeManager {
    /// Build INode tree from V1 manifest.
    pub fn from_v1_manifest(&mut self, manifest: &v2023_03_03::AssetManifest) {
        for entry in &manifest.paths {
            // V1: All entries are files with single hash
            // No symlinks, no executable bit, no chunking
            self.add_file_from_v1(entry);
        }
    }

    /// Build INode tree from V2 manifest.
    pub fn from_v2_manifest(&mut self, manifest: &v2025_12_04::AssetManifest) {
        // Create explicit directories first
        for dir in &manifest.dirs {
            if !dir.deleted {
                self.add_directory_from_path(&dir.path);
            }
        }

        for entry in &manifest.paths {
            // Skip deleted entries
            if entry.deleted {
                continue;
            }

            if let Some(ref target) = entry.symlink_target {
                // V2 symlink
                self.add_symlink_from_v2(entry, target);
            } else if let Some(ref chunkhashes) = entry.chunkhashes {
                // V2 chunked file (>256MB)
                self.add_chunked_file_from_v2(entry, chunkhashes);
            } else {
                // V2 regular file (or small file with single hash)
                self.add_file_from_v2(entry);
            }
        }
    }
}
```

#### Permission Mapping

```rust
impl INodeFile {
    /// Get FUSE permissions for this file.
    pub fn permissions(&self) -> u16 {
        if self.executable {
            0o755  // rwxr-xr-x (V2 runnable)
        } else {
            0o644  // rw-r--r-- (default)
        }
    }
}

impl INodeSymlink {
    /// Symlinks always have 0o777 permissions (target determines access).
    pub fn permissions(&self) -> u16 {
        0o777
    }
}
```

#### Chunked File Read Handling

```rust
impl DeadlineVfs {
    /// Read from a file, handling both single-hash and chunked files.
    async fn read_file(
        &self,
        file: &INodeFile,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, VfsError> {
        match &file.content {
            FileContent::SingleHash(hash) => {
                // V1 or small V2 file: single block
                let key = BlockKey::single(hash);
                let handle = self.pool.acquire(&key, || self.fetch(hash)).await?;
                Ok(handle.data()[offset as usize..][..size as usize].to_vec())
            }
            FileContent::Chunked(chunkhashes) => {
                // V2 large file: may span multiple chunks
                self.read_chunked(file, chunkhashes, offset, size).await
            }
        }
    }

    /// Read from a chunked file, potentially spanning multiple 256MB chunks.
    async fn read_chunked(
        &self,
        file: &INodeFile,
        chunkhashes: &[String],
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, VfsError> {
        let chunk_size = CHUNK_SIZE_V2;
        let start_chunk = (offset / chunk_size) as usize;
        let end_chunk = ((offset + size as u64 - 1) / chunk_size) as usize;

        let mut result = Vec::with_capacity(size as usize);

        for chunk_idx in start_chunk..=end_chunk {
            let chunk_hash = &chunkhashes[chunk_idx];
            let key = BlockKey::new(chunk_hash, chunk_idx as u32);
            let handle = self.pool.acquire(&key, || self.fetch(chunk_hash)).await?;

            let chunk_start = chunk_idx as u64 * chunk_size;
            let read_start = if chunk_idx == start_chunk {
                (offset - chunk_start) as usize
            } else {
                0
            };
            let read_end = if chunk_idx == end_chunk {
                ((offset + size as u64) - chunk_start) as usize
            } else {
                handle.len()
            };

            result.extend_from_slice(&handle.data()[read_start..read_end]);
        }

        Ok(result)
    }
}
```

### INodeManager

```rust
pub struct INodeManager {
    next_id: AtomicU64,
    inodes: RwLock<HashMap<INodeId, Arc<dyn INode>>>,
    path_index: RwLock<HashMap<String, INodeId>>,
}

impl INodeManager {
    pub fn new() -> Self;                                    // Creates root dir
    pub fn get(&self, id: INodeId) -> Option<Arc<dyn INode>>;
    pub fn get_by_path(&self, path: &str) -> Option<Arc<dyn INode>>;
    pub fn add_file(&self, parent: INodeId, entry: &ManifestFilePath) -> INodeId;
    pub fn add_directory(&self, parent: INodeId, name: &str, path: &str) -> INodeId;
    pub fn add_symlink(&self, parent: INodeId, name: &str, path: &str, target: &str) -> INodeId;
}
```

### FileStore Trait

```rust
#[async_trait]
pub trait FileStore: Send + Sync {
    async fn retrieve(&self, hash: &str, algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError>;
    async fn retrieve_range(&self, hash: &str, algorithm: HashAlgorithm, 
                            offset: u64, size: u64) -> Result<Vec<u8>, VfsError>;
}

pub struct S3CasStore {
    client: S3Client,
    bucket: String,
    cas_prefix: String,  // e.g., "Data"
}

pub struct CachedStore<S: FileStore> {
    inner: S,
    cache_dir: PathBuf,
    max_size: u64,
}
```

### Memory Pool (v2 - DashMap-based)

The memory pool manages fixed-size blocks (256MB, matching V2 chunk size) with LRU eviction.
Uses DashMap for lock-free concurrent access on hot paths. Supports both immutable (read-only)
and mutable (dirty) blocks in a unified pool.

```
┌─────────────────────────────────────────────────────────────────┐
│                      MemoryPool (v2)                             │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  blocks: DashMap<PoolBlockId, Arc<PoolBlock>>           │    │
│  │  key_index: DashMap<BlockKey, PoolBlockId>              │    │
│  │  pending_fetches: DashMap<BlockKey, SharedFetch>        │    │
│  │  lru_state: Mutex<LruState>  (cold path only)           │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘

PoolBlock:
  - data: BlockData (Immutable or Mutable)
  - key: BlockKey
  - ref_count: AtomicUsize
  - needs_flush: AtomicBool (for dirty blocks)

BlockData:
  - Immutable(Arc<Vec<u8>>)  - lock-free reads for read-only content
  - Mutable(Arc<RwLock<Vec<u8>>>)  - in-place modification for dirty content
```

#### Core Types

```rust
/// Identifier for block content - either hash-based or inode-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentId {
    /// Folded hash for read-only content (64-bit for efficiency).
    Hash(u64),
    /// Inode-based ID for dirty content (must be flushed before eviction).
    Inode(u64),
}

/// Key identifying a unique block (content ID + chunk_index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockKey {
    pub id: ContentId,
    pub chunk_index: u32,
}

impl BlockKey {
    /// Create a block key from a content hash hex string.
    pub fn from_hash_hex(hash_hex: &str, chunk_index: u32) -> Self;
    /// Create a block key from an inode ID (for dirty blocks).
    pub fn from_inode(inode_id: u64, chunk_index: u32) -> Self;
}

/// Configuration for the memory pool.
pub struct MemoryPoolConfig {
    pub max_size: u64,    // Default: 8GB
    pub block_size: u64,  // Default: 256MB (CHUNK_SIZE_V2)
}

/// RAII handle for immutable blocks - holds Arc to data, auto-releases on drop.
pub struct BlockHandle {
    data: Arc<Vec<u8>>,   // Direct access, no lock needed
    block: Arc<PoolBlock>,
}

/// RAII handle for mutable (dirty) blocks.
pub struct MutableBlockHandle {
    data: Arc<RwLock<Vec<u8>>>,
    block: Arc<PoolBlock>,
}

impl MutableBlockHandle {
    /// Access data with a read lock (efficient, no copy).
    pub fn with_data<F, R>(&self, f: F) -> R where F: FnOnce(&[u8]) -> R;
    /// Get a copy of the data.
    pub fn data(&self) -> Vec<u8>;
}
```

### VFS Options

Configuration for VFS behavior including caching, prefetching, and performance tuning.

```rust
/// Main configuration struct for the VFS.
pub struct VfsOptions {
    pub pool: MemoryPoolConfig,       // Memory pool settings
    pub prefetch: PrefetchStrategy,   // Chunk prefetch behavior
    pub kernel_cache: KernelCacheOptions,
    pub read_ahead: ReadAheadOptions,
    pub timeouts: TimeoutOptions,
}

/// Strategy for prefetching chunks.
pub enum PrefetchStrategy {
    /// No prefetching - lazy load on read (default).
    None,
    /// Prefetch first N chunks on file open.
    OnOpen { chunks: u32 },
    /// Prefetch next chunk during sequential reads.
    Sequential { look_ahead: u32 },
    /// Prefetch all chunks on open (use with caution).
    Eager,
}

/// Kernel-level cache settings (FUSE).
pub struct KernelCacheOptions {
    pub enable_page_cache: bool,   // Default: true
    pub enable_attr_cache: bool,   // Default: true
    pub attr_timeout_secs: u64,    // Default: 86400 (24h)
    pub entry_timeout_secs: u64,   // Default: 86400 (24h)
}

/// Read-ahead behavior for sequential access.
pub struct ReadAheadOptions {
    pub detect_sequential: bool,      // Default: true
    pub sequential_threshold: u32,    // Default: 2 reads
    pub max_concurrent_prefetch: u32, // Default: 4
}

/// Timeout settings.
pub struct TimeoutOptions {
    pub fetch_timeout_secs: u64,  // Default: 300 (5 min)
    pub open_timeout_secs: u64,   // Default: 60 (1 min)
}
```

#### Usage Example

```rust
let options = VfsOptions::default()
    .with_prefetch(PrefetchStrategy::OnOpen { chunks: 2 })
    .with_pool_config(MemoryPoolConfig::with_max_size(16 * GB))
    .with_read_ahead(ReadAheadOptions::aggressive());

let vfs = DeadlineVfs::new(manifest, store, options)?;
fuser::mount2(vfs, "/mnt/assets", &[MountOption::RO])?;
```

#### Pool Operations

```rust
impl MemoryPool {
    /// Create pool with configuration.
    pub fn new(config: MemoryPoolConfig) -> Self;
    
    /// Acquire block, fetching if not cached. Returns RAII handle.
    /// Uses fetch coordination to prevent duplicate fetches.
    pub async fn acquire<F, Fut>(&self, key: &BlockKey, fetch: F) 
        -> Result<BlockHandle, MemoryPoolError>;
    
    /// Try to get cached block without fetching.
    pub fn try_get(&self, key: &BlockKey) -> Option<BlockHandle>;
    
    /// Get pool statistics.
    pub fn stats(&self) -> MemoryPoolStats;
    
    // ========== Dirty block management (unified memory pool) ==========
    
    /// Check if a dirty block exists (lock-free).
    pub fn has_dirty(&self, inode_id: u64, chunk_index: u32) -> bool;
    
    /// Get a dirty block if it exists (lock-free lookup).
    pub fn get_dirty(&self, inode_id: u64, chunk_index: u32) -> Option<MutableBlockHandle>;
    
    /// Insert or update a dirty block with mutable storage.
    pub fn insert_dirty(&self, inode_id: u64, chunk_index: u32, data: Vec<u8>) 
        -> Result<MutableBlockHandle, MemoryPoolError>;
    
    /// Modify a dirty block in place using a closure (zero-copy).
    pub fn modify_dirty_in_place<F>(&self, inode_id: u64, chunk_index: u32, modifier: F)
        -> Result<usize, MemoryPoolError>
    where F: FnOnce(&mut Vec<u8>);
    
    /// Modify a dirty block in place with a slice (zero-copy, no Vec clone).
    pub fn modify_dirty_in_place_with_slice(&self, inode_id: u64, chunk_index: u32, 
        offset: usize, data: &[u8]) -> Result<(), MemoryPoolError>;
    
    /// Mark a dirty block as flushed (can be evicted without flush).
    pub fn mark_flushed(&self, inode_id: u64, chunk_index: u32) -> bool;
    
    /// Mark a dirty block as needing flush.
    pub fn mark_needs_flush(&self, inode_id: u64, chunk_index: u32) -> bool;
    
    /// Invalidate all read-only blocks for a given hash.
    /// Called when a file is modified to prevent stale reads.
    pub fn invalidate_hash(&self, hash: &str) -> usize;
    
    /// Remove all blocks for an inode (clean up on file delete).
    pub fn remove_inode_blocks(&self, inode_id: u64) -> usize;
    
    /// Shrink a dirty block's allocation after flush to save memory.
    pub fn shrink_dirty_block(&self, inode_id: u64, chunk_index: u32) -> bool;
    
    /// Get the number of dirty blocks for an inode.
    pub fn dirty_block_count(&self, inode_id: u64) -> usize;
}
```

#### Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `DEFAULT_MAX_POOL_SIZE` | 8GB | Maximum pool memory |
| `DEFAULT_BLOCK_SIZE` | 256MB | Block size (matches V2 chunk) |

#### Multi-threaded Access Design

The memory pool is designed for concurrent access from multiple FUSE read operations:

1. **Lock-Free Data Access**: Block data is stored in `Arc<Vec<u8>>`.
   - `BlockHandle` holds a clone of the Arc
   - Reading data requires no locks - just dereference the Arc
   - Multiple readers can access the same block simultaneously

2. **Atomic Reference Counting**: `ref_count` uses `AtomicUsize`.
   - Increment/decrement without holding pool lock
   - Blocks with `ref_count > 0` cannot be evicted
   - `BlockHandle::drop()` decrements atomically

3. **Fetch Coordination**: Prevents thundering herd / duplicate fetches.
   - `pending_fetches: HashMap<BlockKey, Shared<Future>>` tracks in-flight fetches
   - First thread to request a key starts the fetch and inserts a shared future
   - Subsequent threads for the same key await the existing future
   - After fetch completes, future is removed and block is inserted

   ```
   Thread A: acquire(key) → cache miss → start fetch, insert pending future
   Thread B: acquire(key) → cache miss → find pending future → await it
   Thread C: acquire(key) → cache miss → find pending future → await it
   [fetch completes]
   All threads: receive same data, only one S3 request made
   ```

4. **Lock Granularity**:
   - Pool metadata protected by `Mutex<MemoryPoolInner>` (not RwLock - mutations are common)
   - Lock held only for quick HashMap operations
   - Async fetch happens outside the lock
   - Data reads are lock-free via Arc

5. **Deadlock Prevention**:
   - Single lock (no nested locks)
   - No lock held across await points
   - Atomic operations for ref_count

6. **Memory Pressure**: When pool is full and all blocks are in use:
   - Returns `MemoryPoolError::PoolExhausted`
   - Caller should retry or fail the read operation

### DeadlineVfs (FUSE Implementation)

```rust
pub struct DeadlineVfs {
    inodes: INodeManager,
    store: Arc<dyn FileStore>,
    handles: RwLock<HashMap<u64, OpenHandle>>,
    next_handle: AtomicU64,
}

impl fuser::Filesystem for DeadlineVfs {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent_dir = self.inodes.get(parent)?.as_dir()?;
        let child_id = parent_dir.get_child(name.to_str()?)?;
        let child = self.inodes.get(child_id)?;
        reply.entry(&TTL, &child.to_fuser_attr(), 0);
    }
    
    fn read(&mut self, _req: &Request, ino: u64, fh: u64, offset: i64, 
            size: u32, _flags: i32, _lock: Option<u64>, reply: ReplyData) {
        let file = self.inodes.get(ino)?.as_file()?;
        let data = self.store.retrieve(&file.hash, file.hash_algorithm)?;
        reply.data(&data[offset as usize..][..size as usize]);
    }
    // ... other methods
}
```

---

## Usage Example

```rust
use std::sync::Arc;
use rusty_attachments_model::Manifest;
use rusty_attachments_storage::{S3Location, StorageClient, StorageSettings};
use rusty_attachments_storage_crt::CrtStorageClient;
use rusty_attachments_vfs::{DeadlineVfs, FileStore, VfsError, VfsOptions};

// Adapter to bridge StorageClient → FileStore
struct StorageClientAdapter<C: StorageClient> {
    client: C,
    location: S3Location,
}

impl<C: StorageClient> StorageClientAdapter<C> {
    fn new(client: C, location: S3Location) -> Self {
        Self { client, location }
    }
}

#[async_trait::async_trait]
impl<C: StorageClient + 'static> FileStore for StorageClientAdapter<C> {
    async fn retrieve(
        &self,
        hash: &str,
        algorithm: rusty_attachments_model::HashAlgorithm,
    ) -> Result<Vec<u8>, VfsError> {
        let key = self.location.cas_key(hash, algorithm);
        self.client
            .get_object(&self.location.bucket, &key)
            .await
            .map_err(|e| VfsError::ContentRetrievalFailed {
                hash: hash.to_string(),
                source: format!("{}", e).into(),
            })
    }

    async fn retrieve_range(
        &self,
        hash: &str,
        algorithm: rusty_attachments_model::HashAlgorithm,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, VfsError> {
        // Fetch full content and slice (could optimize with S3 range requests)
        let data = self.retrieve(hash, algorithm).await?;
        let start = offset as usize;
        let end = (offset + size).min(data.len() as u64) as usize;
        Ok(data[start..end].to_vec())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load manifest
    let json = std::fs::read_to_string("manifest.json")?;
    let manifest = Manifest::decode(&json)?;

    // Create storage client using existing infrastructure
    let settings = StorageSettings {
        region: "us-west-2".to_string(),
        ..Default::default()
    };
    let client = CrtStorageClient::new(settings).await?;
    let location = S3Location::new("my-bucket", "DeadlineCloud", "Data", "Manifests");

    // Wrap in adapter to implement FileStore
    let store: Arc<dyn FileStore> = Arc::new(StorageClientAdapter::new(client, location));

    // Build VFS and mount
    let vfs = DeadlineVfs::new(&manifest, store, VfsOptions::default())?;
    
    // Mount (blocking - returns on unmount)
    rusty_attachments_vfs::mount(vfs, "/mnt/assets")?;
    
    Ok(())
}
```

---

## Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    #[error("Inode not found: {0}")]
    InodeNotFound(INodeId),
    
    #[error("Not a directory: {0}")]
    NotADirectory(INodeId),
    
    #[error("Content retrieval failed for hash {hash}")]
    ContentRetrievalFailed { hash: String, source: Box<dyn std::error::Error + Send + Sync> },
    
    #[error("Hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    
    #[error("Mount failed: {0}")]
    MountFailed(String),
}
```

---

## Fus3 C++ Reference (for porting)

Key files and functions to reference when implementing:

| Rust Component | Fus3 C++ Reference |
|----------------|-------------------|
| `INodeManager::add_file` | `INodeManager::AddPathINodes()` in `inode_manager.cpp:237` |
| `DeadlineVfs::lookup` | `HandleLookup()` in `simurgh_fuse_low.cpp:903` |
| `DeadlineVfs::read` | `HandleRead()` + `ReadFUSE()` in `simurgh_fuse_low.cpp:137` |
| `S3CasStore::retrieve` | `AWSVFS::GetFileInfoFromBucket()` in `aws_vfs.cpp:378` |
| `S3CasStore::cas_key` | `DeadlineVFS::GetS3Key()` in `deadline_vfs.cpp:137` |
| Hash verification | `DeadlineVFS::IsObjectIntegrityValid()` in `deadline_vfs.cpp:109` |

---

## Dependencies

```toml
[dependencies]
async-trait = "0.1"
fuser = { version = "0.14", optional = true }
futures = "0.3"
libc = { version = "0.2", optional = true }
thiserror = "1.0"
tokio = { version = "1", features = ["rt-multi-thread", "sync", "time"] }
tracing = "0.1"

# Internal crates
rusty-attachments-common = { path = "../common" }
rusty-attachments-model = { path = "../model" }

[dev-dependencies]
# For mount_vfs example - uses existing storage infrastructure
rusty-attachments-storage = { path = "../storage" }
rusty-attachments-storage-crt = { path = "../storage-crt" }
ctrlc = "3.4"
dirs = "5.0"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time"] }

[features]
default = []
fuse = ["dep:fuser", "dep:libc"]
```

Note: The storage crates are dev-dependencies because the VFS crate only defines the `FileStore` trait. The actual S3 implementation (`StorageClientAdapter`) lives in the example binary, allowing flexibility in how the VFS is used.

---

## Future Considerations

1. **Multiple manifests** - Mount multiple manifests as subdirectories
2. **Windows support** - WinFSP or Dokan integration
3. **Trace-based prefetching** - Use access traces to predict and prefetch files (see `design/vfs-trace-precaching.md`)
4. **Hash verification** - Verify content integrity on read using xxhash

---

## Write Support Design (✅ IMPLEMENTED)

Write support is now fully implemented using a Copy-on-Write (COW) approach with the unified memory pool.

### Implementation Overview

The writable VFS (`fuse_writable.rs`) provides full read-write support:

| Component | Location | Description |
|-----------|----------|-------------|
| `WritableDeadlineVfs` | `src/fuse_writable.rs` | FUSE filesystem with write support |
| `DirtyFileManager` | `src/write/dirty.rs` | Tracks modified files using unified pool |
| `DirtyDirManager` | `src/write/dirty_dir.rs` | Tracks directory modifications |
| `MaterializedCache` | `src/diskcache/materialized.rs` | Disk cache for dirty content |
| `DiffManifestExporter` | `src/write/export.rs` | Export changes as diff manifest |

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    WritableDeadlineVfs                           │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  pool: Arc<MemoryPool>  (unified - read + dirty blocks) │    │
│  │  dirty_files: DirtyFileManager                          │    │
│  │  dirty_dirs: DirtyDirManager                            │    │
│  │  write_cache: Arc<dyn WriteCache>                       │    │
│  │  executor: Arc<AsyncExecutor>                           │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

### Write Flow

```
write(ino, offset, data):
  1. Check if file is already dirty in pool
  2. If not dirty:
     a. Fetch original content from S3 (or use empty for new files)
     b. Insert as mutable block in pool (COW)
  3. Modify dirty block in place (zero-copy via modify_dirty_in_place_with_slice)
  4. Update DirtyFileMetadata (size, mtime)
  5. Periodically flush to disk cache (MaterializedCache)

flush/fsync(ino):
  1. Get dirty block data from pool
  2. Write to MaterializedCache on disk
  3. Mark block as flushed (can be evicted without data loss)
```

### Read Flow with Dirty Blocks

```
read(ino, offset, size):
  1. Check pool for dirty block (pool.get_dirty)
  2. If dirty: return from dirty block
  3. Else: fetch from S3 via pool.acquire (existing read-only flow)
```

### Key Design Decisions

1. **Unified memory pool**: Both read-only and dirty blocks share the same pool with LRU eviction. Dirty blocks are marked `needs_flush` and must be flushed before eviction.

2. **Zero-copy writes**: `modify_dirty_in_place_with_slice` writes directly to the block buffer without intermediate copies.

3. **Async executor**: All async I/O (S3 fetches, disk cache writes) runs on a dedicated executor to avoid FUSE callback deadlocks.

4. **Disk cache persistence**: `MaterializedCache` stores dirty content on disk, surviving process restarts and memory pressure.

5. **Diff manifest export**: Changes can be exported as a diff manifest for upload to S3.


---

## Fus3 vs Rust VFS: File Open/Read/Close Flow Comparison

This section traces through the file lifecycle in both implementations to identify differences and potential performance implications.

### Fus3 (C++) Flow

```
Thread A: open("/file.txt")
├─ HandleOpen(ino)
│  ├─ GetINode(ino) → iNode
│  └─ OpenObject(iNode, isOpen=true, createNotExists=false)
│     ├─ LOCK(m_openFileMutex)
│     ├─ Check m_openFileINodeMap[ino] → miss
│     ├─ Check m_pendingOpenFileINodeMap[ino] → miss
│     ├─ Create OpenFileInfo, add to m_pendingOpenFileINodeMap
│     ├─ LOCK(fileBuffer.bufferMutex) ← held during download
│     ├─ UNLOCK(m_openFileMutex)
│     ├─ m_fileStorage->Retrieve() or GetFileInfoFromBucket()
│     │  └─ S3 GetObject (blocking, ~100ms-10s)
│     ├─ TransferComplete()
│     │  ├─ LOCK(m_openFileMutex)
│     │  ├─ Move from pending → openFileINodeMap
│     │  ├─ UNLOCK(m_openFileMutex)
│     │  └─ SetOpen() → notify waiters
│     └─ UNLOCK(fileBuffer.bufferMutex)
└─ Return file handle

Thread B: open("/file.txt") [while Thread A downloading]
├─ HandleOpen(ino)
│  └─ OpenObject(iNode)
│     ├─ LOCK(m_openFileMutex)
│     ├─ Check m_openFileINodeMap[ino] → miss
│     ├─ Check m_pendingOpenFileINodeMap[ino] → HIT!
│     ├─ UNLOCK(m_openFileMutex)
│     ├─ WaitForOpen() ← blocks until Thread A completes
│     ├─ Check IsInitialized() → true
│     ├─ IncrementHandleCount()
│     └─ Return same OpenFileInfo
└─ Return file handle (same underlying buffer)
```

**Key Fus3 Characteristics:**
- Per-file `OpenFileInfo` with handle count (multiple opens share one buffer)
- `m_pendingOpenFileINodeMap` for in-flight downloads
- `WaitForOpen()` blocks concurrent openers until download completes
- Single S3 request per file, regardless of concurrent opens
- Buffer mutex held during entire download

### Proposed Rust VFS Flow

```
Thread A: open("/file.txt")
├─ lookup(parent, "file.txt") → ino
├─ open(ino) → create OpenHandle, return fh
└─ [no download yet - lazy]

Thread A: read(ino, fh, offset=0, size=256MB)
├─ Get chunk_key from INode (hash + chunk_index)
├─ pool.acquire(&chunk_key, fetch_fn)
│  ├─ LOCK(pool.inner)
│  ├─ Check key_index → miss
│  ├─ Check pending_fetches → miss
│  ├─ Insert shared future into pending_fetches
│  ├─ UNLOCK(pool.inner)
│  ├─ fetch_fn() → S3 GetObject (async, ~100ms-10s)
│  ├─ LOCK(pool.inner)
│  ├─ Remove from pending_fetches
│  ├─ Insert block, acquire ref
│  ├─ UNLOCK(pool.inner)
│  └─ Return BlockHandle
└─ Return data slice

Thread B: read(ino, fh, offset=0, size=256MB) [while Thread A downloading]
├─ pool.acquire(&chunk_key, fetch_fn)
│  ├─ LOCK(pool.inner)
│  ├─ Check key_index → miss
│  ├─ Check pending_fetches → HIT!
│  ├─ Clone shared future
│  ├─ UNLOCK(pool.inner)
│  ├─ await shared_future ← waits for Thread A
│  ├─ LOCK(pool.inner)
│  ├─ Lookup block, acquire ref
│  ├─ UNLOCK(pool.inner)
│  └─ Return BlockHandle
└─ Return data slice
```

### Key Differences

| Aspect | Fus3 (C++) | Rust VFS |
|--------|------------|----------|
| **Download trigger** | `open()` - eager | `read()` - lazy |
| **Granularity** | Per-file buffer | Per-chunk (256MB blocks) |
| **Concurrent open handling** | `WaitForOpen()` on pending map | Shared future in `pending_fetches` |
| **Data sharing** | Single `OpenFileInfo` per file | `Arc<Vec<u8>>` per chunk |
| **Lock during download** | Buffer mutex held | No lock held (async) |
| **Handle model** | Handle count on `OpenFileInfo` | Separate `BlockHandle` per acquire |

### Performance Implications

**Advantages of Rust Design:**

1. **Lazy download**: Files opened but never read don't trigger S3 requests. Fus3 downloads on open even if file is never read.

2. **Chunk-level caching**: For V2 manifests, only accessed chunks are downloaded. A 10GB file with 40 chunks only downloads the chunks actually read.

3. **No lock during download**: Fus3 holds `bufferMutex` during the entire S3 download. Rust releases the pool lock before async fetch.

4. **Better parallelism for large files**: Multiple chunks of the same file can be fetched in parallel by different threads.

**Potential Disadvantages:**

1. **More S3 requests for small files**: Fus3 downloads entire file once. If Rust VFS reads a small file in multiple `read()` calls, it's still one chunk, but the lazy approach means the first read has full latency.

2. **No prefetching**: Fus3's eager download means data is ready when `read()` is called. Rust's lazy approach means first `read()` always waits.

3. **Chunk boundary overhead**: Reading across chunk boundaries requires acquiring multiple blocks.

### Recommended Enhancements

1. **Prefetch on open (optional)**: Add a config option to prefetch first N chunks on `open()` for latency-sensitive workloads.

2. **Sequential read detection**: If reads are sequential, prefetch next chunk while current chunk is being read.

3. **Small file optimization**: For files < 256MB (single chunk), behavior is equivalent to Fus3.

```rust
// Optional prefetch on open
fn open(&mut self, ino: u64, flags: i32) -> Result<OpenHandle> {
    let handle = self.create_handle(ino, flags)?;
    
    if self.config.prefetch_on_open {
        let file = self.inodes.get(ino)?;
        let first_chunk = BlockKey::new(&file.hash, 0);
        // Fire-and-forget prefetch
        tokio::spawn(self.pool.acquire(&first_chunk, || self.fetch_chunk(&first_chunk)));
    }
    
    Ok(handle)
}
```
