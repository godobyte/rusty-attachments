# Rusty Attachments: macOS FSKit VFS Module Design

**Status: ✅ IMPLEMENTED**

## Overview

This document describes the design for the `rusty-attachments-vfs-fskit` crate that provides a macOS-native virtual filesystem using Apple's FSKit framework (macOS 15.4+). This crate bridges the existing VFS primitives to FSKit via the [fskit-rs](https://github.com/debox-network/fskit-rs) Rust crate.

**Implemented Features**:
- Read-only manifest mounting (v2023-03-03 and v2025-12-04-beta formats)
- Copy-on-write (COW) file modifications
- New file/directory creation
- File/directory deletion
- Chunked file support (v2025 format)
- Symlink support (v2025 format)
- Live statistics dashboard (`--stats` flag)
- Dirty state tracking for diff manifest export

### Why FSKit?

| Aspect | FUSE (macFUSE) | FSKit |
|--------|---------------|-------|
| **macOS Support** | Third-party, requires kernel extension | Native Apple framework |
| **Security** | Kernel extension approval required | User-space, sandboxed |
| **Performance** | Good | Better (native integration) |
| **Future** | Deprecated path | Apple's recommended approach |
| **Requirements** | macFUSE installation | macOS 15.4+, FSKitBridge app |

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    FSKitBridge (Swift appex)                                 │
│                    Handles FSKit/XPC ↔ macOS                                │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                          TCP + Protobuf (localhost)
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                    fskit-rs (protocol layer)                                 │
│                    Filesystem trait + session management                     │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                    rusty-attachments-vfs-fskit                               │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  DeadlineFsKit (implements fskit_rs::Filesystem)                    │    │
│  │    - Maps FSKit operations to VFS primitives                        │    │
│  │    - Manages item_id ↔ INodeId mapping                              │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Shared VFS Primitives (existing)                          │
│  ┌────────────────────────────┐  ┌────────────────────────────────────┐    │
│  │  INodeManager              │  │  MemoryPool (v2)                   │    │
│  │  FileStore trait           │  │  DirtyFileManager                  │    │
│  │  FileContent enum          │  │  DirtyDirManager                   │    │
│  └────────────────────────────┘  └────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Storage Layer (existing)                                  │
│  ┌────────────────────────────┐  ┌────────────────────────────────────┐    │
│  │  StorageClient trait       │  │  CrtStorageClient (S3)             │    │
│  │  S3Location                │  │  StorageSettings                   │    │
│  └────────────────────────────┘  └────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Key Differences: FUSE vs FSKit

| FUSE Concept | FSKit Equivalent | Notes |
|--------------|------------------|-------|
| `inode: u64` | `item_id: u64` | Direct mapping |
| `lookup(parent, name)` | `lookup_item(name, directory_id)` | Same semantics |
| `getattr(ino)` | `get_attributes(item_id)` | Returns `ItemAttributes` |
| `setattr(ino, ...)` | `set_attributes(item_id, attrs)` | Truncate, mtime, etc. |
| `readdir(ino, offset)` | `enumerate_directory(dir_id, cookie, verifier)` | Cookie-based pagination |
| `open(ino, flags)` | `open_item(item_id, modes)` | Modes: READ/WRITE |
| `read(ino, fh, offset, size)` | `read(item_id, offset, length)` | No file handle |
| `write(ino, fh, offset, data)` | `write(contents, item_id, offset)` | Returns bytes written |
| `release(ino, fh)` | `close_item(item_id, modes)` | Mode-based close, triggers flush |
| `readlink(ino)` | `read_symbolic_link(item_id)` | Same semantics |
| `create(parent, name, mode)` | `create_item(name, type, dir_id, attrs)` | File creation |
| `mkdir(parent, name, mode)` | `create_item(name, DIRECTORY, dir_id, attrs)` | Directory creation |
| `unlink(parent, name)` | `remove_item(item_id, name, dir_id)` | File deletion |
| `rmdir(parent, name)` | `remove_item(item_id, name, dir_id)` | Directory deletion |
| `fsync(ino)` | `synchronize(flags)` | Flush to disk |
| `mount2()` | `activate(options)` | Returns root Item |
| `unmount` | `deactivate()` | Cleanup |

### FSKit-Specific Concepts

1. **Item Lifecycle**: FSKit uses `reclaim_item()` to release resources after kernel no longer references an item
2. **Verifier**: Directory enumeration uses verifiers to detect changes between calls
3. **Volume Identity**: Must provide `ResourceIdentifier` and `VolumeIdentifier`
4. **Capabilities**: Declare supported features via `SupportedCapabilities`
5. **No File Handles**: FSKit uses `item_id` directly for read/write (no separate file handle)

## Design Decisions

This section documents key architectural decisions made during the design process, with references to fskit-rs source code and learnings from the FUSE implementation.

### Decision 1: No AsyncExecutor Bridge Required

**Question**: Does FSKit need the `AsyncExecutor` pattern used in FUSE to bridge sync callbacks to async operations?

**Analysis**: Examining `fskit-rs/src/socket.rs`, the handler runs within a Tokio runtime:

```rust
// fskit-rs/src/socket.rs - handler spawned in async context
tokio::spawn(async move {
    if let Err(err) = handle_stream(stream, handler, shutdown_rx).await {
        // ...
    }
});
```

And in `handle_stream`, trait methods are called with `.await`:

```rust
// fskit-rs/src/handler.rs - trait methods are async
match handler.handle(content).await {
    Ok(content) => Some(content),
    // ...
}
```

**Contrast with FUSE**: The FUSE `fuser::Filesystem` trait uses synchronous callbacks:

```rust
// fuser::Filesystem - sync callbacks
fn read(&mut self, _req: &Request, ino: u64, ..., reply: ReplyData) {
    // Cannot .await here! Must use executor bridge
    let result = self.executor.block_on(async { ... });
}
```

This is why `crates/vfs/src/fuse_writable.rs` requires `AsyncExecutor`:

```rust
// FUSE requires executor bridge for async operations
let exec_result = self.executor.block_on(async move { 
    dm.read(ino, offset_u64, size).await 
});
```

**Decision**: FSKit does NOT need `AsyncExecutor`. The `fskit_rs::Filesystem` trait methods are already `async fn`, so we can directly `.await` on S3 fetches and other async operations.

**Impact**: Simpler code, no executor overhead, no potential deadlocks from blocking on async.

---

### Decision 2: Arc<Inner> Pattern for Clone Requirement

**Question**: How should we handle thread safety given FSKit's requirements?

**Analysis**: FSKit-rs requires `Filesystem: Clone + Send + Sync + 'static`:

```rust
// fskit-rs/src/lib.rs - mount signature
pub async fn mount<FS>(fs: FS, opts: MountOptions) -> Result<Session>
where
    FS: Filesystem + Send + Sync + Clone + 'static,
```

The handler is cloned per-connection in `socket.rs`. This means our struct must be cheaply cloneable while sharing mutable state.

**Contrast with FUSE**: FUSE passes `&mut self` to callbacks - no Clone requirement, but requires careful synchronization:

```rust
// FUSE - mutable reference, no Clone needed
fn write(&mut self, _req: &Request, ino: u64, ...) { ... }
```

**Decision**: Wrap all mutable state in `Arc<Inner>`:

```rust
#[derive(Clone)]
pub struct WritableFsKit {
    inner: Arc<WritableFsKitInner>,  // Clone just clones the Arc pointer
}

struct WritableFsKitInner {
    inodes: Arc<INodeManager>,
    dirty_manager: Arc<DirtyFileManager>,
    // ... other shared state
}
```

**Impact**: Cheap cloning (just Arc pointer copy), thread-safe shared state via existing `Arc`/`RwLock`/`DashMap` in primitives.

---

### Decision 3: Memory Allocation Optimization for Writes

**Question**: Can we reduce buffer copying compared to FUSE?

**Analysis**: Comparing the write signatures:

```rust
// FUSE - receives borrowed slice, must clone for async
fn write(&mut self, ..., data: &[u8], ..., reply: ReplyWrite) {
    let data_vec: Vec<u8> = data.to_vec();  // Clone required!
    self.executor.block_on(async move { dm.write(..., &data_vec).await });
}

// FSKit - receives owned Vec, can move directly
async fn write(&mut self, contents: Vec<u8>, item_id: u64, offset: i64) -> Result<i64> {
    // No clone needed - we own the data!
    self.inner.dirty_manager.write_owned(item_id, offset, contents).await
}
```

**Decision**: Leverage FSKit's owned `Vec<u8>` parameter:
1. For sync fast-path: pass `&contents` slice (no allocation)
2. For async path: move `contents` directly to dirty manager (no clone)

**Impact**: Write-heavy workloads are more efficient than FUSE. The FUSE implementation must clone every write buffer for the async path; FSKit can move ownership.

---

### Decision 4: Complete Filesystem Trait Implementation

**Question**: Which `Filesystem` trait methods must be implemented vs. can return ENOSYS?

**Analysis**: Examining `fskit-rs/examples/basic_fs.rs`, all 32 trait methods must be implemented. The example returns `ENOSYS` for unimplemented methods:

```rust
// basic_fs.rs - skeleton returns ENOSYS
async fn rename_item(...) -> Result<Vec<u8>> {
    Err(Error::Posix(libc::ENOSYS))
}
```

**Required for feature parity with FUSE WritableVfs**:
- `activate`, `deactivate` - mount/unmount lifecycle
- `lookup_item`, `get_attributes`, `set_attributes` - metadata
- `enumerate_directory` - directory listing
- `read`, `write` - file I/O
- `create_item`, `remove_item` - file/directory creation/deletion
- `open_item`, `close_item` - open/close tracking (flush on close)
- `synchronize` - fsync equivalent
- `read_symbolic_link` - symlink support

**Not needed for job attachments use case** (return ENOSYS):
- `create_symbolic_link`, `create_link` - symlink/hardlink creation
- `rename_item` - rename (TODO: future work)
- `*_xattr*` methods - extended attributes
- `preallocate_space` - space preallocation
- `set_volume_name` - volume rename

**Decision**: Implement all methods required for FUSE parity; return ENOSYS for others with clear documentation.

---

### Decision 5: Reuse Existing VFS Primitives

**Question**: Should we create new primitives or reuse existing ones from the FUSE implementation?

**Analysis**: The FUSE `WritableVfs` uses well-tested primitives:
- `DirtyFileManager` - COW file tracking, chunk management
- `DirtyDirManager` - directory creation/deletion tracking
- `MaterializedCache` - disk-backed write cache
- `MemoryPool` - unified memory management
- `INodeManager` - inode metadata

These primitives are already:
- Thread-safe (use `Arc`, `RwLock`, `DashMap`)
- Async-compatible (async methods where needed)
- Well-tested with the FUSE implementation

**Decision**: Reuse all existing primitives. FSKit is just a different "frontend" to the same backend.

**Impact**: 
- Reduced code duplication
- Consistent behavior between FUSE and FSKit
- Shared bug fixes and improvements
- Follows pyramid architecture (FSKit is a new "top layer" using existing "middle layer")

---

### Summary: FUSE vs FSKit Implementation Comparison

| Aspect | FUSE Implementation | FSKit Implementation |
|--------|---------------------|----------------------|
| **Async Model** | Sync callbacks + `AsyncExecutor` bridge | Native async trait methods |
| **Thread Safety** | `&mut self` + internal locks | `Clone` + `Arc<Inner>` |
| **Write Buffer** | `&[u8]` → must clone for async | `Vec<u8>` → can move ownership |
| **File Handles** | Explicit `fh: u64` tracking | No file handles (use `item_id`) |
| **Directory Enum** | Offset-based pagination | Cookie + verifier pagination |
| **Primitives** | `DirtyFileManager`, `MemoryPool`, etc. | Same (shared) |
| **Platform** | macOS (macFUSE), Linux | macOS 15.4+ only |

## Data Structures

### Core Types

```rust
/// Configuration for FSKit VFS.
///
/// Mirrors `VfsOptions` from the FUSE implementation for consistency.
/// FSKit-specific options (volume_name, volume_id) are added.
#[derive(Debug, Clone)]
pub struct FsKitVfsOptions {
    /// Memory pool configuration.
    pub pool: MemoryPoolConfig,
    /// Prefetch strategy for chunk loading.
    pub prefetch: PrefetchStrategy,
    /// Read-ahead configuration.
    pub read_ahead: ReadAheadOptions,
    /// Timeout settings.
    pub timeouts: TimeoutOptions,
    /// Read cache configuration (disk cache for immutable content).
    pub read_cache: ReadCacheConfig,
    /// Volume name displayed in Finder.
    pub volume_name: String,
    /// Unique volume identifier (UUID recommended).
    pub volume_id: String,
}

impl Default for FsKitVfsOptions {
    fn default() -> Self {
        Self {
            pool: MemoryPoolConfig::default(),
            prefetch: PrefetchStrategy::default(),
            read_ahead: ReadAheadOptions::default(),
            timeouts: TimeoutOptions::default(),
            read_cache: ReadCacheConfig::default(),
            volume_name: "Deadline Assets".to_string(),
            volume_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl FsKitVfsOptions {
    /// Set memory pool configuration.
    ///
    /// # Arguments
    /// * `pool` - Memory pool configuration
    pub fn with_pool_config(mut self, pool: MemoryPoolConfig) -> Self {
        self.pool = pool;
        self
    }

    /// Set the prefetch strategy.
    ///
    /// # Arguments
    /// * `prefetch` - Prefetch strategy to use
    pub fn with_prefetch(mut self, prefetch: PrefetchStrategy) -> Self {
        self.prefetch = prefetch;
        self
    }

    /// Set read-ahead options.
    ///
    /// # Arguments
    /// * `read_ahead` - Read-ahead configuration
    pub fn with_read_ahead(mut self, read_ahead: ReadAheadOptions) -> Self {
        self.read_ahead = read_ahead;
        self
    }

    /// Set timeout options.
    ///
    /// # Arguments
    /// * `timeouts` - Timeout configuration
    pub fn with_timeouts(mut self, timeouts: TimeoutOptions) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// Set read cache configuration.
    ///
    /// # Arguments
    /// * `read_cache` - Read cache configuration
    pub fn with_read_cache(mut self, read_cache: ReadCacheConfig) -> Self {
        self.read_cache = read_cache;
        self
    }

    /// Set volume name displayed in Finder.
    ///
    /// # Arguments
    /// * `name` - Volume name
    pub fn with_volume_name(mut self, name: impl Into<String>) -> Self {
        self.volume_name = name.into();
        self
    }

    /// Set volume identifier.
    ///
    /// # Arguments
    /// * `id` - Volume UUID
    pub fn with_volume_id(mut self, id: impl Into<String>) -> Self {
        self.volume_id = id.into();
        self
    }
}

/// Configuration for write behavior (mirrors FUSE WriteOptions).
#[derive(Debug, Clone)]
pub struct FsKitWriteOptions {
    /// Directory for materialized cache.
    pub cache_dir: PathBuf,
    /// Whether to sync to disk on every write (slower but safer).
    pub sync_on_write: bool,
    /// Maximum dirty file size before forcing flush.
    pub max_dirty_size: u64,
}

impl Default for FsKitWriteOptions {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from("/tmp/vfs-fskit-cache"),
            sync_on_write: true,
            max_dirty_size: 1024 * 1024 * 1024, // 1GB
        }
    }
}

/// FSKit mount options (passed to fskit_rs::mount).
pub struct FsKitMountOptions {
    /// FSKit extension bundle ID.
    pub fskit_id: String,
    /// Mount point path.
    pub mount_point: PathBuf,
    /// Force unmount existing mount.
    pub force: bool,
}
```

### Options Parity with FUSE

| FUSE `VfsOptions` Field | FSKit `FsKitVfsOptions` Field | Notes |
|-------------------------|-------------------------------|-------|
| `pool: MemoryPoolConfig` | `pool: MemoryPoolConfig` | Same type, shared |
| `prefetch: PrefetchStrategy` | `prefetch: PrefetchStrategy` | Same type, shared |
| `kernel_cache: KernelCacheOptions` | N/A | FSKit manages its own caching |
| `read_ahead: ReadAheadOptions` | `read_ahead: ReadAheadOptions` | Same type, shared |
| `timeouts: TimeoutOptions` | `timeouts: TimeoutOptions` | Same type, shared |
| `read_cache: ReadCacheConfig` | `read_cache: ReadCacheConfig` | Same type, shared |
| `executor: ExecutorConfig` | N/A | FSKit runs in Tokio, no executor needed |
| N/A | `volume_name: String` | FSKit-specific |
| N/A | `volume_id: String` | FSKit-specific |

**Note**: `KernelCacheOptions` and `ExecutorConfig` are not needed for FSKit:
- FSKit manages kernel caching internally (no user control)
- FSKit-rs runs in Tokio context, so no `AsyncExecutor` bridge is needed

### Item ID Mapping

FSKit uses `item_id: u64` which maps directly to our `INodeId`. The root item has a special ID returned by `activate()`.

```rust
/// Maps FSKit item_id to VFS INodeId.
/// 
/// FSKit item_id == VFS INodeId (direct 1:1 mapping).
/// Root directory uses ROOT_INODE (1).
const ROOT_ITEM_ID: u64 = ROOT_INODE;
```

### ItemAttributes Conversion

```rust
/// Convert VFS INode to FSKit ItemAttributes.
///
/// # Arguments
/// * `inode` - VFS inode to convert
fn to_item_attributes(inode: &dyn INode) -> ItemAttributes {
    ItemAttributes {
        item_id: Some(inode.id()),
        parent_id: Some(inode.parent_id()),
        r#type: Some(match inode.inode_type() {
            INodeType::File => ItemType::File as i32,
            INodeType::Directory => ItemType::Directory as i32,
            INodeType::Symlink => ItemType::Symlink as i32,
        }),
        mode: Some(inode.permissions() as u32),
        link_count: Some(if inode.inode_type() == INodeType::Directory { 2 } else { 1 }),
        uid: Some(unsafe { libc::getuid() }),
        gid: Some(unsafe { libc::getgid() }),
        size: Some(inode.size()),
        alloc_size: Some(inode.size()),
        modify_time: Some(to_proto_timestamp(inode.mtime())),
        access_time: Some(to_proto_timestamp(inode.mtime())),
        change_time: Some(to_proto_timestamp(inode.mtime())),
        // ... other fields
    }
}

/// Convert VFS INode to FSKit Item.
///
/// # Arguments
/// * `inode` - VFS inode to convert
/// * `name` - Item name (filename)
fn to_item(inode: &dyn INode, name: &str) -> Item {
    Item {
        item_id: inode.id(),
        name: name.as_bytes().to_vec(),
        attributes: Some(to_item_attributes(inode)),
    }
}
```

## Runtime Model: FSKit vs FUSE

### Key Difference: FSKit is Already Async

Unlike FUSE (which uses synchronous callbacks requiring `AsyncExecutor` bridging), FSKit-rs runs within a Tokio runtime:

```rust
// fskit-rs/src/socket.rs - handler runs in async context
tokio::spawn(async move {
    handle_stream(stream, handler, shutdown_rx).await
});

// Inside handle_stream:
match handler.handle(content).await {  // Already async!
    Ok(content) => Some(content),
    // ...
}
```

**Implication**: We do NOT need the `AsyncExecutor` bridge. All `Filesystem` trait methods are `async fn` and can directly `.await` on async operations like S3 fetches.

### Thread Safety: Clone Requirement

FSKit-rs requires `Filesystem: Clone + Send + Sync + 'static` because handlers are cloned per-connection:

```rust
pub async fn mount<FS>(fs: FS, opts: MountOptions) -> Result<Session>
where
    FS: Filesystem + Send + Sync + Clone + 'static,
```

This is why we wrap all state in `Arc<Inner>` - cloning the outer struct just clones the Arc pointer.

## Memory Allocation Optimizations

### Zero-Copy Where Possible

FSKit-rs passes data with specific ownership semantics that we can leverage:

| Method | Data Parameter | Optimization |
|--------|---------------|--------------|
| `write()` | `contents: Vec<u8>` | **Owned** - no clone needed, can move directly to dirty manager |
| `read()` | Returns `Vec<u8>` | Allocate once, return owned |
| `create_symbolic_link()` | `contents: Vec<u8>` | **Owned** - move directly |
| `rename_item()` | Returns `Vec<u8>` | Allocate once for new name |

### Write Path Optimization

```rust
async fn write(&mut self, contents: Vec<u8>, item_id: u64, offset: i64) -> Result<i64> {
    // FSKit gives us owned Vec<u8> - no clone needed!
    // FUSE gives &[u8] requiring clone for async path
    
    let offset_u64: u64 = offset as u64;
    
    // Fast path: sync write for already-dirty files
    // Pass slice reference - dirty manager can copy to its buffer
    if let Some(result) = self.inner.dirty_manager.write_sync(item_id, offset_u64, &contents) {
        return result
            .map(|written| written as i64)
            .map_err(|_| Error::Posix(libc::EIO));
    }
    
    // Async path: can move the Vec directly (no clone!)
    self.inner.dirty_manager.write_owned(item_id, offset_u64, contents).await
        .map(|written| written as i64)
        .map_err(|_| Error::Posix(libc::EIO))
}
```

### Read Path Optimization

```rust
async fn read(&mut self, item_id: u64, offset: i64, length: i64) -> Result<Vec<u8>> {
    // Return owned Vec<u8> - FSKit takes ownership
    // Memory pool can provide pre-allocated buffers
    
    let offset_u64: u64 = offset as u64;
    let size: u32 = length as u32;
    
    // Try to get data from memory pool (already allocated)
    if let Some(pooled_data) = self.inner.pool.get_cached(item_id, offset_u64, size) {
        // Clone from pool - pool retains for future reads
        return Ok(pooled_data.to_vec());
    }
    
    // Fetch and cache
    // ...
}
```

### Comparison: FUSE vs FSKit Memory Handling

| Aspect | FUSE | FSKit |
|--------|------|-------|
| Write input | `&[u8]` (borrowed) | `Vec<u8>` (owned) |
| Clone needed for async write | Yes | No |
| Read output | `reply.data(&[u8])` | `Vec<u8>` return |
| Executor bridge | Required (sync→async) | Not needed |
| Buffer ownership | Kernel owns | Rust owns |

This means FSKit can be more efficient for write-heavy workloads since we avoid the clone that FUSE requires when bridging sync callbacks to async operations.

## Implementation

### WritableFsKit Structure

```rust
/// Writable FSKit filesystem implementation for Deadline Cloud job attachments.
/// 
/// Implements `fskit_rs::Filesystem` trait with full COW write support.
/// 
/// # Thread Safety
/// All mutable state is wrapped in `Arc` with interior mutability (`RwLock`/`DashMap`).
/// The struct implements `Clone` by cloning Arc pointers (cheap).
/// 
/// # Async Model
/// Unlike FUSE, FSKit-rs runs in a Tokio context, so we can directly `.await`
/// on async operations without needing an `AsyncExecutor` bridge.
#[derive(Clone)]
pub struct WritableFsKit {
    /// Shared state (wrapped in Arc for Clone requirement).
    inner: Arc<WritableFsKitInner>,
}

struct WritableFsKitInner {
    /// Inode manager for file metadata.
    inodes: Arc<INodeManager>,
    /// File store for fetching content from S3.
    store: Arc<dyn FileStore>,
    /// Memory pool for caching content.
    pool: Arc<MemoryPool>,
    /// Optional disk cache for read-only content.
    read_cache: Option<Arc<ReadCache>>,
    /// Dirty file manager (COW layer).
    dirty_manager: Arc<DirtyFileManager>,
    /// Dirty directory manager.
    dirty_dir_manager: Arc<DirtyDirManager>,
    /// Original directories from manifest (for diff tracking).
    original_dirs: HashSet<String>,
    /// Hash algorithm from manifest.
    hash_algorithm: HashAlgorithm,
    /// VFS options.
    options: FsKitVfsOptions,
    /// Write options.
    write_options: FsKitWriteOptions,
    /// Directory enumeration state (verifiers).
    enum_state: RwLock<HashMap<u64, u64>>,
    /// Next inode ID for new files (starts at 0x8000_0000).
    next_inode: AtomicU64,
    /// VFS creation time for uptime tracking.
    start_time: Instant,
}

// Note: No AsyncExecutor needed - FSKit runs in Tokio context
```

### Constructor

```rust
impl WritableFsKit {
    /// Create a new writable FSKit VFS.
    ///
    /// # Arguments
    /// * `manifest` - Manifest to mount
    /// * `store` - File store for reading original content
    /// * `options` - VFS options
    /// * `write_options` - Write-specific options
    pub fn new(
        manifest: &Manifest,
        store: Arc<dyn FileStore>,
        options: FsKitVfsOptions,
        write_options: FsKitWriteOptions,
    ) -> Result<Self, VfsError> {
        let inodes: Arc<INodeManager> = Arc::new(build_from_manifest(manifest));
        let hash_algorithm: HashAlgorithm = manifest.hash_alg();
        let pool = Arc::new(MemoryPool::new(options.pool.clone()));

        // Initialize read cache if enabled
        let read_cache: Option<Arc<ReadCache>> = if options.read_cache.enabled {
            let cache_options = ReadCacheOptions {
                cache_dir: options.read_cache.cache_dir.clone(),
                write_through: options.read_cache.write_through,
            };
            Some(Arc::new(ReadCache::new(cache_options)?))
        } else {
            None
        };

        // Initialize write cache (MaterializedCache)
        let write_cache: Arc<dyn WriteCache> = Arc::new(
            MaterializedCache::new(write_options.cache_dir.clone())?
        );

        // Create DirtyFileManager with unified memory pool
        let mut dirty_manager = DirtyFileManager::new(
            write_cache,
            store.clone(),
            inodes.clone(),
            pool.clone(),
        );
        if let Some(ref rc) = read_cache {
            dirty_manager.set_read_cache(rc.clone());
        }
        let dirty_manager = Arc::new(dirty_manager);

        // Collect original directories for diff tracking
        let original_dirs: HashSet<String> = match manifest {
            Manifest::V2025_12_04_beta(m) => {
                m.dirs.iter().map(|d| d.name.clone()).collect()
            }
            Manifest::V2023_03_03(_) => HashSet::new(),
        };

        // Create dirty directory manager
        let dirty_dir_manager = Arc::new(DirtyDirManager::new(
            inodes.clone(),
            original_dirs.clone(),
        ));

        // NOTE: No AsyncExecutor needed - FSKit-rs runs in Tokio context
        // All Filesystem trait methods are async fn and can directly .await

        Ok(Self {
            inner: Arc::new(WritableFsKitInner {
                inodes,
                store,
                pool,
                read_cache,
                dirty_manager,
                dirty_dir_manager,
                original_dirs,
                hash_algorithm,
                options,
                write_options,
                enum_state: RwLock::new(HashMap::new()),
                next_inode: AtomicU64::new(0x8000_0000),
                start_time: Instant::now(),
            }),
        })
    }

    /// Get the dirty file manager.
    ///
    /// # Returns
    /// Reference to the dirty file manager for external access.
    pub fn dirty_manager(&self) -> &Arc<DirtyFileManager> {
        &self.inner.dirty_manager
    }

    /// Get the dirty directory manager.
    ///
    /// # Returns
    /// Reference to the dirty directory manager for external access.
    pub fn dirty_dir_manager(&self) -> &Arc<DirtyDirManager> {
        &self.inner.dirty_dir_manager
    }

    /// Get a stats collector for this writable VFS.
    ///
    /// The collector can be cloned and used from another thread to query
    /// statistics without blocking filesystem operations.
    ///
    /// # Returns
    /// Collector that provides memory pool stats, cache hit rates, and dirty file lists.
    pub fn stats_collector(&self) -> WritableVfsStatsCollector {
        WritableVfsStatsCollector::new(
            self.inner.pool.clone(),
            self.inner.dirty_manager.clone(),
            self.inner.inodes.inode_count(),
            self.inner.start_time,
        )
    }
}
```

### Key Method Implementations

#### activate (Mount)

```rust
async fn activate(&mut self, _options: TaskOptions) -> Result<Item> {
    // Return root directory item
    let root: Arc<dyn INode> = self.inner.inodes.get(ROOT_INODE)
        .ok_or(Error::Posix(libc::EIO))?;
    
    Ok(to_item(root.as_ref(), ""))
}
```

#### lookup_item

```rust
async fn lookup_item(&mut self, name: &OsStr, directory_id: u64) -> Result<Item> {
    let name_str: &str = name.to_str().ok_or(Error::Posix(libc::EINVAL))?;
    
    let parent: Arc<dyn INode> = self.inner.inodes.get(directory_id)
        .ok_or(Error::Posix(libc::ENOENT))?;
    
    if parent.inode_type() != INodeType::Directory {
        return Err(Error::Posix(libc::ENOTDIR));
    }
    
    // Build full path
    let path: String = if parent.path().is_empty() {
        name_str.to_string()
    } else {
        format!("{}/{}", parent.path(), name_str)
    };
    
    let child: Arc<dyn INode> = self.inner.inodes.get_by_path(&path)
        .ok_or(Error::Posix(libc::ENOENT))?;
    
    Ok(to_item(child.as_ref(), name_str))
}
```

#### enumerate_directory

```rust
async fn enumerate_directory(
    &mut self,
    directory_id: u64,
    cookie: u64,
    verifier: u64,
) -> Result<DirectoryEntries> {
    let inode: Arc<dyn INode> = self.inner.inodes.get(directory_id)
        .ok_or(Error::Posix(libc::ENOENT))?;
    
    if inode.inode_type() != INodeType::Directory {
        return Err(Error::Posix(libc::ENOTDIR));
    }
    
    // Check verifier (detect directory changes)
    let current_verifier: u64 = self.get_dir_verifier(directory_id);
    if verifier != 0 && verifier != current_verifier {
        return Err(Error::Posix(libc::EINVAL)); // FSError::invalidDirectoryCookie
    }
    
    let mut entries: Vec<directory_entries::Entry> = Vec::new();
    let mut next_cookie: u64 = 0;
    
    // Add . and .. for initial enumeration
    if cookie == 0 {
        entries.push(make_dir_entry(".", directory_id, ItemType::Directory, 1));
        entries.push(make_dir_entry("..", inode.parent_id(), ItemType::Directory, 2));
        next_cookie = 2;
    }
    
    // Add children
    if let Some(children) = self.inner.inodes.get_dir_children(directory_id) {
        let children_vec: Vec<_> = children.into_iter().collect();
        let start_idx: usize = if cookie <= 2 { 0 } else { (cookie - 2) as usize };
        
        for (i, (name, child_id)) in children_vec.iter().enumerate().skip(start_idx) {
            if let Some(child) = self.inner.inodes.get(*child_id) {
                let item_type: ItemType = match child.inode_type() {
                    INodeType::File => ItemType::File,
                    INodeType::Directory => ItemType::Directory,
                    INodeType::Symlink => ItemType::Symlink,
                };
                entries.push(make_dir_entry(&name, *child_id, item_type, (i + 3) as u64));
                next_cookie = (i + 3) as u64;
            }
        }
    }
    
    Ok(DirectoryEntries {
        entries,
        verifier: current_verifier,
        initial: cookie == 0,
    })
}
```

#### read (with dirty file support)

```rust
async fn read(&mut self, item_id: u64, offset: i64, length: i64) -> Result<Vec<u8>> {
    // Check dirty layer first (COW files and new files)
    if self.inner.dirty_manager.is_dirty(item_id) {
        let offset_u64: u64 = offset as u64;
        let size: u32 = length as u32;
        
        // Try sync read first (fast path for already-loaded chunks)
        if let Some(data) = self.inner.dirty_manager.read_sync(item_id, offset_u64, size) {
            return Ok(data);
        }
        
        // Async path: need to load chunks from S3/cache
        return self.inner.dirty_manager.read(item_id, offset_u64, size).await
            .map_err(|_| Error::Posix(libc::EIO));
    }
    
    // Fall back to read-only store for manifest files
    let content: FileContent = self.inner.inodes.get_file_content(item_id)
        .ok_or(Error::Posix(libc::ENOENT))?;
    
    let inode: Arc<dyn INode> = self.inner.inodes.get(item_id)
        .ok_or(Error::Posix(libc::ENOENT))?;
    
    let file_size: u64 = inode.size();
    let off: u64 = offset as u64;
    
    if off >= file_size {
        return Ok(Vec::new());
    }
    
    let actual: u64 = (length as u64).min(file_size - off);
    
    match content {
        FileContent::SingleHash(hash) => {
            self.read_single_hash(&hash, off, actual).await
        }
        FileContent::Chunked(hashes) => {
            self.read_chunked(&hashes, off, actual).await
        }
    }
}
```

#### write (COW support)

```rust
async fn write(&mut self, contents: Vec<u8>, item_id: u64, offset: i64) -> Result<i64> {
    let offset_u64: u64 = offset as u64;
    
    // Try sync write first (fast path for already-dirty files)
    if let Some(result) = self.inner.dirty_manager.write_sync(item_id, offset_u64, &contents) {
        return result
            .map(|written| written as i64)
            .map_err(|_| Error::Posix(libc::EIO));
    }
    
    // Async path: need COW or chunk loading
    self.inner.dirty_manager.write(item_id, offset_u64, &contents).await
        .map(|written| written as i64)
        .map_err(|_| Error::Posix(libc::EIO))
}
```

#### create_item (file and directory creation)

```rust
async fn create_item(
    &mut self,
    name: &OsStr,
    r#type: ItemType,
    directory_id: u64,
    attributes: ItemAttributes,
) -> Result<Item> {
    let name_str: &str = name.to_str().ok_or(Error::Posix(libc::EINVAL))?;
    
    // Validate name
    if name_str.is_empty() || name_str.contains('/') {
        return Err(Error::Posix(libc::EINVAL));
    }
    
    // Get parent path
    let parent_path: String = self.get_parent_path(directory_id)?;
    
    // Build new item path
    let new_path: String = if parent_path.is_empty() {
        name_str.to_string()
    } else {
        format!("{}/{}", parent_path, name_str)
    };
    
    match r#type {
        ItemType::File => self.create_file(name_str, directory_id, new_path, &attributes).await,
        ItemType::Directory => self.create_directory(name_str, directory_id, &attributes).await,
        _ => Err(Error::Posix(libc::EINVAL)),
    }
}

/// Create a new file.
async fn create_file(
    &self,
    name: &str,
    parent_id: u64,
    path: String,
    attributes: &ItemAttributes,
) -> Result<Item> {
    // Check if file already exists
    if self.inner.inodes.get_by_path(&path).is_some() {
        return Err(Error::Posix(libc::EEXIST));
    }
    
    // Allocate new inode ID (high range to avoid manifest conflicts)
    let new_ino: u64 = self.inner.next_inode.fetch_add(1, Ordering::SeqCst);
    
    // Create dirty file entry
    self.inner.dirty_manager.create_file(new_ino, path.clone(), parent_id)
        .map_err(|_| Error::Posix(libc::EIO))?;
    
    // Build response item
    Ok(Item {
        item_id: new_ino,
        name: name.as_bytes().to_vec(),
        attributes: Some(ItemAttributes {
            item_id: Some(new_ino),
            parent_id: Some(parent_id),
            r#type: Some(ItemType::File as i32),
            mode: attributes.mode.or(Some(0o644)),
            size: Some(0),
            ..Default::default()
        }),
    })
}

/// Create a new directory.
async fn create_directory(
    &self,
    name: &str,
    parent_id: u64,
    attributes: &ItemAttributes,
) -> Result<Item> {
    let new_id: u64 = self.inner.dirty_dir_manager.create_dir(parent_id, name)
        .map_err(|e| match e {
            VfsError::AlreadyExists(_) => Error::Posix(libc::EEXIST),
            VfsError::NotADirectory(_) => Error::Posix(libc::ENOTDIR),
            VfsError::InodeNotFound(_) => Error::Posix(libc::ENOENT),
            _ => Error::Posix(libc::EIO),
        })?;
    
    Ok(Item {
        item_id: new_id,
        name: name.as_bytes().to_vec(),
        attributes: Some(ItemAttributes {
            item_id: Some(new_id),
            parent_id: Some(parent_id),
            r#type: Some(ItemType::Directory as i32),
            mode: attributes.mode.or(Some(0o755)),
            link_count: Some(2),
            ..Default::default()
        }),
    })
}
```

#### remove_item (file and directory deletion)

```rust
async fn remove_item(
    &mut self,
    item_id: u64,
    name: &OsStr,
    directory_id: u64,
) -> Result<()> {
    let name_str: &str = name.to_str().ok_or(Error::Posix(libc::EINVAL))?;
    
    // Check if it's a file or directory
    let is_directory: bool = if let Some(inode) = self.inner.inodes.get(item_id) {
        inode.inode_type() == INodeType::Directory
    } else if self.inner.dirty_dir_manager.is_new_dir(item_id) {
        true
    } else if self.inner.dirty_manager.is_new_file(item_id) {
        false
    } else {
        return Err(Error::Posix(libc::ENOENT));
    };
    
    if is_directory {
        self.remove_directory(directory_id, name_str).await
    } else {
        self.remove_file(item_id).await
    }
}

/// Remove a file.
async fn remove_file(&self, item_id: u64) -> Result<()> {
    self.inner.dirty_manager.delete_file(item_id).await
        .map_err(|_| Error::Posix(libc::EIO))
}

/// Remove an empty directory.
async fn remove_directory(&self, parent_id: u64, name: &str) -> Result<()> {
    // Get directory path for cleanup
    let dir_path: String = self.get_child_path(parent_id, name)?;
    
    // Clean up any new files under this directory
    let _: usize = self.inner.dirty_manager.remove_new_files_under_path(&dir_path);
    
    // Delete directory
    self.inner.dirty_dir_manager.delete_dir(parent_id, name)
        .map_err(|e| match e {
            VfsError::NotFound(_) => Error::Posix(libc::ENOENT),
            VfsError::NotADirectory(_) => Error::Posix(libc::ENOTDIR),
            VfsError::DirectoryNotEmpty(_) => Error::Posix(libc::ENOTEMPTY),
            _ => Error::Posix(libc::EIO),
        })
}
```

#### set_attributes (truncate support)

```rust
async fn set_attributes(
    &mut self,
    item_id: u64,
    attributes: ItemAttributes,
) -> Result<ItemAttributes> {
    // Handle truncate (size change)
    if let Some(new_size) = attributes.size {
        self.inner.dirty_manager.truncate(item_id, new_size).await
            .map_err(|_| Error::Posix(libc::EIO))?;
    }
    
    // Return updated attributes
    self.get_attributes(item_id).await
}
```

#### get_attributes

```rust
async fn get_attributes(&mut self, item_id: u64) -> Result<ItemAttributes> {
    // Check manifest inodes first
    if let Some(inode) = self.inner.inodes.get(item_id) {
        // Check if deleted
        if self.inner.dirty_manager.get_state(item_id) == Some(DirtyState::Deleted) {
            return Err(Error::Posix(libc::ENOENT));
        }
        if self.inner.dirty_dir_manager.get_state(item_id) == Some(DirtyDirState::Deleted) {
            return Err(Error::Posix(libc::ENOENT));
        }
        
        let mut attrs: ItemAttributes = to_item_attributes(inode.as_ref());
        
        // Override with dirty state if applicable
        if let Some(size) = self.inner.dirty_manager.get_size(item_id) {
            attrs.size = Some(size);
        }
        if let Some(mtime) = self.inner.dirty_manager.get_mtime(item_id) {
            attrs.modify_time = Some(to_proto_timestamp(mtime));
            attrs.access_time = Some(to_proto_timestamp(mtime));
        }
        
        return Ok(attrs);
    }
    
    // Check new files
    if self.inner.dirty_manager.is_new_file(item_id) {
        let size: u64 = self.inner.dirty_manager.get_size(item_id).unwrap_or(0);
        let mtime: SystemTime = self.inner.dirty_manager.get_mtime(item_id)
            .unwrap_or(SystemTime::now());
        
        return Ok(ItemAttributes {
            item_id: Some(item_id),
            r#type: Some(ItemType::File as i32),
            mode: Some(0o644),
            size: Some(size),
            modify_time: Some(to_proto_timestamp(mtime)),
            access_time: Some(to_proto_timestamp(mtime)),
            ..Default::default()
        });
    }
    
    // Check new directories
    if self.inner.dirty_dir_manager.is_new_dir(item_id) {
        return Ok(ItemAttributes {
            item_id: Some(item_id),
            r#type: Some(ItemType::Directory as i32),
            mode: Some(0o755),
            link_count: Some(2),
            ..Default::default()
        });
    }
    
    Err(Error::Posix(libc::ENOENT))
}
```

#### close_item (flush on close)

```rust
async fn close_item(&mut self, item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
    // Flush dirty file to disk on close
    if self.inner.dirty_manager.is_dirty(item_id) {
        self.inner.dirty_manager.flush_to_disk(item_id).await
            .map_err(|_| Error::Posix(libc::EIO))?;
    }
    Ok(())
}
```

#### synchronize (fsync)

```rust
async fn synchronize(&mut self, _flags: SyncFlags) -> Result<()> {
    // Flush all dirty files to disk
    self.inner.dirty_manager.flush_all().await
        .map_err(|_| Error::Posix(libc::EIO))
}
```

#### lookup_item (with dirty file/dir support)

```rust
async fn lookup_item(&mut self, name: &OsStr, directory_id: u64) -> Result<Item> {
    let name_str: &str = name.to_str().ok_or(Error::Posix(libc::EINVAL))?;
    
    // Get parent path - check manifest inodes first, then new directories
    let parent_path: String = self.get_parent_path(directory_id)?;
    
    let path: String = if parent_path.is_empty() {
        name_str.to_string()
    } else {
        format!("{}/{}", parent_path, name_str)
    };
    
    // Check manifest files first
    if let Some(child) = self.inner.inodes.get_by_path(&path) {
        // Check if deleted
        if self.inner.dirty_manager.get_state(child.id()) == Some(DirtyState::Deleted) {
            return Err(Error::Posix(libc::ENOENT));
        }
        if self.inner.dirty_dir_manager.get_state(child.id()) == Some(DirtyDirState::Deleted) {
            return Err(Error::Posix(libc::ENOENT));
        }
        
        return Ok(self.to_item_with_dirty_attrs(child.as_ref(), name_str));
    }
    
    // Check new files
    if let Some(new_ino) = self.inner.dirty_manager.lookup_new_file(directory_id, name_str) {
        return Ok(self.new_file_to_item(new_ino, name_str));
    }
    
    // Check new directories
    if let Some(new_ino) = self.inner.dirty_dir_manager.lookup_new_dir(directory_id, name_str) {
        return Ok(self.new_dir_to_item(new_ino, name_str));
    }
    
    Err(Error::Posix(libc::ENOENT))
}
```

#### enumerate_directory (with dirty entries)

```rust
async fn enumerate_directory(
    &mut self,
    directory_id: u64,
    cookie: u64,
    verifier: u64,
) -> Result<DirectoryEntries> {
    // Get directory inode (manifest or new)
    let (dir_path, parent_id): (String, u64) = if let Some(inode) = self.inner.inodes.get(directory_id) {
        if inode.inode_type() != INodeType::Directory {
            return Err(Error::Posix(libc::ENOTDIR));
        }
        (inode.path().to_string(), inode.parent_id())
    } else if let Some(path) = self.inner.dirty_dir_manager.get_new_dir_path(directory_id) {
        (path, directory_id) // New dir
    } else {
        return Err(Error::Posix(libc::ENOENT));
    };
    
    // Check verifier
    let current_verifier: u64 = self.get_dir_verifier(directory_id);
    if verifier != 0 && verifier != current_verifier {
        return Err(Error::Posix(libc::EINVAL));
    }
    
    let mut entries: Vec<directory_entries::Entry> = Vec::new();
    
    // Add . and .. for initial enumeration
    if cookie == 0 {
        entries.push(make_dir_entry(".", directory_id, ItemType::Directory, 1));
        entries.push(make_dir_entry("..", parent_id, ItemType::Directory, 2));
    }
    
    // Add manifest children (excluding deleted)
    if let Some(children) = self.inner.inodes.get_dir_children(directory_id) {
        let start_idx: usize = if cookie <= 2 { 0 } else { (cookie - 2) as usize };
        
        for (i, (name, child_id)) in children.into_iter().enumerate().skip(start_idx) {
            // Skip deleted files
            if self.inner.dirty_manager.get_state(child_id) == Some(DirtyState::Deleted) {
                continue;
            }
            // Skip deleted directories
            if self.inner.dirty_dir_manager.get_state(child_id) == Some(DirtyDirState::Deleted) {
                continue;
            }
            
            if let Some(child) = self.inner.inodes.get(child_id) {
                let item_type: ItemType = match child.inode_type() {
                    INodeType::File => ItemType::File,
                    INodeType::Directory => ItemType::Directory,
                    INodeType::Symlink => ItemType::Symlink,
                };
                entries.push(make_dir_entry(&name, child_id, item_type, (i + 3) as u64));
            }
        }
    }
    
    // Add new files created in this directory
    let new_files: Vec<(u64, String)> = self.inner.dirty_manager.get_new_files_in_dir(directory_id);
    for (new_ino, name) in new_files {
        entries.push(make_dir_entry(&name, new_ino, ItemType::File, entries.len() as u64 + 1));
    }
    
    // Add new directories created in this directory
    let new_dirs: Vec<(u64, String)> = self.inner.dirty_dir_manager.get_new_dirs_in_parent(directory_id);
    for (new_ino, name) in new_dirs {
        entries.push(make_dir_entry(&name, new_ino, ItemType::Directory, entries.len() as u64 + 1));
    }
    
    Ok(DirectoryEntries {
        entries,
        verifier: current_verifier,
        initial: cookie == 0,
    })
}
```

### Volume Configuration Methods

```rust
async fn get_resource_identifier(&mut self) -> Result<ResourceIdentifier> {
    Ok(ResourceIdentifier {
        name: Some(self.inner.options.volume_name.clone()),
        container_id: Some(self.inner.options.volume_id.clone()),
    })
}

async fn get_volume_identifier(&mut self) -> Result<VolumeIdentifier> {
    Ok(VolumeIdentifier {
        id: Some(self.inner.options.volume_id.clone()),
        name: Some(self.inner.options.volume_name.clone()),
    })
}

async fn get_volume_behavior(&mut self) -> Result<VolumeBehavior> {
    Ok(VolumeBehavior {
        // Default behaviors - can be customized
        ..Default::default()
    })
}

async fn get_path_conf_operations(&mut self) -> Result<PathConfOperations> {
    Ok(PathConfOperations {
        max_name_length: Some(255),
        max_path_length: Some(1024),
        // ... other path configuration
        ..Default::default()
    })
}

async fn get_volume_capabilities(&mut self) -> Result<SupportedCapabilities> {
    Ok(SupportedCapabilities {
        // Writable filesystem capabilities
        supports_hard_links: Some(false),
        supports_symbolic_links: Some(true),
        case_format: Some(CaseFormat::Sensitive as i32),
        supports_sparse_files: Some(true),
        // ... other capabilities
    })
}

async fn get_volume_statistics(&mut self) -> Result<StatFsResult> {
    Ok(StatFsResult {
        total_blocks: Some(self.inner.inodes.inode_count() as u64),
        available_blocks: Some(u64::MAX), // Writable - report large available
        free_blocks: Some(u64::MAX),
        block_size: Some(512),
        // ... other stats
    })
}
```

### Lifecycle Methods

```rust
async fn mount(&mut self, _options: TaskOptions) -> Result<()> {
    // FSKit calls mount before activate
    // No-op for our implementation - state already initialized in constructor
    Ok(())
}

async fn unmount(&mut self) -> Result<()> {
    // Flush all dirty files before unmount
    self.inner.dirty_manager.flush_all().await
        .map_err(|_| Error::Posix(libc::EIO))
}

async fn deactivate(&mut self) -> Result<()> {
    // Cleanup after volume is deactivated
    // Flush any remaining dirty state
    let _ = self.inner.dirty_manager.flush_all().await;
    Ok(())
}

async fn reclaim_item(&mut self, item_id: u64) -> Result<()> {
    // FSKit calls this when kernel no longer references an item
    // We can release any cached resources for this item
    // For now, no-op - memory pool handles eviction automatically
    tracing::trace!("reclaim_item: {}", item_id);
    Ok(())
}

async fn deactivate_item(&mut self, item_id: u64) -> Result<()> {
    // Kernel is no longer making immediate use of this item
    // Opportunity to release resources, but item may be accessed again
    tracing::trace!("deactivate_item: {}", item_id);
    Ok(())
}
```

### File Open/Close Tracking

```rust
async fn open_item(&mut self, item_id: u64, modes: Vec<OpenMode>) -> Result<()> {
    // Track open modes for the item
    // Check if deleted
    if self.inner.dirty_manager.get_state(item_id) == Some(DirtyState::Deleted) {
        return Err(Error::Posix(libc::ENOENT));
    }
    
    // Validate item exists (manifest or new file)
    if self.inner.inodes.get(item_id).is_none() 
        && !self.inner.dirty_manager.is_new_file(item_id) {
        return Err(Error::Posix(libc::ENOENT));
    }
    
    tracing::trace!("open_item: {} modes: {:?}", item_id, modes);
    Ok(())
}

async fn close_item(&mut self, item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
    // Flush dirty file to disk on close
    if self.inner.dirty_manager.is_dirty(item_id) {
        self.inner.dirty_manager.flush_to_disk(item_id).await
            .map_err(|_| Error::Posix(libc::EIO))?;
    }
    Ok(())
}
```

### Symlink and Link Operations

```rust
async fn read_symbolic_link(&mut self, item_id: u64) -> Result<Vec<u8>> {
    // Read symlink target from manifest
    match self.inner.inodes.get_symlink_target(item_id) {
        Some(target) => Ok(target.as_bytes().to_vec()),
        None => Err(Error::Posix(libc::EINVAL)),
    }
}

async fn create_symbolic_link(
    &mut self,
    name: &OsStr,
    directory_id: u64,
    _new_attributes: ItemAttributes,
    contents: Vec<u8>,
) -> Result<Item> {
    // Symlink creation not supported in writable VFS
    // (matches FUSE implementation which doesn't implement symlink)
    let _ = (name, directory_id, contents);
    Err(Error::Posix(libc::ENOSYS))
}

async fn create_link(
    &mut self,
    _item_id: u64,
    _name: &OsStr,
    _directory_id: u64,
) -> Result<Vec<u8>> {
    // Hard links not supported
    Err(Error::Posix(libc::ENOSYS))
}
```

### Rename Operation

```rust
async fn rename_item(
    &mut self,
    item_id: u64,
    source_directory_id: u64,
    source_name: &OsStr,
    destination_name: &OsStr,
    destination_directory_id: u64,
    over_item_id: Option<u64>,
) -> Result<Vec<u8>> {
    // Rename not yet implemented
    // TODO: Implement rename support in DirtyFileManager and DirtyDirManager
    let _ = (item_id, source_directory_id, source_name, destination_name, 
             destination_directory_id, over_item_id);
    Err(Error::Posix(libc::ENOSYS))
}
```

### Access Control

```rust
async fn check_access(&mut self, item_id: u64, access: Vec<AccessMask>) -> Result<bool> {
    // Check if item exists and is not deleted
    if self.inner.dirty_manager.get_state(item_id) == Some(DirtyState::Deleted) {
        return Err(Error::Posix(libc::ENOENT));
    }
    if self.inner.dirty_dir_manager.get_state(item_id) == Some(DirtyDirState::Deleted) {
        return Err(Error::Posix(libc::ENOENT));
    }
    
    // Check manifest inodes
    if self.inner.inodes.get(item_id).is_some() {
        // Allow all access for existing items (FSKit uses noowners)
        return Ok(true);
    }
    
    // Check new files/directories
    if self.inner.dirty_manager.is_new_file(item_id) 
        || self.inner.dirty_dir_manager.is_new_dir(item_id) {
        return Ok(true);
    }
    
    let _ = access;
    Err(Error::Posix(libc::ENOENT))
}
```

### Extended Attributes (xattr)

```rust
async fn get_supported_xattr_names(&mut self, _item_id: u64) -> Result<Xattrs> {
    // No extended attributes supported
    Ok(Xattrs { names: vec![] })
}

async fn get_xattr(&mut self, _name: &OsStr, _item_id: u64) -> Result<Vec<u8>> {
    // Extended attributes not supported
    Err(Error::Posix(libc::ENOATTR))
}

async fn set_xattr(
    &mut self,
    _name: &OsStr,
    _value: Option<Vec<u8>>,
    _item_id: u64,
    _policy: SetXattrPolicy,
) -> Result<()> {
    // Extended attributes not supported
    Err(Error::Posix(libc::ENOSYS))
}

async fn get_xattrs(&mut self, _item_id: u64) -> Result<Xattrs> {
    // Return empty list - no xattrs
    Ok(Xattrs { names: vec![] })
}
```

### Volume Management

```rust
async fn set_volume_name(&mut self, _name: Vec<u8>) -> Result<Vec<u8>> {
    // Volume name is read-only
    Err(Error::Posix(libc::ENOSYS))
}

async fn preallocate_space(
    &mut self,
    _item_id: u64,
    _offset: i64,
    _length: i64,
    _flags: Vec<PreallocateFlag>,
) -> Result<i64> {
    // Preallocation not supported - writes allocate on demand
    Err(Error::Posix(libc::ENOSYS))
}
```

### DiffManifestExporter Implementation

```rust
#[async_trait]
impl DiffManifestExporter for WritableFsKit {
    /// Export changes as a diff manifest.
    async fn export_diff_manifest(
        &self,
        parent_manifest: &Manifest,
        parent_encoded: &str,
    ) -> Result<Manifest, VfsError> {
        // Delegate to dirty managers for diff generation
        // TODO: Full implementation
        Err(VfsError::MountFailed("Not yet implemented".to_string()))
    }

    /// Clear all dirty state after successful export.
    fn clear_dirty(&self) -> Result<(), VfsError> {
        self.inner.dirty_manager.clear();
        self.inner.dirty_dir_manager.clear();
        Ok(())
    }

    /// Get summary of dirty state.
    fn dirty_summary(&self) -> DirtySummary {
        let entries: Vec<DirtyEntry> = self.inner.dirty_manager.get_dirty_entries();
        let dir_entries: Vec<DirtyDirEntry> = self.inner.dirty_dir_manager.get_dirty_dir_entries();

        let mut summary = DirtySummary::default();

        for entry in entries {
            match entry.state {
                DirtyState::New => summary.new_count += 1,
                DirtyState::Modified => summary.modified_count += 1,
                DirtyState::Deleted => summary.deleted_count += 1,
            }
        }

        for dir_entry in dir_entries {
            match dir_entry.state {
                DirtyDirState::New => summary.new_dir_count += 1,
                DirtyDirState::Deleted => summary.deleted_dir_count += 1,
            }
        }

        summary
    }
}
```

## Project Structure

```
crates/vfs-fskit/
├── Cargo.toml
├── README.md
├── src/
│   ├── lib.rs              # Public API exports
│   ├── error.rs            # FsKitVfsError enum
│   ├── options.rs          # FsKitVfsOptions, FsKitWriteOptions, FsKitMountOptions
│   ├── fskit.rs            # WritableFsKit implementation
│   ├── convert.rs          # INode ↔ ItemAttributes conversion
│   ├── helpers.rs          # Path helpers, directory entry builders
│   └── stats.rs            # Statistics collection (WritableVfsStatsCollector)
└── examples/
    └── mount_fskit.rs      # CLI example with writable support
```

## Cargo.toml

```toml
[package]
name = "rusty-attachments-vfs-fskit"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "FSKit-based virtual filesystem for Deadline Cloud job attachments (macOS 15.4+)"

[target.'cfg(target_os = "macos")'.dependencies]
fskit-rs = { git = "https://github.com/debox-network/fskit-rs" }

[dependencies]
async-trait = "0.1"
libc = "0.2"
parking_lot = "0.12"
thiserror = "1.0"
tokio = { version = "1", features = ["rt-multi-thread", "sync"] }
tracing = "0.1"
uuid = { version = "1.0", features = ["v4"] }

# Internal crates (shared primitives)
rusty-attachments-common = { path = "../common" }
rusty-attachments-model = { path = "../model" }
rusty-attachments-storage = { path = "../storage" }
rusty-attachments-storage-crt = { path = "../storage-crt" }

# Shared VFS primitives (extracted or re-exported)
rusty-attachments-vfs = { path = "../vfs", default-features = false }
```

## Usage Example

```rust
use rusty_attachments_vfs_fskit::{
    WritableFsKit, FsKitVfsOptions, FsKitWriteOptions, FsKitMountOptions
};
use rusty_attachments_vfs::WritableVfsStatsCollector;
use rusty_attachments_model::Manifest;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load manifest
    let manifest_json: String = std::fs::read_to_string("manifest.json")?;
    let manifest: Manifest = Manifest::decode(&manifest_json)?;
    
    // Create storage client
    let settings = StorageSettings::default();
    let client = CrtStorageClient::new(settings).await?;
    let location = S3Location::new("bucket", "root", "Data", "Manifests");
    let store: Arc<dyn FileStore> = Arc::new(StorageClientAdapter::new(client, location));
    
    // Create writable FSKit VFS
    let options = FsKitVfsOptions {
        volume_name: "My Assets".to_string(),
        ..Default::default()
    };
    let write_options = FsKitWriteOptions {
        cache_dir: "/tmp/vfs-fskit-cache".into(),
        ..Default::default()
    };
    
    let vfs = WritableFsKit::new(&manifest, store, options, write_options)?;
    
    // Get stats collector for background monitoring
    let stats_collector: WritableVfsStatsCollector = vfs.stats_collector();
    
    // Mount via FSKit
    let mount_opts = FsKitMountOptions {
        mount_point: "/tmp/deadline-assets".into(),
        ..Default::default()
    };
    
    let session = fskit_rs::mount(vfs.clone(), mount_opts.into()).await?;
    
    // Spawn background stats thread (optional)
    let running = Arc::new(AtomicBool::new(true));
    let stats_handle = spawn_stats_thread(stats_collector, running.clone(), 5);
    
    println!("Mounted at /tmp/deadline-assets. Press Ctrl+C to unmount.");
    tokio::signal::ctrl_c().await?;
    
    // Stop stats thread
    running.store(false, Ordering::SeqCst);
    let _ = stats_handle.join();
    
    // Print dirty summary before unmount
    let summary = vfs.dirty_summary();
    println!("Changes: {} new, {} modified, {} deleted files",
        summary.new_count, summary.modified_count, summary.deleted_count);
    println!("         {} new, {} deleted directories",
        summary.new_dir_count, summary.deleted_dir_count);
    
    drop(session); // Unmounts
    Ok(())
}

/// Spawn a background thread that prints stats periodically.
///
/// # Arguments
/// * `collector` - Stats collector (cloneable, thread-safe)
/// * `running` - Atomic flag to signal shutdown
/// * `interval_secs` - Seconds between stats prints
///
/// # Returns
/// Join handle for the spawned thread.
fn spawn_stats_thread(
    collector: WritableVfsStatsCollector,
    running: Arc<AtomicBool>,
    interval_secs: u64,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while running.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_secs(interval_secs));
            
            let stats = collector.collect();
            println!("--- VFS Stats ---");
            println!("  Inodes: {}", stats.inode_count);
            println!("  Uptime: {}s", stats.uptime_secs);
            println!("  Cache hit rate: {:.1}%", stats.cache_hit_rate * 100.0);
            println!("  Pool: {} used / {} capacity",
                stats.pool_stats.used_bytes, stats.pool_stats.capacity_bytes);
            println!("  Dirty: {} new, {} modified, {} deleted",
                stats.dirty_summary.new_count,
                stats.dirty_summary.modified_count,
                stats.dirty_summary.deleted_count);
        }
    })
}
```

## Feature Parity with FUSE WritableVfs

### Core Features

| Feature | FUSE WritableVfs | FSKit WritableFsKit |
|---------|------------------|---------------------|
| Read manifest files | ✅ | ✅ |
| Read chunked files (V2) | ✅ | ✅ |
| Read symlinks | ✅ | ✅ |
| Memory pool caching | ✅ | ✅ (shared) |
| Disk read cache | ✅ | ✅ (shared) |
| COW file modification | ✅ | ✅ |
| Create new files | ✅ | ✅ |
| Create new directories | ✅ | ✅ |
| Delete files | ✅ | ✅ |
| Delete directories | ✅ | ✅ |
| Truncate files | ✅ | ✅ |
| Flush on close | ✅ | ✅ |
| MaterializedCache | ✅ | ✅ (shared) |
| DirtyFileManager | ✅ | ✅ (shared) |
| DirtyDirManager | ✅ | ✅ (shared) |
| DiffManifestExporter | ✅ | ✅ |
| Stats collection | ✅ | ✅ |

### FSKit Filesystem Trait Methods

| Method | Implementation | Notes |
|--------|---------------|-------|
| `get_resource_identifier()` | ✅ | Volume name and ID |
| `get_volume_identifier()` | ✅ | Volume UUID |
| `get_volume_behavior()` | ✅ | Default behaviors |
| `get_path_conf_operations()` | ✅ | Path limits |
| `get_volume_capabilities()` | ✅ | Feature flags |
| `get_volume_statistics()` | ✅ | Block counts |
| `mount()` | ✅ | No-op (state in constructor) |
| `unmount()` | ✅ | Flush dirty files |
| `synchronize()` | ✅ | Flush all to disk |
| `get_attributes()` | ✅ | With dirty state overlay |
| `set_attributes()` | ✅ | Truncate support |
| `lookup_item()` | ✅ | Manifest + dirty files/dirs |
| `reclaim_item()` | ✅ | Resource cleanup (no-op) |
| `read_symbolic_link()` | ✅ | From manifest |
| `create_item()` | ✅ | Files and directories |
| `create_symbolic_link()` | ❌ | Returns ENOSYS |
| `create_link()` | ❌ | Hard links not supported |
| `remove_item()` | ✅ | Files and directories |
| `rename_item()` | ❌ | TODO: Implement |
| `enumerate_directory()` | ✅ | With dirty entries |
| `activate()` | ✅ | Returns root item |
| `deactivate()` | ✅ | Cleanup |
| `get_supported_xattr_names()` | ✅ | Empty list |
| `get_xattr()` | ✅ | Returns ENOATTR |
| `set_xattr()` | ❌ | Returns ENOSYS |
| `get_xattrs()` | ✅ | Empty list |
| `open_item()` | ✅ | Validates item exists |
| `close_item()` | ✅ | Flush on close |
| `read()` | ✅ | Dirty + manifest fallback |
| `write()` | ✅ | COW support |
| `check_access()` | ✅ | Always allow (noowners) |
| `set_volume_name()` | ❌ | Read-only |
| `preallocate_space()` | ❌ | Not supported |
| `deactivate_item()` | ✅ | Resource hint (no-op) |

### Not Implemented (Future Work)

| Feature | Reason |
|---------|--------|
| `rename_item()` | Requires DirtyFileManager/DirtyDirManager changes |
| `create_symbolic_link()` | Not needed for job attachments use case |
| `create_link()` | Hard links not supported |
| Extended attributes | Not needed for job attachments use case |
| `preallocate_space()` | Writes allocate on demand |

## Platform Considerations

### macOS 15.4+ Requirement

FSKit requires macOS 15.4 or later. The crate should:

1. Use `#[cfg(target_os = "macos")]` for platform-specific code
2. Check macOS version at runtime and fail gracefully on older versions
3. Provide clear error messages about FSKit requirements

```rust
/// Check if FSKit is available on this system.
///
/// # Returns
/// True if macOS 15.4+ and FSKit is available.
pub fn fskit_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        // Check macOS version >= 15.4
        let version = macos_version();
        version.major >= 15 && (version.major > 15 || version.minor >= 4)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}
```

### FSKitBridge Installation

Users must install the FSKitBridge host app before mounting:

```rust
// Optional: Install FSKitBridge if not present
if !fskit_bridge_installed() {
    fskit_rs::install("/path/to/FSKitBridge.app", true)?;
}
```

## Testing Strategy

1. **Unit Tests**: Test conversion functions (`to_item_attributes`, `to_item`)
2. **Integration Tests**: Mount with mock `FileStore`, verify directory listing and write operations
3. **COW Tests**: Verify copy-on-write behavior matches FUSE implementation
4. **Manual Tests**: Mount real manifest, create/modify/delete files in Finder

## Migration Path

For users currently using the FUSE-based VFS:

| FUSE API | FSKit API |
|----------|-----------|
| `WritableVfs::new()` | `WritableFsKit::new()` |
| `fuser::mount2()` | `fskit_rs::mount()` |
| `VfsOptions` | `FsKitVfsOptions` |
| `VfsOptions.pool` | `FsKitVfsOptions.pool` |
| `VfsOptions.prefetch` | `FsKitVfsOptions.prefetch` |
| `VfsOptions.read_ahead` | `FsKitVfsOptions.read_ahead` |
| `VfsOptions.timeouts` | `FsKitVfsOptions.timeouts` |
| `VfsOptions.read_cache` | `FsKitVfsOptions.read_cache` |
| `VfsOptions.kernel_cache` | N/A (FSKit manages internally) |
| `VfsOptions.executor` | N/A (FSKit runs in Tokio) |
| `WriteOptions` | `FsKitWriteOptions` |
| `spawn_mount_writable()` | `fskit_rs::mount()` (async) |
| `dirty_manager()` | `dirty_manager()` |
| `dirty_dir_manager()` | `dirty_dir_manager()` |
| `stats_collector()` | `stats_collector()` |
| `dirty_summary()` | `dirty_summary()` |

The internal primitives (`INodeManager`, `MemoryPool`, `FileStore`, `DirtyFileManager`, `DirtyDirManager`, `MaterializedCache`, `WritableVfsStatsCollector`) remain unchanged and are shared between FUSE and FSKit implementations.

## Summary

This design creates a new `rusty-attachments-vfs-fskit` crate that:

1. **Full feature parity** with FUSE `WritableVfs` including COW writes, file/directory creation and deletion
2. **Reuses existing primitives** from the VFS crate (pyramid architecture): `DirtyFileManager`, `DirtyDirManager`, `MaterializedCache`, `MemoryPool`
3. **Implements `fskit_rs::Filesystem`** trait for FSKit integration
4. **Provides macOS-native** filesystem support without kernel extensions
5. **Maintains API similarity** with the FUSE-based VFS for easy migration
6. **Supports diff manifest export** for tracking changes
