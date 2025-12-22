# Implementation Order for Bundle Submit

This document outlines the implementation order to build the `submit_bundle_attachments()` function, based on the design documents and existing code.

---

## Phase 1: Foundation (Already Done)

- [x] `model` crate - Manifest structures, encode/decode
  - [x] `merge_manifests()` - Merge multiple manifests (deduplicates by path)
  - [x] `merge_manifests_chronologically()` - Merge manifests sorted by timestamp (oldest first)
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

Directory scanning, manifest creation, and diff operations.

- [x] `GlobFilter` - Include/exclude pattern matching
- [x] `expand_input_paths()` - Directory-to-file expansion
- [x] `validate_input_paths()` - Path validation
- [x] `StatCache` - File stat caching (LRU)
- [x] `FileSystemScanner`
  - `snapshot()` - Create manifest from files
  - `snapshot_structure()` - Create manifest without hashing
- [x] Symlink validation (security checks)
- [x] `DiffEngine`
  - `diff()` - Compare directory against manifest
  - `create_diff_manifest()` - Create diff manifest with parentManifestHash
- [x] `DiffMode` - Fast (mtime/size) vs Hash comparison
- [x] `DiffOptions`, `DiffResult`, `DiffStats`, `FileEntry` structs

---

## Phase 4: Caching Layer

**Location:** `storage` crate ✅ COMPLETE

### 4a. Hash Cache

- [x] `HashCacheBackend` trait
- [x] `HashCacheKey`, `HashCacheEntry` structs
- [x] `HashCache` wrapper with TTL
- [x] `SqliteHashCache` implementation

### 4b. S3 Check Cache

- [x] `S3CheckCacheBackend` trait
- [x] `S3CheckCacheKey`, `S3CheckCacheEntry` structs
- [x] `S3CheckCache` wrapper with integrity verification
- [x] `SqliteS3CheckCache` implementation

---

## Phase 5: Storage Profiles & Path Grouping

**Crate:** `profiles` ✅ COMPLETE

Storage profiles and path grouping logic, separate from network operations.

- [x] `FileSystemLocationType` enum (Local/Shared)
- [x] `FileSystemLocation` struct
- [x] `StorageProfile` struct
- [x] `StorageProfileOsFamily` enum
- [x] `StorageProfileWithId` struct
- [x] `AssetRootGroup` struct
- [x] `group_asset_paths()` - Basic grouping logic
- [x] `PathValidationMode`, `PathGroupingResult`, `PathGroupingError`
- [x] `group_asset_paths_validated()` - Grouping with validation

---

## Phase 6: Upload Infrastructure

**Location:** `storage` crate

### 6a. Manifest Storage ✅ COMPLETE

- [x] `ManifestLocation` struct
- [x] `ManifestS3Metadata` struct
- [x] `ManifestUploadResult` struct
- [x] `upload_input_manifest()` - Upload manifest with metadata
- [x] `upload_task_output_manifest()` - Upload task-level output manifest
- [x] `upload_step_output_manifest()` - Upload step-level output manifest
- [x] S3 Key Format Functions (contract for S3 key structure):
  - `format_input_manifest_s3_key()` - Format input manifest S3 key
  - `format_task_output_manifest_s3_key()` - Format task-level output manifest S3 key
  - `format_step_output_manifest_s3_key()` - Format step-level output manifest S3 key
- [x] Utility functions:
  - `float_to_iso_datetime_string()` - Timestamp conversion
  - `generate_random_guid()` - GUID generation
  - `compute_manifest_name_hash()` - Manifest naming
  - `compute_root_path_hash()` - Root path hash for output manifests
  - `get_manifest_content_type()` - Content type selection
  - `build_partial_input_manifest_prefix()` - Build partial prefix for input manifests

### 6c. Manifest Download ✅ COMPLETE

- [x] `download_manifest()` - Download manifest by S3 key
- [x] `download_input_manifest()` - Download manifest by hash
- [x] Output manifest discovery primitives:
  - [x] `OutputManifestScope` enum
  - [x] `build_output_manifest_prefix()` - Build S3 prefix for listing
  - [x] `filter_output_manifest_objects()` - Filter to manifest files
  - [x] `parse_manifest_keys()` - Extract task IDs and session folders
  - [x] `group_manifests_by_task()` - Group by task ID
  - [x] `select_latest_manifests_per_task()` - Select latest per task
- [x] `discover_output_manifest_keys()` - Composed discovery function
- [x] `download_output_manifests_by_asset_root()` - Download and group by root
- [x] `download_manifests_parallel()` - Parallel manifest downloads with configurable concurrency
- [x] `ManifestDownloadOptions` - Options struct for download operations (max_concurrency)
- [x] `match_manifests_to_roots()` - Match manifest keys to job attachment roots
- [x] `find_manifests_by_session_action_id()` - Find manifests for specific session action
- [x] S3 Prefix Format Functions:
  - [x] `format_job_output_prefix()` - Format job-level output prefix
  - [x] `format_step_output_prefix()` - Format step-level output prefix
  - [x] `format_task_output_prefix()` - Format task-level output prefix
- [x] `ObjectMetadata` struct - Extended metadata from HEAD operations
- [x] `head_object_with_metadata()` - StorageClient trait method for metadata retrieval
- [x] `MissingAssetRoot` error - For compatibility with Python's MissingAssetRootError

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
common → filesystem → profiles → upload orchestrator → bundle submit
              ↓           ↓
           caches    path mapping
         (parallel)
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
