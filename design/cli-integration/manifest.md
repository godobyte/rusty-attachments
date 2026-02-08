# CLI Integration: `deadline manifest`

## Command Group Overview

The `deadline manifest` group provides four commands for local manifest operations and
S3 manifest transfer. Source: `src/deadline/client/cli/_groups/manifest_group.py`.

---

## Command: `manifest snapshot`

### Call Chain

```
CLI: manifest_snapshot(root, destination, name, include, exclude, include_exclude_config, diff, force_rehash)
  → _manifest_snapshot()                          # api/manifest.py
    → _process_glob_inputs() / GlobConfig         # _glob.py — parse include/exclude JSON or args
    → _glob_paths(root, include, exclude)          # _glob.py — os.walk + glob.glob matching
    ├─ [no diff] _create_manifest_for_single_root(files, root)
    │    → S3AssetManager().prepare_paths_for_upload()   # upload.py — path grouping
    │    → _hash_attachments()                           # _job_attachment.py
    │      → asset_manager.hash_assets_and_create_manifest()  # upload.py
    │        → HashCache — SQLite lookup (path, mtime, size → hash)
    │        → hash_file() — xxh128 hashing              # hash_algorithms.py
    │        → BaseAssetManifest construction             # base_manifest.py
    │
    └─ [with diff] decode_manifest(source_manifest_str)  # decode.py
       ├─ [fast] _fast_file_list_to_manifest_diff()      # _diff.py — mtime/size comparison
       └─ [force_rehash] _create_manifest_for_single_root() + compare_manifest()
          → _create_manifest_for_single_root(changed_paths)

  → _write_manifest(root, manifest, destination, name)   # api/manifest.py
    → hash_data(root) for filename                        # hash_algorithms.py
    → manifest.encode() → JSON string                     # base_manifest.py
    → write to local file
```

### Python-Side Responsibilities (KEEP)
- CLI arg parsing (click options: `--root`, `--destination`, `--include`, `--exclude`, etc.)
- Input validation (`os.path.isdir(root)`, `os.path.isdir(destination)`)
- Windows long path warning
- JSON output formatting via `ClickLogger`
- Progress bar via `_ProgressBarCallbackManager`

### Rust-Side Responsibilities (REPLACE)
- `_glob_paths()` → `crates/filesystem/` `GlobFilter` + directory walking
- `_create_manifest_for_single_root()` → `FileSystemScanner::snapshot()`
- `hash_file()` → `crates/common/` xxh128 hashing
- `HashCache` lookups → `crates/hash-cache/` SQLite cache
- `_fast_file_list_to_manifest_diff()` → `crates/filesystem/` `FileSystemScanner::diff()` (fast mode)
- `compare_manifest()` → `crates/model/` `compare_manifests()`
- `_write_manifest()` → `crates/model/` `Manifest::encode()` + Rust file write
- `decode_manifest()` → `crates/model/` `Manifest::decode()`

### Cut Line

```
Python                          │  Rust (PyO3)
────────────────────────────────┼──────────────────────────────────────
click CLI parsing               │
input validation                │
ClickLogger / progress bar      │
                                │  manifest_snapshot(
                                │      root: str,
                                │      destination: str,
                                │      name: Option<str>,
                                │      include: Vec<str>,
                                │      exclude: Vec<str>,
                                │      include_exclude_config: Option<str>,
                                │      diff_manifest_path: Option<str>,
                                │      force_rehash: bool,
                                │      hash_cache_dir: str,
                                │      progress_callback: Callable,
                                │  ) -> Option<ManifestSnapshotResult>
                                │
                                │  struct ManifestSnapshotResult {
                                │      root: String,
                                │      manifest_path: String,
                                │  }
JSON output formatting          │
```

---

## Command: `manifest diff`

### Call Chain

```
CLI: manifest_diff(root, manifest, include, exclude, include_exclude_config, force_rehash)
  → _manifest_diff()                              # api/manifest.py
    → _glob_files(root, include, exclude)          # api/manifest.py → _glob.py
    ├─ [force_rehash]
    │    → S3AssetManager()._create_manifest_file()  # upload.py — hash all files
    │      → HashCache
    │      → hash_file()
    │    → compare_manifest(reference, compare)       # _diff.py
    │
    └─ [fast mode]
         → _fast_file_list_to_manifest_diff()         # _diff.py
           → stat() each file for mtime/size
           → compare against manifest entries

  → [CLI] pretty_print_cli() or JSON output
```

### Python-Side Responsibilities (KEEP)
- CLI arg parsing
- Input validation (`os.path.isfile(manifest)`, `os.path.isdir(root)`)
- `pretty_print_cli()` — colored tree output (presentation only)
- JSON output formatting

### Rust-Side Responsibilities (REPLACE)
- `_glob_files()` → `GlobFilter` + directory walking
- `_fast_file_list_to_manifest_diff()` → `FileSystemScanner::diff()` with `DiffMode::Fast`
- `compare_manifest()` → `compare_manifests()` with `DiffMode::Hash`
- `decode_manifest()` → `Manifest::decode()`
- `HashCache` → Rust hash cache
- `hash_file()` → Rust xxh128

### Cut Line

```
Python                          │  Rust (PyO3)
────────────────────────────────┼──────────────────────────────────────
click CLI parsing               │
input validation                │
                                │  manifest_diff(
                                │      root: str,
                                │      manifest_path: str,
                                │      include: Vec<str>,
                                │      exclude: Vec<str>,
                                │      include_exclude_config: Option<str>,
                                │      force_rehash: bool,
                                │      hash_cache_dir: str,
                                │      progress_callback: Callable,
                                │  ) -> ManifestDiffResult
                                │
                                │  struct ManifestDiffResult {
                                │      new: Vec<String>,
                                │      modified: Vec<String>,
                                │      deleted: Vec<String>,
                                │  }
pretty_print_cli() / JSON out   │
```

---

## Command: `manifest download`

### Call Chain

```
CLI: manifest_download(download_dir, job_id, step_id, asset_type)
  → _apply_cli_options_to_config()                 # _common.py
  → config_file.get_setting("defaults.queue_id")   # config
  → api.get_boto3_session()                        # _session.py
  → _manifest_download()                           # api/manifest.py
    → deadline.get_queue()                          # boto3 Deadline API
    → _get_queue_user_boto3_session()               # assume queue role
    → deadline.get_job()                            # boto3 Deadline API
    │
    ├─ [input manifests]
    │    → get_manifest_from_s3(key, bucket, session)  # download.py
    │      → s3.get_object() → decode_manifest()
    │    → [step deps] deadline.list_step_dependencies()
    │      → get_output_manifests_by_asset_root()      # download.py
    │
    ├─ [output manifests]
    │    → get_output_manifests_by_asset_root()         # download.py
    │      → s3.list_objects_v2() — discover manifest keys
    │      → get_asset_root_and_manifest_from_s3()
    │
    └─ merge_asset_manifests() per root                 # download.py
       → merged_manifest.encode() → write to local file
```

### Python-Side Responsibilities (KEEP)
- CLI arg parsing
- Config/credential resolution (`_apply_cli_options_to_config`, `get_boto3_session`)
- Deadline API calls: `get_queue()`, `get_job()`, `list_step_dependencies()`
- Queue role assumption (`_get_queue_user_boto3_session`)
- JSON output formatting

### Rust-Side Responsibilities (REPLACE)
- `get_manifest_from_s3()` → `crates/storage/` `download_manifest()`
- `get_output_manifests_by_asset_root()` → `crates/storage/` `download_output_manifests_by_asset_root()`
- S3 object listing and manifest discovery → `crates/storage/` `discover_output_manifest_keys()`
- `merge_asset_manifests()` → `crates/model/` `merge_manifests()`
- `decode_manifest()` / `encode()` → `crates/model/`
- Local file writing of merged manifests

### Cut Line

This command is more nuanced because it mixes Deadline API calls (Python/boto3) with
S3 data operations (Rust). The cut requires Python to resolve the manifest S3 keys
via Deadline APIs, then hand them to Rust for download + merge.

```
Python                          │  Rust (PyO3)
────────────────────────────────┼──────────────────────────────────────
click CLI parsing               │
config/credential resolution    │
deadline.get_queue()            │
deadline.get_job()              │
list_step_dependencies()        │
extract input_manifest_paths    │
  from job["attachments"]       │
queue role session → creds      │
                                │  download_and_merge_manifests(
                                │      s3_bucket: str,
                                │      root_prefix: str,
                                │      input_manifest_keys: Vec<(str, str)>,  // (s3_key, root_path)
                                │      output_manifest_scope: OutputScope,     // job/step level
                                │      download_dir: str,
                                │      credentials: AwsCredentials,
                                │      region: str,
                                │  ) -> ManifestDownloadResponse
                                │
                                │  enum OutputScope {
                                │      Job { farm_id, queue_id, job_id },
                                │      Step { farm_id, queue_id, job_id, step_id },
                                │  }
                                │
                                │  struct ManifestDownloadResponse {
                                │      downloaded: Vec<ManifestDownload>,
                                │  }
                                │  struct ManifestDownload {
                                │      manifest_root: String,
                                │      local_manifest_path: String,
                                │  }
JSON output formatting          │
```

**Alternative (simpler) cut**: Python does all Deadline API + S3 manifest discovery,
passes a flat list of `(s3_key, root_path)` pairs to Rust which only does
`download_manifest()` + `merge_manifests()` + local write. This avoids Rust needing
S3 list operations for output manifest discovery.

---

## Command: `manifest upload`

### Call Chain

```
CLI: manifest_upload(manifest_file, s3_cas_uri, s3_manifest_prefix)
  → _apply_cli_options_to_config()
  → api.get_boto3_session()
  → [resolve bucket/prefix from queue or URI]
    → deadline.get_queue() or JobAttachmentS3Settings.from_s3_root_uri()
    → api.get_queue_user_boto3_session()
  → _manifest_upload()                             # api/manifest.py
    → S3AssetUploader(session).upload_bytes_to_s3() # upload.py
      → s3.put_object(Body=manifest_bytes, Bucket, Key, Metadata)
```

### Python-Side Responsibilities (KEEP)
- CLI arg parsing
- Config/credential resolution
- S3 settings resolution (from queue or `--s3-cas-uri`)
- Queue role assumption

### Rust-Side Responsibilities (REPLACE)
- `upload_bytes_to_s3()` → `crates/storage/` `StorageClient::put_object()`

### Cut Line

```
Python                          │  Rust (PyO3)
────────────────────────────────┼──────────────────────────────────────
click CLI parsing               │
config/credential resolution    │
S3 settings resolution          │
queue role session → creds      │
read manifest file bytes        │
                                │  upload_manifest(
                                │      manifest_bytes: bytes,
                                │      s3_bucket: str,
                                │      s3_key: str,
                                │      metadata: Dict[str, str],
                                │      credentials: AwsCredentials,
                                │      region: str,
                                │  ) -> ()
success message                 │
```

This is the simplest cut — it's essentially a single S3 PUT. The value of Rust here
is minimal on its own, but it shares the `StorageClient` infrastructure with the
upload/download paths that benefit enormously from CRT-based transfers.

---

## Summary: Manifest Group Integration Points

| Python Function | Rust Replacement | Crate |
|----------------|-----------------|-------|
| `_glob_paths()` | `GlobFilter` + dir walk | `filesystem` |
| `_create_manifest_for_single_root()` | `FileSystemScanner::snapshot()` | `filesystem` |
| `hash_file()` | xxh128 hashing | `common` |
| `HashCache` | SQLite hash cache | `hash-cache` (or `common`) |
| `_fast_file_list_to_manifest_diff()` | `FileSystemScanner::diff(Fast)` | `filesystem` |
| `compare_manifest()` | `compare_manifests()` | `model` |
| `decode_manifest()` / `encode()` | `Manifest::decode/encode` | `model` |
| `merge_asset_manifests()` | `merge_manifests()` | `model` |
| `_write_manifest()` | Manifest encode + file write | `model` |
| `get_manifest_from_s3()` | `download_manifest()` | `storage` |
| `get_output_manifests_by_asset_root()` | `download_output_manifests_by_asset_root()` | `storage` |
| `upload_bytes_to_s3()` | `StorageClient::put_object()` | `storage` |
