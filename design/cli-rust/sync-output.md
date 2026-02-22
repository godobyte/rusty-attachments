# Pure-Rust CLI: `ra sync-output`

## Command

```
ra sync-output \
    [--farm-id <id>] \
    [--queue-id <id>] \
    [--profile <aws_profile>] \
    [--checkpoint-dir <dir>] \
    [--bootstrap-lookback-minutes <int>] \
    [--conflict-resolution SKIP|OVERWRITE|CREATE_COPY] \
    [--json]
```

## Overview

Incrementally downloads output files for all jobs in a queue since the last
checkpoint. This is the Rust equivalent of `deadline queue sync-output`.

This is the most complex download command because it orchestrates:
1. Job discovery via SearchJobs API
2. Session/session-action enumeration
3. Storage profile resolution and path mapping
4. Checkpoint state management
5. Manifest download, merge, and file download

## Implementation

```
main()
  → parse_args()
  → load_config(farm_id, queue_id, profile)
  → resolve_credentials(profile)
  │
  ├─ 1. CHECKPOINT MANAGEMENT
  │    → load_checkpoint(checkpoint_dir, farm_id, queue_id)
  │    │  // JSON file: { jobs: { job_id: { status, sessions, ... } }, last_sync: timestamp }
  │    → determine lookback_time = max(checkpoint.last_sync, now - bootstrap_lookback)
  │
  ├─ 2. JOB DISCOVERY (Deadline API)
  │    → deadline_api::search_jobs(farm_id, queue_id, since=lookback_time)
  │    → categorize_jobs(checkpoint, discovered_jobs)
  │    │  → new_jobs: not in checkpoint
  │    │  → updated_jobs: in checkpoint but have new sessions
  │    │  → completed_jobs: terminal state, no new sessions
  │
  ├─ 3. SESSION ENUMERATION (Deadline API)
  │    → for each (new + updated) job:
  │    │    → deadline_api::list_sessions(farm_id, queue_id, job_id)
  │    │    → for each session:
  │    │         → deadline_api::list_session_actions(farm_id, queue_id, job_id, session_id)
  │    │         → filter to actions with output manifests
  │    → collect manifest_specs: Vec<ManifestDownloadSpec>
  │
  ├─ 4. STORAGE PROFILE RESOLUTION
  │    → deadline_api::get_queue(farm_id, queue_id)
  │    → deadline_api::get_storage_profile(farm_id, queue_id, profile_id)
  │    → build path_mapping_rules from storage profile locations
  │
  ├─ 5. MANIFEST DOWNLOAD + MERGE + FILE DOWNLOAD (all Rust)
  │    → for each manifest_spec:
  │    │    → download_manifest(client, bucket, spec.s3_key)
  │    → apply path_mapping_rules to asset roots
  │    → group by mapped root
  │    → merge_manifests_chronologically() per root
  │    → for each (root, merged_manifest):
  │    │    → DownloadOrchestrator::download_manifest_contents(
  │    │          manifest, root, conflict_resolution, progress
  │    │      )
  │    → accumulate TransferStatistics
  │
  ├─ 6. CHECKPOINT UPDATE
  │    → update checkpoint with processed jobs/sessions
  │    → save_checkpoint(checkpoint_dir, farm_id, queue_id)
  │
  └─ 7. OUTPUT
       → format_output(stats, json)
```

## Checkpoint Format

Compatible with the Python CLI's checkpoint format for interoperability:

```json
{
  "schema_version": 1,
  "farm_id": "farm-xxx",
  "queue_id": "queue-xxx",
  "last_sync_time": "2026-02-22T10:30:00Z",
  "jobs": {
    "job-abc": {
      "status": "SUCCEEDED",
      "last_session_action_id": "sessionaction-xyz",
      "processed_session_actions": ["sessionaction-001", "sessionaction-002"]
    }
  }
}
```

```rust
#[derive(Debug, Serialize, Deserialize)]
struct SyncCheckpoint {
    schema_version: u32,
    farm_id: String,
    queue_id: String,
    last_sync_time: String,
    jobs: HashMap<String, JobCheckpoint>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JobCheckpoint {
    status: String,
    last_session_action_id: Option<String>,
    processed_session_actions: Vec<String>,
}
```

## Improvements over Python

1. **Concurrent API calls** — Job sessions and session actions are fetched
   concurrently across jobs using `tokio::spawn`. Python fetches sequentially
   per job.

2. **Streaming pipeline** — Manifest download → path mapping → merge →
   file download runs as a streaming pipeline. Python buffers all manifests
   in memory before starting file downloads.

3. **Chronological merge in Rust** — `merge_manifests_chronologically()`
   sorts by `last_modified` and merges in a single pass. Python sorts
   manifests, then iterates to merge.

4. **Atomic checkpoint writes** — Write to temp file then rename, preventing
   corruption on crash. Python writes directly to the checkpoint file.

5. **Parallel file downloads** — CRT-based parallel downloads with
   configurable concurrency. Python uses `ThreadPoolExecutor` with GIL.

## Error Recovery

The checkpoint-based design provides natural error recovery:

```rust
/// Resume from the last successful checkpoint on error.
fn sync_with_recovery(args: &SyncArgs) -> Result<SyncResult, CliError> {
    let checkpoint: SyncCheckpoint = load_checkpoint(&args.checkpoint_dir, &args.farm_id, &args.queue_id)?;

    match run_sync(&checkpoint, args) {
        Ok(result) => {
            save_checkpoint(&result.updated_checkpoint, &args.checkpoint_dir)?;
            Ok(result)
        }
        Err(e) => {
            // Checkpoint is not updated on error — next run retries from same point
            eprintln!("Sync failed: {}. Checkpoint preserved for retry.", e);
            Err(e)
        }
    }
}
```
