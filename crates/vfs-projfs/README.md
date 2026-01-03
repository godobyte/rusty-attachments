# rusty-attachments-vfs-projfs

ProjFS-based virtual filesystem for Deadline Cloud job attachments (Windows).

## Overview

This crate provides a Windows-native virtual filesystem using Microsoft's Projected File System (ProjFS). It mounts Deadline Cloud job attachment manifests as local directories, fetching file content on-demand from S3 CAS (Content-Addressable Storage).

The design is heavily influenced by [VFSForGit](https://github.com/microsoft/VFSForGit), Microsoft's production-grade ProjFS implementation.

## Architecture

The implementation follows a layered pyramid architecture:

```
Layer 3: ProjFsVirtualizer (ProjFS callbacks)
Layer 2: VfsCallbacks (coordination & dirty state)
Layer 1: ManifestProjection (in-memory manifest tree)
Layer 0: Shared VFS primitives (INodeManager, MemoryPool, etc.)
```

### Key Design Patterns

**From VFSForGit:**
- Pre-loading all enumeration data in `StartDirectoryEnumerationCallback` (fast path)
- Session-based enumeration with `ActiveEnumeration` state tracking
- Background task queue for non-critical operations
- Fast/slow path separation (memory vs I/O operations)

**Threading Model:**
- Reuses the `AsyncExecutor` from the FUSE implementation
- Blocks on oneshot channels (condvar wait), not Tokio's `block_on()`
- Avoids runtime access issues and deadlocks
- Same pattern works for both FUSE and ProjFS

## Features

- **On-demand content fetching**: Files appear immediately but content is fetched only when accessed
- **Copy-on-write support**: Modifications create dirty copies without affecting original manifest
- **Memory-efficient**: Unified memory pool with LRU eviction across read and write caches
- **Fast enumeration**: Pre-sorted in-memory tree structure for instant directory listings
- **Manifest version support**: Both V1 (v2023-03-03) and V2 (v2025-12-04-beta) formats

## Requirements

- Windows 10 1809+ or Windows Server 2019+
- ProjFS Windows feature enabled

## Usage

```rust
use rusty_attachments_vfs_projfs::{WritableProjFs, ProjFsOptions, ProjFsWriteOptions};
use rusty_attachments_model::Manifest;
use std::path::PathBuf;

// Load manifest
let manifest = Manifest::decode(&json_str)?;

// Create storage client
let storage = Arc::new(MyStorageClient::new());

// Configure options
let options = ProjFsOptions::new(PathBuf::from("C:\\mount\\point"))
    .with_worker_threads(4);

let write_options = ProjFsWriteOptions::default()
    .with_cache_dir(PathBuf::from("C:\\Temp\\vfs-cache"));

// Create and start virtualizer
let vfs = WritableProjFs::new(&manifest, storage, options, write_options)?;
vfs.start()?;

// Files are now accessible at C:\mount\point
// ...

// Cleanup
vfs.stop()?;
```

## Example

See `examples/mount_projfs.rs` for a complete example:

```bash
cargo run --example mount_projfs -- manifest.json C:\mount\point
```

## Comparison with Other Platforms

| Aspect | FUSE (Linux) | FSKit (macOS) | ProjFS (Windows) |
|--------|--------------|---------------|------------------|
| Callback Model | Sync | Async trait | Sync |
| Async Bridge | `AsyncExecutor` | Native | `AsyncExecutor` (shared) |
| Enumeration | Per-call | Per-call | Session-based |
| Sorting | None | None | PrjFileNameCompare |
| Pre-load Data | No | No | Yes (VFSForGit pattern) |

## Performance Optimizations

1. **Enumeration Data Sharing**: Uses `Arc<[ProjectedFileInfo]>` to avoid cloning
2. **Stack-allocated Strings**: Paths under 256 chars use stack allocation
3. **Path Caching**: Frequently accessed directory listings are cached
4. **Unified Memory Pool**: Global memory limit enforcement across all caches

## Testing

```bash
# Run unit tests
cargo test -p rusty-attachments-vfs-projfs

# Run with logging
RUST_LOG=debug cargo test -p rusty-attachments-vfs-projfs
```

## References

- [VFSForGit Source](https://github.com/microsoft/VFSForGit)
- [ProjFS Programming Guide](https://learn.microsoft.com/en-us/windows/win32/projfs/projfs-programming-guide)
- [ProjFS API Reference](https://learn.microsoft.com/en-us/windows/win32/api/projectedfslib/)

## License

See the LICENSE file in the repository root.
