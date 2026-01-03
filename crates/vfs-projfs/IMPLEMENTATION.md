# ProjFS VFS Implementation Summary

This document summarizes the implementation of the ProjFS-based virtual filesystem for Windows.

## Created Files

### Core Library Structure

```
crates/vfs-projfs/
├── Cargo.toml                          # Package configuration
├── README.md                           # User documentation
├── IMPLEMENTATION.md                   # This file
├── src/
│   ├── lib.rs                          # Public API and re-exports
│   ├── error.rs                        # Error types
│   ├── options.rs                      # Configuration options
│   │
│   ├── util/                           # Layer 0: Utilities
│   │   ├── mod.rs
│   │   ├── wstr.rs                     # Wide string conversion
│   │   ├── filetime.rs                 # FILETIME conversion
│   │   └── compare.rs                  # ProjFS file name comparison
│   │
│   ├── projection/                     # Layer 1: Backing store
│   │   ├── mod.rs
│   │   ├── types.rs                    # Data types
│   │   ├── folder.rs                   # Folder data structures
│   │   └── manifest.rs                 # Manifest projection
│   │
│   ├── callbacks/                      # Layer 2: Coordination
│   │   ├── mod.rs
│   │   ├── background.rs               # Background task runner
│   │   ├── modified_paths.rs           # Diff manifest tracking
│   │   ├── path_registry.rs            # Path↔inode mapping
│   │   └── vfs_callbacks.rs            # VFS callbacks with dirty state
│   │
│   └── virtualizer/                    # Layer 3: ProjFS
│       ├── mod.rs
│       ├── callbacks.rs                # ProjFS callback implementations
│       ├── enumeration.rs              # Active enumeration sessions
│       ├── sendable.rs                 # Thread-safe context wrapper
│       └── projfs.rs                   # ProjFS virtualizer
│
├── examples/
│   └── mount_projfs.rs                 # Example usage
│
└── tests/
    └── integration_tests.rs            # Integration tests
```

## Architecture

### Layer 0: Utilities
- **wstr.rs**: Wide string (UTF-16) conversion with stack allocation optimization
- **filetime.rs**: FILETIME ↔ SystemTime conversion
- **compare.rs**: ProjFS file name comparison wrapper

### Layer 1: Projection (Backing Store)
- **types.rs**: Core data types (ProjectedFileInfo, ContentHash, FileData, etc.)
- **folder.rs**: Folder data structures with ProjFS sorting
- **manifest.rs**: In-memory tree built from manifest with caching

Key features:
- Pre-sorted entries using ProjFS collation order
- Arc-wrapped slices to avoid cloning during enumeration
- Path cache for frequently accessed directories
- Support for both V1 and V2 manifest formats

### Layer 2: Callbacks (Coordination)
- **background.rs**: Background task runner for non-critical operations
- **modified_paths.rs**: Database tracking modifications for diff manifest generation
- **path_registry.rs**: Bidirectional path↔inode mapping with case-insensitive lookup
- **vfs_callbacks.rs**: Coordination between virtualizer and projection with dirty state

Key features:
- Dirty state tracking (files and directories)
- Background task queue for notifications
- Memory pool coordination with acquire/try_get pattern
- S3 content fetching with caching
- Modification tracking for diff manifest generation

### Layer 3: Virtualizer (ProjFS)
- **callbacks.rs**: ProjFS callback implementations
- **enumeration.rs**: Active enumeration session management
- **sendable.rs**: Thread-safe ProjFS context wrapper
- **projfs.rs**: Main ProjFS virtualizer implementation

Key features:
- AsyncExecutor integration (same pattern as FUSE)
- Lifecycle management (start/stop)
- Copy-on-write support via DirtyFileManager
- Unified memory pool

## Design Patterns

### 1. Layered Architecture
Each layer only depends on the layer below it:
- Layer 3 (Virtualizer) → Layer 2 (Callbacks)
- Layer 2 (Callbacks) → Layer 1 (Projection)
- Layer 1 (Projection) → Layer 0 (Utilities)

### 2. Memory Optimization
- **Arc-wrapped slices**: Enumeration data shared, not cloned
- **Stack allocation**: Paths <256 chars use stack (SmallVec)
- **Path caching**: Frequently accessed directories cached
- **Memory Pool v2**: DashMap-based implementation with lock-free hot paths
  - `blocks: DashMap<PoolBlockId, Arc<PoolBlock>>` for concurrent block access
  - `key_index: DashMap<BlockKey, PoolBlockId>` for lock-free lookups
  - `pending_fetches: DashMap<BlockKey, SharedFetch>` for deduplication
  - `lru_state: Mutex<LruState>` for cold path eviction only

### 3. Threading Model
- **AsyncExecutor**: Reused from FUSE implementation
- **Oneshot channels**: Block on condvar wait, not Tokio
- **Background tasks**: Non-critical work queued separately

### 4. VFSForGit Patterns
- **Pre-loaded enumeration**: All data loaded in StartDirectoryEnumeration
- **Session-based**: ActiveEnumeration tracks state per session
- **Fast/slow path**: Sync for memory ops, async for I/O

## Configuration

### ProjFsOptions
- `root_path`: Virtualization root directory
- `worker_threads`: AsyncExecutor worker count (default: 4)
- `memory_pool`: Memory pool configuration
- `notifications`: Which ProjFS notifications to receive

### ProjFsWriteOptions
- `cache_dir`: Write cache directory
- `use_disk_cache`: Enable/disable disk cache

## Testing

### Unit Tests (60 tests)
- Utility functions (wstr, filetime, compare)
- Folder data structures and sorting
- Manifest projection and caching
- Background task runner
- Path registry (case-insensitive, normalization)
- Modified paths database (create, modify, delete, rename)
- Active enumeration sessions

### Integration Tests (5 tests)
- Virtualizer creation and lifecycle
- Enumeration through callbacks
- Path lookup (case-insensitive)
- File info retrieval
- Edge cases (empty manifest, deep nesting)

## Implementation Status

### ✅ Completed
- Complete project structure
- All data types and utilities
- Manifest projection with caching
- **PathRegistry**: Bidirectional path↔inode mapping
- **ModifiedPathsDatabase**: Diff manifest tracking
- **VfsCallbacks dirty state integration**: Full write support wiring
- **fetch_file_content with caching**: Memory pool acquire/try_get pattern
- **V2 chunked file support**: Multi-chunk fetching via fetch_file_content_for_path
- **Directory notification handlers**: on_dir_created, on_dir_deleted
- ProjFS callback implementations (enumeration, placeholder info, file data)
- Virtualizer lifecycle management
- Memory pool v2 integration (DashMap-based)
- Comprehensive tests (60 unit + 5 integration)
- Example and documentation

### 🔧 Runtime Dependencies
- AWS CRT DLLs required for storage client tests
- ProjFS Windows feature must be enabled

## Usage Example

```rust
use rusty_attachments_vfs_projfs::{WritableProjFs, ProjFsOptions, ProjFsWriteOptions};
use rusty_attachments_model::Manifest;
use std::path::PathBuf;

// Load manifest
let manifest = Manifest::decode(&json_str)?;

// Create storage client
let storage = Arc::new(StorageClientAdapter::new(crt_client));

// Configure
let options = ProjFsOptions::new(PathBuf::from("C:\\mount"))
    .with_worker_threads(4);

let write_options = ProjFsWriteOptions::default()
    .with_cache_dir(PathBuf::from("C:\\Temp\\cache"));

// Mount
let vfs = WritableProjFs::new(&manifest, storage, options, write_options)?;
vfs.start()?;

// Use filesystem...

// Get modification summary for diff manifest
let summary = vfs.callbacks().get_modification_summary();
println!("Created files: {:?}", summary.created_files);
println!("Modified files: {:?}", summary.modified_files);
println!("Deleted files: {:?}", summary.deleted_files);

// Unmount
vfs.stop()?;
```

## References

- Design document: `design/win-fs.md`
- Remaining work: `design/win-fs-remaining-work.md`
- VFSForGit: https://github.com/microsoft/VFSForGit
- ProjFS docs: https://learn.microsoft.com/en-us/windows/win32/projfs/
