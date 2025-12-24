# Rusty Attachments: Virtual File System (VFS) Module Design

**Status: 📋 DESIGN**

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
│    (INode, INodeDir, etc.)    │    (S3CasStore, DiskCache)                  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Existing Crates                                           │
│              model (Manifest)  │  storage (S3 client)                        │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Project Structure

```
crates/vfs/src/
├── lib.rs
├── error.rs              # VfsError enum
│
├── inode/                # Layer 1: INode primitives
│   ├── mod.rs
│   ├── types.rs          # INodeId, INodeType, FileAttr
│   ├── file.rs           # INodeFile (hash, size, mtime)
│   ├── dir.rs            # INodeDir (children map)
│   ├── symlink.rs        # INodeSymlink (target path)
│   └── manager.rs        # INodeManager (allocation, lookup)
│
├── content/              # Layer 1: Content retrieval
│   ├── mod.rs
│   ├── store.rs          # FileStore trait
│   ├── s3_cas.rs         # S3CasStore implementation
│   └── cached.rs         # CachedStore wrapper with DiskCache
│
├── builder.rs            # Layer 2: Build INode tree from Manifest
│
└── fuse.rs               # Layer 3: fuser::Filesystem implementation
```

---

## Core Types

### INode Types

```rust
pub type INodeId = u64;
pub const ROOT_INODE: INodeId = 1;

pub enum INodeType { File, Directory, Symlink }

pub struct INodeFile {
    id: INodeId,
    parent_id: INodeId,
    name: String,
    path: String,
    size: u64,
    mtime: SystemTime,
    hash: String,              // Content hash for CAS lookup
    hash_algorithm: HashAlgorithm,
    executable: bool,
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
    target: String,            // Symlink target path
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
use rusty_attachments_vfs::{DeadlineVfs, S3CasStore, CachedStore};
use rusty_attachments_model::Manifest;

// Load manifest
let manifest = Manifest::from_file("manifest.json")?;

// Create content store with caching
let s3_store = S3CasStore::new(s3_client, "my-bucket", "Data");
let cached_store = CachedStore::new(s3_store, "/var/cache/vfs", 10_GB);

// Build VFS and mount
let vfs = DeadlineVfs::from_manifest(&manifest, Arc::new(cached_store))?;
fuser::mount2(vfs, "/mnt/assets", &[MountOption::RO, MountOption::AutoUnmount])?;
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
fuser = "0.14"
tokio = { version = "1", features = ["rt-multi-thread", "sync"] }
async-trait = "0.1"
thiserror = "1"
tracing = "0.1"

# Internal crates
rusty-attachments-model = { path = "../model" }
rusty-attachments-storage = { path = "../storage" }
rusty-attachments-common = { path = "../common" }
```

---

## Future Considerations

1. **Chunk-based retrieval** - Use V2 chunk hashes for parallel/partial downloads
2. **Write support** - Write-back to local cache for temporary files
3. **Multiple manifests** - Mount multiple manifests as subdirectories
4. **Windows support** - WinFSP or Dokan integration
