# Windows Virtual Filesystem Options

**Status: 📋 RESEARCH**

## Overview

This document surveys Rust crates available for implementing virtual filesystems on Windows, as a companion to the macOS FSKit design (`vfs-fskit.md`) and existing FUSE implementation.

## Available Options

### 1. WinFSP (`winfsp` crate) — Recommended

[WinFSP](https://github.com/winfsp/winfsp) (Windows File System Proxy) is the most mature user-space filesystem framework for Windows.

| Aspect | Details |
|--------|---------|
| **Crate** | [`winfsp`](https://crates.io/crates/winfsp) |
| **Documentation** | https://docs.rs/winfsp |
| **Upstream** | https://github.com/winfsp/winfsp |
| **License** | GPLv3 with FLOSS exception |
| **Maturity** | Production-ready, actively maintained |
| **Requirements** | WinFSP driver installation |

#### Architecture

WinFSP uses a kernel-mode driver that forwards filesystem requests to a user-space process:

```
┌─────────────────────────────────────────┐
│           Windows Kernel                 │
│  ┌─────────────────────────────────┐    │
│  │  WinFSP.sys (kernel driver)     │    │
│  └─────────────────────────────────┘    │
└─────────────────────────────────────────┘
                    │
            DeviceIoControl
                    │
┌─────────────────────────────────────────┐
│           User Space                     │
│  ┌─────────────────────────────────┐    │
│  │  winfsp-rs (Rust bindings)      │    │
│  │  FileSystemHost trait impl      │    │
│  └─────────────────────────────────┘    │
└─────────────────────────────────────────┘
```

#### Trait Model

```rust
use winfsp::filesystem::{FileSystemHost, FileContext};

impl FileSystemHost for MyFilesystem {
    /// Read file contents.
    fn read(
        &self,
        file_context: &FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> Result<u32, NTSTATUS>;

    /// Write file contents.
    fn write(
        &self,
        file_context: &FileContext,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        constrained_io: bool,
    ) -> Result<u32, NTSTATUS>;

    /// Get file/directory information.
    fn get_file_info(
        &self,
        file_context: &FileContext,
    ) -> Result<FileInfo, NTSTATUS>;

    // ... other methods
}
```

#### Async Model

WinFSP uses **synchronous callbacks** similar to FUSE, meaning we would need the `AsyncExecutor` bridge pattern:

```rust
fn read(&self, file_context: &FileContext, buffer: &mut [u8], offset: u64) -> Result<u32, NTSTATUS> {
    // Must bridge sync callback to async runtime
    let result = self.executor.block_on(async {
        self.inner.read_async(file_context.item_id, offset, buffer.len()).await
    });
    // ...
}
```

---

### 2. Dokan (`dokan` crate)

[Dokan](https://dokan-dev.github.io/) is an older user-space filesystem library for Windows.

| Aspect | Details |
|--------|---------|
| **Crate** | [`dokan`](https://crates.io/crates/dokan) |
| **Documentation** | https://docs.rs/dokan |
| **Upstream** | https://github.com/dokan-dev/dokany |
| **License** | LGPL/MIT |
| **Maturity** | Stable but less active than WinFSP |
| **Requirements** | Dokan driver installation |

#### Considerations

- Less actively maintained than WinFSP
- WinFSP is generally considered more performant
- Similar sync callback model to WinFSP

---

### 3. ProjFS (Windows Projected File System)

Microsoft's native virtualization framework, used by Windows features like OneDrive placeholders.

| Aspect | Details |
|--------|---------|
| **Rust Crate** | No mature Rust bindings available |
| **Upstream** | Built into Windows 10 1809+ |
| **License** | Proprietary (Windows component) |
| **Use Case** | Cloud storage placeholders, lazy hydration |

#### Notes

- No driver installation required (built into Windows)
- Designed for "placeholder" files that hydrate on access
- Would require creating Rust bindings via `windows-rs`
- More complex API than WinFSP/Dokan

---

## Comparison Matrix

| Aspect | WinFSP | Dokan | ProjFS |
|--------|--------|-------|--------|
| **Rust Crate** | `winfsp` ✅ | `dokan` ✅ | None ❌ |
| **Maturity** | High | Medium | N/A |
| **Performance** | Excellent | Good | Excellent |
| **Driver Required** | Yes (installer) | Yes (installer) | No (built-in) |
| **Async Model** | Sync callbacks | Sync callbacks | Sync callbacks |
| **Active Development** | Yes | Limited | Yes (Microsoft) |
| **License** | GPLv3 + exception | LGPL/MIT | Proprietary |

---

## Cross-Platform Comparison

| Aspect | WinFSP (Windows) | FSKit (macOS) | FUSE (Linux/macOS) |
|--------|------------------|---------------|---------------------|
| **Architecture** | User-space + kernel driver | User-space (appex) | Kernel module + user-space |
| **Rust Crate** | `winfsp` | `fskit-rs` | `fuser` |
| **Async Model** | Sync callbacks | Async trait ✅ | Sync callbacks |
| **Executor Bridge** | Required | Not needed | Required |
| **Maturity** | Production-ready | New (macOS 15.4+) | Very mature |
| **Installation** | WinFSP installer | Built-in | macFUSE/libfuse |
| **Write Buffer** | `&[u8]` (borrowed) | `Vec<u8>` (owned) | `&[u8]` (borrowed) |

---

## Recommendation

**Use `winfsp` crate** for Windows VFS implementation:

1. Most mature and actively maintained Rust bindings
2. WinFSP has excellent performance and stability
3. Similar architecture to FUSE allows code reuse patterns
4. Well-documented with examples

### Implementation Approach

Following the pyramid architecture, a Windows VFS would:

1. **Reuse existing primitives**: `DirtyFileManager`, `DirtyDirManager`, `MemoryPool`, `INodeManager`
2. **Use `AsyncExecutor` bridge**: Same pattern as FUSE (sync callbacks → async operations)
3. **Create `rusty-attachments-vfs-winfsp` crate**: Platform-specific frontend

```rust
/// Windows VFS using WinFSP.
/// 
/// Similar to FUSE implementation - uses AsyncExecutor to bridge
/// sync callbacks to async S3 operations.
pub struct WritableWinFsp {
    inner: Arc<WritableWinFspInner>,
    executor: Arc<AsyncExecutor>,
}

struct WritableWinFspInner {
    inodes: Arc<INodeManager>,
    store: Arc<dyn FileStore>,
    pool: Arc<MemoryPool>,
    dirty_manager: Arc<DirtyFileManager>,
    dirty_dir_manager: Arc<DirtyDirManager>,
    // ... same primitives as FUSE/FSKit
}
```

---

## Next Steps

1. Evaluate `winfsp` crate API in detail
2. Create design document similar to `vfs-fskit.md`
3. Identify any Windows-specific considerations (paths, permissions, etc.)
4. Prototype basic read-only mount

## References

- WinFSP GitHub: https://github.com/winfsp/winfsp
- winfsp-rs crate: https://docs.rs/winfsp
- Dokan project: https://dokan-dev.github.io/
- ProjFS documentation: https://docs.microsoft.com/en-us/windows/win32/projfs/projected-file-system
