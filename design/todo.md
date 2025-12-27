# Rusty Attachments: TODO

## Design Documents Completed
- [x] Model design (manifest structures)
- [x] Storage design (upload/download orchestration)
- [x] Manifest storage (S3 manifest upload/download with metadata)
- [x] File system (snapshot/diff operations)
- [x] Hash cache (local file hash caching)
- [x] S3 check cache (S3 existence caching)
- [x] Storage profiles (file system locations)
- [x] Job submission (manifest to job attachments conversion)
- [x] Manifest utilities (diff/merge operations)
- [x] Path mapping (dynamic path mapping, unique directory naming)
- [x] Common module (shared constants, utilities, error types)

## Implementation TODO

### Core
- [x] Implement `common` crate with shared utilities
- [ ] Implement business logic to upload a manifest
- [ ] Implement logic to upload the manifest file
- [ ] File folder scanning, snapshot folder, diff a folder
- [ ] Manifest utilities: diff manifest, merge manifest

### VFS (Virtual File System)
- [x] Read-only FUSE VFS (`crates/vfs/src/fuse.rs`)
- [x] Writable FUSE VFS with COW (`crates/vfs/src/fuse_writable.rs`)
- [x] DirtyFileManager for file modifications (`crates/vfs/src/write/dirty.rs`)
- [x] DirtyDirManager for directory operations (`crates/vfs/src/write/dirty_dir.rs`)
- [x] WriteCache trait and implementations (`crates/vfs/src/write/cache.rs`)
- [x] DiffManifestExporter trait (`crates/vfs/src/write/export.rs`)
- [ ] Full diff manifest export implementation (currently returns "not yet implemented")
- [ ] FUSE integration tests for mkdir/rmdir

### Caches
- [ ] Hash cache SQLite backend
- [ ] S3 check cache SQLite backend

### Testing
- [ ] Fuzz testing with weird file paths (unicode, special chars, long paths)
- [ ] Edge cases: merging manifests with time ordering
- [ ] Compatibility with Python manifest file names
- [ ] Roundtrip tests: Python create → Rust read → Python read

### Features
- [ ] S3 object tags for manifests
- [ ] Path mapping utilities
- [ ] Storage profile utilities

### CLI Design
- [ ] Disk capacity validation before download (check available space vs manifest total size)
- [ ] Detailed error guidance messages (status code specific hints for 403, 404, KMS errors)

### Bindings
- [ ] Python bindings (PyO3)
- [ ] WASM bindings

---

## Download Module Analysis (from Python download.py)

### Python Functions Analyzed

| Function | Purpose | Design Coverage |
|----------|---------|-----------------|
| `get_manifest_from_s3` | Download manifest by S3 key | ✅ manifest-storage.md |
| `get_asset_root_and_manifest_from_s3` | Download manifest + extract asset root from metadata | ✅ manifest-storage.md |
| `_get_asset_root_and_manifest_from_s3_with_last_modified` | Same + LastModified timestamp | ✅ manifest-storage.md |
| `_get_asset_root_from_metadata` | Parse asset-root/asset-root-json from S3 metadata | ✅ manifest-storage.md |
| `_get_output_manifest_prefix` | Build S3 prefix for output manifests (job/step/task) | ✅ manifest-storage.md |
| `_list_s3_objects_with_error_handling` | Paginated S3 list with error handling | ✅ manifest-storage.md |
| `_get_tasks_manifests_keys_from_s3` | List output manifest keys, select latest per task | ✅ manifest-storage.md |
| `get_job_input_paths_by_asset_root` | Get input files grouped by asset root | ✅ manifest-utils.md (ManifestPathGroup) |
| `get_job_input_output_paths_by_asset_root` | Combine input + output paths by root | ✅ manifest-utils.md |
| `get_job_output_paths_by_asset_root` | Get output files grouped by asset root | ✅ manifest-storage.md |
| `get_output_manifests_by_asset_root` | Download + merge output manifests chronologically | ✅ manifest-storage.md |
| `_get_new_copy_file_path` | Generate unique filename for conflict resolution | ✅ storage-design.md |
| `download_files_in_directory` | Download files matching a directory prefix | 📋 TODO (low priority) |
| `download_file` | Download single file from CAS to local path | ✅ storage-design.md |
| `_download_files_parallel` | Parallel file download with ThreadPoolExecutor | ✅ storage-design.md (CRT handles) |
| `download_files` | Download files from manifest | ✅ storage-design.md |
| `download_files_from_manifests` | Download from multiple manifests by root | ✅ storage-design.md |
| `merge_asset_manifests` | Merge manifests (last-write-wins) | ✅ manifest-utils.md |
| `_merge_asset_manifests_sorted_asc_by_last_modified` | Merge with chronological ordering | ✅ manifest-utils.md |
| `_ensure_paths_within_directory` | Security: validate paths within root | ✅ file_system.md (symlink security) |
| `_set_fs_group` | Set file permissions (POSIX/Windows) | ✅ file_system.md |
| `OutputDownloader` class | High-level download orchestrator | ✅ storage-design.md |
| `OutputDownloader.set_root_path` | Remap asset root for download | 🚫 Skipped |
| `_get_manifests_by_session_action_id` | Get manifests for specific session action | 📋 TODO (low priority) |

### Remaining Implementation TODOs

#### Medium Priority
- [ ] Directory-filtered download (`download_files_in_directory`)
- [ ] Session action ID manifest lookup (`_get_manifests_by_session_action_id`)

---

## Skipped Features

Features intentionally not implemented in Rust, with rationale:

### AssetSync Orchestrator Class

**Python Feature:** `AssetSync` class managing full lifecycle (input sync → job → output sync)

**Why Skipped:**
- This is application-level orchestration, not core library functionality
- The individual components (upload, download, manifest operations) are designed
- Orchestration logic is better implemented by the consuming application (worker agent)
- Keeps the Rust library focused on primitives that can be composed

### VFS (Virtual File System) Mode

**Python Feature:** `VFSProcessManager`, `mount_vfs_from_manifests()`

**Why Skipped:**
- VFS is a separate system component (deadline-cloud-worker-agent)
- Requires OS-specific FUSE/similar implementations
- Out of scope for the core attachments library
- Can be implemented as a separate crate if needed

### S3 Check Cache Integrity Verification

**Python Feature:** `verify_hash_cache_integrity()` - sample 30 entries and HEAD each

**Why Skipped:**
- This is an optimization/recovery feature, not core functionality
- The cache already handles staleness via TTL
- Can be added later if cache corruption becomes an issue in practice
- Simpler to just reset cache on first 404 during upload

### Local Manifest Writing with S3 Mapping File

**Python Feature:** `_write_local_manifest()` and `manifest_write_dir` parameter

**Why Skipped:**
- Debugging/tooling feature, not core functionality
- Snapshot mode covers offline use case
- Rust's path handling is more robust for long paths

### Asset Root Remapping (`OutputDownloader.set_root_path`)

**Python Feature:** `OutputDownloader.set_root_path(original_root, new_root)`

**Why Skipped:**
- Convenience feature for CLI/GUI
- Download orchestrator accepts destination root parameter
- Path mapping is a separate concern (see path-mapping.md)

### S3 Key Fallback Without Hash Algorithm Extension

**Python Feature:** Retry download without `.xxh128` suffix if 404

**Why Skipped:**
- Backwards compatibility for very old data
- New implementations should use correct key format

### Progress Tracker Threading Details

**Python Feature:** `ProgressTracker` with `Lock`, chunked reporting, logging intervals

**Why Skipped:**
- CRT handles progress reporting internally
- Rust async model differs from Python threading
- Progress callbacks are defined in storage-design.md

### Download Summary Statistics Extensions

**Python Feature:** `DownloadSummaryStatistics` with `file_counts_by_root_directory` and `downloaded_files`

**Why Skipped:**
- Application-level reporting concern
- Base `SummaryStatistics` in storage-design.md is sufficient
- Can be computed by caller from download results

**Optional Enhancement:** If needed, extend `SummaryStatistics` with:
```rust
#[derive(Debug, Clone, Default)]
pub struct DownloadSummaryStatistics {
    pub stats: SummaryStatistics,
    pub file_counts_by_root: HashMap<String, u64>,
    pub downloaded_files: Vec<PathBuf>,
}
```

### Snapshot Mode (Local Copy)

**Python Feature:** `_snapshot_assets()`, `snapshot_assets()`

**Why Skipped:**
- Debugging/testing feature
- Can be implemented as CLI wrapper
- Core upload logic is designed

### Hash Cache Entry Structure Differences

**Python Feature:** Uses `(file_path, hash_algorithm)` as key with `last_modified_time` as string

**Why Skipped:**
- Our design uses `(path, size, mtime)` as key which is more robust
- Detects file changes even if mtime is unreliable
- Different but equivalent approach

---

## Related Documents

- [common.md](common.md) - Shared constants, utilities, error types
- [model-design.md](model-design.md) - Manifest data structures
- [storage-design.md](storage-design.md) - Upload/download orchestration
- [manifest-storage.md](manifest-storage.md) - Manifest S3 operations
- [file_system.md](file_system.md) - Directory scanning and security
- [hash-cache.md](hash-cache.md) - Local caching
- [storage-profiles.md](storage-profiles.md) - File system locations
- [job-submission.md](job-submission.md) - Job attachments format
- [manifest-utils.md](manifest-utils.md) - Diff/merge/output detection
- [path-mapping.md](path-mapping.md) - Path transformation utilities
