# Implementation Order for Bundle Submit

This document outlines the implementation order to build the `submit_bundle_attachments()` function, based on the design documents and existing code.

---

## Phase 1: Foundation (Already Done)

- [x] `model` crate - Manifest structures, encode/decode
- [x] `storage` crate - Types, traits, CAS utilities

---

## Phase 2: Common Utilities

**Crate:** `common` ✅ COMPLETE

Shared utilities referenced by all other crates.

- [x] Path utilities
  - `to_absolute()`
  - `lexical_normalize()`
  - `normalize_for_manifest()`
  - `to_posix_path()`
  - `from_posix_path()`
- [x] Hash utilities
  - `hash_file()`
  - `hash_bytes()`
- [x] Constants
  - `CHUNK_SIZE_V2`
  - `DEFAULT_HASH_CACHE_TTL_DAYS`
  - `DEFAULT_STAT_CACHE_CAPACITY`
- [x] `ProgressCallback<T>` trait
- [x] `get_machine_id()` - Platform-specific machine identifier

---

## Phase 3: File System Operations

**Crate:** `filesystem` ✅ COMPLETE

Directory scanning and manifest creation.

- [x] `GlobFilter` - Include/exclude pattern matching
- [x] `expand_input_paths()` - Directory-to-file expansion
- [x] `validate_input_paths()` - Path validation
- [x] `StatCache` - File stat caching (LRU)
- [x] `FileSystemScanner`
  - `snapshot()` - Create manifest from files
  - `snapshot_structure()` - Create manifest without hashing
- [x] Symlink validation (security checks)

---

## Phase 4: Caching Layer

**Location:** `storage` crate

### 4a. Hash Cache

- [ ] `HashCacheBackend` trait
- [ ] `HashCacheKey`, `HashCacheEntry` structs
- [ ] `SqliteHashCache` implementation
- [ ] `HashCache` wrapper with TTL

### 4b. S3 Check Cache

- [ ] `S3CheckCacheBackend` trait
- [ ] `S3CheckCacheKey`, `S3CheckCacheEntry` structs
- [ ] `SqliteS3CheckCache` implementation
- [ ] `S3CheckCache` wrapper with integrity verification

---

## Phase 5: Storage Profiles & Path Grouping

**Location:** `storage` crate

- [ ] `FileSystemLocationType` enum (Local/Shared)
- [ ] `FileSystemLocation` struct
- [ ] `StorageProfile` struct
- [ ] `AssetRootGroup` struct
- [ ] `group_asset_paths()` - Basic grouping logic
- [ ] `PathValidationMode`, `PathGroupingResult`, `PathGroupingError`
- [ ] `group_asset_paths_validated()` - Grouping with validation

---

## Phase 6: Upload Infrastructure

**Location:** `storage` crate

### 6a. Manifest Storage

- [ ] `ManifestLocation` struct
- [ ] `ManifestS3Metadata` struct
- [ ] `ManifestUploadResult` struct
- [ ] `upload_input_manifest()` - Upload manifest with metadata

### 6b. Upload Orchestrator

- [ ] `UploadOrchestrator` struct
- [ ] `upload_manifest()` - Upload CAS objects from manifest
- [ ] `with_expected_bucket_owner()` - Security configuration
- [ ] Integration with S3 check cache

---

## Phase 7: Job Submission Conversion

**Location:** `storage` crate

- [ ] `PathFormat` enum (Windows/Posix)
- [ ] `ManifestProperties` struct
- [ ] `Attachments` struct
- [ ] `AssetRootManifest` struct
- [ ] `build_manifest_properties()`
- [ ] `build_attachments()`

---

## Phase 8: Integration

**Location:** `storage` crate or new `bundle` crate

- [ ] `AssetReferences` struct
- [ ] `BundleSubmitResult` struct
- [ ] `BundleSubmitError` enum
- [ ] `submit_bundle_attachments()` - Main entry point

---

## Critical Path

```
common → filesystem → storage profiles → upload orchestrator → bundle submit
              ↓
           caches (parallel)
```

---

## Suggested Approach

1. Start with **Phase 2 (common)** - everything depends on it
2. **Phase 3 (filesystem)** and **Phase 4 (caches)** can be done in parallel
3. **Phase 5-7** build sequentially
4. **Phase 8** composes everything together

---

## Related Documents

- [example-bundle-submit.md](../examples/example-bundle-submit.md) - Full usage example
- [common.md](../common.md) - Common utilities design
- [file_system.md](../file_system.md) - File system operations design
- [hash-cache.md](../hash-cache.md) - Caching design
- [storage-profiles.md](../storage-profiles.md) - Storage profile design
- [storage-design.md](../storage-design.md) - Upload/download design
- [manifest-storage.md](../manifest-storage.md) - Manifest S3 operations
- [job-submission.md](../job-submission.md) - Job attachments format
