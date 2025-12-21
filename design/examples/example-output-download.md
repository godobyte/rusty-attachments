# Example: Job Output Download Use Case

This example demonstrates how to use rusty-attachments primitives to implement the `deadline job download-output` CLI command.

## Overview

Output download retrieves job outputs from S3 CAS to the local machine. Key challenges:
- Jobs may have been submitted from a different OS (path format mismatch)
- Multiple output manifests need to be merged chronologically
- User may want to override download locations
- File conflicts need resolution (skip, overwrite, create copy)
- Long paths on Windows need special handling

## Primitives Used

| Primitive | Module | Purpose |
|-----------|--------|---------|
| `discover_output_manifest_keys()` | `manifest-storage` | Find output manifests in S3 |
| `download_manifest_with_metadata()` | `manifest-storage` | Download manifest with asset root and timestamp |
| `merge_manifests_chronologically()` | `manifest-utils` | Merge manifests by timestamp |
| `PathMappingApplier` | `path-mapping` | Transform paths between OS formats |
| `resolve_manifest_paths()` | `path-mapping` | Convert manifest paths to absolute local paths |
| `DownloadOrchestrator.download_to_resolved_paths()` | `storage-design` | Download files to pre-resolved paths |
| `set_file_permissions()` | `file_system` | Set ownership/permissions after download |

## Complete Example

```rust
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusty_attachments_common::ProgressCallback;
use rusty_attachments_model::{Manifest, PathFormat};
use rusty_attachments_storage::{
    // Manifest operations
    ManifestLocation, OutputManifestScope, OutputManifestDiscoveryOptions,
    discover_output_manifest_keys, download_manifest_with_metadata,
    ManifestDownloadMetadata,
    // Path mapping
    PathMappingRule, PathMappingApplier,
    resolve_manifest_paths, ResolvedManifestPath, UnmappedPath,
    // Download
    DownloadOrchestrator, S3Location, StorageClient,
    ConflictResolution, TransferProgress,
    // Progress
    DownloadSummaryStatistics,
};
use rusty_attachments_model::merge::merge_manifests_chronologically;

/// Result of output download operation
pub struct OutputDownloadResult {
    pub stats: DownloadSummaryStatistics,
    pub downloaded_paths: HashMap<String, Vec<PathBuf>>,
}

/// Output paths grouped by asset root
pub struct OutputPathsByRoot {
    pub paths_by_root: HashMap<String, Vec<String>>,
    pub root_path_formats: HashMap<String, PathFormat>,
}

/// Handler for downloading job outputs
pub struct OutputDownloader<C: StorageClient> {
    client: C,
    s3_location: S3Location,
    manifest_location: ManifestLocation,
    /// Manifests grouped by asset root
    outputs_by_root: HashMap<String, MergedOutputManifest>,
    /// Original root path format (from job submission)
    root_path_formats: HashMap<String, PathFormat>,
}

struct MergedOutputManifest {
    manifest: Manifest,
    paths: Vec<String>,
}

impl<C: StorageClient> OutputDownloader<C> {
    /// Create a new output downloader for a job/step/task.
    pub async fn new(
        client: C,
        s3_location: S3Location,
        manifest_location: ManifestLocation,
        job_id: &str,
        step_id: Option<&str>,
        task_id: Option<&str>,
        session_action_id: Option<&str>,
    ) -> Result<Self, StorageError> {
        // 1. Discover output manifests
        let scope = match (step_id, task_id) {
            (Some(step), Some(task)) => OutputManifestScope::Task {
                job_id: job_id.to_string(),
                step_id: step.to_string(),
                task_id: task.to_string(),
                session_action_id: session_action_id.map(String::from),
            },
            (Some(step), None) => OutputManifestScope::Step {
                job_id: job_id.to_string(),
                step_id: step.to_string(),
            },
            _ => OutputManifestScope::Job {
                job_id: job_id.to_string(),
            },
        };
        
        let options = OutputManifestDiscoveryOptions {
            scope,
            select_latest_per_task: true,
        };
        
        let manifest_keys = discover_output_manifest_keys(
            &client,
            &manifest_location,
            &options,
        ).await?;
        
        // 2. Download all manifests with metadata
        let mut manifests_with_metadata: Vec<(i64, Manifest, ManifestDownloadMetadata)> = Vec::new();
        
        for key in &manifest_keys {
            let (manifest, metadata) = download_manifest_with_metadata(
                &client,
                &manifest_location.bucket,
                key,
            ).await?;
            
            let timestamp = metadata.last_modified_us.unwrap_or(0);
            manifests_with_metadata.push((timestamp, manifest, metadata));
        }
        
        // 3. Group by asset root and merge chronologically
        let mut by_root: HashMap<String, Vec<(i64, Manifest)>> = HashMap::new();
        let mut root_path_formats: HashMap<String, PathFormat> = HashMap::new();
        
        for (ts, manifest, metadata) in manifests_with_metadata {
            let root = metadata.asset_root.ok_or(StorageError::MissingAssetRoot)?;
            by_root.entry(root.clone()).or_default().push((ts, manifest));
            
            if let Some(format) = metadata.root_path_format {
                root_path_formats.insert(root, format);
            }
        }
        
        let mut outputs_by_root: HashMap<String, MergedOutputManifest> = HashMap::new();
        
        for (root, mut manifests) in by_root {
            let merged = merge_manifests_chronologically(&mut manifests)?
                .expect("non-empty manifest list");
            
            let paths: Vec<String> = merged.paths()
                .iter()
                .map(|p| p.path.clone())
                .collect();
            
            outputs_by_root.insert(root, MergedOutputManifest {
                manifest: merged,
                paths,
            });
        }
        
        Ok(Self {
            client,
            s3_location,
            manifest_location,
            outputs_by_root,
            root_path_formats,
        })
    }
    
    /// Get output paths grouped by asset root.
    pub fn get_output_paths_by_root(&self) -> OutputPathsByRoot {
        let paths_by_root: HashMap<String, Vec<String>> = self.outputs_by_root
            .iter()
            .map(|(root, m)| (root.clone(), m.paths.clone()))
            .collect();
        
        OutputPathsByRoot {
            paths_by_root,
            root_path_formats: self.root_path_formats.clone(),
        }
    }
    
    /// Change the download root for a specific asset root.
    pub fn set_root_path(&mut self, original_root: &str, new_root: &str) -> Result<(), StorageError> {
        if !self.outputs_by_root.contains_key(original_root) {
            return Err(StorageError::RootNotFound {
                root: original_root.to_string(),
            });
        }
        
        if original_root == new_root {
            return Ok(());
        }
        
        // Handle collision if new_root already exists
        if let Some(existing) = self.outputs_by_root.remove(new_root) {
            let mut original = self.outputs_by_root.remove(original_root).unwrap();
            
            // Prefix conflicting paths with sanitized original root
            let prefix = sanitize_path_for_filename(original_root);
            let existing_paths: HashSet<_> = existing.paths.iter().collect();
            
            for path in original.manifest.paths_mut() {
                if existing_paths.contains(&path.path) {
                    path.path = format!("{}_{}", prefix, path.path);
                }
            }
            
            // Merge manifests
            let merged = merge_manifests_chronologically(&mut vec![
                (0, existing.manifest),
                (1, original.manifest),
            ])?.unwrap();
            
            let paths: Vec<String> = merged.paths()
                .iter()
                .map(|p| p.path.clone())
                .collect();
            
            self.outputs_by_root.insert(new_root.to_string(), MergedOutputManifest {
                manifest: merged,
                paths,
            });
        } else {
            // Simple rename
            let manifest = self.outputs_by_root.remove(original_root).unwrap();
            self.outputs_by_root.insert(new_root.to_string(), manifest);
        }
        
        // Update path format mapping
        if let Some(format) = self.root_path_formats.remove(original_root) {
            self.root_path_formats.insert(new_root.to_string(), format);
        }
        
        Ok(())
    }
    
    /// Download all outputs to their respective roots.
    pub async fn download_job_output(
        &self,
        conflict_resolution: ConflictResolution,
        progress: Option<&dyn ProgressCallback<TransferProgress>>,
    ) -> Result<OutputDownloadResult, StorageError> {
        let orchestrator = DownloadOrchestrator::new(
            self.client.clone(),
            self.s3_location.clone(),
        );
        
        let mut all_resolved: Vec<ResolvedManifestPath> = Vec::new();
        let mut downloaded_paths: HashMap<String, Vec<PathBuf>> = HashMap::new();
        
        for (root, output) in &self.outputs_by_root {
            // Resolve paths (no mapping needed - root already adjusted)
            let resolved = resolve_manifest_paths_direct(
                &output.manifest,
                Path::new(root),
            )?;
            
            all_resolved.extend(resolved);
        }
        
        // Calculate totals for progress
        let total_bytes: u64 = all_resolved.iter().map(|r| r.size).sum();
        let total_files = all_resolved.len();
        
        // Download all files
        let stats = orchestrator.download_to_resolved_paths(
            &all_resolved,
            conflict_resolution,
            progress,
        ).await?;
        
        // Group downloaded paths by root
        for resolved in &all_resolved {
            downloaded_paths
                .entry(resolved.root.clone())
                .or_default()
                .push(resolved.local_path.clone());
        }
        
        Ok(OutputDownloadResult {
            stats,
            downloaded_paths,
        })
    }
}

fn sanitize_path_for_filename(path: &str) -> String {
    path.replace('/', "_")
        .replace('\\', "_")
        .replace(':', "_")
}

/// Resolve manifest paths directly to local paths (no path mapping).
fn resolve_manifest_paths_direct(
    manifest: &Manifest,
    root: &Path,
) -> Result<Vec<ResolvedManifestPath>, StorageError> {
    manifest.paths()
        .iter()
        .map(|entry| {
            let local_path = root.join(&entry.path);
            Ok(ResolvedManifestPath {
                root: root.to_string_lossy().to_string(),
                relative_path: entry.path.clone(),
                local_path,
                hash: entry.hash.clone(),
                size: entry.size.unwrap_or(0),
            })
        })
        .collect()
}
```

## CLI Integration

```rust
use clap::Parser;

#[derive(Parser)]
struct DownloadOutputArgs {
    #[arg(long)]
    farm_id: String,
    #[arg(long)]
    queue_id: String,
    #[arg(long)]
    job_id: String,
    #[arg(long)]
    step_id: Option<String>,
    #[arg(long)]
    task_id: Option<String>,
    #[arg(long, default_value = "CREATE_COPY")]
    conflict_resolution: String,
    #[arg(long)]
    yes: bool,
}

async fn cli_download_output(args: DownloadOutputArgs) -> Result<(), Error> {
    let client = get_queue_role_client(&args.farm_id, &args.queue_id).await?;
    let s3_settings = get_queue_s3_settings(&args.farm_id, &args.queue_id).await?;
    
    // Get job to find session action ID if task specified
    let session_action_id = if args.task_id.is_some() {
        let task = get_task(&args.farm_id, &args.queue_id, &args.job_id, 
                           args.step_id.as_ref().unwrap(), args.task_id.as_ref().unwrap()).await?;
        task.latest_session_action_id
    } else {
        None
    };
    
    // Create downloader
    let mut downloader = OutputDownloader::new(
        client,
        s3_settings.s3_location(),
        s3_settings.manifest_location(),
        &args.job_id,
        args.step_id.as_deref(),
        args.task_id.as_deref(),
        session_action_id.as_deref(),
    ).await?;
    
    let output_paths = downloader.get_output_paths_by_root();
    
    if output_paths.paths_by_root.is_empty() {
        println!("No output files available for download.");
        return Ok(());
    }
    
    // Check for OS mismatch and prompt for new roots
    let host_format = PathFormat::get_host_format();
    for (root, format) in &output_paths.root_path_formats {
        if *format != host_format {
            println!("Root path {} was created on {:?}, but you're on {:?}.", 
                     root, format, host_format);
            let new_root: String = prompt("Enter new root path")?;
            downloader.set_root_path(root, &new_root)?;
        }
    }
    
    // Show summary and confirm
    let output_paths = downloader.get_output_paths_by_root();
    println!("\nFiles to download:");
    for (root, paths) in &output_paths.paths_by_root {
        println!("  {} ({} files)", root, paths.len());
    }
    
    if !args.yes && !confirm("Proceed with download?")? {
        println!("Download cancelled.");
        return Ok(());
    }
    
    // Check for conflicts
    let conflict_resolution = parse_conflict_resolution(&args.conflict_resolution)?;
    
    // Download with progress
    let progress = ProgressBarCallback::new("Downloading Outputs");
    let result = downloader.download_job_output(
        conflict_resolution,
        Some(&progress),
    ).await?;
    
    println!("\nDownload Summary:");
    println!("  Files: {}", result.stats.files_downloaded);
    println!("  Bytes: {}", human_readable_size(result.stats.bytes_downloaded));
    println!("  Time: {:.2}s", result.stats.duration_secs);
    
    Ok(())
}
```

## Key Design Decisions

### Manifest Merging Strategy

Output manifests are merged chronologically by S3 `LastModified` timestamp:

```rust
// Sort by timestamp (oldest first), then merge
// Later entries overwrite earlier ones with same path
let merged = merge_manifests_chronologically(&mut manifests)?;
```

This ensures that if a file is written multiple times during job execution, the latest version is downloaded.

### Root Path Collision Handling

When user changes a root to an existing root, paths are prefixed to avoid collisions:

```rust
// Original: /mnt/projects/job1/output.exr
// New root: /mnt/shared (already has output.exr)
// Result: /mnt/shared/_mnt_projects_job1_output.exr
```

### Session Action Filtering

For task-level downloads with `session_action_id`, manifests are filtered to only include outputs from that specific session action:

```rust
// S3 path format: .../step_id/task_id/timestamp_sessionaction-xxx-N/hash_output
// Filter by session action ID in path
```

### Windows Long Path Handling

Paths exceeding 260 characters are automatically prefixed with `\\?\`:

```rust
use rusty_attachments_common::to_long_path;

let local_path = to_long_path(&resolved.local_path);
```

## Related Documents

- [storage-design.md](../storage-design.md) - `DownloadOrchestrator`
- [manifest-storage.md](../manifest-storage.md) - Manifest discovery and download
- [manifest-utils.md](../manifest-utils.md) - `merge_manifests_chronologically()`
- [path-mapping.md](../path-mapping.md) - Path transformation utilities
- [file_system.md](../file_system.md) - `set_file_permissions()`
