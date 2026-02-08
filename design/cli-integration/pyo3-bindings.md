# PyO3 Bindings Design: CLI Integration API

This document defines the PyO3 binding API for `crates/python/` that bridges
all Rust-beneficial deadline-cloud CLIs to rusty-attachments.

## Design Principles

1. One async Python function per CLI cut point (not per Rust primitive)
2. Reuse existing `Py*` wrapper types — extend, don't duplicate
3. All functions accept pre-resolved credentials and return structured results
4. Python owns: CLI parsing, config, Deadline API calls, user prompts, progress rendering
5. Rust owns: file I/O, hashing, manifest ops, S3 CAS transfer

## Integration Flow

```
┌─────────────────────────────────────────────────────────────────────┐
│  Python (deadline-cloud CLI)                                        │
│                                                                     │
│  click CLI ──► config resolution ──► Deadline API ──► credentials   │
│                                                          │          │
│                                                          ▼          │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  import rusty_attachments                                     │  │
│  │                                                               │  │
│  │  result = await rusty_attachments.<binding_fn>(                │  │
│  │      region, s3_location, ..., progress_callback              │  │
│  │  )                                                            │  │
│  └───────────────────┬───────────────────────────────────────────┘  │
│                      │ PyO3 async bridge                            │
├──────────────────────┼──────────────────────────────────────────────┤
│  Rust (crates/python) │                                             │
│                      ▼                                              │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  pyo3_async_runtimes::tokio::future_into_py                   │  │
│  │                                                               │  │
│  │  ┌─────────────┐  ┌──────────────┐  ┌──────────────────────┐ │  │
│  │  │ filesystem   │  │ model        │  │ storage              │ │  │
│  │  │ ─────────── │  │ ──────────── │  │ ──────────────────── │ │  │
│  │  │ GlobFilter   │  │ decode()     │  │ UploadOrchestrator   │ │  │
│  │  │ Scanner      │  │ encode()     │  │ DownloadOrchestrator │ │  │
│  │  │ DiffOptions  │  │ merge()      │  │ HashCache            │ │  │
│  │  └─────────────┘  │ diff()       │  │ S3CheckCache         │ │  │
│  │                    └──────────────┘  │ ManifestStorage      │ │  │
│  │  ┌─────────────┐  ┌──────────────┐  └──────────────────────┘ │  │
│  │  │ common       │  │ profiles     │  ┌──────────────────────┐ │  │
│  │  │ ─────────── │  │ ──────────── │  │ storage-crt          │ │  │
│  │  │ hash_file()  │  │ StorageProf. │  │ ──────────────────── │ │  │
│  │  │ hash_bytes() │  │ AssetGroup   │  │ DefaultClient (CRT)  │ │  │
│  │  └─────────────┘  └──────────────┘  └──────────────────────┘ │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

## Shared Data Structures

These types are shared across multiple binding functions. Existing types
(`PyS3Location`, `PyManifestLocation`, `PySummaryStatistics`, etc.) are
reused as-is. New types are listed below.

### New PyO3 Types

```rust
// ── Conflict Resolution ─────────────────────────────────────────

#[pyclass(name = "ConflictResolution")]
struct PyConflictResolution(ConflictResolution);

// Constructed from Python string: "SKIP" | "OVERWRITE" | "CREATE_COPY"
```

```rust
// ── Path Mapping Rule ───────────────────────────────────────────

#[pyclass(name = "PathMappingRule")]
struct PyPathMappingRule {
    source_path_format: String,
    source_path: String,
    destination_path: String,
}
```

```rust
// ── Download Statistics ─────────────────────────────────────────
// Extends PySummaryStatistics with per-root file counts

#[pyclass(name = "DownloadSummaryStatistics")]
struct PyDownloadSummaryStatistics {
    stats: TransferStatistics,
    file_counts_by_root: HashMap<String, u64>,
}
```

```rust
// ── Manifest Diff Result ────────────────────────────────────────

#[pyclass(name = "ManifestDiffResult")]
struct PyManifestDiffResult {
    new: Vec<String>,
    modified: Vec<String>,
    deleted: Vec<String>,
}
```

```rust
// ── Manifest Snapshot Result ────────────────────────────────────

#[pyclass(name = "ManifestSnapshotResult")]
struct PyManifestSnapshotResult {
    root: String,
    manifest_path: String,
}
```

```rust
// ── Upload Manifest Info ────────────────────────────────────────

#[pyclass(name = "UploadManifestInfo")]
struct PyUploadManifestInfo {
    output_manifest_path: String,
    output_manifest_hash: String,
    source_path: String,
}
```

```rust
// ── Manifest Download Entry ─────────────────────────────────────

#[pyclass(name = "ManifestDownloadEntry")]
struct PyManifestDownloadEntry {
    manifest_root: String,
    local_manifest_path: String,
}
```

```rust
// ── Output Manifest Scope ───────────────────────────────────────

#[pyclass(name = "OutputManifestScope")]
struct PyOutputManifestScope {
    farm_id: String,
    queue_id: String,
    job_id: String,
    step_id: Option<String>,
}
```

See [Appendix A](#appendix-a-full-pyclass-definitions) for complete `#[pymethods]` blocks.

---

## Binding Functions

### 1. `manifest_snapshot`

Replaces: `_manifest_snapshot()` in `api/manifest.py`

CLI: `deadline manifest snapshot`

```
Python                              Rust
──────────────────────────────      ──────────────────────────────
click args, input validation   ──►  manifest_snapshot_py(
                                        root: str,
                                        destination: str,
                                        name: Option<str>,
                                        include: Vec<str>,
                                        exclude: Vec<str>,
                                        include_exclude_config: Option<str>,
                                        diff_manifest: Option<str>,
                                        force_rehash: bool,
                                        hash_cache_dir: str,
                                        progress_callback: Option<PyObject>,
                                    ) -> Option<ManifestSnapshotResult>
JSON output formatting         ◄──
```

Rust internals: `GlobFilter` → `FileSystemScanner::snapshot()` or `diff()` →
`HashCache` → `Manifest::encode()` → write to `destination/`

---

### 2. `manifest_diff`

Replaces: `_manifest_diff()` in `api/manifest.py`

CLI: `deadline manifest diff`

```
Python                              Rust
──────────────────────────────      ──────────────────────────────
click args, input validation   ──►  manifest_diff_py(
                                        root: str,
                                        manifest_path: str,
                                        include: Vec<str>,
                                        exclude: Vec<str>,
                                        include_exclude_config: Option<str>,
                                        force_rehash: bool,
                                        hash_cache_dir: str,
                                        progress_callback: Option<PyObject>,
                                    ) -> ManifestDiffResult
pretty_print / JSON output     ◄──
```

Rust internals: `GlobFilter` → `Manifest::decode()` → `FileSystemScanner::diff()`
or `compute_diff_manifest()` → `HashCache`

---

### 3. `manifest_download`

Replaces: `_manifest_download()` in `api/manifest.py`

CLI: `deadline manifest download`

This function has a mixed cut — Python resolves Deadline API data (job attachments,
step dependencies) then passes manifest S3 keys to Rust for download + merge + write.

```
Python                              Rust
──────────────────────────────      ──────────────────────────────
click args, config resolution
deadline.get_queue()
deadline.get_job()
list_step_dependencies()
extract manifest S3 keys       ──►  manifest_download_py(
queue role creds                        region: str,
                                        s3_location: S3Location,
                                        input_manifest_keys: Vec<(str, str)>,
                                        output_scope: Option<OutputManifestScope>,
                                        download_dir: str,
                                        progress_callback: Option<PyObject>,
                                    ) -> Vec<ManifestDownloadEntry>
JSON output formatting         ◄──
```

`input_manifest_keys` is a list of `(s3_key, root_path)` tuples extracted from
`job["attachments"]["manifests"]`. `output_scope` triggers S3 list + discover for
output manifests when present.

Rust internals: `download_manifest()` → `merge_manifests()` → `Manifest::encode()`
→ write to `download_dir/`. If `output_scope` is set:
`discover_output_manifest_keys()` → `download_manifests_parallel()` → merge per root.

---

### 4. `manifest_upload`

Replaces: `_manifest_upload()` in `api/manifest.py`

CLI: `deadline manifest upload`

```
Python                              Rust
──────────────────────────────      ──────────────────────────────
click args, config resolution
S3 settings, queue role creds  ──►  manifest_upload_py(
read manifest file bytes                region: str,
                                        s3_location: S3Location,
                                        manifest_bytes: Vec<u8>,
                                        s3_key: str,
                                        metadata: HashMap<str, str>,
                                    ) -> ()
success message                ◄──
```

Rust internals: `StorageClient::put_object()`. Minimal standalone value but
shares the CRT client infrastructure.

---

### 5. `attachment_download`

Replaces: `_attachment_download()` in `api/attachment.py`

CLI: `deadline attachment download`

```
Python                              Rust
──────────────────────────────      ──────────────────────────────
click args, config resolution
S3 settings, queue role creds  ──►  attachment_download_py(
                                        region: str,
                                        s3_location: S3Location,
                                        manifest_paths: Vec<str>,
                                        path_mapping_rules: Option<Vec<PathMappingRule>>,
                                        conflict_resolution: str,
                                        progress_callback: Option<PyObject>,
                                    ) -> DownloadSummaryStatistics
JSON output formatting         ◄──
```

Rust internals: `Manifest::decode()` per file → resolve destinations via path
mapping → `DownloadOrchestrator::download_manifest_contents()` per root

---

### 6. `attachment_upload`

Replaces: `_attachment_upload()` in `api/attachment.py`

CLI: `deadline attachment upload`

```
Python                              Rust
──────────────────────────────      ──────────────────────────────
click args, config resolution
S3 settings, queue role creds  ──►  attachment_upload_py(
                                        region: str,
                                        s3_location: S3Location,
                                        manifest_location: ManifestLocation,
                                        manifest_paths: Vec<str>,
                                        root_dirs: Vec<str>,
                                        path_mapping_rules: Option<Vec<PathMappingRule>>,
                                        upload_manifest_path: Option<str>,
                                        s3_check_cache_dir: str,
                                        progress_callback: Option<PyObject>,
                                    ) -> Vec<UploadManifestInfo>
print upload results           ◄──
```

Rust internals: `Manifest::decode()` → resolve source roots via path mapping →
`UploadOrchestrator::upload_manifest_contents()` → `upload_input_manifest()` per
manifest → return S3 keys + hashes

---

### 7. `submit_bundle_attachments` (existing — no changes)

Already implemented in `crates/python/src/lib.rs` as `submit_bundle_attachments_py`.

CLI: `deadline bundle submit`, `deadline bundle gui-submit`

The existing two-phase design (scan+classify → user confirm → hash+upload) is
handled internally. No API changes needed.

---

### 8. `download_job_output`

Replaces: `OutputDownloader` pipeline in `download.py`

CLI: `deadline job download-output`, `deadline handle-web-url download-output`

This is a two-phase binding because the user interactively selects root paths
and conflict resolution between manifest discovery and file download.

```
Python                              Rust
──────────────────────────────      ──────────────────────────────
click args, config resolution
deadline.get_job/step/task()
deadline.get_queue()
queue role creds               ──►  Phase 1:
                                    discover_output_manifests_py(
                                        region: str,
                                        s3_location: S3Location,
                                        farm_id: str,
                                        queue_id: str,
                                        job_id: str,
                                        step_id: Option<str>,
                                        task_id: Option<str>,
                                        session_action_id: Option<str>,
                                    ) -> OutputManifestDiscovery

[interactive] root selection   ◄──  OutputManifestDiscovery {
[interactive] conflict res.             outputs_by_root: dict[str, list[str]],
                                        // opaque handle for phase 2
                                        _manifests_handle: u64,
                                    }

                               ──►  Phase 2:
                                    download_output_files_py(
                                        manifests_handle: u64,
                                        root_overrides: HashMap<str, str>,
                                        conflict_resolution: str,
                                        progress_callback: Option<PyObject>,
                                    ) -> DownloadSummaryStatistics
summary output                 ◄──
```

The opaque `_manifests_handle` avoids re-serializing the full manifest data
across the Python/Rust boundary between phases. Rust holds the manifests in
a handle map; Python passes the handle ID back.

Rust internals (Phase 1): `discover_output_manifest_keys()` →
`download_manifests_parallel()` → `merge_manifests()` per root → store in handle map

Rust internals (Phase 2): retrieve from handle map → apply root overrides →
`DownloadOrchestrator::download_manifest_contents()` per root

---

### 9. `incremental_download`

Replaces: phases 2+3 of `_incremental_output_download()` in `_incremental_download.py`

CLI: `deadline queue sync-output`

Python retains all Deadline API orchestration (SearchJobs, ListSessions,
ListSessionActions) and checkpoint state management. Rust handles manifest
download → path mapping → merge → file download.

```
Python                              Rust
──────────────────────────────      ──────────────────────────────
click args, config resolution
checkpoint load
SearchJobs, ListSessions,
  ListSessionActions
categorize jobs, storage
  profile resolution
path mapping rule construction ──►  incremental_download_py(
                                        region: str,
                                        s3_location: S3Location,
                                        manifest_specs: Vec<ManifestDownloadSpec>,
                                        path_mapping_rules: Vec<PathMappingRule>,
                                        conflict_resolution: str,
                                        progress_callback: Option<PyObject>,
                                    ) -> DownloadSummaryStatistics
checkpoint save                ◄──
summary output
```

```rust
#[pyclass(name = "ManifestDownloadSpec")]
struct PyManifestDownloadSpec {
    s3_key: String,
    asset_root: String,
    last_modified: f64,  // epoch seconds, for merge ordering
}
```

Rust internals: `download_manifests_parallel()` → apply `PathMappingRule` →
`merge_manifests_chronologically()` per root → `DownloadOrchestrator::download_manifest_contents()`

---

## Progress Callback Protocol

All binding functions accept an optional `progress_callback: Callable[[dict], bool]`.
The dict schema is consistent across all operations:

```python
{
    "phase": "scanning" | "hashing" | "uploading" | "downloading" | "merging",
    "current_path": Optional[str],
    "files_processed": int,
    "total_files": int,
    "bytes_processed": int,
    "total_bytes": int,
}
```

Return `True` to continue, `False` to cancel. The existing `PyProgressCallback`
wrapper handles the GIL crossing.

---

## Module Registration

```rust
#[pymodule]
fn rusty_attachments(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Existing
    m.add_class::<PyS3Location>()?;
    m.add_class::<PyManifestLocation>()?;
    m.add_class::<PyAssetReferences>()?;
    m.add_class::<PyBundleSubmitOptions>()?;
    m.add_class::<PyBundleSubmitResult>()?;
    m.add_class::<PySummaryStatistics>()?;
    m.add_class::<PyFileSystemLocation>()?;
    m.add_class::<PyStorageProfile>()?;
    m.add_class::<PyManifest>()?;

    // New types
    m.add_class::<PyPathMappingRule>()?;
    m.add_class::<PyDownloadSummaryStatistics>()?;
    m.add_class::<PyManifestDiffResult>()?;
    m.add_class::<PyManifestSnapshotResult>()?;
    m.add_class::<PyUploadManifestInfo>()?;
    m.add_class::<PyManifestDownloadEntry>()?;
    m.add_class::<PyOutputManifestScope>()?;
    m.add_class::<PyManifestDownloadSpec>()?;
    m.add_class::<PyOutputManifestDiscovery>()?;

    // Existing functions
    m.add_function(wrap_pyfunction!(submit_bundle_attachments_py, m)?)?;
    m.add_function(wrap_pyfunction!(decode_manifest, m)?)?;

    // New binding functions
    m.add_function(wrap_pyfunction!(manifest_snapshot_py, m)?)?;
    m.add_function(wrap_pyfunction!(manifest_diff_py, m)?)?;
    m.add_function(wrap_pyfunction!(manifest_download_py, m)?)?;
    m.add_function(wrap_pyfunction!(manifest_upload_py, m)?)?;
    m.add_function(wrap_pyfunction!(attachment_download_py, m)?)?;
    m.add_function(wrap_pyfunction!(attachment_upload_py, m)?)?;
    m.add_function(wrap_pyfunction!(discover_output_manifests_py, m)?)?;
    m.add_function(wrap_pyfunction!(download_output_files_py, m)?)?;
    m.add_function(wrap_pyfunction!(incremental_download_py, m)?)?;

    Ok(())
}
```

---

## Python-Side Integration Pattern

Each CLI group replaces its core function call with the Rust binding. The pattern
is identical across all CLIs:

```python
# Before (Python-native)
from deadline.job_attachments.api.manifest import _manifest_snapshot
result = _manifest_snapshot(root, destination, name, ...)

# After (Rust via PyO3)
import rusty_attachments
result = await rusty_attachments.manifest_snapshot(root, destination, name, ...)
```

The `await` is required because all Rust functions use `pyo3_async_runtimes::tokio`
to run on the Tokio runtime. Python callers wrap in `asyncio.run()` or use an
existing event loop.

For the two-phase `download_job_output` pattern:

```python
# Phase 1: discover (returns opaque handle + metadata for UI)
discovery = await rusty_attachments.discover_output_manifests(
    region, s3_location, farm_id, queue_id, job_id, step_id, task_id, session_action_id
)

# Python handles interactive prompts using discovery.outputs_by_root
root_overrides = prompt_user_for_root_selection(discovery.outputs_by_root)
conflict = prompt_user_for_conflict_resolution()

# Phase 2: download using the handle
stats = await rusty_attachments.download_output_files(
    discovery.manifests_handle, root_overrides, conflict, progress_callback
)
```

---

## CLI → Binding Function Mapping

| CLI Command | Python Function Replaced | Rust Binding |
|---|---|---|
| `manifest snapshot` | `_manifest_snapshot()` | `manifest_snapshot_py` |
| `manifest diff` | `_manifest_diff()` | `manifest_diff_py` |
| `manifest download` | `_manifest_download()` | `manifest_download_py` |
| `manifest upload` | `_manifest_upload()` | `manifest_upload_py` |
| `attachment download` | `_attachment_download()` | `attachment_download_py` |
| `attachment upload` | `_attachment_upload()` | `attachment_upload_py` |
| `bundle submit` | `_process_job_attachments()` | `submit_bundle_attachments_py` (existing) |
| `job download-output` | `OutputDownloader` pipeline | `discover_output_manifests_py` + `download_output_files_py` |
| `handle-web-url` | delegates to `job download-output` | same as above |
| `queue sync-output` | phases 2+3 of `_incremental_output_download()` | `incremental_download_py` |

---

## Crate Dependency Graph

```
crates/python
├── crates/common          (hash_file, hash_bytes, path_utils)
├── crates/filesystem      (GlobFilter, Scanner, DiffOptions)
├── crates/model           (Manifest decode/encode/merge/diff)
├── crates/profiles        (StorageProfile, AssetRootGroup)
├── crates/storage         (Upload/DownloadOrchestrator, HashCache, S3CheckCache, ManifestStorage)
├── crates/storage-crt     (DefaultClient — CRT-based S3)
└── crates/ja-deadline-utils (BundleSubmitOptions, build_attachments, ManifestProperties)
```

All binding functions follow the same internal pattern:
1. Convert `Py*` wrappers → Rust types
2. Create `DefaultClient` from region (reuse across calls via `Arc`)
3. Call composed Rust functions on Tokio runtime
4. Convert results → `Py*` wrappers
5. Return via `pyo3_async_runtimes::tokio::future_into_py`

---

## Implementation Order

1. Shared types (`PyPathMappingRule`, `PyDownloadSummaryStatistics`, etc.)
2. `manifest_snapshot_py` + `manifest_diff_py` — local-only, no S3, easiest to test
3. `manifest_upload_py` — single S3 PUT, minimal
4. `attachment_download_py` + `attachment_upload_py` — bulk S3 transfer
5. `manifest_download_py` — S3 list + download + merge
6. `discover_output_manifests_py` + `download_output_files_py` — two-phase with handle
7. `incremental_download_py` — most complex, depends on all above primitives

---

## Appendix A: Full `#[pyclass]` Definitions

### `PyPathMappingRule`

```rust
/// A path mapping rule for translating source paths to destination paths.
#[pyclass(name = "PathMappingRule")]
#[derive(Clone)]
struct PyPathMappingRule {
    source_path_format: String,
    source_path: String,
    destination_path: String,
}

#[pymethods]
impl PyPathMappingRule {
    /// Create a new path mapping rule.
    ///
    /// # Arguments
    /// * `source_path_format` - Path format of the source ("windows" or "posix")
    /// * `source_path` - Original path to map from
    /// * `destination_path` - Target path to map to
    #[new]
    fn new(source_path_format: String, source_path: String, destination_path: String) -> Self {
        Self {
            source_path_format,
            source_path,
            destination_path,
        }
    }

    #[getter]
    fn source_path_format(&self) -> &str {
        &self.source_path_format
    }

    #[getter]
    fn source_path(&self) -> &str {
        &self.source_path
    }

    #[getter]
    fn destination_path(&self) -> &str {
        &self.destination_path
    }

    fn __repr__(&self) -> String {
        format!(
            "PathMappingRule(source='{}', dest='{}', fmt='{}')",
            self.source_path, self.destination_path, self.source_path_format
        )
    }
}
```

### `PyDownloadSummaryStatistics`

```rust
/// Download statistics with per-root file counts.
#[pyclass(name = "DownloadSummaryStatistics")]
#[derive(Clone)]
struct PyDownloadSummaryStatistics {
    stats: TransferStatistics,
    file_counts_by_root: HashMap<String, u64>,
}

#[pymethods]
impl PyDownloadSummaryStatistics {
    #[getter]
    fn total_files(&self) -> u64 {
        self.stats.files_processed
    }

    #[getter]
    fn total_bytes(&self) -> u64 {
        self.stats.bytes_transferred + self.stats.bytes_skipped
    }

    #[getter]
    fn downloaded_files(&self) -> u64 {
        self.stats.files_transferred
    }

    #[getter]
    fn downloaded_bytes(&self) -> u64 {
        self.stats.bytes_transferred
    }

    #[getter]
    fn skipped_files(&self) -> u64 {
        self.stats.files_skipped
    }

    #[getter]
    fn skipped_bytes(&self) -> u64 {
        self.stats.bytes_skipped
    }

    #[getter]
    fn file_counts_by_root_directory(&self) -> HashMap<String, u64> {
        self.file_counts_by_root.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "DownloadSummaryStatistics(downloaded={}, skipped={}, roots={})",
            self.stats.files_transferred,
            self.stats.files_skipped,
            self.file_counts_by_root.len()
        )
    }
}
```

### `PyManifestDiffResult`

```rust
/// Result of a manifest diff operation.
#[pyclass(name = "ManifestDiffResult")]
#[derive(Clone)]
struct PyManifestDiffResult {
    new: Vec<String>,
    modified: Vec<String>,
    deleted: Vec<String>,
}

#[pymethods]
impl PyManifestDiffResult {
    #[getter]
    fn new_files(&self) -> Vec<String> {
        self.new.clone()
    }

    #[getter]
    fn modified(&self) -> Vec<String> {
        self.modified.clone()
    }

    #[getter]
    fn deleted(&self) -> Vec<String> {
        self.deleted.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestDiffResult(new={}, modified={}, deleted={})",
            self.new.len(),
            self.modified.len(),
            self.deleted.len()
        )
    }
}
```

### `PyManifestSnapshotResult`

```rust
/// Result of a manifest snapshot operation.
#[pyclass(name = "ManifestSnapshotResult")]
#[derive(Clone)]
struct PyManifestSnapshotResult {
    root: String,
    manifest_path: String,
}

#[pymethods]
impl PyManifestSnapshotResult {
    #[getter]
    fn root(&self) -> &str {
        &self.root
    }

    #[getter]
    fn manifest_path(&self) -> &str {
        &self.manifest_path
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestSnapshotResult(root='{}', manifest='{}')",
            self.root, self.manifest_path
        )
    }
}
```

### `PyUploadManifestInfo`

```rust
/// Information about an uploaded manifest.
#[pyclass(name = "UploadManifestInfo")]
#[derive(Clone)]
struct PyUploadManifestInfo {
    output_manifest_path: String,
    output_manifest_hash: String,
    source_path: String,
}

#[pymethods]
impl PyUploadManifestInfo {
    #[getter]
    fn output_manifest_path(&self) -> &str {
        &self.output_manifest_path
    }

    #[getter]
    fn output_manifest_hash(&self) -> &str {
        &self.output_manifest_hash
    }

    #[getter]
    fn source_path(&self) -> &str {
        &self.source_path
    }

    fn __repr__(&self) -> String {
        format!(
            "UploadManifestInfo(path='{}', hash='{}', source='{}')",
            self.output_manifest_path, self.output_manifest_hash, self.source_path
        )
    }
}
```

### `PyManifestDownloadEntry`

```rust
/// A single downloaded manifest entry.
#[pyclass(name = "ManifestDownloadEntry")]
#[derive(Clone)]
struct PyManifestDownloadEntry {
    manifest_root: String,
    local_manifest_path: String,
}

#[pymethods]
impl PyManifestDownloadEntry {
    #[getter]
    fn manifest_root(&self) -> &str {
        &self.manifest_root
    }

    #[getter]
    fn local_manifest_path(&self) -> &str {
        &self.local_manifest_path
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestDownloadEntry(root='{}', path='{}')",
            self.manifest_root, self.local_manifest_path
        )
    }
}
```

### `PyOutputManifestScope`

```rust
/// Scope for output manifest discovery (job or step level).
#[pyclass(name = "OutputManifestScope")]
#[derive(Clone)]
struct PyOutputManifestScope {
    farm_id: String,
    queue_id: String,
    job_id: String,
    step_id: Option<String>,
}

#[pymethods]
impl PyOutputManifestScope {
    /// Create a new output manifest scope.
    ///
    /// # Arguments
    /// * `farm_id` - Deadline farm ID
    /// * `queue_id` - Deadline queue ID
    /// * `job_id` - Deadline job ID
    /// * `step_id` - Optional step ID (None = job-level scope)
    #[new]
    #[pyo3(signature = (farm_id, queue_id, job_id, step_id=None))]
    fn new(
        farm_id: String,
        queue_id: String,
        job_id: String,
        step_id: Option<String>,
    ) -> Self {
        Self {
            farm_id,
            queue_id,
            job_id,
            step_id,
        }
    }

    #[getter]
    fn farm_id(&self) -> &str {
        &self.farm_id
    }

    #[getter]
    fn queue_id(&self) -> &str {
        &self.queue_id
    }

    #[getter]
    fn job_id(&self) -> &str {
        &self.job_id
    }

    #[getter]
    fn step_id(&self) -> Option<&str> {
        self.step_id.as_deref()
    }

    fn __repr__(&self) -> String {
        format!(
            "OutputManifestScope(farm='{}', queue='{}', job='{}', step={:?})",
            self.farm_id, self.queue_id, self.job_id, self.step_id
        )
    }
}
```

### `PyManifestDownloadSpec`

```rust
/// Specification for downloading a manifest from S3 (used by incremental download).
#[pyclass(name = "ManifestDownloadSpec")]
#[derive(Clone)]
struct PyManifestDownloadSpec {
    s3_key: String,
    asset_root: String,
    last_modified: f64,
}

#[pymethods]
impl PyManifestDownloadSpec {
    /// Create a new manifest download specification.
    ///
    /// # Arguments
    /// * `s3_key` - S3 key of the manifest object
    /// * `asset_root` - Asset root path this manifest belongs to
    /// * `last_modified` - Last modified timestamp (epoch seconds) for merge ordering
    #[new]
    fn new(s3_key: String, asset_root: String, last_modified: f64) -> Self {
        Self {
            s3_key,
            asset_root,
            last_modified,
        }
    }

    #[getter]
    fn s3_key(&self) -> &str {
        &self.s3_key
    }

    #[getter]
    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    #[getter]
    fn last_modified(&self) -> f64 {
        self.last_modified
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestDownloadSpec(key='{}', root='{}', modified={})",
            self.s3_key, self.asset_root, self.last_modified
        )
    }
}
```

### `PyOutputManifestDiscovery`

```rust
/// Result of output manifest discovery (phase 1 of download_job_output).
///
/// Contains the discovered output paths for user interaction, plus an opaque
/// handle to the pre-fetched manifests for phase 2.
#[pyclass(name = "OutputManifestDiscovery")]
struct PyOutputManifestDiscovery {
    outputs_by_root: HashMap<String, Vec<String>>,
    manifests_handle: u64,
}

#[pymethods]
impl PyOutputManifestDiscovery {
    /// Output file paths grouped by asset root, for user display/selection.
    #[getter]
    fn outputs_by_root(&self) -> HashMap<String, Vec<String>> {
        self.outputs_by_root.clone()
    }

    /// Opaque handle to pass to download_output_files() for phase 2.
    #[getter]
    fn manifests_handle(&self) -> u64 {
        self.manifests_handle
    }

    fn __repr__(&self) -> String {
        format!(
            "OutputManifestDiscovery(roots={}, handle={})",
            self.outputs_by_root.len(),
            self.manifests_handle
        )
    }
}
```

---

## Appendix B: Full `#[pyfunction]` Signatures

### `manifest_snapshot_py`

```rust
/// Create a manifest snapshot of a directory.
///
/// Scans the directory, hashes files (with cache), and writes the manifest
/// to the destination directory. Optionally computes a diff against an
/// existing manifest.
///
/// # Arguments
/// * `root` - Root directory to snapshot
/// * `destination` - Directory to write the manifest file
/// * `name` - Optional manifest name (defaults to sanitized root path)
/// * `include` - Glob include patterns (default: ["**/*"])
/// * `exclude` - Glob exclude patterns (default: [])
/// * `include_exclude_config` - Path to JSON config with include/exclude
/// * `diff_manifest` - Path to existing manifest for diff mode
/// * `force_rehash` - If true, hash all files even in diff mode
/// * `hash_cache_dir` - Directory for the SQLite hash cache
/// * `progress_callback` - Optional callback(dict) -> bool
///
/// # Returns
/// ManifestSnapshotResult with root and manifest path, or None if no files found.
#[pyfunction]
#[pyo3(signature = (
    root,
    destination,
    name=None,
    include=None,
    exclude=None,
    include_exclude_config=None,
    diff_manifest=None,
    force_rehash=false,
    hash_cache_dir=None,
    progress_callback=None,
))]
fn manifest_snapshot_py<'py>(
    py: Python<'py>,
    root: String,
    destination: String,
    name: Option<String>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    include_exclude_config: Option<String>,
    diff_manifest: Option<String>,
    force_rehash: bool,
    hash_cache_dir: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```

### `manifest_diff_py`

```rust
/// Diff a directory against an existing manifest.
///
/// # Arguments
/// * `root` - Root directory to compare
/// * `manifest_path` - Path to the reference manifest file
/// * `include` - Glob include patterns
/// * `exclude` - Glob exclude patterns
/// * `include_exclude_config` - Path to JSON config with include/exclude
/// * `force_rehash` - If true, compare by hash instead of mtime/size
/// * `hash_cache_dir` - Directory for the SQLite hash cache
/// * `progress_callback` - Optional callback(dict) -> bool
///
/// # Returns
/// ManifestDiffResult with new, modified, and deleted file lists.
#[pyfunction]
#[pyo3(signature = (
    root,
    manifest_path,
    include=None,
    exclude=None,
    include_exclude_config=None,
    force_rehash=false,
    hash_cache_dir=None,
    progress_callback=None,
))]
fn manifest_diff_py<'py>(
    py: Python<'py>,
    root: String,
    manifest_path: String,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    include_exclude_config: Option<String>,
    force_rehash: bool,
    hash_cache_dir: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```

### `manifest_download_py`

```rust
/// Download and merge manifests from S3, write to local directory.
///
/// # Arguments
/// * `region` - AWS region
/// * `s3_location` - S3Location with bucket and prefixes
/// * `input_manifest_keys` - List of (s3_key, root_path) tuples from job attachments
/// * `output_scope` - Optional OutputManifestScope for output manifest discovery
/// * `download_dir` - Local directory to write merged manifests
/// * `progress_callback` - Optional callback(dict) -> bool
///
/// # Returns
/// List of ManifestDownloadEntry with root and local path for each merged manifest.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    input_manifest_keys,
    output_scope=None,
    download_dir=".",
    progress_callback=None,
))]
fn manifest_download_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    input_manifest_keys: Vec<(String, String)>,
    output_scope: Option<PyOutputManifestScope>,
    download_dir: String,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```

### `manifest_upload_py`

```rust
/// Upload a manifest file to S3.
///
/// # Arguments
/// * `region` - AWS region
/// * `s3_location` - S3Location with bucket and prefixes
/// * `manifest_bytes` - Raw manifest content as bytes
/// * `s3_key` - Full S3 key to upload to
/// * `metadata` - S3 object metadata key-value pairs
///
/// # Returns
/// None on success.
#[pyfunction]
fn manifest_upload_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_bytes: Vec<u8>,
    s3_key: String,
    metadata: HashMap<String, String>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```

### `attachment_download_py`

```rust
/// Download attachment files from S3 CAS using local manifest files.
///
/// # Arguments
/// * `region` - AWS region
/// * `s3_location` - S3Location with bucket and CAS prefix
/// * `manifest_paths` - Local paths to manifest files
/// * `path_mapping_rules` - Optional path mapping rules for destination resolution
/// * `conflict_resolution` - "SKIP", "OVERWRITE", or "CREATE_COPY"
/// * `progress_callback` - Optional callback(dict) -> bool
///
/// # Returns
/// DownloadSummaryStatistics with file counts and byte totals.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    manifest_paths,
    path_mapping_rules=None,
    conflict_resolution="CREATE_COPY",
    progress_callback=None,
))]
fn attachment_download_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_paths: Vec<String>,
    path_mapping_rules: Option<Vec<PyPathMappingRule>>,
    conflict_resolution: String,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```

### `attachment_upload_py`

```rust
/// Upload attachment files to S3 CAS using local manifest files.
///
/// # Arguments
/// * `region` - AWS region
/// * `s3_location` - S3Location with bucket and CAS prefix
/// * `manifest_location` - ManifestLocation for manifest S3 uploads
/// * `manifest_paths` - Local paths to manifest files
/// * `root_dirs` - Root directories holding the actual files
/// * `path_mapping_rules` - Optional path mapping rules
/// * `upload_manifest_path` - Optional S3 prefix for manifest uploads
/// * `s3_check_cache_dir` - Directory for S3 existence check cache
/// * `progress_callback` - Optional callback(dict) -> bool
///
/// # Returns
/// List of UploadManifestInfo with S3 keys and hashes.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    manifest_location,
    manifest_paths,
    root_dirs,
    path_mapping_rules=None,
    upload_manifest_path=None,
    s3_check_cache_dir=None,
    progress_callback=None,
))]
fn attachment_upload_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_location: PyManifestLocation,
    manifest_paths: Vec<String>,
    root_dirs: Vec<String>,
    path_mapping_rules: Option<Vec<PyPathMappingRule>>,
    upload_manifest_path: Option<String>,
    s3_check_cache_dir: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```

### `discover_output_manifests_py`

```rust
/// Discover and pre-fetch output manifests for a job (phase 1 of download_job_output).
///
/// # Arguments
/// * `region` - AWS region
/// * `s3_location` - S3Location with bucket and prefixes
/// * `farm_id` - Deadline farm ID
/// * `queue_id` - Deadline queue ID
/// * `job_id` - Deadline job ID
/// * `step_id` - Optional step ID filter
/// * `task_id` - Optional task ID filter
/// * `session_action_id` - Optional session action ID filter
///
/// # Returns
/// OutputManifestDiscovery with outputs_by_root for UI and opaque manifests_handle.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    farm_id,
    queue_id,
    job_id,
    step_id=None,
    task_id=None,
    session_action_id=None,
))]
fn discover_output_manifests_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    farm_id: String,
    queue_id: String,
    job_id: String,
    step_id: Option<String>,
    task_id: Option<String>,
    session_action_id: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```

### `download_output_files_py`

```rust
/// Download output files using a pre-fetched manifest handle (phase 2 of download_job_output).
///
/// # Arguments
/// * `manifests_handle` - Opaque handle from discover_output_manifests()
/// * `root_overrides` - Map of original_root -> new_root for user-selected paths
/// * `conflict_resolution` - "SKIP", "OVERWRITE", or "CREATE_COPY"
/// * `progress_callback` - Optional callback(dict) -> bool
///
/// # Returns
/// DownloadSummaryStatistics with file counts and byte totals.
#[pyfunction]
#[pyo3(signature = (manifests_handle, root_overrides, conflict_resolution="CREATE_COPY", progress_callback=None))]
fn download_output_files_py<'py>(
    py: Python<'py>,
    manifests_handle: u64,
    root_overrides: HashMap<String, String>,
    conflict_resolution: String,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```

### `incremental_download_py`

```rust
/// Download manifests and files incrementally for queue sync-output.
///
/// Downloads manifest files from S3, applies path mapping rules, merges
/// manifests chronologically per root, then downloads all referenced files.
///
/// # Arguments
/// * `region` - AWS region
/// * `s3_location` - S3Location with bucket and CAS prefix
/// * `manifest_specs` - List of ManifestDownloadSpec with S3 keys and ordering
/// * `path_mapping_rules` - Path mapping rules from storage profile resolution
/// * `conflict_resolution` - "SKIP", "OVERWRITE", or "CREATE_COPY"
/// * `progress_callback` - Optional callback(dict) -> bool
///
/// # Returns
/// DownloadSummaryStatistics with file counts and byte totals.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    manifest_specs,
    path_mapping_rules,
    conflict_resolution="CREATE_COPY",
    progress_callback=None,
))]
fn incremental_download_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_specs: Vec<PyManifestDownloadSpec>,
    path_mapping_rules: Vec<PyPathMappingRule>,
    conflict_resolution: String,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    // ... async implementation
}
```
