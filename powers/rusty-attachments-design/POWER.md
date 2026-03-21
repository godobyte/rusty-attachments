---
name: rusty-attachments-design
version: 1.2.0
description: Design documentation reference for rusty-attachments project
keywords:
  - rusty-attachments
  - design
  - manifest
  - storage
  - upload
  - download
  - s3
  - cas
  - deadline
  - job-attachments
  - vfs
  - fuse
  - fskit
  - projfs
  - cli
  - wasm
---

# Rusty Attachments Design Reference

This power provides quick access to design documentation for the rusty-attachments project - a Rust implementation of AWS Deadline Cloud job attachments.

## Design Document Summaries

Use these summaries to identify which document to read for specific topics. Reference the full document when you need implementation details.

### Core Architecture

| Document | Purpose | Key Topics |
|----------|---------|------------|
| `model-design.md` | Manifest data structures | v2023/v2025 formats, typed wrappers (Snapshot/AbsSnapshot), SpecVersion, PathStyle, composable operations (compose/diff/filter/subtree/partition/join) |
| `common.md` | Shared utilities | Hash functions, path utilities, constants (CHUNK_SIZE_V2=256MB), ProgressCallback trait, machine_id |
| `storage-design.md` | S3 storage abstraction | StorageClient trait, ContentAddressedDataCache trait, S3DataCache, FileSystemDataCache, UploadOrchestrator, DownloadOrchestrator, CRT backend |

### File Operations

| Document | Purpose | Key Topics |
|----------|---------|------------|
| `file_system.md` | Directory scanning | GlobFilter, SnapshotOptions, DiffOptions, expand_input_paths(), FileSystemScanner, StatCache, DiffEngine, SymlinkPolicy |
| `manifest-utils.md` | Manifest operations | compose_manifests(), compute_diff_manifest(), filter_manifest(), subtree/partition/join, IncludeExcludeFilter, trie-based merge |
| `hash-cache.md` | Caching layer | HashCache (path,size,mtime→hash), S3CheckCache (s3_key→exists), SQLite backends |

### S3 Operations

| Document | Purpose | Key Topics |
|----------|---------|------------|
| `manifest-storage.md` | Manifest S3 ops | upload_input_manifest(), output manifest discovery, S3 key formats, metadata handling |

### Path & Profile Management

| Document | Purpose | Key Topics |
|----------|---------|------------|
| `storage-profiles.md` | Storage profiles | FileSystemLocation (Local/Shared), AssetRootGroup, group_asset_paths(), path validation |
| `path-mapping.md` | Path transformation | PathMappingRule, PathMappingApplier (trie-based), cross-platform path handling |

### Virtual File System (VFS)

| Document | Purpose | Key Topics |
|----------|---------|------------|
| `vfs.md` | VFS core design | FUSE interface, INode primitives, FileStore trait, MemoryPool v2 (lock-free), AsyncExecutor, ReadCache (disk), FSKit (macOS), ProjFS (Windows) |
| `vfs-writes.md` | Write support (COW) | DirtyFileManager, MaterializedCache, WriteCache trait, DiffManifestExporter |
| `vfs-dirs.md` | Directory operations | mkdir/rmdir FUSE ops, DirtyDirManager, directory tracking for diff manifests |

### Job Integration

| Document | Purpose | Key Topics |
|----------|---------|------------|
| `job-submission.md` | Job attachments format | ManifestProperties, Attachments struct, submit_bundle_attachments(), worker_sync stubs |
| `bindings.md` | Python bindings (PyO3) | Full API: submit, snapshot, diff, download, upload, discover outputs, incremental download |

### Implementation

| Document | Purpose | Key Topics |
|----------|---------|------------|
| `todo.md` | Remaining work | Feature checklist, skipped features with rationale, Python function mapping |
| `utilities.md` | CLI utilities | filter_redundant_known_paths(), classify_paths(), warning message generation |

## Quick Reference

### Manifest Versions
- **v2023-03-03**: Original format, single hash per file, no directories
- **v2025-12**: Chunked files, directories, symlinks, diff manifests, typed wrappers (Snapshot/AbsSnapshot), spec versions (absolute/relative × snapshot/diff)

### Key Constants
- `CHUNK_SIZE_V2` = 256MB (chunking threshold)
- `SMALL_FILE_THRESHOLD` = 80MB (parallel upload threshold)
- Hash algorithm: XXH128

### Crate Structure
```
crates/
├── common/        # Shared utilities, hash, path, machine_id
├── model/         # Manifest structures + composable operations
├── filesystem/    # Directory scanning, stat cache
├── profiles/      # Storage profiles
├── storage/       # Upload/download orchestration, CAS, caches
├── storage-crt/   # AWS SDK backend (TransferManagerClient)
├── vfs/           # Virtual file system (FUSE, read/write)
├── vfs-fskit/     # macOS FSKit VFS backend
├── vfs-projfs/    # Windows ProjFS VFS backend
├── ja-deadline-utils/  # High-level job attachment utilities
├── cli/           # `ra` CLI tool
├── python/        # PyO3 bindings
└── wasm/          # WASM bindings (manifest decode/encode)
```

### VFS Architecture
```
Layer 3: Platform Interface
         ├── FUSE (fuse.rs, fuse_writable.rs) — Linux/macOS
         ├── FSKit (vfs-fskit/) — macOS 15.4+
         └── ProjFS (vfs-projfs/) — Windows
Layer 2: VFS Operations (builder.rs, write/, AsyncExecutor)
Layer 1: Primitives (inode/, content/, memory_pool_v2.rs, diskcache/)
```

## Usage

When working on rusty-attachments code:
1. Check this summary to find the relevant design document
2. Read the full document for implementation details
3. Follow the coding style in `.kiro/steering/coding-style.md`
4. Follow the design principles in `.kiro/steering/design-steering.md`
