# Performance Improvements Summary (2026-02-01)

This document summarizes the main improvements identified in `context/rusty-attachments-perf` that are not yet in the main branch.

## Overview

The perf context has **9 commits ahead** of main, adding approximately **4,300+ lines of new code** focused on performance improvements.

### File Changes

```
crates/storage-crt/Cargo.toml            |   1 +
crates/storage-crt/src/lib.rs            |  20 +-
crates/storage-crt/src/transfer_manager.rs  | 504 ++++++++
crates/storage/Cargo.toml                |  11 +
crates/storage/examples/bench_hash_upload.rs | 889 +++++++++++++++
crates/storage/src/data_cache.rs         | 149 +++
crates/storage/src/hash_upload/deduplication.rs | 204 ++++
crates/storage/src/hash_upload/memory_pool.rs   | 197 ++++
crates/storage/src/hash_upload/mod.rs    | 647 +++++++++++
crates/storage/src/hash_upload/options.rs | 186 +++
crates/storage/src/hash_upload/pipeline.rs | 515 +++++++++
crates/storage/src/hash_upload/progress.rs | 273 +++++
crates/storage/src/hash_upload/staged_pipeline.rs | 738 ++++++++++++
crates/storage/src/lib.rs                |  10 +-
```

---

## 1. AWS CRT Transfer Manager Client

**Files:** `crates/storage-crt/src/transfer_manager.rs`

A new `StorageClient` implementation using AWS's high-performance Transfer Manager:
- Automatic multipart uploads
- Parallel byte-range downloads
- Handles large files efficiently without manual chunking

**Design:** See [01-crt-transfer-manager.md](./01-crt-transfer-manager.md)

---

## 2. Pipelined Hash+Upload Module

**Files:** `crates/storage/src/hash_upload/mod.rs`, `pipeline.rs`

Combines hashing and uploading into a single optimized pass:
- Single file read (vs. read for hash, then read again for upload)
- Memory backpressure via semaphore-based `MemoryPool`
- Hash deduplication prevents duplicate uploads
- Concurrent processing with configurable limits

**Design:** See [02-pipelined-hash-upload.md](./02-pipelined-hash-upload.md)

---

## 3. 3-Stage Pipeline Architecture

**Files:** `crates/storage/src/hash_upload/staged_pipeline.rs`

Advanced pipeline with separate concurrent stages:
- Read Stage (16 concurrent) - Reads files from disk
- Hash Stage (16 concurrent) - Computes XXH128 hashes
- Upload Stage (32 concurrent) - Uploads to S3

Stages connected by async channels for maximum throughput.

**Design:** See [03-staged-pipeline.md](./03-staged-pipeline.md)

---

## 4. Content-Addressable Data Cache

**Files:** `crates/storage/src/data_cache.rs`

New abstractions for CAS storage:
- `S3DataCache` - S3-backed with optional existence check cache
- `OwnedS3DataCache` - Arc-wrapped for `'static` lifetime
- `FileSystemDataCache` - Local filesystem backend
- Integration with `S3CheckCache` to avoid redundant HEAD requests

**Design:** See [04-data-cache.md](./04-data-cache.md)

---

## 5. Supporting Infrastructure

**Files:** `deduplication.rs`, `memory_pool.rs`, `options.rs`, `progress.rs`

Building blocks for the pipeline:
- `MemoryPool` - Semaphore-based memory allocation with backpressure
- `UploadDeduplicator` - Broadcast channel coordination for duplicates
- `ProgressTracker` - Thread-safe atomic progress tracking
- `HashUploadOptions` - Builder pattern with auto-memory detection

**Design:** See [05-supporting-infrastructure.md](./05-supporting-infrastructure.md)

---

## Implementation Order

1. **CRT Transfer Manager** - Foundation for all S3 operations
2. **Supporting Infrastructure** - Building blocks (memory pool, deduplication, progress)
3. **Data Cache** - CAS abstraction layer
4. **Pipelined Hash+Upload** - Basic pipeline
5. **Staged Pipeline** - Advanced 3-stage architecture
