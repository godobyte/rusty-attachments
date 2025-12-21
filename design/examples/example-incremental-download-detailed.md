# Example: Incremental Download Implementation in Rusty-Attachments

## Overview

This document provides a detailed analysis of the Python `_incremental_output_download` implementation and maps it to rusty-attachments primitives. The focus is on manifest operations and file downloading, excluding CLI parsing, job state tracking, and checkpoint management.

---

## Python Implementation Flow Analysis

### High-Level Flow

```
1. Get download candidate jobs (SearchJobs API)
2. Categorize jobs (compare with checkpoint)
3. Get sessions and session actions (ListSessions, ListSessionActions APIs)
4. Populate manifest S3 keys from S3 listing
5. Get storage profiles (GetStorageProfileForQueue API)
6. Create path mapping rules (if storage profiles differ)
7. Download all manifests from S3
8. Make manifest paths absolute
9. Apply path mapping (if needed)
10. Merge manifests chronologically
11. Download files from CAS
```

### Key Manifest Operations

#### 1. Manifest Discovery (`_add_output_manifests_from_s3`)

**What it does:**
- Lists S3 objects under the output manifest prefix
- Extracts session action IDs from S3 keys using regex
- Matches manifest keys to job attachment roots using hash
- Populates session actions with `outputManifestPath`

**S3 Key Pattern:**
```
{rootPrefix}/Manifests/{farmId}/{queueId}/{jobId}/{stepId}/{taskId}/{timestamp}_{sessionActionId}/{hash}_output
```

**Rust Equivalent:**
```rust
use rusty_attachments_storage::{
    ManifestLocation, OutputManifestScope, OutputManifestDiscoveryOptions,
    discover_output_manifest_keys,
};

// Discover manifests for a job
let options = OutputManifestDiscoveryOptions {
    scope: OutputManifestScope::Job { job_id: job_id.to_string() },
    select_latest_per_task: false, // Get all, not just latest
};

let manifest_keys = discover_output_manifest_keys(
    &client,
    &location,
    &options,
).await?;
```

#### 2. Manifest Download with Path Transformation (`_download_manifest_and_make_paths_absolute`)

**What it does:**
- Downloads manifest from S3
- Extracts `LastModified` timestamp
- Joins manifest paths with root path
- Applies path mapping if storage profiles differ
- Tracks unmapped paths

**Python Code:**
```python
# Download manifest
_, last_modified, manifest = _get_asset_root_and_manifest_from_s3_with_last_modified(
    manifest_s3_key, s3_bucket, boto3_session
)

# Make paths absolute
for manifest_path in manifest.paths:
    manifest_path.path = source_os_path.normpath(
        source_os_path.join(root_path, manifest_path.path)
    )
    
    # Apply path mapping
    if path_mapping_rule_applier:
        try:
            manifest_path.path = str(
                path_mapping_rule_applier.strict_transform(manifest_path.path)
            )
            new_manifest_paths.append(manifest_path)
        except ValueError:
            output_unmapped_paths.append((job_id, manifest_path.path))
```

**Rust Equivalent:**
```rust
use rusty_attachments_storage::{
    download_manifest, PathMappingApplier, PathFormat,
    resolve_manifest_paths,
};

// Download manifest with metadata
let (manifest, metadata) = download_manifest(&client, bucket, &s3_key).await?;
let last_modified = metadata.last_modified.unwrap_or(0);

// Resolve paths with mapping
let resolved = resolve_manifest_paths(
    &manifest,
    &metadata.asset_root,
    source_path_format,
    applier.as_ref(),
)?;

// Track unmapped paths
for unmapped in &resolved.unmapped {
    unmapped_paths.entry(job_id.clone())
        .or_default()
        .push(unmapped.absolute_source_path.clone());
}
```

#### 3. Manifest Merging (`_merge_absolute_path_manifest_list`)

**What it does:**
- Sorts manifests by `LastModified` timestamp (oldest first)
- Merges paths using normalized path as key
- Later files overwrite earlier ones (last-write-wins)

**Python Code:**
```python
# Sort by last modified
downloaded_manifests.sort(key=lambda item: item[0])

# Merge using dict (later overwrites earlier)
merged_manifest_paths_dict = {}
for _, manifest in downloaded_manifests:
    for manifest_path in manifest.paths:
        merged_manifest_paths_dict[os.path.normcase(manifest_path.path)] = manifest_path

return list(merged_manifest_paths_dict.values())
```

**Rust Equivalent:**
```rust
use rusty_attachments_model::merge::merge_manifests_chronologically;
use std::collections::HashMap;

// Prepare manifests with timestamps
let mut manifests_with_timestamps: Vec<(i64, &Manifest)> = downloaded_manifests
    .iter()
    .map(|(ts, manifest)| (*ts, manifest))
    .collect();

// Merge chronologically (handles last-write-wins)
let merged = merge_manifests_chronologically(&mut manifests_with_timestamps)?
    .expect("non-empty manifest list");

// Extract file paths for download
let files_to_download: Vec<&ManifestFilePath> = merged.files()?.iter().collect();
```

#### 4. File Download (`_download_manifest_paths`)

**What it does:**
- Downloads files from CAS using hash as key
- Uses transfer manager for large files (>1MB)
- Uses get_object for small files
- Handles conflict resolution (skip, overwrite, create copy)
- Sets file mtime from manifest
- Verifies file size after download
- Reports progress

**Python Code:**
```python
def _download_file(file, hash_algorithm, s3_bucket, cas_prefix, ...):
    s3_key = f"{cas_prefix}/{file.hash}.{hash_algorithm.value}"
    
    # Conflict resolution
    if local_file_path.is_file():
        if file_conflict_resolution == FileConflictResolution.SKIP:
            return
        elif file_conflict_resolution == FileConflictResolution.CREATE_COPY:
            local_file_path = _get_new_copy_file_path(...)
    
    # Download based on size
    if (file.size or 0) > 1024 * 1024:
        _download_file_with_transfer_manager(...)
    else:
        _download_file_with_get_object(...)
    
    # Set mtime
    modified_time_override = (file.mtime or 0) / 1000000
    os.utime(local_file_path, (modified_time_override, modified_time_override))
    
    # Verify size
    file_size_on_disk = os.path.getsize(local_file_path)
    if file_size_on_disk != file.size:
        raise JobAttachmentsError(...)
```

**Rust Equivalent:**
```rust
use rusty_attachments_storage::{
    DownloadOrchestrator, ConflictResolution, TransferProgress,
};

// Download to resolved paths
let stats = orchestrator.download_to_resolved_paths(
    &resolved.resolved,
    manifest.hash_alg(),
    ConflictResolution::CreateCopy,
    Some(&progress_callback),
).await?;
```

---

## Rusty-Attachments Implementation

### Core Data Structures

```rust
/// Manifest with metadata for chronological merging
#[derive(Debug, Clone)]
pub struct ManifestWithTimestamp {
    pub manifest: Manifest,
    pub last_modified: i64,  // Unix timestamp (seconds)
    pub asset_root: String,
}

/// Result of downloading and resolving manifests
#[derive(Debug, Clone)]
pub struct ResolvedManifests {
    /// Successfully resolved paths ready for download
    pub resolved: Vec<ResolvedManifestPath>,
    /// Paths that couldn't be mapped
    pub unmapped: HashMap<String, Vec<String>>,  // job_id -> unmapped_paths
}

/// Session action with manifest information
#[derive(Debug, Clone)]
pub struct SessionActionWithManifests {
    pub session_action_id: String,
    pub session_id: String,
    pub job_id: String,
    /// Manifest S3 keys (one per job attachment root)
    pub manifest_keys: Vec<Option<String>>,
}
```

### Step 1: Discover Output Manifests

```rust
use rusty_attachments_storage::{
    ManifestLocation, OutputManifestScope, OutputManifestDiscoveryOptions,
    discover_output_manifest_keys,
};

/// Discover all output manifests for jobs with new outputs
pub async fn discover_job_output_manifests<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    job_ids: &[String],
) -> Result<HashMap<String, Vec<String>>, StorageError> {
    let mut manifests_by_job: HashMap<String, Vec<String>> = HashMap::new();
    
    for job_id in job_ids {
        let options = OutputManifestDiscoveryOptions {
            scope: OutputManifestScope::Job { job_id: job_id.clone() },
            select_latest_per_task: false,  // Get all manifests
        };
        
        let keys = discover_output_manifest_keys(client, location, &options).await?;
        manifests_by_job.insert(job_id.clone(), keys);
    }
    
    Ok(manifests_by_job)
}
```

### Step 2: Match Manifests to Session Actions

```rust
use rusty_attachments_storage::manifest_storage::{
    compute_root_path_hash, match_manifests_to_roots, JobAttachmentRoot,
};

// Build job attachment roots from job.attachments.manifests
let job_attachment_roots: Vec<JobAttachmentRoot> = job["attachments"]["manifests"]
    .iter()
    .map(|m| JobAttachmentRoot {
        root_path: m["rootPath"].clone(),
        file_system_location_name: m.get("fileSystemLocationName").cloned(),
    })
    .collect();

// Match discovered manifest keys to roots
let manifests_by_session_action = match_manifests_to_roots(
    &manifest_keys,
    &job_attachment_roots,
)?;

// manifests_by_session_action["sessionaction-abc-0"] = [Some("key1"), Some("key2"), None]
// where the Vec is indexed by root position
```

### Step 3: Download and Transform Manifests

```rust
use rusty_attachments_storage::{
    download_manifest, PathMappingApplier, resolve_manifest_paths,
};

/// Download a single manifest and resolve its paths
pub async fn download_and_resolve_manifest<C: StorageClient>(
    client: &C,
    bucket: &str,
    s3_key: &str,
    source_path_format: PathFormat,
    applier: Option<&PathMappingApplier>,
) -> Result<ManifestWithTimestamp, StorageError> {
    // Download manifest with metadata
    let (manifest, metadata) = download_manifest(client, bucket, s3_key).await?;
    
    let asset_root = metadata.asset_root
        .ok_or_else(|| StorageError::MissingAssetRoot)?;
    let last_modified = metadata.last_modified.unwrap_or(0);
    
    Ok(ManifestWithTimestamp {
        manifest,
        last_modified,
        asset_root,
    })
}

/// Download all manifests and resolve paths
pub async fn download_all_manifests_with_resolution<C: StorageClient>(
    client: &C,
    bucket: &str,
    manifest_keys: &[(String, String, PathFormat)],  // (job_id, s3_key, source_format)
    path_mapping_appliers: &HashMap<String, Option<PathMappingApplier>>,
) -> Result<ResolvedManifests, StorageError> {
    let mut all_manifests: Vec<ManifestWithTimestamp> = Vec::new();
    let mut unmapped_by_job: HashMap<String, Vec<String>> = HashMap::new();
    
    // Download manifests in parallel
    let futures: Vec<_> = manifest_keys
        .iter()
        .map(|(job_id, s3_key, source_format)| {
            let applier = path_mapping_appliers.get(job_id).and_then(|a| a.as_ref());
            async move {
                let manifest_with_ts = download_and_resolve_manifest(
                    client,
                    bucket,
                    s3_key,
                    *source_format,
                    applier,
                ).await?;
                
                // Resolve paths
                let resolved = resolve_manifest_paths(
                    &manifest_with_ts.manifest,
                    &manifest_with_ts.asset_root,
                    *source_format,
                    applier,
                )?;
                
                Ok::<_, StorageError>((job_id.clone(), manifest_with_ts, resolved))
            }
        })
        .collect();
    
    // Await all downloads
    for result in futures::future::join_all(futures).await {
        let (job_id, manifest_with_ts, resolved) = result?;
        
        all_manifests.push(manifest_with_ts);
        
        // Track unmapped paths
        if !resolved.unmapped.is_empty() {
            unmapped_by_job.entry(job_id)
                .or_default()
                .extend(resolved.unmapped.iter().map(|u| u.absolute_source_path.clone()));
        }
    }
    
    Ok(ResolvedManifests {
        resolved: all_manifests.into_iter()
            .flat_map(|m| {
                // Convert manifest to resolved paths
                // This is simplified - actual implementation would use resolve_manifest_paths
                vec![]
            })
            .collect(),
        unmapped: unmapped_by_job,
    })
}
```

### Step 4: Merge Manifests by Asset Root

```rust
use rusty_attachments_model::merge::merge_manifests_chronologically;

/// Group manifests by asset root and merge chronologically
pub fn merge_manifests_by_root(
    manifests: Vec<ManifestWithTimestamp>,
) -> Result<HashMap<String, Manifest>, ManifestError> {
    // Group by asset root
    let mut by_root: HashMap<String, Vec<(i64, Manifest)>> = HashMap::new();
    
    for manifest_with_ts in manifests {
        by_root
            .entry(manifest_with_ts.asset_root.clone())
            .or_default()
            .push((manifest_with_ts.last_modified, manifest_with_ts.manifest));
    }
    
    // Merge each group chronologically
    let mut merged_by_root: HashMap<String, Manifest> = HashMap::new();
    
    for (root, mut manifests) in by_root {
        let merged = merge_manifests_chronologically(&mut manifests)?
            .ok_or_else(|| ManifestError::EmptyManifestList)?;
        
        merged_by_root.insert(root, merged);
    }
    
    Ok(merged_by_root)
}
```

### Step 5: Download Files

```rust
use rusty_attachments_storage::{
    DownloadOrchestrator, ConflictResolution, TransferProgress,
};

/// Download all files from merged manifests
pub async fn download_merged_manifests<C: StorageClient>(
    orchestrator: &DownloadOrchestrator<C>,
    merged_manifests: HashMap<String, Manifest>,
    conflict_resolution: ConflictResolution,
    progress: Option<&dyn ProgressCallback<TransferProgress>>,
) -> Result<TransferStatistics, StorageError> {
    let mut all_resolved: Vec<ResolvedManifestPath> = Vec::new();
    
    // Collect all resolved paths from all manifests
    for (root, manifest) in &merged_manifests {
        let resolved = resolve_manifest_paths_direct(&manifest, Path::new(root))?;
        all_resolved.extend(resolved);
    }
    
    // Download all files
    orchestrator.download_to_resolved_paths(
        &all_resolved,
        HashAlgorithm::Xxh128,  // Assume xxh128 for now
        conflict_resolution,
        progress,
    ).await
}

/// Helper to resolve manifest paths directly (no mapping)
fn resolve_manifest_paths_direct(
    manifest: &Manifest,
    root: &Path,
) -> Result<Vec<ResolvedManifestPath>, StorageError> {
    manifest.files()?
        .iter()
        .filter(|f| !f.deleted)
        .map(|entry| {
            let local_path = root.join(&entry.path);
            Ok(ResolvedManifestPath {
                manifest_entry: entry.clone(),
                local_path,
            })
        })
        .collect()
}
```

---

## Complete Incremental Download Function

```rust
use rusty_attachments_storage::{
    ManifestLocation, OutputManifestScope, OutputManifestDiscoveryOptions,
    discover_output_manifest_keys, download_manifest,
    PathMappingApplier, PathFormat, resolve_manifest_paths,
    DownloadOrchestrator, ConflictResolution, TransferProgress,
    StorageClient, S3Location,
};
use rusty_attachments_model::{Manifest, merge::merge_manifests_chronologically};
use std::collections::HashMap;

/// Incremental download for a set of jobs
pub async fn incremental_download_jobs<C: StorageClient>(
    client: &C,
    s3_location: &S3Location,
    manifest_location: &ManifestLocation,
    job_ids: &[String],
    job_attachment_roots: &HashMap<String, Vec<JobAttachmentRoot>>,
    path_mapping_appliers: &HashMap<String, Option<PathMappingApplier>>,
    conflict_resolution: ConflictResolution,
    progress: Option<&dyn ProgressCallback<TransferProgress>>,
) -> Result<IncrementalDownloadResult, StorageError> {
    // 1. Discover all output manifests
    let mut all_manifest_keys: Vec<(String, String, PathFormat)> = Vec::new();
    
    for job_id in job_ids {
        let options = OutputManifestDiscoveryOptions {
            scope: OutputManifestScope::Job { job_id: job_id.clone() },
            select_latest_per_task: false,
        };
        
        let keys = discover_output_manifest_keys(client, manifest_location, &options).await?;
        
        // Get source path format from job's storage profile
        let source_format = get_job_source_format(job_id, job_attachment_roots)?;
        
        for key in keys {
            all_manifest_keys.push((job_id.clone(), key, source_format));
        }
    }
    
    // 2. Download all manifests with path resolution
    let mut manifests_with_ts: Vec<ManifestWithTimestamp> = Vec::new();
    let mut unmapped_by_job: HashMap<String, Vec<String>> = HashMap::new();
    
    for (job_id, s3_key, source_format) in &all_manifest_keys {
        let applier = path_mapping_appliers
            .get(job_id)
            .and_then(|a| a.as_ref());
        
        // Download manifest
        let (manifest, metadata) = download_manifest(
            client,
            &manifest_location.bucket,
            s3_key,
        ).await?;
        
        let asset_root = metadata.asset_root
            .ok_or_else(|| StorageError::MissingAssetRoot)?;
        let last_modified = metadata.last_modified.unwrap_or(0);
        
        // Resolve paths
        let resolved = resolve_manifest_paths(
            &manifest,
            &asset_root,
            *source_format,
            applier,
        )?;
        
        // Track unmapped
        if !resolved.unmapped.is_empty() {
            unmapped_by_job
                .entry(job_id.clone())
                .or_default()
                .extend(resolved.unmapped.iter().map(|u| u.absolute_source_path.clone()));
        }
        
        manifests_with_ts.push(ManifestWithTimestamp {
            manifest,
            last_modified,
            asset_root,
        });
    }
    
    // 3. Group by asset root and merge chronologically
    let mut by_root: HashMap<String, Vec<(i64, Manifest)>> = HashMap::new();
    
    for manifest_with_ts in manifests_with_ts {
        by_root
            .entry(manifest_with_ts.asset_root.clone())
            .or_default()
            .push((manifest_with_ts.last_modified, manifest_with_ts.manifest));
    }
    
    let mut all_resolved: Vec<ResolvedManifestPath> = Vec::new();
    
    for (root, mut manifests) in by_root {
        // Merge chronologically
        let merged = merge_manifests_chronologically(&mut manifests)?
            .ok_or_else(|| ManifestError::EmptyManifestList)?;
        
        // Resolve to absolute paths
        let resolved = resolve_manifest_paths_direct(&merged, Path::new(&root))?;
        all_resolved.extend(resolved);
    }
    
    // 4. Download all files
    let orchestrator = DownloadOrchestrator::new(client.clone(), s3_location.clone());
    
    let stats = orchestrator.download_to_resolved_paths(
        &all_resolved,
        HashAlgorithm::Xxh128,
        conflict_resolution,
        progress,
    ).await?;
    
    Ok(IncrementalDownloadResult {
        stats,
        unmapped_paths: unmapped_by_job,
        files_downloaded: all_resolved.len(),
    })
}

#[derive(Debug, Clone)]
pub struct IncrementalDownloadResult {
    pub stats: TransferStatistics,
    pub unmapped_paths: HashMap<String, Vec<String>>,
    pub files_downloaded: usize,
}

fn get_job_source_format(
    job_id: &str,
    job_attachment_roots: &HashMap<String, Vec<JobAttachmentRoot>>,
) -> Result<PathFormat, StorageError> {
    job_attachment_roots
        .get(job_id)
        .and_then(|roots| roots.first())
        .map(|root| root.root_path_format)
        .ok_or_else(|| StorageError::MissingJobInfo { job_id: job_id.to_string() })
}
```

---

## Key Differences from Python

### 1. Manifest Discovery

**Python:** Lists all S3 objects, extracts session action IDs with regex, matches to roots
**Rust:** Uses `discover_output_manifest_keys()` primitive which handles S3 listing and filtering

### 2. Path Resolution

**Python:** Manually joins paths, applies path mapping in-place
**Rust:** Uses `resolve_manifest_paths()` which returns resolved and unmapped paths separately

### 3. Manifest Merging

**Python:** Uses dict with normcased path as key for last-write-wins
**Rust:** Uses `merge_manifests_chronologically()` which handles merging at manifest level

### 4. File Download

**Python:** Manual thread pool, size-based download strategy, conflict resolution
**Rust:** `DownloadOrchestrator.download_to_resolved_paths()` handles all of this

---

## Primitives Used

| Operation | Rust Primitive | Module |
|-----------|---------------|--------|
| Discover manifests | `discover_output_manifest_keys()` | `manifest-storage` |
| Download manifest | `download_manifest()` | `manifest-storage` |
| Resolve paths | `resolve_manifest_paths()` | `path-mapping` |
| Merge manifests | `merge_manifests_chronologically()` | `manifest-utils` |
| Download files | `download_to_resolved_paths()` | `storage-design` |
| Path mapping | `PathMappingApplier` | `path-mapping` |

---

## Related Documents

- [manifest-storage.md](../manifest-storage.md) - Manifest discovery and download
- [manifest-utils.md](../manifest-utils.md) - Manifest merging
- [path-mapping.md](../path-mapping.md) - Path transformation
- [storage-design.md](../storage-design.md) - File download orchestration
- [example-incremental-download.md](example-incremental-download.md) - Simplified example


---

## Analysis: Comparison with deadline-cloud Implementation

This section analyzes the rusty-attachments design against the actual deadline-cloud Python implementation in `_incremental_download.py` and `_manifest_s3_downloads.py`.

### Summary

The rusty-attachments design **covers all core primitives** needed for incremental download. However, there are some **gaps and refinements** worth noting.

---

### ✅ Well-Covered Areas

#### 1. Manifest Discovery from S3

**Python (`_add_output_manifests_from_s3`):**
- Lists S3 objects under output manifest prefix
- Extracts session action IDs using regex: `(sessionaction-[^/-]+-[^/-]+)/`
- Matches manifest keys to job attachment roots using hash of `{fileSystemLocationName}{rootPath}`

**Rust Design (`manifest-storage.md`):**
- `discover_output_manifest_keys()` handles S3 listing and filtering
- `parse_manifest_keys()` extracts session action IDs
- `group_manifests_by_task()` and `select_latest_manifests_per_task()` for filtering

**Assessment:** ✅ Complete - The Rust design provides equivalent primitives.

#### 2. Manifest Download with Metadata

**Python (`_get_asset_root_and_manifest_from_s3_with_last_modified`):**
- Downloads manifest from S3
- Extracts `LastModified` timestamp from S3 response
- Parses `asset-root` and `asset-root-json` from S3 metadata

**Rust Design (`manifest-storage.md`):**
- `download_manifest()` returns `(Manifest, ManifestDownloadMetadata)`
- `ManifestDownloadMetadata` includes `asset_root`, `file_system_location_name`, `last_modified`
- `ManifestS3Metadata::from_s3_metadata()` handles both ASCII and JSON-encoded paths

**Assessment:** ✅ Complete

#### 3. Path Resolution and Mapping

**Python (`_download_manifest_and_make_paths_absolute`):**
- Joins manifest paths with root using source OS path module (`ntpath` or `posixpath`)
- Applies `_PathMappingRuleApplier.strict_transform()` for cross-profile mapping
- Tracks unmapped paths separately

**Rust Design (`path-mapping.md`):**
- `resolve_manifest_paths()` handles joining and mapping
- `PathMappingApplier` with trie-based matching
- `ResolvedManifestPaths` separates `resolved` and `unmapped` paths

**Assessment:** ✅ Complete

#### 4. Chronological Manifest Merging

**Python (`_merge_absolute_path_manifest_list`):**
- Sorts manifests by `LastModified` timestamp (oldest first)
- Uses dict with `os.path.normcase(path)` as key for last-write-wins

**Rust Design (`manifest-utils.md`):**
- `merge_manifests_chronologically()` sorts by timestamp and merges
- Uses `HashMap` with path as key for last-write-wins semantics

**Assessment:** ✅ Complete

#### 5. File Download with Conflict Resolution

**Python (`_download_file`):**
- Size-based strategy: `>1MB` uses transfer manager, else `get_object`
- Conflict resolution: SKIP, OVERWRITE, CREATE_COPY
- `_get_new_copy_file_path()` generates unique names with file locking
- Sets mtime from manifest after download
- Verifies file size matches manifest

**Rust Design (`storage-design.md`):**
- `download_to_resolved_paths()` handles batch downloads
- `ConflictResolution` enum with Skip, Overwrite, CreateCopy
- `generate_unique_copy_path()` with atomic file creation
- File metadata setting mentioned in download orchestrator

**Assessment:** ✅ Complete

---

### ⚠️ Gaps and Refinements Needed

#### 1. ~~Session Action ID Matching to Manifest Roots~~ ✅ DONE

**Status:** Added to `manifest-storage.md`

- `compute_root_path_hash()` - Computes xxh128 hash of `{fileSystemLocationName}{rootPath}`
- `match_manifests_to_roots()` - Matches manifest S3 keys to job attachment roots
- `JobAttachmentRoot` - Data structure for root information

#### 2. Manifest Index Correspondence

**Python Implementation Detail:**
```python
# The manifests lists from the job and session action correspond, so we can zip them
for job_manifest, session_action_manifest in zip(
    job["attachments"]["manifests"], session_action["manifests"]
):
```

Each session action has a `manifests` array that corresponds 1:1 with the job's `attachments.manifests` array. This is critical for:
- Knowing which root path each output manifest belongs to
- Handling jobs with multiple asset roots

**Rust Design Gap:**
This correspondence isn't explicitly documented. The design assumes manifests are matched by hash, but the index-based correspondence is also important for the orchestration layer.

**Recommendation:** Document this in `manifest-storage.md` or a new `incremental-download.md` design doc:
```rust
/// Session action manifest information.
/// 
/// The `manifests` array corresponds 1:1 with the job's `attachments.manifests` array.
/// Each entry contains the output manifest path for that asset root, or None if
/// the session action didn't produce output for that root.
pub struct SessionActionManifests {
    pub session_action_id: String,
    /// One entry per job attachment root, in the same order as job.attachments.manifests
    pub manifests: Vec<Option<SessionActionManifestInfo>>,
}

pub struct SessionActionManifestInfo {
    pub output_manifest_path: String,
}
```

#### 3. Source Path Format Handling in Path Resolution

**Python Implementation Detail:**
```python
if path_mapping_rule_applier:
    if path_mapping_rule_applier.source_path_format == PathFormat.WINDOWS.value:
        source_os_path: Any = ntpath
    else:
        source_os_path = posixpath
else:
    source_os_path = os.path

# Join using source format
manifest_path.path = source_os_path.normpath(
    source_os_path.join(root_path, manifest_path.path)
)
```

The Python code uses the source path format (from the job's storage profile) to determine how to join paths, even when no path mapping is applied.

**Rust Design Status:**
The `resolve_manifest_paths()` function in `path-mapping.md` takes `source_path_format` as a parameter and uses `join_path()` which handles this correctly.

**Assessment:** ✅ Covered, but worth verifying the `join_path()` implementation handles edge cases like:
- Windows UNC paths (`\\server\share`)
- Mixed separators in input
- Trailing separators on root

#### 4. Progress Callback with Cancellation

**Python Implementation:**
```python
def _update_download_progress(download_metadata: ProgressReportMetadata) -> bool:
    # ... progress reporting ...
    return sigint_handler.continue_operation  # Return False to cancel
```

The progress callback returns a boolean to signal cancellation.

**Rust Design:**
The `ProgressCallback<T>` trait in `common.md` returns `bool` from `on_progress()`:
```rust
pub trait ProgressCallback<T>: Send + Sync {
    fn on_progress(&self, progress: &T) -> bool;  // false = cancel
}
```

**Assessment:** ✅ Complete

#### 5. ~~File Size Verification After Download~~ ✅ DONE

**Status:** Added to `storage-design.md`

- `DownloadOptions` struct with `verify_size: bool` (default: `true`)
- `verify_file_size()` helper function
- `apply_post_download_options()` applies all post-download processing

#### 6. ~~mtime Setting After Download~~ ✅ DONE

**Status:** Added to `storage-design.md`

- `DownloadOptions` struct with `set_mtime: bool` (default: `true`)
- `set_file_mtime()` helper function that converts microseconds to system time
- `set_file_executable()` for runnable files (also in `DownloadOptions`)
- `apply_post_download_options()` applies all post-download processing

---

### 🔍 Design Observations

#### 1. Orchestration vs Primitives

The rusty-attachments design correctly separates:
- **Primitives** (manifest-storage, path-mapping, manifest-utils): Low-level operations
- **Orchestration** (this example document): How to compose primitives

The Python `_incremental_download.py` is primarily orchestration code that:
- Calls Deadline APIs (SearchJobs, ListSessions, ListSessionActions)
- Manages checkpoint state
- Coordinates manifest discovery and download

This orchestration logic should remain in the consuming application (worker-agent), not in rusty-attachments core.

#### 2. Parallel Download Strategy

**Python:**
```python
max_workers = S3_DOWNLOAD_MAX_CONCURRENCY  # Typically 10
with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as executor:
    futures = [executor.submit(_download_file, ...) for manifest_path in manifest_paths_to_download]
```

**Rust (CRT):**
The CRT handles parallelism internally, so the Rust design doesn't need explicit thread pool management. This is correctly noted in `storage-design.md`.

#### 3. Error Handling Granularity

The Python implementation has detailed error types for S3 operations:
- `JobAttachmentsS3ClientError` with status code guidance
- `JobAttachmentS3BotoCoreError` for boto errors
- `AssetSyncCancelledError` for cancellation

The Rust `StorageError` enum covers these cases but could benefit from more detailed guidance messages for common errors (403, 404).

---

### Conclusions

The rusty-attachments design is **well-suited for implementing incremental download**. The previously identified gaps have been addressed:

1. ✅ **Root path hash computation** - Added `compute_root_path_hash()` and `match_manifests_to_roots()` to manifest-storage.md
2. ⚠️ **Manifest index correspondence** - Document the 1:1 relationship between job manifests and session action manifests (orchestration-level concern)
3. ✅ **Source path format handling** - Already covered in `resolve_manifest_paths()`
4. ✅ **Progress callback with cancellation** - Already covered in `ProgressCallback<T>`
5. ✅ **Post-download size verification** - Added `DownloadOptions.verify_size` and `verify_file_size()` to storage-design.md
6. ✅ **mtime setting after download** - Added `DownloadOptions.set_mtime` and `set_file_mtime()` to storage-design.md

The remaining item (#2) is an orchestration-level concern that belongs in the consuming application (worker-agent), not the core library.

No fundamental design changes are required. The primitives are complete and composable.

---

## Related Documents

- [manifest-storage.md](../manifest-storage.md) - Manifest discovery and download
- [manifest-utils.md](../manifest-utils.md) - Manifest merging
- [path-mapping.md](../path-mapping.md) - Path transformation
- [storage-design.md](../storage-design.md) - File download orchestration
- [example-worker-agent.md](example-worker-agent.md) - Worker agent integration analysis
