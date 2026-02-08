# CLI Integration: `deadline attachment`

## Command Group Overview

The `deadline attachment` group provides two commands for bulk CAS upload and download
using pre-existing manifest files. Source: `src/deadline/client/cli/_groups/attachment_group.py`.

These commands operate on already-created manifests (from `deadline manifest snapshot`)
and transfer the referenced file contents to/from S3 CAS.

---

## Command: `attachment download`

### Call Chain

```
CLI: attachment_download(manifests, s3_root_uri, path_mapping_rules, conflict_resolution)
  → _apply_cli_options_to_config()
  → api.get_boto3_session()
  → [resolve S3 settings]
    ├─ [with --profile] use s3_root_uri directly
    └─ [without --profile]
       → config_file.get_setting("defaults.queue_id/farm_id")
       → get_queue(farm_id, queue_id, session)         # _aws/deadline.py
       → queue.jobAttachmentSettings → s3_root_uri
       → api.get_queue_user_boto3_session()
  → _attachment_download()                              # api/attachment.py
    → _read_manifests(manifest_paths)                   # api/_utils.py
    │   → open each file → decode_manifest()            # decode.py
    │
    → _process_path_mapping(path_mapping_rules)         # api/attachment.py
    │   → read JSON file → List[PathMappingRule]
    │
    → [match manifests to destinations via hashed source path in filename]
    │
    → download_files_from_manifests()                   # download.py
      → JobAttachmentS3Settings.from_s3_root_uri()
      → for each (root, manifest):
        → for each file in manifest.paths:
          → s3_key = f"{cas_prefix}/{file.hash}.{hash_alg}"
          → download_file()                             # download.py
            → s3.get_object() or s3.download_file()
            → write to local path (root / file.path)
            → restore mtime
            → handle conflict resolution (skip/overwrite/create_copy)
      → DownloadSummaryStatistics
```

### Deep Dive: `download_files_from_manifests()`

This is the core download function (download.py:877). It:
1. Iterates manifests keyed by destination root
2. For each manifest, iterates `manifest.paths`
3. Constructs S3 CAS key: `{cas_prefix}/{hash}.{hash_alg}`
4. Downloads via `download_file()` which handles:
   - Conflict resolution (SKIP, OVERWRITE, CREATE_COPY)
   - File permission restoration
   - mtime restoration from manifest
   - Parallel downloads via `_download_files_parallel()` using ThreadPoolExecutor
5. Returns `DownloadSummaryStatistics`

### Python-Side Responsibilities (KEEP)
- CLI arg parsing (click options)
- Config/credential resolution
- S3 settings resolution (from queue or `--s3-root-uri`)
- Queue role assumption
- `ClickLogger` JSON output
- Conflict resolution config reading

### Rust-Side Responsibilities (REPLACE)
- `_read_manifests()` → `Manifest::decode()` for each file
- `download_files_from_manifests()` → `DownloadOrchestrator::download_manifest_contents()`
  - Parallel S3 GET operations via CRT
  - File writing with conflict resolution
  - mtime restoration
  - Progress reporting
- Path mapping application during download

### Cut Line

```
Python                          │  Rust (PyO3)
────────────────────────────────┼──────────────────────────────────────
click CLI parsing               │
config/credential resolution    │
S3 settings resolution          │
queue role session → creds      │
conflict resolution config      │
                                │  attachment_download(
                                │      manifest_paths: Vec<str>,
                                │      s3_bucket: str,
                                │      cas_prefix: str,
                                │      path_mapping_rules: Option<str>,  // JSON file path
                                │      conflict_resolution: str,          // "SKIP"|"OVERWRITE"|"CREATE_COPY"
                                │      credentials: AwsCredentials,
                                │      region: str,
                                │      progress_callback: Callable,
                                │  ) -> DownloadSummaryStatistics
                                │
                                │  struct DownloadSummaryStatistics {
                                │      total_files: u64,
                                │      total_bytes: u64,
                                │      downloaded_files: u64,
                                │      downloaded_bytes: u64,
                                │      skipped_files: u64,
                                │      skipped_bytes: u64,
                                │      failed_files: u64,
                                │      transfer_rate: f64,
                                │  }
JSON output formatting          │
```

### Why This Cut Works

The entire download pipeline from manifest decode through S3 GET through local file write
is a single Rust-owned operation. Python only needs to:
1. Resolve where the CAS is (bucket + prefix)
2. Obtain credentials
3. Pass manifest file paths (Rust reads and decodes them)
4. Receive summary statistics for display

---

## Command: `attachment upload`

### Call Chain

```
CLI: attachment_upload(manifests, root_dirs, path_mapping_rules, s3_root_uri, upload_manifest_path)
  → _apply_cli_options_to_config()
  → api.get_boto3_session()
  → [resolve S3 settings — same pattern as download]
  → _attachment_upload()                              # api/attachment.py
    → _read_manifests(manifest_paths)                 # api/_utils.py
    │   → decode_manifest() for each
    │
    → _process_path_mapping(path_mapping_rules, root_dirs)  # api/attachment.py
    │   → read JSON or create identity rules from root_dirs
    │
    → S3AssetUploader(session=boto3_session)           # upload.py
    │
    → for each manifest (in order):
      → match to PathMappingRule via hashed source path in filename
      → build S3 metadata (asset-root, asset-root-json, file-system-location-name)
      → asset_uploader.upload_assets()                 # upload.py
        → _snapshot_assets() or upload_input_files()
          → for each file in manifest.paths:
            → s3_key = f"{cas_prefix}/{hash}.{hash_alg}"
            → file_already_uploaded(bucket, key)?       # HEAD check
            │   → S3CheckCache lookup
            │   → s3.head_object() fallback
            → upload_file_to_s3(file_path, bucket, key) # upload.py
              → s3.upload_file() or s3.put_object()
              → multipart for large files
        → upload manifest file to S3 Manifests/ prefix
        → return (manifest_key, manifest_hash)
      → UploadManifestInfo(output_manifest_path, output_manifest_hash, source_path)
```

### Deep Dive: `S3AssetUploader.upload_assets()`

This is the core upload function (upload.py:187). It:
1. Reads manifest, iterates paths
2. For each file: constructs CAS key from hash
3. Checks if already uploaded (S3CheckCache → HEAD fallback)
4. Uploads missing files (small: PUT, large: multipart)
5. Optionally uploads the manifest file itself to `Manifests/` prefix
6. Returns `(manifest_s3_key, manifest_hash)`

### Python-Side Responsibilities (KEEP)
- CLI arg parsing
- Config/credential resolution
- S3 settings resolution
- Queue role assumption
- `ClickLogger` output

### Rust-Side Responsibilities (REPLACE)
- `_read_manifests()` → `Manifest::decode()`
- `_process_path_mapping()` → path mapping rule parsing
- `S3AssetUploader.upload_assets()` → `UploadOrchestrator::upload_manifest_contents()`
  - S3CheckCache lookups
  - HEAD object existence checks
  - Parallel CAS uploads via CRT
  - Manifest file upload to S3
  - Progress reporting
- S3 metadata construction (asset-root encoding)

### Cut Line

```
Python                          │  Rust (PyO3)
────────────────────────────────┼──────────────────────────────────────
click CLI parsing               │
config/credential resolution    │
S3 settings resolution          │
queue role session → creds      │
                                │  attachment_upload(
                                │      manifest_paths: Vec<str>,
                                │      root_dirs: Vec<str>,
                                │      s3_bucket: str,
                                │      cas_prefix: str,
                                │      path_mapping_rules: Option<str>,
                                │      upload_manifest_path: Option<str>,
                                │      credentials: AwsCredentials,
                                │      region: str,
                                │      s3_check_cache_dir: str,
                                │      progress_callback: Callable,
                                │  ) -> Vec<UploadManifestInfo>
                                │
                                │  struct UploadManifestInfo {
                                │      output_manifest_path: String,  // S3 key
                                │      output_manifest_hash: String,
                                │      source_path: String,
                                │  }
print upload results            │
```

---

## Summary: Attachment Group Integration Points

| Python Function | Rust Replacement | Crate |
|----------------|-----------------|-------|
| `_read_manifests()` | `Manifest::decode()` per file | `model` |
| `_process_path_mapping()` | Path mapping rule parsing | `profiles` or `path-mapping` |
| `download_files_from_manifests()` | `DownloadOrchestrator::download_manifest_contents()` | `storage` |
| `download_file()` | CRT-based S3 GET + local write | `storage-crt` |
| `_download_files_parallel()` | Tokio async parallelism | `storage` |
| `S3AssetUploader.upload_assets()` | `UploadOrchestrator::upload_manifest_contents()` | `storage` |
| `upload_file_to_s3()` | CRT-based S3 PUT/multipart | `storage-crt` |
| `file_already_uploaded()` | `S3CheckCache` + HEAD | `storage` + `hash-cache` |
| `upload_bytes_to_s3()` | `StorageClient::put_object()` | `storage` |
| Conflict resolution logic | `DownloadOrchestrator` conflict handling | `storage` |

## Key Observation

The attachment commands are the **cleanest integration point** because they operate on
pre-existing manifests and do pure data transfer. There is no Deadline API interaction
within the hot path — all service calls happen before the cut line in Python.
The entire file I/O + S3 transfer pipeline maps directly to the Rust
`UploadOrchestrator` / `DownloadOrchestrator` already designed in `crates/storage/`.
