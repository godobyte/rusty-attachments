---
name: job-attachments-design
description: Domain-specific design guidelines for rusty-attachments — S3 patterns, path mapping, storage profiles, VFS, manifest versions, and relaxed consistency.
triggers:
  - s3
  - storage
  - upload
  - download
  - manifest
  - path mapping
  - storage profile
  - vfs
  - fuse
  - relaxed
  - cas
  - deadline
  - job attachments
  - file system location
  - head_object
  - list_objects
  - pending uploads
  - completion marker
  - upload agent
---

# Job Attachments Design Checklist

Domain-specific design guidelines for the rusty-attachments project. These complement the general Rust guidelines in `design-steering.md` and `coding-style.md`. Activate the `rusty-attachments-design` power for full design docs.

## S3 Operations

- Prefer `head_object` over `list_objects` when you know the exact key. HEAD is ~50ms, LIST is 200ms+ with pagination overhead and returns unneeded metadata.
- For checking multiple known keys, fan out concurrent HEAD requests via `join_all` rather than a single LIST with prefix. Cap concurrency at a configurable `batch_size`.
- Use the existing `S3CheckCache` (see `design/hash-cache.md`) to avoid redundant HEAD requests. Check cache before hitting S3.
- CAS keys are deterministic: `{root_prefix}/Data/{hash}.{algorithm}`. Never use LIST to discover CAS objects — compute the key and HEAD it.
- For uploads, always check existence first (HEAD or S3CheckCache) before uploading. CAS is immutable — if the key exists, the content is identical.
- Multipart upload for files >256MB. Single PUT for smaller files. The `StorageClient` trait handles this (see `design/storage-design.md`).

## Path Mapping & Storage Profiles

When designing features that involve file paths across submitter and worker machines:

- Always consider both cases: **with storage profile** (mapped via `FileSystemLocation` names) and **without storage profile** (dynamic `assetroot-{hash}` directories).
- The `relative_path` within a root is the invariant — it's identical on both submitter and worker. Only the root prefix changes.
- Storage profiles define `LOCAL` (uploaded) vs `SHARED` (accessible, not uploaded) locations. Relaxed consistency roots are a third category: `LOCAL` but uploaded on-demand.
- Path mapping rules translate submitter paths → worker paths. The `PathMappingApplier` uses a trie for O(path_depth) lookup. See `design/path-mapping.md`.
- When sending messages (SQS, API calls) that reference files, always include the submitter's `source_path` so the receiving side can resolve the file on the original filesystem.
- Cross-platform: Windows uses `\` separators and case-insensitive paths. Posix uses `/` and case-sensitive. Normalize to posix for manifest storage, convert back for local filesystem access.

## Manifest Versions

- Always handle both v2023-03-03 and v2025-12-04-beta manifest formats.
- Use the `Manifest` enum wrapper — never match on the inner types directly in business logic.
- Features not supported by v2023 (chunking, directories, symlinks, diff manifests, relaxed consistency) must return `VersionNotCompatibleError` with a clear message. Mark with `// COMPAT:` comments.
- New features should target v2025 format only. v2023 is frozen.

## VFS Design

- The VFS intercepts filesystem calls (FUSE/FSKit/ProjFS). Content is fetched on-demand, not pre-downloaded. This is the only mode that supports relaxed consistency.
- `COPIED` mode downloads all files to disk before the job runs — no interception point. Validate that relaxed roots are rejected in COPIED mode.
- Three platform backends exist: FUSE (Linux), FSKit (macOS), ProjFS (Windows). Shared logic lives in `crates/vfs/`, platform-specific code in `crates/vfs-{fskit,projfs}/`.
- The `FileStore` trait abstracts content retrieval. `StorageClientAdapter` bridges `StorageClient` → `FileStore`. Mock implementations exist for testing.
- The `MemoryPool` manages 256MB blocks with LRU eviction. Fetch coordination prevents duplicate S3 requests for the same block.
- For relaxed consistency: auto-vivify directory INodes on lookup miss under relaxed roots. Convert to file INode on `open()`. Promote to `SingleHash`/`Chunked` after resolution.

## Concurrency & Async

- FUSE callbacks are synchronous. Use the `AsyncExecutor` to bridge to async code without deadlocks. Never hold locks across await points.
- `DashMap` for lock-free concurrent access on hot paths (pending tracker, memory pool index).
- `tokio::sync::Notify` for waking blocked readers when a relaxed file becomes available.
- Background tasks (polling, prefetching) run on the executor. Use `tokio::time::sleep` for intervals, not `std::thread::sleep`.

## Configuration & CLI

- Strong consistency is the default. Relaxed consistency is opt-in via `--relaxed-roots <config.json>`.
- Config files use Deadline's own structures where possible (storage profiles, farm/queue IDs) rather than inventing custom formats.
- The `RelaxedLaunchConfig` JSON is shared between the VFS launcher and the upload agent.
- All config structs should be serde-serializable for roundtrip testing.

## Testing

- Every new type needs serde roundtrip tests (serialize → deserialize → assert fields match).
- Mock implementations (`MemoryFileStore`, `MemoryRelaxedStore`, `MemoryWriteCache`) for testing without S3.
- Test both storage profile cases: mapped (with `fileSystemLocationName`) and unmapped (dynamic mount path).
- Test version compatibility: verify that v2023 manifests produce `VersionNotCompatibleError` for v2025-only features.
- Integration tests in `tests/` directory for cross-module flows. Unit tests in `#[cfg(test)] mod tests` within each file.

## Design Documents

Full design docs are in `design/`. Key references:

| Topic | Document |
|-------|----------|
| Manifest model | `design/model-design.md` |
| S3 storage | `design/storage-design.md` |
| Upload/download | `design/manifest-storage.md` |
| Path mapping | `design/path-mapping.md` |
| Storage profiles | `design/storage-profiles.md` |
| VFS core | `design/vfs.md` |
| VFS writes (COW) | `design/vfs-writes.md` |
| Job submission | `design/job-submission.md` |
| Hash cache | `design/hash-cache.md` |
| Relaxed consistency | `design/async-upload-relaxed-consistency.md` |
| VFS launcher integration | `design/vfs-launcher-relaxed-integration.md` |
