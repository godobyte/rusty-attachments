# CLI Integration: Implementation Summary

Generated from code review of the rusty-attachments PyO3 bindings and
deadline-cloud Python integration layer.

## What Was Built

### Rust Side (`crates/python/src/lib.rs`)

9 new PyO3 binding functions bridging deadline-cloud CLI commands to Rust:

| Binding Function | CLI Command | Rust Primitives Used |
|---|---|---|
| `manifest_snapshot_py` | `manifest snapshot` | `FileSystemScanner::snapshot()`, `DiffEngine::diff()`, `GlobFilter` |
| `manifest_diff_py` | `manifest diff` | `DiffEngine::diff()`, `GlobFilter` |
| `manifest_download_py` | `manifest download` | `download_manifest()`, `discover_output_manifest_keys()`, `merge_manifests()` |
| `manifest_upload_py` | `manifest upload` | `StorageClient::put_object()` |
| `attachment_download_py` | `attachment download` | `DownloadOrchestrator::download_manifest_contents()` |
| `attachment_upload_py` | `attachment upload` | `UploadOrchestrator::upload_manifest_contents()`, `upload_input_manifest()` |
| `discover_output_manifests_py` | `job download-output` (phase 1) | `discover_output_manifest_keys()`, `download_manifest()` |
| `download_output_files_py` | `job download-output` (phase 2) | `DownloadOrchestrator::download_manifest_contents()`, `merge_manifests()` |
| `incremental_download_py` | `queue sync-output` | `download_manifest()`, `merge_manifests_chronologically()`, `DownloadOrchestrator` |

9 new PyO3 data types: `PathMappingRule`, `DownloadSummaryStatistics`,
`ManifestDiffResult`, `ManifestSnapshotResult`, `UploadManifestInfo`,
`ManifestDownloadEntry`, `OutputManifestScope`, `ManifestDownloadSpec`,
`OutputManifestDiscovery`.

Helper infrastructure: `build_glob_filter()`, `write_manifest_to_dir()`,
`apply_path_mapping()`, `create_client()`, `PyTransferProgressCallback`,
`PyConflictResolution`, global handle map for two-phase download.

### Python Side (deadline-cloud)

4 new modules under `src/deadline/job_attachments/api/`:

| Module | Purpose |
|---|---|
| `_rusty_common.py` | Shared helpers: `conflict_to_str()`, `s3_location_from_settings()`, `to_rust_rules()` |
| `_rusty_attachment.py` | `_attachment_download_rust()`, `_attachment_upload_rust()` |
| `_rusty_manifest.py` | `_manifest_snapshot_rust()`, `_manifest_diff_rust()`, `_manifest_upload_rust()`, `_manifest_download_rust()` |
| `_rusty_download.py` | `discover_output_manifests_rust()`, `download_output_files_rust()`, `incremental_download_rust()` |

2 modified CLI group files with `try/except ImportError` fallback pattern:
- `attachment_group.py` — `attachment download`, `attachment upload`
- `manifest_group.py` — `manifest snapshot`, `manifest diff`, `manifest download`, `manifest upload`

## Design Decisions

**Fallback pattern**: Every CLI command uses `_USE_RUST` flag with
`try/except ImportError` so deadline-cloud works without rusty_attachments
installed. Rust is opt-in acceleration, not a hard dependency.

**Credential handling**: Rust bindings use the AWS SDK default credential
chain rather than accepting boto3 sessions. Python still handles queue role
assumption and Deadline API calls; Rust takes over at the S3 data transfer
boundary.

**Sequential manifest downloads**: The `download_manifests_parallel()` Rust
function has a higher-rank trait bound issue inside `pyo3_async_runtimes`
closures. Bindings download manifests sequentially via `download_manifest()`
instead. File content transfers still use full CRT parallelism via the
orchestrators.

**Two-phase download_job_output**: Uses a global `Mutex<HashMap>` handle map
to hold pre-fetched manifests between phase 1 (discover) and phase 2
(download), avoiding re-serialization across the Python/Rust boundary.

**Glob filter priority**: When both include and exclude patterns are provided,
exclude takes precedence (include is assumed to be the default "match all").

## Code Review Findings & Fixes Applied

1. **Extracted shared helpers** — `_rusty_common.py` eliminates duplication of
   `_s3_location_from_settings`, `_conflict_to_str`, `_to_rust_rules` across
   three modules.

2. **Fixed glob filter logic** — `build_glob_filter()` previously ignored
   exclude patterns when include was also provided. Now exclude takes priority.

3. **Removed unused imports** — Cleaned `Tuple`, `Dict`, `Callable`,
   `PathMappingRule` imports that ruff flagged.

4. **Scoped `#![allow(unused_variables)]`** — Added explanatory comment for
   why the suppression exists (binding params for API compat).

## Known Limitations

- **No unit tests** for the new binding functions or Python integration
  wrappers. The existing 3019 deadline-cloud tests pass, confirming no
  regressions, but the Rust paths are not exercised without rusty_attachments
  installed.

- **Handle map has no expiry** — `MANIFEST_HANDLE_MAP` entries persist until
  consumed by phase 2. If phase 2 is never called, handles leak. A TTL-based
  cleanup or `Drop` guard would be better for production.

- **Region from env var** — Python wrappers read `AWS_DEFAULT_REGION` from
  environment. A more robust approach would extract region from the boto3
  session or Deadline API response.

- **`job download-output` / `queue sync-output` not wired at CLI level** —
  The Rust functions are exposed as library functions in `_rusty_download.py`
  but not yet wired into the CLI commands. These commands have complex
  interactive prompts and Deadline API orchestration that requires deeper
  integration at the `OutputDownloader` / `_incremental_output_download` level.

## Verification

All checks pass:
- `cargo check -p rusty-attachments-python` — clean compile, no warnings
- `hatch run fmt` — ruff format + ruff check clean
- `hatch run lint` — ruff + mypy clean (364 source files)
- `hatch run test` — 3019 passed, 78 skipped, 2 xfailed, coverage 81.20%
