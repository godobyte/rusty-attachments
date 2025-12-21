# Example: Bundle Submit Use Case

This example demonstrates how to use rusty-attachments primitives to implement job bundle submission with job attachments, equivalent to the Python `deadline bundle submit` CLI command.

## Overview

Bundle submit uploads input files to S3 CAS, creates manifests, and returns an `Attachments` object for the CreateJob API. Key challenges:
- Grouping files by asset root considering storage profiles
- Filtering out SHARED location paths (no upload needed)
- Efficient hashing with local cache
- Deduplication via S3 check cache

## Primitives Used

| Primitive | Module | Purpose |
|-----------|--------|---------|
| `expand_input_paths()` | `file_system` | Expand directories to individual files |
| `group_asset_paths_validated()` | `storage-profiles` | Group paths by asset root with validation |
| `StorageProfile` | `storage-profiles` | LOCAL/SHARED location classification |
| `FileSystemScanner.snapshot()` | `file_system` | Create manifest from directory |
| `HashCache` | `hash-cache` | Avoid re-hashing unchanged files |
| `S3CheckCache` | `hash-cache` | Avoid redundant S3 HEAD requests |
| `UploadOrchestrator.upload_manifest()` | `storage-design` | Upload CAS objects |
| `upload_input_manifest()` | `manifest-storage` | Upload manifest with metadata |
| `build_manifest_properties()` | `job-submission` | Create ManifestProperties |
| `build_attachments()` | `job-submission` | Create Attachments for API |

## Complete Example

```rust
use std::path::{Path, PathBuf};
use std::collections::HashSet;

use rusty_attachments_common::ProgressCallback;
use rusty_attachments_model::Manifest;
use rusty_attachments_storage::{
    // Storage profiles
    StorageProfile, FileSystemLocation, FileSystemLocationType,
    group_asset_paths_validated, PathValidationMode, PathGroupingResult,
    // File system
    FileSystemScanner, SnapshotOptions, expand_input_paths, GlobFilter,
    // Caches
    HashCache, SqliteHashCache, S3CheckCache, SqliteS3CheckCache,
    // Upload
    UploadOrchestrator, S3Location, StorageClient,
    // Manifest storage
    ManifestLocation, upload_input_manifest,
    // Job submission
    AssetRootManifest, ManifestProperties, Attachments,
    build_manifest_properties, build_attachments,
    // Progress
    TransferProgress,
};

/// Asset references parsed from job bundle's asset_references.yaml/json
pub struct AssetReferences {
    pub input_filenames: Vec<PathBuf>,
    pub output_directories: Vec<PathBuf>,
    pub referenced_paths: Vec<PathBuf>,
}

/// Result of bundle submit operation
pub struct BundleSubmitResult {
    pub attachments: Attachments,
    pub hashing_stats: SummaryStatistics,
    pub upload_stats: SummaryStatistics,
}

/// Submit a job bundle with attachments.
///
/// This function:
/// 1. Expands input directories to individual files
/// 2. Groups input/output paths by asset root
/// 3. Filters paths based on storage profile (skip SHARED)
/// 4. Hashes files and creates manifests
/// 5. Uploads CAS objects and manifests to S3
/// 6. Returns Attachments for CreateJob API
pub async fn submit_bundle_attachments<C: StorageClient>(
    client: &C,
    s3_location: &S3Location,
    manifest_location: &ManifestLocation,
    asset_references: &AssetReferences,
    storage_profile: Option<&StorageProfile>,
    require_paths_exist: bool,
    expected_bucket_owner: Option<&str>,
    cache_dir: &Path,
    hashing_progress: Option<&dyn ProgressCallback<ScanProgress>>,
    upload_progress: Option<&dyn ProgressCallback<TransferProgress>>,
) -> Result<BundleSubmitResult, BundleSubmitError> {
    // 1. Expand any directories in input_filenames to individual files
    let expanded = expand_input_paths(
        &asset_references.input_filenames,
        None, // No glob filter - include all files
        !require_paths_exist, // allow_missing = !require_paths_exist
    )?;
    
    // Log expanded directories
    for dir in &expanded.expanded_directories {
        log::debug!("Expanded directory: {} ({} files)", dir.display(), expanded.files.len());
    }
    
    // 2. Group paths by asset root, respecting storage profile
    let grouping_result = group_asset_paths_validated(
        &expanded.files, // Use expanded file list
        &asset_references.output_directories,
        &asset_references.referenced_paths,
        storage_profile,
        PathValidationMode { require_paths_exist },
    )?;
    
    // Log any paths that were demoted to references (when require_paths_exist=false)
    for demoted in &grouping_result.demoted_to_references {
        log::warn!("Input path does not exist, treating as reference: {}", demoted.display());
    }
    
    // Also log missing paths from expansion
    for missing in &expanded.missing {
        log::warn!("Input path does not exist: {}", missing.display());
    }
    
    // 3. Hash files and create manifests for each asset root
    let (hashing_stats, asset_root_manifests) = hash_and_create_manifests(
        &grouping_result.groups,
        cache_dir,
        hashing_progress,
    )?;
    
    // 4. Upload to S3
    let (upload_stats, attachments) = upload_attachments(
        client,
        s3_location,
        manifest_location,
        &asset_root_manifests,
        expected_bucket_owner,
        cache_dir,
        upload_progress,
    ).await?;
    
    Ok(BundleSubmitResult {
        attachments,
        hashing_stats,
        upload_stats,
    })
}

/// Hash files and create manifests for each asset root group.
fn hash_and_create_manifests(
    groups: &[AssetRootGroup],
    cache_dir: &Path,
    progress: Option<&dyn ProgressCallback<ScanProgress>>,
) -> Result<(SummaryStatistics, Vec<AssetRootManifest>), BundleSubmitError> {
    let scanner = FileSystemScanner::new();
    let hash_cache_path = cache_dir.join("hash_cache.db");
    let sqlite_cache = SqliteHashCache::open(&hash_cache_path, get_machine_id())?;
    let hash_cache = HashCache::with_default_ttl(sqlite_cache);
    
    let mut asset_root_manifests = Vec::new();
    let mut total_stats = SummaryStatistics::default();
    
    for group in groups {
        let manifest = if !group.inputs.is_empty() {
            // Create snapshot manifest for this asset root
            let options = SnapshotOptions {
                root: PathBuf::from(&group.root_path),
                hash_cache: Some(hash_cache_path.clone()),
                ..Default::default()
            };
            
            let manifest = scanner.snapshot(&options, progress)?;
            total_stats.merge(&scanner.last_stats());
            Some(manifest)
        } else {
            None
        };
        
        asset_root_manifests.push(AssetRootManifest {
            root_path: group.root_path.clone(),
            asset_manifest: manifest,
            outputs: group.outputs.iter().cloned().collect(),
            file_system_location_name: group.file_system_location_name.clone(),
        });
    }
    
    Ok((total_stats, asset_root_manifests))
}

/// Upload manifests and CAS objects to S3.
async fn upload_attachments<C: StorageClient>(
    client: &C,
    s3_location: &S3Location,
    manifest_location: &ManifestLocation,
    asset_root_manifests: &[AssetRootManifest],
    expected_bucket_owner: Option<&str>,
    cache_dir: &Path,
    progress: Option<&dyn ProgressCallback<TransferProgress>>,
) -> Result<(SummaryStatistics, Attachments), BundleSubmitError> {
    let orchestrator = UploadOrchestrator::new(client.clone(), s3_location.clone())
        .with_expected_bucket_owner(expected_bucket_owner);
    
    // Initialize S3 check cache
    let s3_cache_path = cache_dir.join("s3_check_cache.db");
    let s3_cache = S3CheckCache::new(SqliteS3CheckCache::open(&s3_cache_path)?);
    
    let mut manifest_properties_list = Vec::new();
    let mut total_stats = SummaryStatistics::default();
    
    for arm in asset_root_manifests {
        // Build base manifest properties (handles output directories)
        let mut props = build_manifest_properties(arm, None, None);
        
        if let Some(manifest) = &arm.asset_manifest {
            // Verify S3 check cache integrity before upload
            // If any cached entries are missing from S3, reset the cache
            if !s3_cache.verify_integrity(
                client,
                manifest,
                &s3_location.bucket,
                &s3_location.cas_prefix,
                30, // sample size
            ).await {
                log::info!("S3 check cache integrity failed, resetting cache");
                s3_cache.reset().await;
            }
            
            // Upload CAS objects (files)
            let upload_stats = orchestrator.upload_manifest(
                manifest,
                &arm.root_path,
                progress,
            ).await?;
            total_stats.merge(&upload_stats);
            
            // Upload manifest file with metadata
            let result = upload_input_manifest(
                client,
                manifest_location,
                manifest,
                &arm.root_path,
                arm.file_system_location_name.as_deref(),
            ).await?;
            
            props.input_manifest_path = Some(result.s3_key);
            props.input_manifest_hash = Some(result.manifest_hash);
        }
        
        manifest_properties_list.push(props);
    }
    
    // Build final attachments object
    let attachments = build_attachments(manifest_properties_list, "COPIED");
    
    Ok((total_stats, attachments))
}

/// Get a unique machine identifier for hash cache partitioning.
fn get_machine_id() -> String {
    // Implementation would use platform-specific machine ID
    // e.g., /etc/machine-id on Linux, IOPlatformUUID on macOS
    "machine-id-placeholder".to_string()
}
```

## Usage in CLI Context

```rust
use clap::Parser;

#[derive(Parser)]
struct BundleSubmitArgs {
    /// Path to job bundle directory
    job_bundle_dir: PathBuf,
    
    /// Storage profile ID to use
    #[arg(long)]
    storage_profile_id: Option<String>,
    
    /// Return error if any input files are missing
    #[arg(long)]
    require_paths_exist: bool,
    
    /// File system mode: COPIED or VIRTUAL
    #[arg(long, default_value = "COPIED")]
    job_attachments_file_system: String,
}

async fn cli_bundle_submit(args: BundleSubmitArgs) -> Result<(), Error> {
    // 1. Parse job bundle
    let asset_references = parse_asset_references(&args.job_bundle_dir)?;
    
    // 2. Get storage profile from API if configured
    let storage_profile = if let Some(profile_id) = &args.storage_profile_id {
        Some(get_storage_profile_for_queue(farm_id, queue_id, profile_id).await?)
    } else {
        None
    };
    
    // 3. Set up progress bars
    let hash_progress = ProgressBarCallback::new("Hashing Attachments");
    let upload_progress = ProgressBarCallback::new("Uploading Attachments");
    
    // 4. Submit attachments
    let result = submit_bundle_attachments(
        &client,
        &s3_location,
        &manifest_location,
        &asset_references,
        storage_profile.as_ref(),
        args.require_paths_exist,
        &cache_dir,
        Some(&hash_progress),
        Some(&upload_progress),
    ).await?;
    
    // 5. Print summaries
    println!("Hashing Summary:\n{}", result.hashing_stats);
    println!("Upload Summary:\n{}", result.upload_stats);
    
    // 6. Build CreateJob args with attachments
    let mut create_job_args = build_create_job_args(&args.job_bundle_dir)?;
    create_job_args.attachments = Some(result.attachments);
    create_job_args.attachments.as_mut().unwrap().file_system = 
        args.job_attachments_file_system.clone();
    
    // 7. Call CreateJob API
    let job_id = deadline_client.create_job(create_job_args).await?;
    println!("Job ID: {}", job_id);
    
    Ok(())
}
```

## Key Design Decisions

### Storage Profile Filtering

Files under SHARED locations are automatically excluded from upload:

```rust
// In group_asset_paths_validated():
// - Paths under SHARED locations → skipped entirely
// - Paths under LOCAL locations → grouped by that location
// - Other paths → grouped by top-level directory
```

### Hash Cache Strategy

The hash cache uses `(path, size, mtime)` as the key, which is more robust than just `(path, hash_algorithm)`:

```rust
// Cache hit: file unchanged since last hash
// Cache miss: file is new or modified, needs rehashing
let hash = match hash_cache.get(&path, size, mtime).await {
    Some(h) => h,
    None => {
        let h = hash_file(&path, HashAlgorithm::Xxh128)?;
        hash_cache.put(&path, size, mtime, h.clone()).await;
        h
    }
};
```

### S3 Check Cache Integrity

Before uploading, we verify a sample of cached entries actually exist in S3:

```rust
// Sample 30 cached entries and HEAD each one
// If any are missing, reset the entire cache
// This handles cases where S3 objects were deleted externally
if !s3_cache.verify_integrity(...).await {
    s3_cache.reset().await;
}
```

### Small vs Large File Upload Strategy

Files are separated by size for optimal upload performance:

```rust
// Small files (<80MB): parallel object uploads
// Large files (>80MB): serial processing with parallel multipart
// This minimizes wasted bandwidth on cancellation
```

## Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum BundleSubmitError {
    #[error("Missing input files:\n{}", .missing.join("\n"))]
    MissingInputFiles { missing: Vec<String> },
    
    #[error("Directories specified as input files:\n{}", .directories.join("\n"))]
    DirectoriesAsFiles { directories: Vec<String> },
    
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    
    #[error("File system error: {0}")]
    FileSystem(#[from] FileSystemError),
    
    #[error("Operation cancelled")]
    Cancelled,
}
```

## Related Documents

- [storage-profiles.md](../storage-profiles.md) - `group_asset_paths_validated()`, storage profile types
- [file_system.md](../file_system.md) - `FileSystemScanner.snapshot()`
- [hash-cache.md](../hash-cache.md) - `HashCache`, `S3CheckCache`
- [storage-design.md](../storage-design.md) - `UploadOrchestrator`
- [manifest-storage.md](../manifest-storage.md) - `upload_input_manifest()`
- [job-submission.md](../job-submission.md) - `build_manifest_properties()`, `build_attachments()`
