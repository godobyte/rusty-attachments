# Python Bindings Design Summary

**Full doc:** `design/bindings.md`  
**Status:** ✅ Phase 1+2 IMPLEMENTED in `crates/python/`

## Purpose
PyO3 bindings for Python CLI layer on top of Rust job attachments library.

## Implementation Status

### ✅ Implemented
- `S3Location`, `ManifestLocation`
- `AssetReferences`, `BundleSubmitOptions`, `BundleSubmitResult`
- `SummaryStatistics`, `DownloadSummaryStatistics`
- `FileSystemLocation`, `StorageProfile`
- `Manifest` (decode/encode)
- `PathMappingRule`
- `ConflictResolution`
- `OutputManifestScope`, `ManifestDownloadSpec`, `OutputManifestDiscovery`
- `ManifestSnapshotResult`, `ManifestDiffResult`
- `UploadManifestInfo`, `ManifestDownloadEntry`
- Exception classes: `AttachmentError`, `StorageError`, `ValidationError`

### ✅ Async Functions
- `submit_bundle_attachments_py()` - Full bundle submit workflow
- `manifest_snapshot_py()` - Create snapshot manifest from directory
- `manifest_diff_py()` - Diff directory against existing manifest
- `manifest_download_py()` - Download manifest from S3
- `manifest_upload_py()` - Upload manifest to S3
- `attachment_download_py()` - Download files from manifest
- `attachment_upload_py()` - Upload files from manifest
- `discover_output_manifests_py()` - Discover output manifests in S3
- `download_output_files_py()` - Download output files by scope
- `incremental_download_py()` - Incremental download with diff support

### ❌ Not Yet Implemented
- `sync_inputs()`, `sync_outputs()` (worker sync)

## Key API

```python
from rusty_attachments import (
    submit_bundle_attachments_py,
    manifest_snapshot_py, manifest_diff_py,
    manifest_download_py, manifest_upload_py,
    attachment_download_py, attachment_upload_py,
    discover_output_manifests_py, download_output_files_py,
    incremental_download_py,
    decode_manifest,
    S3Location, ManifestLocation, AssetReferences,
    BundleSubmitOptions, StorageProfile, PathMappingRule,
    OutputManifestScope, ManifestDownloadSpec,
)

# Bundle submit
result = await submit_bundle_attachments_py(
    region="us-west-2",
    s3_location=S3Location(...),
    manifest_location=ManifestLocation(...),
    asset_references=AssetReferences(...),
    progress_callback=lambda p: print(f"{p.phase} {p.files_processed}/{p.total_files}"),
)

# Snapshot a directory
result = await manifest_snapshot_py(region="us-west-2", root="/projects/job1", ...)

# Download attachments
stats = await attachment_download_py(
    region="us-west-2", manifest_json=..., root="/dest",
    s3_root_uri="s3://bucket/prefix", conflict_resolution="CREATE_COPY",
)

# Discover and download output manifests
discovery = await discover_output_manifests_py(
    region="us-west-2", scope=OutputManifestScope(...), s3_root_uri=...,
)
```

## Async Support
Uses `pyo3-asyncio` with tokio runtime for async functions.

## Progress Callbacks
Two callback types:
- `ProgressCallback` (scan phase): receives `ScanProgress`
- `TransferProgressCallback` (transfer phase): receives `TransferProgress`

## Module Structure
```
rusty_attachments/
├── __init__.py          # Re-exports
├── _rusty_attachments.pyd  # Compiled Rust
├── rusty_attachments.pyi   # Type stubs
└── py.typed             # PEP 561 marker
```

## When to Read Full Doc
- Adding new Python bindings
- Understanding async patterns
- Progress callback implementation
- Type stub (.pyi) updates
