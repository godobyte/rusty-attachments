//! Bundle submit orchestration for job attachments.

use std::path::PathBuf;

use rusty_attachments_common::ProgressCallback as CommonProgressCallback;
use rusty_attachments_filesystem::{
    expand_input_paths, ExpandedInputPaths, FileSystemScanner, GlobFilter, ScanProgress,
    SnapshotOptions,
};
use rusty_attachments_model::ManifestVersion;
use rusty_attachments_profiles::{
    group_asset_paths_validated, PathGroupingResult, PathValidationMode, StorageProfile,
};
use rusty_attachments_storage::{
    upload_input_manifest, ManifestLocation, ManifestUploadResult, ProgressCallback, S3Location,
    StorageClient, TransferStatistics, UploadOrchestrator,
};

use crate::conversion::{build_attachments, build_manifest_properties};
use crate::error::BundleSubmitError;
use crate::types::{AssetReferences, AssetRootManifest, Attachments, ManifestProperties};

/// Options for bundle submit operation.
#[derive(Debug, Clone)]
pub struct BundleSubmitOptions {
    /// If true, return error if any input files are missing.
    /// If false, missing files are treated as references.
    pub require_paths_exist: bool,
    /// File system mode: "COPIED" or "VIRTUAL".
    pub file_system_mode: String,
    /// Directory for hash cache database.
    pub cache_dir: PathBuf,
    /// Optional glob filter for input files.
    pub glob_filter: Option<GlobFilter>,
    /// Manifest version to create.
    pub manifest_version: ManifestVersion,
}

impl Default for BundleSubmitOptions {
    fn default() -> Self {
        Self {
            require_paths_exist: false,
            file_system_mode: "COPIED".into(),
            cache_dir: default_cache_dir(),
            glob_filter: None,
            manifest_version: ManifestVersion::V2025_12_04_beta,
        }
    }
}

/// Get the default cache directory for hash caching.
///
/// # Returns
/// Platform-appropriate cache directory path.
fn default_cache_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:\\ProgramData"))
            .join("deadline")
            .join("cache")
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join(".deadline")
            .join("cache")
    }
}

/// Result of bundle submit operation.
#[derive(Debug, Clone)]
pub struct BundleSubmitResult {
    /// Attachments payload for CreateJob API.
    pub attachments: Attachments,
    /// Statistics from hashing phase.
    pub hashing_stats: SummaryStatistics,
    /// Statistics from upload phase.
    pub upload_stats: SummaryStatistics,
}

/// Summary statistics for an operation phase.
#[derive(Debug, Clone, Default)]
pub struct SummaryStatistics {
    /// Number of files processed.
    pub processed_files: u64,
    /// Total bytes processed.
    pub processed_bytes: u64,
    /// Number of files transferred.
    pub files_transferred: u64,
    /// Total bytes transferred.
    pub bytes_transferred: u64,
    /// Number of files skipped.
    pub files_skipped: u64,
    /// Total bytes skipped.
    pub bytes_skipped: u64,
}

impl From<TransferStatistics> for SummaryStatistics {
    fn from(stats: TransferStatistics) -> Self {
        Self {
            processed_files: stats.files_processed,
            processed_bytes: stats.bytes_transferred + stats.bytes_skipped,
            files_transferred: stats.files_transferred,
            bytes_transferred: stats.bytes_transferred,
            files_skipped: stats.files_skipped,
            bytes_skipped: stats.bytes_skipped,
        }
    }
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
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `s3_location` - S3 bucket and prefix configuration
/// * `manifest_location` - Manifest storage location
/// * `asset_references` - Input files, output dirs, and references
/// * `storage_profile` - Optional storage profile for path classification
/// * `options` - Bundle submit options
/// * `hashing_progress` - Optional progress callback for hashing phase
/// * `upload_progress` - Optional progress callback for upload phase
///
/// # Returns
/// BundleSubmitResult containing Attachments and statistics.
///
/// # Errors
/// Returns `BundleSubmitError` if validation fails or operations error.
pub async fn submit_bundle_attachments<C: StorageClient>(
    client: &C,
    s3_location: &S3Location,
    manifest_location: &ManifestLocation,
    asset_references: &AssetReferences,
    storage_profile: Option<&StorageProfile>,
    options: &BundleSubmitOptions,
    hashing_progress: Option<&dyn CommonProgressCallback<ScanProgress>>,
    upload_progress: Option<&dyn ProgressCallback>,
) -> Result<BundleSubmitResult, BundleSubmitError> {
    // 1. Expand input directories to individual files
    let expanded: ExpandedInputPaths = expand_input_paths(
        &asset_references.input_filenames,
        options.glob_filter.as_ref(),
        !options.require_paths_exist,
    )?;

    // 2. Group paths by asset root, respecting storage profile
    let validation_mode: PathValidationMode = PathValidationMode {
        require_paths_exist: options.require_paths_exist,
    };

    let grouping_result: PathGroupingResult = group_asset_paths_validated(
        &expanded.files,
        &asset_references.output_directories,
        &asset_references.referenced_paths,
        storage_profile,
        validation_mode,
    )?;

    // 3. Process each asset root group
    let mut manifest_properties: Vec<ManifestProperties> = Vec::new();
    let mut total_hashing_stats: SummaryStatistics = SummaryStatistics::default();
    let mut total_upload_stats: SummaryStatistics = SummaryStatistics::default();

    let scanner: FileSystemScanner = FileSystemScanner::new();
    let upload_orchestrator: UploadOrchestrator<'_, C> =
        UploadOrchestrator::new(client, s3_location.clone());

    for group in grouping_result.groups {
        // Skip groups with no inputs (output-only groups)
        if group.inputs.is_empty() {
            let asset_root_manifest: AssetRootManifest = AssetRootManifest {
                root_path: group.root_path.clone(),
                asset_manifest: None,
                outputs: group.outputs.into_iter().collect(),
                file_system_location_name: group.file_system_location_name.clone(),
            };

            let props: ManifestProperties =
                build_manifest_properties(&asset_root_manifest, None, None);
            manifest_properties.push(props);
            continue;
        }

        // 4. Create manifest by hashing files
        let input_files: Vec<PathBuf> = group.inputs.into_iter().collect();
        let root_path: PathBuf = PathBuf::from(&group.root_path);

        let snapshot_options: SnapshotOptions = SnapshotOptions {
            root: root_path.clone(),
            input_files: Some(input_files),
            version: options.manifest_version,
            filter: options.glob_filter.clone().unwrap_or_default(),
            ..Default::default()
        };

        let manifest = scanner.snapshot(&snapshot_options, hashing_progress)?;

        // Update hashing stats
        total_hashing_stats.processed_files += manifest.file_count() as u64;
        total_hashing_stats.processed_bytes += manifest.total_size();

        // 5. Upload CAS objects
        let upload_stats: TransferStatistics = upload_orchestrator
            .upload_manifest_contents(&manifest, &root_path.to_string_lossy(), upload_progress)
            .await?;

        let phase_upload_stats: SummaryStatistics = SummaryStatistics::from(upload_stats);
        total_upload_stats.processed_files += phase_upload_stats.processed_files;
        total_upload_stats.processed_bytes += phase_upload_stats.processed_bytes;
        total_upload_stats.files_transferred += phase_upload_stats.files_transferred;
        total_upload_stats.bytes_transferred += phase_upload_stats.bytes_transferred;
        total_upload_stats.files_skipped += phase_upload_stats.files_skipped;
        total_upload_stats.bytes_skipped += phase_upload_stats.bytes_skipped;

        // 6. Upload manifest
        let manifest_result: ManifestUploadResult = upload_input_manifest(
            client,
            manifest_location,
            &manifest,
            &group.root_path,
            group.file_system_location_name.as_deref(),
        )
        .await?;

        // 7. Build manifest properties
        let asset_root_manifest: AssetRootManifest = AssetRootManifest {
            root_path: group.root_path.clone(),
            asset_manifest: Some(manifest),
            outputs: group.outputs.into_iter().collect(),
            file_system_location_name: group.file_system_location_name.clone(),
        };

        let props: ManifestProperties = build_manifest_properties(
            &asset_root_manifest,
            Some(&extract_partial_key(&manifest_result.s3_key, manifest_location)),
            Some(&manifest_result.manifest_hash),
        );
        manifest_properties.push(props);
    }

    // 8. Build final attachments payload
    let attachments: Attachments =
        build_attachments(manifest_properties, &options.file_system_mode);

    Ok(BundleSubmitResult {
        attachments,
        hashing_stats: total_hashing_stats,
        upload_stats: total_upload_stats,
    })
}

/// Extract the partial manifest key from a full S3 key.
///
/// The partial key excludes the root prefix and "Manifests/" prefix.
/// Example: "DeadlineCloud/Manifests/farm-123/queue-456/Inputs/guid/hash_input"
///       -> "farm-123/queue-456/Inputs/guid/hash_input"
///
/// # Arguments
/// * `full_key` - Full S3 key
/// * `location` - Manifest location for prefix extraction
///
/// # Returns
/// Partial key suitable for ManifestProperties.
fn extract_partial_key(full_key: &str, location: &ManifestLocation) -> String {
    let prefix: String = location.manifest_prefix();
    if let Some(stripped) = full_key.strip_prefix(&prefix) {
        stripped.trim_start_matches('/').to_string()
    } else {
        full_key.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_options() {
        let options: BundleSubmitOptions = BundleSubmitOptions::default();
        assert!(!options.require_paths_exist);
        assert_eq!(options.file_system_mode, "COPIED");
        assert!(options.glob_filter.is_none());
        assert_eq!(options.manifest_version, ManifestVersion::V2025_12_04_beta);
    }

    #[test]
    fn test_summary_statistics_from_transfer() {
        let transfer_stats: TransferStatistics = TransferStatistics {
            files_processed: 10,
            files_transferred: 7,
            files_skipped: 3,
            bytes_transferred: 1000,
            bytes_skipped: 500,
            errors: vec![],
        };

        let summary: SummaryStatistics = SummaryStatistics::from(transfer_stats);
        assert_eq!(summary.processed_files, 10);
        assert_eq!(summary.processed_bytes, 1500);
        assert_eq!(summary.files_transferred, 7);
        assert_eq!(summary.bytes_transferred, 1000);
        assert_eq!(summary.files_skipped, 3);
        assert_eq!(summary.bytes_skipped, 500);
    }

    #[test]
    fn test_default_cache_dir() {
        let cache_dir: PathBuf = default_cache_dir();
        #[cfg(target_os = "windows")]
        assert!(cache_dir.to_string_lossy().contains("deadline"));
        #[cfg(not(target_os = "windows"))]
        assert!(cache_dir.to_string_lossy().contains(".deadline"));
    }
}
