# CLI Integration: `deadline bundle submit`

## Command Group Overview

The `deadline bundle submit` command is the most complex integration target. It orchestrates
the full job submission pipeline: reading a job bundle, resolving parameters, scanning input
files, hashing, uploading to CAS, building attachment metadata, and calling CreateJob.

Source: `src/deadline/client/cli/_groups/bundle_group.py` → `src/deadline/client/api/_submit_job_bundle.py`.

---

## Call Chain: `bundle submit`

```
CLI: bundle_submit(job_bundle_dir, parameter, name, priority, ...)
  → _apply_cli_options_to_config(required={"farm_id", "queue_id"})
  → resolve force_s3_check from CLI flag or config
  → _ProgressBarCallbackManager (hashing + uploading)
  → api.create_job_from_job_bundle()                    # _submit_job_bundle.py
```

### `create_job_from_job_bundle()` — Full Breakdown

```
create_job_from_job_bundle(job_bundle_dir, job_parameters, name, ...)
  │
  ├─ 1. TEMPLATE RESOLUTION (Python-only)
  │  → validate_directory_symlink_containment(job_bundle_dir)
  │  → read_yaml_or_json(job_bundle_dir, "template")
  │  → substitute job name if provided
  │
  ├─ 2. DEADLINE API SETUP (Python-only)
  │  → api.get_boto3_client("deadline")
  │  → deadline.get_queue(farmId, queueId)
  │  → get_storage_profile_for_queue() if storage_profile_id set
  │  → read_job_bundle_parameters(job_bundle_dir)
  │  → read_yaml_or_json_object(job_bundle_dir, "asset_references")
  │  → AssetReferences.from_dict()
  │  → get_queue_parameter_definitions()
  │  → merge_queue_parameters + apply_job_parameters
  │  → preprocess_job_parameters() — resolve PATH parameters to real paths
  │
  ├─ 3. JOB ATTACHMENTS PROCESSING ← PRIMARY INTEGRATION TARGET
  │  → _process_job_attachments()                       # _submit_job_bundle.py
  │    │
  │    ├─ [ENABLE_SNAPSHOTS_LIBRARY = True] — NEW PATH
  │    │  → _process_job_attachments_with_snapshots()
  │    │    → collect_abs_snapshot(directories, filenames, ...)  # _snapshots/
  │    │    → [user confirmation prompt]
  │    │    → S3DataCache or FileSystemDataCache setup
  │    │    → hash_upload_abs_manifest(manifest, data_cache, hash_cache, on_progress)
  │    │    │   → hash each file (xxh128)
  │    │    │   → check S3 existence (S3CheckCache → HEAD)
  │    │    │   → upload missing files to CAS
  │    │    │   → progress reporting
  │    │    → partition_snapshot_by_storage_profile()     # _upload_v2.py
  │    │    → for each group:
  │    │    │   → snapshot_to_v2023_manifest()
  │    │    │   → manifest.encode() → hash
  │    │    │   → upload manifest to S3 Manifests/ prefix
  │    │    │   → build ManifestProperties
  │    │    → return attachment_settings dict
  │    │
  │    └─ [ENABLE_SNAPSHOTS_LIBRARY = False] — LEGACY PATH
  │       → _process_job_attachments_with_s3_asset_manager()
  │         → S3AssetManager(farm_id, queue_id, settings, session)
  │         → asset_manager.prepare_paths_for_upload()
  │         │   → _group_asset_paths() — group by storage profile
  │         │   → filter SHARED locations
  │         │   → compute common roots
  │         → [user confirmation prompt]
  │         → _hash_attachments()
  │         │   → asset_manager.hash_assets_and_create_manifest()
  │         │     → HashCache lookups
  │         │     → hash_file() for each input
  │         │     → construct AssetRootManifest per group
  │         → _upload_attachments() or _snapshot_attachments()
  │         │   → asset_manager.upload_assets()
  │         │     → S3AssetUploader.upload_input_files()
  │         │       → file_already_uploaded() — S3CheckCache + HEAD
  │         │       → upload_file_to_s3() — PUT or multipart
  │         │     → upload manifest to S3
  │         │     → build ManifestProperties
  │         → return attachment_settings dict
  │
  ├─ 4. CREATE JOB API CALL (Python-only)
  │  → build create_job_args with attachment_settings
  │  → deadline.create_job(**create_job_args)
  │  → wait_for_create_job_to_complete()
  │  → return job_id
  │
  └─ 5. POST-SUBMISSION (Python-only)
     → config_file.set_setting("defaults.job_id", job_id)
     → telemetry recording
```

---

## The Two Code Paths

### Legacy Path: `_process_job_attachments_with_s3_asset_manager()`

Uses `S3AssetManager` + `S3AssetUploader` — the original Python implementation.

Key primitives consumed:
- `S3AssetManager.prepare_paths_for_upload()` → path grouping by storage profile
- `S3AssetManager.hash_assets_and_create_manifest()` → hashing + manifest creation
- `S3AssetUploader.upload_input_files()` → CAS upload with dedup
- `S3AssetUploader.upload_assets()` → manifest upload

### Snapshots Path: `_process_job_attachments_with_snapshots()`

Uses the newer `_snapshots` module — composable operations closer to the Rust design.

Key primitives consumed:
- `collect_abs_snapshot()` → directory walking + file collection
- `hash_upload_abs_manifest()` → combined hash + upload pipeline
- `partition_snapshot_by_storage_profile()` → group by LOCAL/SHARED locations
- `snapshot_to_v2023_manifest()` → convert to v2023 format for API compatibility

The snapshots path is architecturally closer to the Rust design and represents the
natural migration target.

---

## Python-Side Responsibilities (KEEP)

Everything outside the job attachments processing:

1. CLI arg parsing (click options)
2. Config resolution (`_apply_cli_options_to_config`)
3. Job template reading and parameter resolution
4. Deadline API calls: `get_queue()`, `get_storage_profile_for_queue()`,
   `get_queue_parameter_definitions()`, `create_job()`, `get_job()`
5. Queue role assumption
6. User confirmation prompts (`interactive_confirmation_callback`)
7. Progress bar rendering (`_ProgressBarCallbackManager`)
8. Debug snapshot zip creation
9. Telemetry recording
10. Error handling and signal handling (`SigIntHandler`)

## Rust-Side Responsibilities (REPLACE)

The entire `_process_job_attachments()` function body, specifically:

1. Directory walking / file collection → `collect_abs_snapshot()` equivalent
2. Path grouping by storage profile → `group_asset_paths()` / `partition_snapshot_by_storage_profile()`
3. File hashing with cache → `FileSystemScanner::snapshot()` + `HashCache`
4. S3 existence checking → `S3CheckCache` + HEAD
5. CAS file upload → `UploadOrchestrator::upload_manifest_contents()`
6. Manifest file upload → `upload_input_manifest()`
7. Attachment metadata construction → `build_attachments()` / `to_job_attachments()`
8. Upload confirmation message generation (file counts, sizes, path warnings)

---

## Cut Line

The cut is at `_process_job_attachments()`. Python calls into Rust with resolved
credentials and asset references, Rust returns the `attachment_settings` dict.

```
Python                              │  Rust (PyO3)
────────────────────────────────────┼──────────────────────────────────────
click CLI parsing                   │
config resolution                   │
template + parameter resolution     │
deadline.get_queue()                │
storage profile resolution          │
AssetReferences extraction          │
queue role session → creds          │
                                    │
[pre-call] generate confirmation    │
  message for user prompt           │
  (needs file counts + sizes +      │
   path classification)             │
                                    │  // Phase 1: Scan & classify
                                    │  scan_and_classify_assets(
                                    │      input_filenames: Vec<str>,
                                    │      input_directories: Vec<str>,
                                    │      output_directories: Vec<str>,
                                    │      referenced_paths: Vec<str>,
                                    │      storage_profile: Option<StorageProfile>,
                                    │      require_paths_exist: bool,
                                    │  ) -> AssetScanResult
                                    │
                                    │  struct AssetScanResult {
                                    │      total_input_files: u64,
                                    │      total_input_bytes: u64,
                                    │      asset_groups: Vec<AssetGroup>,
                                    │      // for confirmation message generation
                                    │      paths_outside_profile: Vec<String>,
                                    │      shared_paths_filtered: Vec<String>,
                                    │  }
                                    │
[user confirmation prompt]          │
                                    │
                                    │  // Phase 2: Hash + Upload
                                    │  hash_and_upload_attachments(
                                    │      asset_groups: AssetScanResult,
                                    │      s3_bucket: str,
                                    │      root_prefix: str,
                                    │      farm_id: str,
                                    │      queue_id: str,
                                    │      credentials: AwsCredentials,
                                    │      region: str,
                                    │      hash_cache_dir: str,
                                    │      s3_check_cache_dir: str,
                                    │      force_s3_check: bool,
                                    │      file_system_mode: str,
                                    │      progress_callback: Callable,
                                    │  ) -> AttachmentSettings
                                    │
                                    │  struct AttachmentSettings {
                                    │      manifests: Vec<ManifestProperties>,
                                    │      file_system: String,
                                    │  }
                                    │  // This is the dict that goes into
                                    │  // create_job_args["attachments"]
                                    │
deadline.create_job(attachments=..) │
wait_for_create_job_to_complete()   │
telemetry                           │
```

### Why Two Phases?

The user confirmation prompt sits between scanning (to know file counts/sizes) and
uploading (the expensive operation). Splitting into two Rust calls preserves the
Python-side interactive prompt while keeping all compute in Rust.

This matches the existing Python architecture where `prepare_paths_for_upload()` runs
first, then the confirmation prompt, then `hash_assets_and_create_manifest()` +
`upload_assets()`.

### Alternative: Single-Phase with Callback

```python
# Rust calls back to Python for confirmation
result = await submit_bundle_attachments(
    ...,
    confirmation_callback=lambda scan_result: click.confirm(...),
)
```

This is simpler but requires Rust to call back into Python mid-operation, which
adds complexity to the PyO3 async bridge. The two-phase approach is cleaner.

---

## Debug Snapshot Mode

When `--save-debug-snapshot` is provided, the upload phase writes to local filesystem
instead of S3. Rust handles this via `FileSystemDataCache` instead of `S3DataCache`.

```
Python                              │  Rust (PyO3)
────────────────────────────────────┼──────────────────────────────────────
                                    │  hash_and_snapshot_attachments(
                                    │      asset_groups: AssetScanResult,
                                    │      snapshot_dir: str,
                                    │      farm_id: str,
                                    │      queue_id: str,
                                    │      hash_cache_dir: str,
                                    │      file_system_mode: str,
                                    │      progress_callback: Callable,
                                    │  ) -> AttachmentSettings
zip creation (shutil.make_archive)  │
```

---

## `gui-submit` Note

`bundle gui-submit` uses the same `create_job_from_job_bundle()` under the hood
(via the Qt GUI's `show_job_bundle_submitter`). The Rust integration applies
identically — the GUI just provides a different `interactive_confirmation_callback`
and progress display.

---

## Summary: Bundle Submit Integration Points

| Python Function | Rust Replacement | Crate |
|----------------|-----------------|-------|
| `collect_abs_snapshot()` | `collect_abs_snapshot()` (already Rust-like) | `filesystem` |
| `prepare_paths_for_upload()` | `group_asset_paths()` | `profiles` |
| `partition_snapshot_by_storage_profile()` | `group_asset_paths()` + filter | `profiles` |
| `hash_assets_and_create_manifest()` | `FileSystemScanner::snapshot()` | `filesystem` |
| `hash_upload_abs_manifest()` | `UploadOrchestrator` pipeline | `storage` |
| `HashCache` | SQLite hash cache | `hash-cache` |
| `S3CheckCache` | S3 check cache | `hash-cache` |
| `upload_input_files()` | `UploadOrchestrator::upload_manifest_contents()` | `storage` |
| `upload_assets()` (manifest upload) | `upload_input_manifest()` | `storage` |
| `snapshot_to_v2023_manifest()` | `Manifest` encode (v2023) | `model` |
| `build ManifestProperties` | `build_manifest_properties()` | `ja-deadline-utils` |
| `attachment_settings dict` | `build_attachments()` → JSON | `ja-deadline-utils` |

## Key Observation

Bundle submit is where the **maximum performance gain** lives. The hot path is:
1. Directory walking (thousands to millions of files)
2. Hashing (CPU-bound, benefits from Rust parallelism)
3. S3 existence checks (I/O-bound, benefits from CRT async)
4. CAS uploads (I/O-bound, benefits from CRT multipart)

All four are replaced by Rust. Python retains only the orchestration shell:
config → scan → confirm → hash+upload → CreateJob.

The existing `submit_bundle_attachments_py()` binding in `crates/python/` already
targets this exact cut point, confirming the design alignment.
