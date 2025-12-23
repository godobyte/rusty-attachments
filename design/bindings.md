# Python Bindings (PyO3)

## Overview

This document outlines the APIs exported via PyO3 for building a Python CLI layer on top of the Rust job attachments library.

---

## Current Implementation Status

### Implemented (Phase 1 - MVP)

| Component | Status | Description |
|-----------|--------|-------------|
| `S3Location` | ✅ | S3 bucket and prefix configuration |
| `ManifestLocation` | ✅ | Farm/queue manifest location |
| `AssetReferences` | ✅ | Input files, output dirs, referenced paths |
| `BundleSubmitOptions` | ✅ | Options for bundle submit |
| `BundleSubmitResult` | ✅ | Result with attachments JSON and stats |
| `SummaryStatistics` | ✅ | File/byte counts for operations |
| `FileSystemLocation` | ✅ | Single location in storage profile |
| `StorageProfile` | ✅ | Storage profile with locations |
| `Manifest` | ✅ | Manifest wrapper (decode/encode) |
| `submit_bundle_attachments_py()` | ✅ | Async job submission function |
| `decode_manifest()` | ✅ | Parse manifest from JSON |
| Exception classes | ✅ | `AttachmentError`, `StorageError`, `ValidationError` |

### Not Yet Implemented (Phase 2+)

| Component | Status | Description |
|-----------|--------|-------------|
| `sync_inputs()` | ❌ | Worker input download |
| `sync_outputs()` | ❌ | Worker output upload |
| `create_manifest()` | ❌ | Create manifest from directory |
| `diff_manifests()` | ❌ | Compare two manifests |
| Upload progress callback | ❌ | Separate callback for upload phase |

---

## Architecture Note: Single vs Multiple Python Files

### Current Approach: Single Rust Module

The Python bindings are implemented as a single compiled Rust extension (`rusty_attachments.so`/`.pyd`). This is the standard PyO3 approach and has these characteristics:

**Pros:**
- Single compilation unit = faster builds
- All Rust code shares types directly
- No Python import overhead between classes
- Type stubs (`.pyi`) provide IDE support

**Cons:**
- Cannot split into multiple Python files at runtime
- All classes appear in one module

### Can Each Class Be in Its Own Python File?

**Short answer: Not directly with PyO3.**

PyO3 compiles Rust code into a single native extension module. The classes are defined in Rust, not Python, so they cannot be split into separate `.py` files.

**Workarounds if needed:**

1. **Re-export via Python wrapper package** (recommended if organization is important):
   ```
   rusty_attachments/
   ├── __init__.py           # Re-exports everything
   ├── _native.so            # Compiled Rust extension
   ├── s3.py                 # from ._native import S3Location, ManifestLocation
   ├── assets.py             # from ._native import AssetReferences
   ├── submit.py             # from ._native import submit_bundle_attachments_py
   └── manifest.py           # from ._native import Manifest, decode_manifest
   ```

2. **Multiple Rust crates** (not recommended):
   - Would require separate compilation for each module
   - Significant complexity for minimal benefit

3. **Keep current approach with good documentation**:
   - Use comprehensive docstrings in Rust
   - Provide `.pyi` stub file for IDE support
   - Organize by logical sections in documentation

**Recommendation:** Keep the single-module approach. The `.pyi` stub file already provides excellent IDE support, and Sphinx can document the module well regardless of file organization.

---

## Core Use Cases

1. **Job Submission** - Upload attachments and get `Attachments` JSON for `CreateJob` API
2. **Worker Input Sync** - Download job inputs to worker session directory
3. **Worker Output Sync** - Upload job outputs after task completion
4. **Manifest Operations** - Create, read, and diff manifests for advanced workflows

---

## Proposed Python API

### 1. Job Submission

```python
from rusty_attachments import (
    submit_bundle_attachments,
    AssetReferences,
    BundleSubmitOptions,
    S3Location,
    ManifestLocation,
)

# Configure locations
s3_location = S3Location(
    bucket="my-bucket",
    root_prefix="DeadlineCloud",
    cas_prefix="Data",
    manifest_prefix="Manifests",
)

manifest_location = ManifestLocation(
    bucket="my-bucket",
    root_prefix="DeadlineCloud",
    farm_id="farm-xxx",
    queue_id="queue-xxx",
)

# Define assets
asset_refs = AssetReferences(
    input_filenames=["/projects/job/scene.blend", "/projects/job/textures"],
    output_directories=["/projects/job/renders"],
    referenced_paths=[],
)

# Submit (async)
result = await submit_bundle_attachments(
    region="us-west-2",
    s3_location=s3_location,
    manifest_location=manifest_location,
    asset_references=asset_refs,
    storage_profile=None,  # Optional[StorageProfile]
    options=BundleSubmitOptions(
        require_paths_exist=False,
        file_system_mode="COPIED",
    ),
    progress_callback=lambda p: print(f"Progress: {p.phase} {p.files_processed}/{p.total_files}"),
)

# Use result
attachments_json = result.attachments_json  # str - ready for CreateJob API
print(f"Uploaded {result.upload_stats.files_transferred} files")
```

### 2. Worker Input Sync (Future)

```python
from rusty_attachments import sync_inputs, Attachments, WorkerSyncOptions

result = await sync_inputs(
    region="us-west-2",
    s3_location=s3_location,
    attachments=Attachments.from_json(attachments_json),
    session_dir="/sessions/session-xxx",
    options=WorkerSyncOptions(
        conflict_resolution="OVERWRITE",  # SKIP, OVERWRITE, CREATE_COPY
    ),
    progress_callback=lambda p: print(f"Downloading: {p.current_key}"),
)

print(f"Downloaded {result.files_transferred} files to {result.session_dir}")
```

### 3. Worker Output Sync (Future)

```python
from rusty_attachments import sync_outputs, TaskOutputManifestPath

output_path = TaskOutputManifestPath(
    job_id="job-xxx",
    step_id="step-xxx",
    task_id="task-xxx",
    session_action_id="sessionaction-xxx",
    timestamp=time.time(),
)

result = await sync_outputs(
    region="us-west-2",
    s3_location=s3_location,
    manifest_location=manifest_location,
    output_path=output_path,
    attachments=Attachments.from_json(attachments_json),
    session_dir="/sessions/session-xxx",
    progress_callback=lambda p: print(f"Uploading: {p.current_key}"),
)

print(f"Uploaded {result.files_transferred} output files")
```

### 4. Manifest Operations

```python
from rusty_attachments import (
    create_manifest,
    load_manifest,
    diff_manifests,
    SnapshotOptions,
    GlobFilter,
)

# Create manifest from directory
manifest = create_manifest(
    root="/projects/job",
    options=SnapshotOptions(
        version="v2025-12-04-beta",
        filter=GlobFilter(exclude=["**/*.tmp", "**/__pycache__/**"]),
        follow_symlinks=False,
    ),
    progress_callback=lambda p: print(f"Hashing: {p.current_path}"),
)

print(f"Manifest: {manifest.file_count} files, {manifest.total_size} bytes")
manifest_json = manifest.to_json()

# Load existing manifest
manifest = load_manifest(json_str)

# Diff two manifests
diff = diff_manifests(old_manifest, new_manifest)
print(f"Added: {len(diff.added)}, Modified: {len(diff.modified)}, Deleted: {len(diff.deleted)}")
```

### 5. Storage Profile Support

```python
from rusty_attachments import StorageProfile, FileSystemLocation

profile = StorageProfile(
    locations=[
        FileSystemLocation(
            name="ProjectFiles",
            path="/mnt/projects",
            location_type="LOCAL",
        ),
        FileSystemLocation(
            name="SharedAssets",
            path="/mnt/shared",
            location_type="SHARED",  # Files here are skipped
        ),
    ]
)

result = await submit_bundle_attachments(
    ...,
    storage_profile=profile,
)
```

---

## Exported Types

### Data Classes

| Type | Description |
|------|-------------|
| `S3Location` | S3 bucket and prefix configuration |
| `ManifestLocation` | Farm/queue manifest location |
| `AssetReferences` | Input files, output dirs, referenced paths |
| `BundleSubmitOptions` | Options for bundle submit |
| `BundleSubmitResult` | Result with attachments JSON and stats |
| `SummaryStatistics` | File/byte counts for operations |
| `Attachments` | Job attachments payload (serializable) |
| `ManifestProperties` | Single manifest entry in attachments |
| `StorageProfile` | Storage profile with locations |
| `FileSystemLocation` | Single location in profile |
| `GlobFilter` | Include/exclude glob patterns |
| `SnapshotOptions` | Options for manifest creation |
| `WorkerSyncOptions` | Options for worker sync (future) |
| `TaskOutputManifestPath` | Task output path identifiers |
| `StepOutputManifestPath` | Step output path identifiers |

### Enums

| Type | Values |
|------|--------|
| `PathFormat` | `POSIX`, `WINDOWS` |
| `ManifestVersion` | `V2023_03_03`, `V2025_12_04_BETA` |
| `ConflictResolution` | `SKIP`, `OVERWRITE`, `CREATE_COPY` |
| `FileSystemLocationType` | `LOCAL`, `SHARED` |

### Progress Types

| Type | Fields |
|------|--------|
| `ScanProgress` | `phase`, `current_path`, `files_processed`, `total_files`, `bytes_processed`, `total_bytes` |
| `TransferProgress` | `operation`, `current_key`, `current_bytes`, `overall_completed`, `overall_total` |

---

## Functions to Export

### High-Level (ja-deadline-utils)

| Function | Description | Priority |
|----------|-------------|----------|
| `submit_bundle_attachments()` | Upload attachments for job submission | P0 |
| `sync_inputs()` | Download job inputs to worker | P1 |
| `sync_outputs()` | Upload job outputs from worker | P1 |

### Mid-Level (storage)

| Function | Description | Priority |
|----------|-------------|----------|
| `upload_manifest_contents()` | Upload files from manifest | P1 |
| `download_manifest_contents()` | Download files from manifest | P1 |
| `upload_input_manifest()` | Upload manifest JSON | P2 |
| `download_manifest()` | Download manifest JSON | P2 |

### Low-Level (filesystem)

| Function | Description | Priority |
|----------|-------------|----------|
| `create_manifest()` | Create manifest from directory | P1 |
| `load_manifest()` | Parse manifest from JSON | P1 |
| `diff_manifests()` | Compare two manifests | P2 |
| `expand_input_paths()` | Expand directories to files | P2 |

### Utilities

| Function | Description | Priority |
|----------|-------------|----------|
| `hash_file()` | Compute XXH128 hash of file | P2 |
| `GlobFilter.matches()` | Test if path matches filter | P2 |

---

## Implementation Notes

### Async Support

Use `pyo3-asyncio` with tokio runtime:

```rust
#[pyfunction]
fn submit_bundle_attachments<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    // ...
) -> PyResult<&'py PyAny> {
    pyo3_asyncio::tokio::future_into_py(py, async move {
        // Call Rust async function
        let result = ja_deadline_utils::submit_bundle_attachments(...).await?;
        Ok(PyBundleSubmitResult::from(result))
    })
}
```

### Progress Callbacks

Accept Python callables:

```rust
#[pyfunction]
fn submit_bundle_attachments(
    // ...
    progress_callback: Option<PyObject>,
) -> PyResult<...> {
    let callback = progress_callback.map(|cb| {
        PythonProgressCallback::new(cb)
    });
    // ...
}

struct PythonProgressCallback {
    callback: PyObject,
}

impl ProgressCallback<ScanProgress> for PythonProgressCallback {
    fn on_progress(&self, progress: &ScanProgress) -> bool {
        Python::with_gil(|py| {
            let py_progress = PyScanProgress::from(progress);
            self.callback.call1(py, (py_progress,)).is_ok()
        })
    }
}
```

### Error Handling

Map Rust errors to Python exceptions:

```rust
create_exception!(rusty_attachments, AttachmentError, pyo3::exceptions::PyException);
create_exception!(rusty_attachments, StorageError, AttachmentError);
create_exception!(rusty_attachments, ManifestError, AttachmentError);
create_exception!(rusty_attachments, ValidationError, AttachmentError);
```

### Credentials

Use default credential chain (environment, config file, IAM role):

```rust
// CrtStorageClient uses AWS CRT credential provider chain
let client = CrtStorageClient::new(region).await?;
```

For explicit credentials (testing/override):

```python
result = await submit_bundle_attachments(
    region="us-west-2",
    credentials=AwsCredentials(
        access_key_id="...",
        secret_access_key="...",
        session_token="...",  # optional
    ),
    # ...
)
```

---

## Module Structure

```
rusty_attachments/
├── __init__.py          # Re-exports from _rusty_attachments
├── _rusty_attachments.pyd  # Compiled Rust extension
└── py.typed             # PEP 561 marker
```

Stub file for type hints (`rusty_attachments.pyi`):

```python
from typing import Optional, Callable, List
from dataclasses import dataclass

@dataclass
class S3Location:
    bucket: str
    root_prefix: str
    cas_prefix: str
    manifest_prefix: str

@dataclass
class AssetReferences:
    input_filenames: List[str]
    output_directories: List[str]
    referenced_paths: List[str]

async def submit_bundle_attachments(
    region: str,
    s3_location: S3Location,
    manifest_location: ManifestLocation,
    asset_references: AssetReferences,
    storage_profile: Optional[StorageProfile] = None,
    options: Optional[BundleSubmitOptions] = None,
    progress_callback: Optional[Callable[[ScanProgress], bool]] = None,
) -> BundleSubmitResult: ...
```

---

## Priority Phases

### Phase 1 (MVP)
- `submit_bundle_attachments()` - Job submission workflow
- Core types: `S3Location`, `ManifestLocation`, `AssetReferences`, `BundleSubmitOptions`
- Progress callbacks for hashing/upload

### Phase 2 (Worker Support)
- `sync_inputs()` - Worker input download
- `sync_outputs()` - Worker output upload
- `StorageProfile` support

### Phase 3 (Advanced)
- `create_manifest()`, `load_manifest()`, `diff_manifests()`
- Low-level upload/download functions
- `GlobFilter` for custom filtering

---

## Sphinx Documentation

The Python bindings include comprehensive docstrings compatible with Sphinx autodoc.

### Documentation Features

- **Module-level docstring**: Overview and quick-start example
- **Class docstrings**: Description, attributes, and usage examples
- **Method docstrings**: Args, Returns, Raises, and Examples sections
- **Type stubs (`.pyi`)**: Full type annotations for IDE support

### Building Sphinx Docs

```bash
# Install sphinx
pip install sphinx sphinx-rtd-theme

# Create docs directory
mkdir docs && cd docs
sphinx-quickstart

# Add to conf.py:
extensions = ['sphinx.ext.autodoc', 'sphinx.ext.napoleon']

# Generate API docs
sphinx-apidoc -o source/ ../rusty_attachments

# Build HTML
make html
```

### Docstring Format

All docstrings follow Google style (compatible with `sphinx.ext.napoleon`):

```python
def submit_bundle_attachments_py(
    region: str,
    s3_location: S3Location,
    ...
) -> BundleSubmitResult:
    """
    Submit a job bundle with attachments.

    This async function uploads input files to S3 CAS and returns
    the attachments JSON payload for the CreateJob API.

    Args:
        region: AWS region (e.g., "us-west-2").
        s3_location: S3Location configuration for CAS storage.
        ...

    Returns:
        BundleSubmitResult containing attachments_json and statistics.

    Raises:
        StorageError: If S3 operations fail.
        ValidationError: If input validation fails.

    Example:
        >>> result = await submit_bundle_attachments_py(...)
        >>> print(result.attachments_json)
    """
```

---

## Related Documents

- [job-submission.md](job-submission.md) - Bundle submit design
- [storage-design.md](storage-design.md) - Storage layer design
- [implementation/order.md](implementation/order.md) - Implementation phases
