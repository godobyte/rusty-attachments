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
│   ├── vfs/             # FUSE virtual filesystem (Linux/macOS)
│   ├── vfs-fskit/       # FSKit virtual filesystem (macOS 15.4+)
│   ├── vfs-projfs/      # ProjFS virtual filesystem (Windows)
│   ├── example/         # Example applications
│   │   └── tauri/       # Tauri v2 desktop app
│   ├── python/          # PyO3 bindings
│   └── wasm/            # WASM bindings
└── design/              # Design documents
```

## Crates

| Crate | Platform | Description |
|-------|----------|-------------|
| `rusty-attachments-common` | All | Path utilities, hash functions, progress callbacks, constants |
| `rusty-attachments-model` | All | Manifest structures, encode/decode, validation |
| `rusty-attachments-filesystem` | All | Directory scanning, snapshot/diff operations, glob filtering |
| `rusty-attachments-profiles` | All | Storage profiles, path grouping, asset root management |
| `rusty-attachments-storage` | All | S3 storage traits, upload/download orchestration, caching |
| `rusty-attachments-storage-crt` | All | AWS SDK S3 backend (`StorageClient` implementation) |
| `ja-deadline-utils` | All | High-level utilities for Deadline Cloud job attachment workflows |
| `rusty-attachments-vfs` | 🐧🍎 | FUSE-based virtual filesystem (Linux/macOS, requires `fuse` feature) |
| `rusty-attachments-vfs-fskit` | 🍎 | FSKit-based virtual filesystem (macOS 15.4+ only) |
| `rusty-attachments-vfs-projfs` | 🪟 | ProjFS-based virtual filesystem (Windows only) |
| `tauri-example` | All | Tauri v2 desktop application for managing job attachments |
| `rusty-attachments-python` | All | Python bindings via PyO3 |
| `rusty-attachments-wasm` | All | WebAssembly bindings |

Legend: 🐧 Linux | 🍎 macOS | 🪟 Windows | All = cross-platform

## Prerequisites

Before building, ensure you have the required tools installed for your platform.

### All Platforms

1. **Rust Toolchain**
   ```bash
   # Install rustup (Rust installer and version manager)
   # Visit: https://rustup.rs/
   
   # Verify installation
   cargo --version
   rustc --version
   ```

### Windows

2. **Visual Studio Build Tools** (Required for MSVC linker)
   - Download: [Build Tools for Visual Studio 2022](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022)
   - During installation, select:
     - **"Desktop development with C++"** workload, OR
     - **"MSVC v143 - VS 2022 C++ x64/x86 build tools"** + **"Windows 11 SDK"**
   - After installation, restart your terminal

3. **CMake** (Required for AWS SDK dependencies)
   - Download: [CMake Windows Installer](https://cmake.org/download/)
   - During installation, select **"Add CMake to system PATH"**
   - Verify: `cmake --version`

4. **Building on Windows**
   ```powershell
   # Option 1: Use Developer Command Prompt (Recommended)
   # Search for "Developer Command Prompt for VS 2022" in Start Menu
   # Navigate to project directory and run:
   cargo build
   
   # Option 2: Set up environment in PowerShell
   & "C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\Launch-VsDevShell.ps1" -Arch amd64 -HostArch amd64
   cargo build
   ```

### macOS

2. **Xcode Command Line Tools**
   ```bash
   xcode-select --install
   ```

3. **CMake** (Required for AWS SDK dependencies)
   ```bash
   brew install cmake
   ```

### Linux

2. **Build Essentials**
   ```bash
   # Debian/Ubuntu
   sudo apt-get install build-essential pkg-config cmake
   
   # Fedora/RHEL
   sudo dnf install gcc gcc-c++ make pkg-config cmake
   
   # Arch
   sudo pacman -S base-devel pkg-config cmake
   ```

## Building

After installing the prerequisites above, you can build the project:

```bash
# Quick check (recommended first step - faster than full build)
cargo check

# Build all crates in debug mode
cargo build

# Build all crates in release mode (optimized)
cargo build --release

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

# Build FSKit VFS (macOS 15.4+ only)
cargo build -p rusty-attachments-vfs-fskit

# Build ProjFS VFS (Windows only)
cargo build -p rusty-attachments-vfs-projfs
```

### Troubleshooting Build Issues

**Windows: "linker `link.exe` not found"**
- Install Visual Studio Build Tools with C++ support (see Prerequisites)
- Use Developer Command Prompt for VS 2022, or run the Launch-VsDevShell.ps1 script

**Windows: "Missing dependency: cmake"**
- Install CMake and ensure it's in your PATH (see Prerequisites)
- Restart your terminal after installation

**All Platforms: "failed to compile aws-lc-sys"**
- Ensure CMake is installed and accessible: `cmake --version`
- On Windows, ensure you're using Developer Command Prompt or have run Launch-VsDevShell.ps1

**macOS/Linux: FUSE build errors**
- Install platform-specific FUSE development libraries (see VFS Platform Setup section below)

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

The VFS crates provide virtual filesystems for mounting job attachment manifests. Files appear as local files but content is fetched on-demand from S3.

| Crate | Platform | Technology |
|-------|----------|------------|
| `rusty-attachments-vfs` | 🐧🍎 | FUSE (Linux/macOS) |
| `rusty-attachments-vfs-fskit` | 🍎 | FSKit (macOS 15.4+) |
| `rusty-attachments-vfs-projfs` | 🪟 | ProjFS (Windows 10 1809+) |

### Platform Setup

#### macOS (FUSE)
```bash
# Install macFUSE
brew install --cask macfuse

# Reboot, then allow kernel extension in System Settings → Privacy & Security

# Set pkg-config path (add to shell profile)
export PKG_CONFIG_PATH="/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH"

# Build with FUSE
cargo build -p rusty-attachments-vfs --features fuse
```

#### macOS (FSKit - macOS 15.4+)
```bash
# Install FSKitBridge from https://github.com/debox-network/fskit-rs/releases
cp -r FSKitBridge.app /Applications/
xattr -dr com.apple.quarantine /Applications/FSKitBridge.app
open -a /Applications/FSKitBridge.app --args -s

# Enable in System Settings → Privacy & Security → File System Extensions

# Build
cargo build -p rusty-attachments-vfs-fskit
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

#### Windows
```powershell
# Enable ProjFS (run as Administrator)
Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart

# Build
cargo build -p rusty-attachments-vfs-projfs
```

### Running VFS Examples

#### FUSE (Linux/macOS)
```bash
# Mount a manifest
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
    manifest.json ./vfs --writable --stats
```

#### FSKit (macOS 15.4+)
```bash
# Mount a manifest
cargo run -p rusty-attachments-vfs-fskit --example mount_fskit -- \
    --manifest manifest.json \
    --mount-point /tmp/deadline-assets \
    --bucket my-bucket \
    --root-prefix DeadlineCloud \
    --stats
```

#### ProjFS (Windows)
```powershell
# Mount a manifest
cargo run -p rusty-attachments-vfs-projfs --example mount_projfs -- `
    manifest.json vfs --cache-dir vfs-cache --stats --cleanup
```

See individual crate READMEs for detailed documentation:
- `crates/vfs/README.md` - FUSE VFS
- `crates/vfs-fskit/README.md` - FSKit VFS
- `crates/vfs-projfs/README.md` and `crates/vfs-projfs/examples/README.md` - ProjFS VFS

## Example Application

A Tauri v2 desktop application demonstrating job attachment management with a two-tab interface for uploading files and browsing manifests.

### Running the Example

```bash
# Install Tauri CLI v2
cargo install tauri-cli --version "^2.0" --locked

# Run the desktop app
cd crates/example/tauri
cargo tauri dev

# Build production binary
cargo tauri build
```

### Features
- Browse local directories and create snapshots
- Upload files to S3 CAS with progress tracking
- Browse S3 manifest folders
- View manifest contents with file tree visualization
- Support for v2023-03-03 and v2025-12-04-beta formats

See `crates/example/tauri/README.md` for detailed documentation.

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
