# CLI Integration: Other Command Groups

This document evaluates every CLI group in `deadline-cloud` not already covered by
the primary analysis (manifest, attachment, bundle-submit) and determines whether
rusty-attachments integration provides tangible benefit.

---

## Commands WITH Tangible Rust Benefit

### `deadline job download-output`

Source: `job_group.py` → `_download_job_output()` → `OutputDownloader.download_job_output()`

This command downloads output files produced by a completed job/step/task. It is
a significant integration target because it performs bulk S3 CAS downloads.

#### Call Chain

```
CLI: job_download_output(job_id, step_id, task_id, conflict_resolution, output)
  → _apply_cli_options_to_config()
  → _download_job_output()                              # job_group.py (helper)
    → deadline.get_job(), deadline.get_step(), deadline.get_task()
    → deadline.get_queue()
    → api.get_queue_user_boto3_session()
    │
    → OutputDownloader(s3_settings, farm/queue/job/step/task, session)
    │   constructor:
    │     → get_output_manifests_by_asset_root()         # download.py
    │       → s3.list_objects_v2() — discover output manifest keys
    │       → get_asset_root_and_manifest_from_s3()      # s3.get_object + decode
    │       → merge_asset_manifests() per root
    │     → ManifestPathGroup aggregation
    │
    → [interactive] user selects/confirms root paths
    → [interactive] conflict resolution selection
    │
    → OutputDownloader.download_job_output()
      → for each (root, path_group):
        → download_files(files, hash_alg, local_dir, s3_settings, session)
          → _download_files_parallel()                   # download.py
            → ThreadPoolExecutor(max_workers)
            → download_file() per file                   # download.py
              → s3.get_object() or s3.download_file()
              → conflict resolution (skip/overwrite/create_copy)
              → mtime restoration
      → DownloadSummaryStatistics
```

#### Why Rust Helps

The download pipeline is CPU-bound (hash verification) and I/O-bound (S3 GET + local
file write). The Python ThreadPoolExecutor is limited by GIL for any CPU work and by
boto3's connection pool for I/O. Rust's `DownloadOrchestrator` with CRT-based transfers
provides:

1. True parallel S3 downloads via CRT (no GIL, no connection pool limits)
2. Faster hash verification (xxh128 in native code)
3. Efficient multipart downloads for large files
4. Better memory management for high file counts

#### Cut Line

```
Python                              │  Rust (PyO3)
────────────────────────────────────┼──────────────────────────────────────
click CLI parsing                   │
config/credential resolution        │
deadline.get_job/step/task/queue()  │
queue role session → creds          │
                                    │
                                    │  // Phase 1: Discover & merge manifests
                                    │  discover_output_manifests(
                                    │      s3_bucket: str,
                                    │      root_prefix: str,
                                    │      farm_id: str,
                                    │      queue_id: str,
                                    │      job_id: str,
                                    │      step_id: Option<str>,
                                    │      task_id: Option<str>,
                                    │      session_action_id: Option<str>,
                                    │      credentials: AwsCredentials,
                                    │      region: str,
                                    │  ) -> OutputManifestDiscovery
                                    │
                                    │  struct OutputManifestDiscovery {
                                    │      outputs_by_root: HashMap<String, ManifestPathGroup>,
                                    │      root_path_formats: HashMap<String, String>,
                                    │  }
                                    │
[interactive] root path selection   │
[interactive] conflict resolution   │
                                    │
                                    │  // Phase 2: Download files
                                    │  download_output_files(
                                    │      outputs_by_root: OutputManifestDiscovery,
                                    │      root_overrides: HashMap<String, String>,
                                    │      s3_bucket: str,
                                    │      cas_prefix: str,
                                    │      conflict_resolution: str,
                                    │      credentials: AwsCredentials,
                                    │      region: str,
                                    │      progress_callback: Callable,
                                    │  ) -> DownloadSummaryStatistics
                                    │
summary output                      │
```

The two-phase split is necessary because the user interactively selects root paths
and conflict resolution between manifest discovery and file download.

---

### `deadline queue sync-output`

Source: `queue_group.py` → `_incremental_output_download()` (in `_incremental_download.py`)

This is the most complex download command. It incrementally downloads outputs for
all jobs in a queue since the last checkpoint, using session action tracking.

#### Call Chain (Simplified)

```
CLI: sync_output(bootstrap_lookback_minutes, checkpoint_dir, conflict_resolution, ...)
  → _apply_cli_options_to_config()
  → load/create IncrementalDownloadState checkpoint
  → _incremental_output_download()
    │
    ├─ 1. DEADLINE API DISCOVERY (Python-heavy)
    │  → _get_download_candidate_jobs()          # SearchJobs API
    │  → _categorize_jobs_in_checkpoint()         # compare with saved state
    │  → _get_job_sessions()                      # ListSessions + ListSessionActions
    │  → _get_storage_profiles()                  # GetStorageProfileForQueue
    │  → _create_path_mapping_rule_appliers()     # cross-profile path mapping
    │
    ├─ 2. MANIFEST DOWNLOAD & MERGE
    │  → _download_all_manifests_with_absolute_paths()
    │    → for each job's session actions:
    │      → get_manifest_from_s3()               # S3 GET + decode
    │      → apply path mapping rules
    │    → _merge_absolute_path_manifest_list()   # merge by timestamp
    │
    └─ 3. FILE DOWNLOAD ← RUST TARGET
       → _download_manifest_paths()
         → ThreadPoolExecutor(max_workers)
         → _download_file() per manifest path
           → s3.get_object()
           → conflict resolution
           → mtime restoration
```

#### Why Rust Helps

Phase 3 (`_download_manifest_paths`) is the bottleneck — it downloads potentially
thousands of files from S3 CAS using Python's ThreadPoolExecutor. This is the same
download primitive as `job download-output` and `attachment download`.

Phase 2 (manifest download + merge) also benefits from Rust for the same reasons
as `manifest download` — S3 GET operations and manifest decode/merge.

Phase 1 is pure Deadline API calls and checkpoint state management — no benefit from Rust.

#### Cut Line

```
Python                              │  Rust (PyO3)
────────────────────────────────────┼──────────────────────────────────────
click CLI parsing                   │
config/credential resolution        │
checkpoint load/save                │
SearchJobs, ListSessions,           │
  ListSessionActions (Deadline API) │
categorize jobs, update checkpoint  │
storage profile resolution          │
path mapping rule construction      │
                                    │
                                    │  // Phase 2+3 combined: download manifests,
                                    │  // apply path mapping, merge, download files
                                    │  incremental_download_manifests_and_files(
                                    │      manifest_keys: Vec<ManifestDownloadSpec>,
                                    │      path_mapping_rules: Vec<PathMappingRule>,
                                    │      s3_bucket: str,
                                    │      cas_prefix: str,
                                    │      conflict_resolution: str,
                                    │      credentials: AwsCredentials,
                                    │      region: str,
                                    │      progress_callback: Callable,
                                    │  ) -> IncrementalDownloadResult
                                    │
                                    │  struct ManifestDownloadSpec {
                                    │      s3_key: String,
                                    │      asset_root: String,
                                    │      last_modified: f64,  // for merge ordering
                                    │  }
                                    │
                                    │  struct IncrementalDownloadResult {
                                    │      downloaded_files: u64,
                                    │      downloaded_bytes: u64,
                                    │      file_counts_by_root: HashMap<String, u64>,
                                    │  }
                                    │
checkpoint save                     │
summary output                      │
```

The cut keeps all Deadline API orchestration and checkpoint management in Python,
while Rust handles the entire S3 manifest download → path mapping → merge → file
download pipeline.

---

### `deadline handle-web-url` (download-output subcommand)

Source: `handle_web_url_command.py`

This command handles `deadline://download-output?...` URLs from Deadline Cloud Monitor.
It delegates directly to `_download_job_output()` — the same function used by
`job download-output`. No separate integration needed; it inherits the same Rust
cut line automatically.

---

## Commands WITHOUT Tangible Rust Benefit

### `deadline farm` (list, get)

Pure Deadline API calls (`ListFarms`, `GetFarm`). No file I/O, no hashing, no S3
data transfer. Zero benefit from Rust.

### `deadline fleet` (list, get)

Pure Deadline API calls (`ListFleets`, `GetFleet`, `ListQueueFleetAssociations`).
Zero benefit from Rust.

### `deadline queue` (list, get, paramdefs, export-credentials)

Pure Deadline API calls and credential formatting. Zero benefit from Rust.
Note: `queue sync-output` IS covered above as a beneficial target.

### `deadline worker` (list, get)

Pure Deadline API calls (`SearchWorkers`, `GetWorker`). Zero benefit from Rust.

### `deadline job` (list, get, cancel, requeue-tasks, wait, logs)

These subcommands are pure Deadline API calls:
- `list` → `SearchJobs`
- `get` → `GetJob`
- `cancel` → `UpdateJob`
- `requeue-tasks` → `ListSteps` + `ListTasks` + `UpdateTask` (paginated)
- `wait` → polling `GetJob` with exponential backoff
- `logs` → `ListSessions` + CloudWatch `GetLogEvents`

No file I/O, no hashing, no S3 data transfer. Zero benefit from Rust.
Note: `job download-output` IS covered above as a beneficial target.

### `deadline config` (show, set, get, clear, gui)

Local config file operations via Python's `ConfigParser`. Trivial I/O.
Zero benefit from Rust.

### `deadline auth` (login, logout, status)

Credential management and Deadline Cloud Monitor integration.
Zero benefit from Rust.

### `deadline mcp-server`

Starts an MCP protocol server. No attachment operations.
Zero benefit from Rust.

---

## Summary

| CLI Command | Rust Benefit | Reason |
|-------------|:---:|--------|
| `job download-output` | ✅ | Bulk S3 CAS download via `DownloadOrchestrator` |
| `queue sync-output` | ✅ | Manifest download + merge + bulk S3 CAS download |
| `handle-web-url download-output` | ✅ | Delegates to `job download-output` |
| `farm list/get` | ❌ | Pure Deadline API |
| `fleet list/get` | ❌ | Pure Deadline API |
| `queue list/get/paramdefs/export-credentials` | ❌ | Pure Deadline API |
| `worker list/get` | ❌ | Pure Deadline API |
| `job list/get/cancel/requeue-tasks/wait/logs` | ❌ | Pure Deadline API |
| `config show/set/get/clear/gui` | ❌ | Local config file |
| `auth login/logout/status` | ❌ | Credential management |
| `mcp-server` | ❌ | Protocol server, no attachments |

The three beneficial commands (`job download-output`, `queue sync-output`,
`handle-web-url`) all converge on the same Rust primitive: the `DownloadOrchestrator`
from `crates/storage/`. Combined with the three primary CLI groups (manifest,
attachment, bundle-submit), the full set of Rust-beneficial CLIs covers every
command that touches file hashing, manifest operations, or S3 CAS data transfer.
