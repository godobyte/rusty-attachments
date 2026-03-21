# Job Submission Design Summary

**Full doc:** `design/job-submission.md`  
**Status:** ✅ IMPLEMENTED in `crates/ja-deadline-utils/`

## Purpose
Convert manifests and asset metadata into the job submission format for Deadline Cloud CreateJob API. Also provides worker sync stubs for future implementation.

## Key Types

```rust
enum PathFormat { Windows, Posix }

struct ManifestProperties {
    root_path: String,
    root_path_format: PathFormat,
    input_manifest_path: Option<String>,    // S3 key (partial)
    input_manifest_hash: Option<String>,
    output_relative_directories: Option<Vec<String>>,
    file_system_location_name: Option<String>,
}

struct Attachments {
    manifests: Vec<ManifestProperties>,
    file_system: String,  // "COPIED" or "VIRTUAL"
}

struct AssetRootManifest {
    root_path: String,
    asset_manifest: Option<Manifest>,
    outputs: Vec<PathBuf>,
    file_system_location_name: Option<String>,
}
```

## Conversion Functions

```rust
fn build_manifest_properties(
    asset_root_manifest: &AssetRootManifest,
    partial_manifest_key: Option<&str>,
    manifest_hash: Option<&str>,
) -> ManifestProperties;

fn build_attachments(
    manifest_properties: Vec<ManifestProperties>,
    file_system_mode: &str,
) -> Attachments;

fn to_job_attachments(
    upload_results: &[ManifestUploadInfo],
    file_system_mode: &str,
) -> Attachments;
```

## JSON Output Format

```json
{
  "manifests": [
    {
      "rootPath": "/mnt/projects/job1",
      "rootPathFormat": "posix",
      "inputManifestPath": "farm-123/queue-456/Inputs/abc123_input",
      "inputManifestHash": "def456789...",
      "outputRelativeDirectories": ["renders", "cache"],
      "fileSystemLocationName": "ProjectFiles"
    }
  ],
  "fileSystem": "COPIED"
}
```

## High-Level Submit Function

```rust
async fn submit_bundle_attachments<C: StorageClient>(
    client: &C,
    s3_location: &S3Location,
    manifest_location: &ManifestLocation,
    asset_references: &AssetReferences,
    storage_profile: Option<&StorageProfile>,
    options: &BundleSubmitOptions,
    scan_progress: Option<...>,
    transfer_progress: Option<...>,
) -> Result<BundleSubmitResult, BundleSubmitError>;

struct BundleSubmitResult {
    attachments: Attachments,
    attachments_json: String,
    hashing_stats: SummaryStatistics,
    upload_stats: SummaryStatistics,
}
```

Composes: group_asset_paths → snapshot → upload contents → upload manifest → build_attachments.

## Worker Sync (Planned)

`crates/ja-deadline-utils/src/worker_sync.rs` is a placeholder for:
- `sync_inputs()` - Download job inputs to worker session directory
- `sync_outputs()` - Upload job outputs from worker session directory

## When to Read Full Doc
- Implementing job submission
- Understanding attachments JSON format
- Path format handling
- Output directory tracking
- Worker sync design
