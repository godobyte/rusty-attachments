# Manifest Storage Design Summary

**Full doc:** `design/manifest-storage.md`  
**Status:** ✅ IMPLEMENTED in `crates/storage/src/manifest_storage.rs`

## Purpose
Upload and download manifest files to/from S3 with proper naming, metadata, and content types.

## S3 Key Formats

### Input Manifests
```
{root_prefix}/Manifests/{farm_id}/{queue_id}/Inputs/{guid}/{hash}_input
```

### Output Manifests (Task-Level)
```
{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/{timestamp}_{session_action_id}/{hash}_output
```

### Output Manifests (Step-Level)
```
{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{timestamp}_{session_action_id}/{hash}_output
```

## Key Types

```rust
struct ManifestLocation {
    bucket: String,
    root_prefix: String,
    farm_id: String,
    queue_id: String,
}

struct TaskOutputManifestPath {
    job_id: String,
    step_id: String,
    task_id: String,
    session_action_id: String,
    timestamp: f64,
}

struct ManifestS3Metadata {
    asset_root: String,
    file_system_location_name: Option<String>,
}
```

## Key Functions

### Upload
- `upload_input_manifest()`: Upload with metadata, returns s3_key and hash
- `upload_task_output_manifest()`: Task-level output
- `upload_step_output_manifest()`: Step-level output (chunked steps)

### Download
- `download_manifest()`: By S3 key
- `download_input_manifest()`: By hash
- `download_output_manifests_by_asset_root()`: Discover, download, merge by root

### Discovery
- `discover_output_manifest_keys()`: List output manifests with scope filtering
- `select_latest_manifests_per_task()`: Get latest session action per task
- `match_manifests_to_roots()`: Match keys to job attachment roots

## Content Types
- v2023 Snapshot: `application/x-deadline-manifest-2023-03-03`
- v2025 Snapshot: `application/x-deadline-manifest-2025-12-04-beta`
- v2025 Diff: `application/x-deadline-manifest-diff-2025-12-04-beta`

## S3 Metadata
- `asset-root`: ASCII root path
- `asset-root-json`: JSON-encoded for non-ASCII paths
- `file-system-location-name`: Storage profile location

## When to Read Full Doc
- Implementing manifest upload/download
- Understanding S3 key structure
- Output manifest discovery logic
- Metadata handling for non-ASCII paths
