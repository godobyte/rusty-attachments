# rusty-attachments-vfs-fskit

FSKit-based virtual filesystem for Deadline Cloud job attachments on macOS 15.4+.

## Overview

This crate provides a macOS-native virtual filesystem using Apple's FSKit framework. It bridges the existing VFS primitives to FSKit via the [fskit-rs](https://github.com/debox-network/fskit-rs) Rust crate.

### Why FSKit?

| Aspect | FUSE (macFUSE) | FSKit |
|--------|---------------|-------|
| **macOS Support** | Third-party, requires kernel extension | Native Apple framework |
| **Security** | Kernel extension approval required | User-space, sandboxed |
| **Performance** | Good | Better (native integration) |
| **Future** | Deprecated path | Apple's recommended approach |
| **Requirements** | macFUSE installation | macOS 15.4+, FSKitBridge app |

## Requirements

- **macOS 15.4 or later** (FSKit is not available on earlier versions)
- **FSKitBridge host app** installed and enabled in System Settings
- **Rust 1.70+** with Cargo

## Installation

### 1. Install FSKitBridge

FSKit requires a host application to bridge between the kernel and user-space filesystem implementations. Download and install FSKitBridge:

```bash
# Download FSKitBridge from releases
# https://github.com/debox-network/fskit-rs/releases

# Install to /Applications
cp -r FSKitBridge.app /Applications/

# Remove quarantine attribute
xattr -dr com.apple.quarantine /Applications/FSKitBridge.app

# Launch once to register the extension
open -a /Applications/FSKitBridge.app --args -s
```

### 2. Enable FSKit Extension

1. Open **System Settings** → **Privacy & Security** → **File System Extensions**
2. Enable the FSKitBridge extension

### 3. Build the Crate

```bash
# Add to workspace Cargo.toml
cargo build -p rusty-attachments-vfs-fskit
```

## Usage

### Basic Example

```rust
use rusty_attachments_vfs_fskit::{
    WritableFsKit, FsKitVfsOptions, FsKitWriteOptions, FsKitMountOptions
};
use rusty_attachments_model::Manifest;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load manifest
    let manifest_json = std::fs::read_to_string("manifest.json")?;
    let manifest = Manifest::decode(&manifest_json)?;
    
    // Create storage client (connects to S3)
    let settings = StorageSettings::default();
    let client = CrtStorageClient::new(settings).await?;
    let location = S3Location::new("bucket", "root", "Data", "Manifests");
    let store: Arc<dyn FileStore> = Arc::new(StorageClientAdapter::new(client, location));
    
    // Create writable FSKit VFS
    let options = FsKitVfsOptions::default()
        .with_volume_name("My Assets");
    
    let write_options = FsKitWriteOptions::default()
        .with_cache_dir("/tmp/vfs-cache");
    
    let vfs = WritableFsKit::new(&manifest, store, options, write_options)?;
    
    // Mount via FSKit
    let mount_opts = FsKitMountOptions::default()
        .with_mount_point("/tmp/deadline-assets");
    
    let session = fskit_rs::mount(vfs, mount_opts.into()).await?;
    
    println!("Mounted at /tmp/deadline-assets");
    
    // Keep running until Ctrl+C
    tokio::signal::ctrl_c().await?;
    
    // Unmount by dropping session
    drop(session);
    
    Ok(())
}
```

### Running the Example

```bash
# Build and run the mount example
cargo run --example mount_fskit -- \
    --manifest /path/to/manifest.json \
    --mount-point /tmp/deadline-assets \
    --cache-dir /tmp/vfs-cache \
    --bucket my-s3-bucket \
    --root-prefix my-root-prefix \
    --volume-name "My Assets"
```

### Quick Start with Default AWS Credentials

Using the included sample manifest and default AWS credentials:

```bash
# Create mount point
mkdir -p /tmp/deadline-assets

# Mount using FSKit (macOS 15.4+) with live stats dashboard
cargo run -p rusty-attachments-vfs-fskit --example mount_fskit -- \
    --manifest manifest_v2.json \
    --mount-point /tmp/deadline-assets \
    --bucket adeadlineja \
    --root-prefix DeadlineCloud \
    --stats

# Or using FUSE (requires macFUSE)
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
    manifest_v2.json /tmp/deadline-assets \
    --bucket adeadlineja \
    --root-prefix DeadlineCloud \
    --stats
```

The manifest file (`manifest_v2.json`) contains file entries like:
```json
{
  "dirs": [],
  "files": [
    {
      "hash": "bf99fbb2035c2b6187848d7ddcd36eb5",
      "mtime": 1766522075985707,
      "name": "aftereffects/aftereffects/24.6/afterfx_titleflip_sample/expected_output/comp_mainTitle_TORENDER_00100.png",
      "size": 64678
    }
  ],
  "hashAlg": "xxh128",
  "manifestVersion": "2023-03-03"
}
```

### Command-Line Options

| Option | Short | Description | Default |
|--------|-------|-------------|---------|
| `--manifest` | `-m` | Path to manifest JSON file | (required) |
| `--mount-point` | `-p` | Mount point directory | `/tmp/deadline-assets` |
| `--cache-dir` | `-c` | Cache directory for writes | `/tmp/vfs-fskit-cache` |
| `--bucket` | `-b` | S3 bucket name | (required) |
| `--root-prefix` | `-r` | S3 root prefix | `""` |
| `--volume-name` | `-n` | Volume name in Finder | `"Deadline Assets"` |
| `--stats` | `-s` | Show live statistics dashboard | disabled |

## Configuration

### FsKitVfsOptions

```rust
let options = FsKitVfsOptions::default()
    .with_volume_name("My Assets")           // Name shown in Finder
    .with_volume_id("unique-uuid")           // Volume UUID
    .with_pool_config(MemoryPoolConfig {     // Memory pool settings
        capacity_bytes: 512 * 1024 * 1024,   // 512MB
        ..Default::default()
    })
    .with_prefetch(PrefetchStrategy::Adaptive)
    .with_read_cache(ReadCacheConfig {
        enabled: true,
        cache_dir: PathBuf::from("/tmp/read-cache"),
        ..Default::default()
    });
```

### FsKitWriteOptions

```rust
let write_options = FsKitWriteOptions::default()
    .with_cache_dir("/tmp/vfs-cache")        // Directory for write cache
    .with_sync_on_write(true)                // Sync after each write
    .with_max_dirty_size(1024 * 1024 * 1024); // 1GB max dirty data
```

### FsKitMountOptions

```rust
let mount_opts = FsKitMountOptions::default()
    .with_mount_point("/Volumes/MyAssets")   // Mount location
    .with_force(true);                       // Force unmount existing
```

## Features

### Supported Operations

| Operation | Status | Notes |
|-----------|--------|-------|
| Read files | ✅ | From manifest or S3 |
| Read chunked files | ✅ | V2 manifest support |
| Read symlinks | ✅ | From manifest |
| Create files | ✅ | COW support |
| Modify files | ✅ | COW support |
| Delete files | ✅ | Tracked for diff |
| Create directories | ✅ | Tracked for diff |
| Delete directories | ✅ | Tracked for diff |
| Truncate files | ✅ | Via set_attributes |
| Memory pool caching | ✅ | Shared with FUSE |
| Disk read cache | ✅ | Shared with FUSE |
| Stats collection | ✅ | Via stats_collector() |

### Not Supported

| Operation | Reason |
|-----------|--------|
| Rename | Not yet implemented |
| Hard links | Not needed for job attachments |
| Symlink creation | Not needed for job attachments |
| Extended attributes | Not needed for job attachments |

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
│  │  WritableFsKit (implements fskit_rs::Filesystem)                    │    │
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

## Migration from FUSE

If you're currently using the FUSE-based VFS, migration is straightforward:

| FUSE API | FSKit API |
|----------|-----------|
| `WritableVfs::new()` | `WritableFsKit::new()` |
| `fuser::mount2()` | `fskit_rs::mount()` |
| `VfsOptions` | `FsKitVfsOptions` |
| `WriteOptions` | `FsKitWriteOptions` |
| `spawn_mount_writable()` | `fskit_rs::mount()` (async) |
| `dirty_manager()` | `dirty_manager()` |
| `dirty_dir_manager()` | `dirty_dir_manager()` |
| `stats_collector()` | `stats_collector()` |
| `dirty_summary()` | `dirty_summary()` |

The internal primitives (`INodeManager`, `MemoryPool`, `FileStore`, `DirtyFileManager`, etc.) are shared between both implementations.

## Troubleshooting

### "FSKit extension not enabled"

1. Open **System Settings** → **Privacy & Security** → **File System Extensions**
2. Enable the FSKitBridge extension
3. Restart the application

### "Mount failed"

1. Ensure FSKitBridge.app is installed in `/Applications`
2. Check that the mount point directory exists
3. Try with `--force` to unmount any existing mount

### "Connection refused"

1. Launch FSKitBridge.app manually: `open -a /Applications/FSKitBridge.app`
2. Check Console.app for FSKit-related errors

### Performance Issues

1. Increase memory pool size via `FsKitVfsOptions::with_pool_config()`
2. Enable disk read cache via `FsKitVfsOptions::with_read_cache()`
3. Use SSD for cache directories

## License

Apache-2.0
