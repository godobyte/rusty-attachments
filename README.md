# Rusty Attachments

Rust implementation of the Job Attachments manifest model for AWS Deadline Cloud, with Python and WASM bindings.

## Features

- **v2023-03-03**: Original manifest format with files only
- **v2025-12-04-beta**: Extended format with directories, symlinks, chunking, execute bit

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── common/          # Shared utilities (path, hash, progress)
│   ├── model/           # Core manifest model
│   ├── filesystem/      # Directory scanning, diff operations
│   ├── profiles/        # Storage profiles, path grouping
│   ├── storage/         # S3 storage abstraction, caching layers
│   ├── storage-crt/     # AWS SDK S3 backend implementation
│   ├── ja-deadline-utils/# High-level Deadline Cloud utilities
│   ├── vfs/             # FUSE virtual filesystem
│   ├── python/          # PyO3 bindings
│   └── wasm/            # WASM bindings
└── design/              # Design documents
```

## Crates

| Crate | Description |
|-------|-------------|
| `rusty-attachments-common` | Path utilities, hash functions, progress callbacks, constants |
| `rusty-attachments-model` | Manifest structures, encode/decode, validation |
| `rusty-attachments-filesystem` | Directory scanning, snapshot/diff operations, glob filtering |
| `rusty-attachments-profiles` | Storage profiles, path grouping, asset root management |
| `rusty-attachments-storage` | S3 storage traits, upload/download orchestration, caching |
| `rusty-attachments-storage-crt` | AWS SDK S3 backend (`StorageClient` implementation) |
| `ja-deadline-utils` | High-level utilities for Deadline Cloud job attachment workflows |
| `rusty-attachments-vfs` | FUSE-based virtual filesystem for mounting manifests (Linux/macOS) |
| `rusty-attachments-python` | Python bindings via PyO3 |
| `rusty-attachments-wasm` | WebAssembly bindings |

## Building

```bash
# Build all crates
cargo build

# Build specific crates
cargo build -p rusty-attachments-common
cargo build -p rusty-attachments-model
cargo build -p rusty-attachments-filesystem
cargo build -p rusty-attachments-profiles
cargo build -p rusty-attachments-storage
cargo build -p rusty-attachments-storage-crt
cargo build -p ja-deadline-utils

# Build VFS (without FUSE support)
cargo build -p rusty-attachments-vfs

# Build VFS with FUSE support (requires platform setup - see VFS section)
cargo build -p rusty-attachments-vfs --features fuse

# Check without building (faster)
cargo check
```

## Testing

```bash
# Run all tests
cargo test

# Test specific crates
cargo test -p rusty-attachments-common
cargo test -p rusty-attachments-model
cargo test -p rusty-attachments-filesystem
cargo test -p rusty-attachments-profiles
cargo test -p rusty-attachments-storage
cargo test -p rusty-attachments-storage-crt
cargo test -p ja-deadline-utils
cargo test -p rusty-attachments-vfs

# Run tests with output
cargo test -- --nocapture
```

## Usage (Rust)

```rust
use rusty_attachments_model::Manifest;
use rusty_attachments_common::{hash_file, normalize_for_manifest};
use rusty_attachments_filesystem::{FileSystemScanner, SnapshotOptions, DiffEngine, DiffOptions};

// Decode a manifest
let json = r#"{"hashAlg":"xxh128","manifestVersion":"2023-03-03","paths":[],"totalSize":0}"#;
let manifest = Manifest::decode(json)?;
println!("Version: {}", manifest.version());

// Hash a file
let hash = hash_file(Path::new("file.txt"))?;

// Normalize path for manifest storage
let manifest_path = normalize_for_manifest(Path::new("/project/assets/file.txt"), Path::new("/project"))?;
assert_eq!(manifest_path, "assets/file.txt");

// Create a snapshot manifest from a directory
let scanner = FileSystemScanner::new();
let options = SnapshotOptions {
    root: PathBuf::from("/project/assets"),
    ..Default::default()
};
let manifest = scanner.snapshot(&options, None)?;

// Diff a directory against a manifest
let engine = DiffEngine::new();
let diff_options = DiffOptions {
    root: PathBuf::from("/project/assets"),
    ..Default::default()
};
let diff_result = engine.diff(&manifest, &diff_options, None)?;
println!("Added: {}, Modified: {}, Deleted: {}", 
    diff_result.added.len(), 
    diff_result.modified.len(), 
    diff_result.deleted.len());
```

## VFS (Virtual Filesystem)

The VFS crate provides a FUSE-based virtual filesystem for mounting job attachment manifests. Files appear as local files but content is fetched on-demand from S3.

### Platform Setup

#### macOS
```bash
# Install macFUSE
brew install --cask macfuse

# Reboot, then allow kernel extension in System Settings → Privacy & Security

# Set pkg-config path (add to shell profile)
export PKG_CONFIG_PATH="/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH"

# Build with FUSE
cargo build -p rusty-attachments-vfs --features fuse
```

#### Linux
```bash
# Debian/Ubuntu
sudo apt-get install libfuse-dev pkg-config

# Fedora/RHEL
sudo dnf install fuse-devel pkg-config

# Arch
sudo pacman -S fuse2 pkg-config

# Build with FUSE
cargo build -p rusty-attachments-vfs --features fuse
```

See `crates/vfs/README.md` for detailed VFS documentation.

## Python Bindings

```bash
# Install from PyPI
pip install rusty_attachments

# Or build from source (requires maturin)
cd crates/python
maturin develop

# Run Python tests
pytest
```

Example usage:
```python
import asyncio
from rusty_attachments import (
    S3Location, ManifestLocation, AssetReferences,
    submit_bundle_attachments_py
)

async def main():
    result = await submit_bundle_attachments_py(
        region="us-west-2",
        s3_location=S3Location(bucket="my-bucket", root_prefix="DeadlineCloud"),
        manifest_location=ManifestLocation(bucket="my-bucket", farm_id="farm-xxx"),
        asset_references=AssetReferences(input_filenames=["/path/to/files"]),
    )
    print(result.attachments_json)

asyncio.run(main())
```

See `crates/python/README.md` for detailed Python documentation.

## WASM Bindings

```bash
# Build WASM (requires wasm-pack)
cd crates/wasm
wasm-pack build

# Run WASM tests
wasm-pack test --node
```

## License

Apache-2.0
