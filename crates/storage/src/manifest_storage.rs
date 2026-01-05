//! Manifest storage operations for S3.
//!
//! This module provides functions for uploading and downloading manifest files
//! to/from S3. Manifests are stored with specific naming conventions, metadata,
//! and content types.
//!
//! # S3 Key Structure
//!
//! ## Input Manifests
//! ```text
//! s3://{bucket}/{root_prefix}/Manifests/{farm_id}/{queue_id}/Inputs/{guid}/{manifest_hash}_input
//! ```
//!
//! ## Output Manifests (Task-Level)
//! ```text
//! s3://{bucket}/{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/{timestamp}_{session_action_id}/{manifest_hash}_output
//! ```
//!
//! ## Output Manifests (Step-Level)
//! ```text
//! s3://{bucket}/{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{timestamp}_{session_action_id}/{manifest_hash}_output
//! ```

use std::collections::HashMap;

use regex::Regex;
use rusty_attachments_common::hash_bytes;
use rusty_attachments_model::Manifest;

use crate::error::StorageError;
use crate::traits::{ObjectInfo, ObjectMetadata, StorageClient};
use crate::types::S3Location;

// ============================================================================
// Constants
// ============================================================================

/// Content type for v2023-03-03 snapshot manifests.
pub const CONTENT_TYPE_V2023_03_03: &str = "application/x-deadline-manifest-2023-03-03";

/// Content type for v2025-12-04-beta snapshot manifests.
pub const CONTENT_TYPE_V2025_12_04_BETA: &str = "application/x-deadline-manifest-2025-12-04-beta";

/// Content type for v2025-12-04-beta diff manifests.
pub const CONTENT_TYPE_V2025_12_04_BETA_DIFF: &str =
    "application/x-deadline-manifest-diff-2025-12-04-beta";

/// S3 metadata key for asset root (ASCII paths).
pub const METADATA_KEY_ASSET_ROOT: &str = "asset-root";

/// S3 metadata key for asset root (JSON-encoded, for non-ASCII paths).
pub const METADATA_KEY_ASSET_ROOT_JSON: &str = "asset-root-json";

/// S3 metadata key for file system location name.
pub const METADATA_KEY_FILE_SYSTEM_LOCATION_NAME: &str = "file-system-location-name";

// ============================================================================
// Data Structures
// ============================================================================

/// Location for storing/retrieving manifests.
#[derive(Debug, Clone)]
pub struct ManifestLocation {
    /// S3 bucket name.
    pub bucket: String,
    /// Root prefix for all operations (e.g., "DeadlineCloud").
    pub root_prefix: String,
    /// Farm ID.
    pub farm_id: String,
    /// Queue ID.
    pub queue_id: String,
}

impl ManifestLocation {
    /// Create a new manifest location.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `root_prefix` - Root prefix for all operations
    /// * `farm_id` - Farm ID
    /// * `queue_id` - Queue ID
    pub fn new(
        bucket: impl Into<String>,
        root_prefix: impl Into<String>,
        farm_id: impl Into<String>,
        queue_id: impl Into<String>,
    ) -> Self {
        Self {
            bucket: bucket.into(),
            root_prefix: root_prefix.into(),
            farm_id: farm_id.into(),
            queue_id: queue_id.into(),
        }
    }

    /// Create from an S3Location and farm/queue IDs.
    pub fn from_s3_location(location: &S3Location, farm_id: &str, queue_id: &str) -> Self {
        Self {
            bucket: location.bucket.clone(),
            root_prefix: location.root_prefix.clone(),
            farm_id: farm_id.to_string(),
            queue_id: queue_id.to_string(),
        }
    }

    /// Build the full manifest prefix.
    ///
    /// Returns: `{root_prefix}/Manifests`
    pub fn manifest_prefix(&self) -> String {
        if self.root_prefix.is_empty() {
            "Manifests".to_string()
        } else {
            format!("{}/Manifests", self.root_prefix)
        }
    }

    /// Build the input manifest prefix for this farm/queue.
    ///
    /// Returns: `{root_prefix}/Manifests/{farm_id}/{queue_id}/Inputs`
    pub fn input_manifest_prefix(&self) -> String {
        format!(
            "{}/{}/{}/Inputs",
            self.manifest_prefix(),
            self.farm_id,
            self.queue_id
        )
    }
}

/// Identifiers for output manifest path (task-level).
#[derive(Debug, Clone)]
pub struct TaskOutputManifestPath {
    /// Job ID.
    pub job_id: String,
    /// Step ID.
    pub step_id: String,
    /// Task ID.
    pub task_id: String,
    /// Session action ID.
    pub session_action_id: String,
    /// Timestamp as seconds since epoch (converted to ISO 8601).
    pub timestamp: f64,
}

/// Identifiers for output manifest path (step-level, no task).
#[derive(Debug, Clone)]
pub struct StepOutputManifestPath {
    /// Job ID.
    pub job_id: String,
    /// Step ID.
    pub step_id: String,
    /// Session action ID.
    pub session_action_id: String,
    /// Timestamp as seconds since epoch (converted to ISO 8601).
    pub timestamp: f64,
}

/// S3 metadata attached to manifest uploads.
#[derive(Debug, Clone, Default)]
pub struct ManifestS3Metadata {
    /// Root path the manifest was created from.
    pub asset_root: String,
    /// Optional file system location name (for storage profile mapping).
    pub file_system_location_name: Option<String>,
}

impl ManifestS3Metadata {
    /// Create new metadata with just an asset root.
    ///
    /// # Arguments
    /// * `asset_root` - The root path the manifest was created from
    pub fn new(asset_root: impl Into<String>) -> Self {
        Self {
            asset_root: asset_root.into(),
            file_system_location_name: None,
        }
    }

    /// Create new metadata with asset root and file system location.
    ///
    /// # Arguments
    /// * `asset_root` - The root path the manifest was created from
    /// * `file_system_location_name` - File system location name for storage profile mapping
    pub fn with_location(
        asset_root: impl Into<String>,
        file_system_location_name: impl Into<String>,
    ) -> Self {
        Self {
            asset_root: asset_root.into(),
            file_system_location_name: Some(file_system_location_name.into()),
        }
    }

    /// Build S3 metadata headers for upload.
    ///
    /// Handles non-ASCII paths by JSON-encoding them. For backward compatibility,
    /// when the path contains non-ASCII characters, both `asset-root` and
    /// `asset-root-json` fields are populated with the JSON-encoded value.
    pub fn to_s3_metadata(&self) -> HashMap<String, String> {
        let mut metadata: HashMap<String, String> = HashMap::new();

        // Check if asset_root is ASCII
        if self.asset_root.is_ascii() {
            metadata.insert(METADATA_KEY_ASSET_ROOT.to_string(), self.asset_root.clone());
        } else {
            // For non-ASCII paths, JSON-encode and populate both fields for backward compatibility
            let json_encoded: String =
                serde_json::to_string(&self.asset_root).unwrap_or_else(|_| self.asset_root.clone());
            metadata.insert(METADATA_KEY_ASSET_ROOT.to_string(), json_encoded.clone());
            metadata.insert(METADATA_KEY_ASSET_ROOT_JSON.to_string(), json_encoded);
        }

        if let Some(ref location) = self.file_system_location_name {
            metadata.insert(
                METADATA_KEY_FILE_SYSTEM_LOCATION_NAME.to_string(),
                location.clone(),
            );
        }

        metadata
    }

    /// Parse S3 metadata from download response.
    ///
    /// Prefers `asset-root-json` for non-ASCII paths, falls back to `asset-root`.
    ///
    /// # Arguments
    /// * `metadata` - S3 metadata from HEAD or GET response
    ///
    /// # Returns
    /// Parsed metadata, or None if required fields are missing.
    pub fn from_s3_metadata(metadata: &HashMap<String, String>) -> Option<Self> {
        // Prefer asset-root-json for non-ASCII paths
        let asset_root: String = metadata
            .get(METADATA_KEY_ASSET_ROOT_JSON)
            .and_then(|v| serde_json::from_str(v).ok())
            .or_else(|| metadata.get(METADATA_KEY_ASSET_ROOT).cloned())?;

        let file_system_location_name: Option<String> =
            metadata.get(METADATA_KEY_FILE_SYSTEM_LOCATION_NAME).cloned();

        Some(Self {
            asset_root,
            file_system_location_name,
        })
    }
}

/// Metadata returned when downloading a manifest.
#[derive(Debug, Clone)]
pub struct ManifestDownloadMetadata {
    /// Root path the manifest was created from.
    pub asset_root: String,
    /// Optional file system location name.
    pub file_system_location_name: Option<String>,
    /// Content type from S3.
    pub content_type: Option<String>,
    /// S3 LastModified timestamp (seconds since epoch).
    /// Used for chronological manifest merging.
    pub last_modified: Option<i64>,
}

/// Result of uploading a manifest.
#[derive(Debug, Clone)]
pub struct ManifestUploadResult {
    /// Full S3 key where the manifest was uploaded.
    pub s3_key: String,
    /// Hash of the manifest content.
    pub manifest_hash: String,
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Convert a float timestamp (seconds since epoch) to ISO 8601 format.
///
/// Format: `YYYY-MM-DDTHH:MM:SS.ffffffZ`
/// Example: `2025-05-22T22:17:03.409012Z`
///
/// # Arguments
/// * `timestamp` - Seconds since Unix epoch as f64
///
/// # Returns
/// ISO 8601 formatted string with microsecond precision.
pub fn float_to_iso_datetime_string(timestamp: f64) -> String {
    let seconds: i64 = timestamp.trunc() as i64;
    let microseconds: u32 = ((timestamp.fract()) * 1_000_000.0).abs() as u32;

    // Create datetime from timestamp
    let dt: chrono::DateTime<chrono::Utc> =
        chrono::DateTime::from_timestamp(seconds, microseconds * 1000)
            .unwrap_or(chrono::DateTime::UNIX_EPOCH);

    dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

/// Generate a random GUID (32 hex characters, no dashes).
///
/// Used for input manifest path generation.
pub fn generate_random_guid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Compute the hash used for manifest naming.
///
/// The hash is computed from the source root path (as bytes) using XXH128.
/// This is used to generate unique manifest names based on the asset root.
///
/// # Arguments
/// * `source_root` - The source root path
///
/// # Returns
/// 32-character hex hash string.
pub fn compute_manifest_name_hash(source_root: &str) -> String {
    hash_bytes(source_root.as_bytes())
}

/// Compute the hash used to match manifest S3 keys to job attachment roots.
///
/// The hash is computed from the concatenation of the file system location name
/// (if any) and the root path. This hash appears in the manifest S3 key filename.
///
/// # Arguments
/// * `file_system_location_name` - Optional file system location name from storage profile
/// * `root_path` - The root path of the job attachment
///
/// # Returns
/// The xxh128 hash as a hex string.
pub fn compute_root_path_hash(
    file_system_location_name: Option<&str>,
    root_path: &str,
) -> String {
    let data: String = format!("{}{}", file_system_location_name.unwrap_or(""), root_path);
    hash_bytes(data.as_bytes())
}

/// Get the content type for a manifest.
///
/// # Arguments
/// * `manifest` - The manifest to get content type for
///
/// # Returns
/// Content type string for S3 upload.
pub fn get_manifest_content_type(manifest: &Manifest) -> &'static str {
    match manifest {
        Manifest::V2023_03_03(_) => CONTENT_TYPE_V2023_03_03,
        Manifest::V2025_12_04_beta(m) => {
            if m.parent_manifest_hash.is_some() {
                CONTENT_TYPE_V2025_12_04_BETA_DIFF
            } else {
                CONTENT_TYPE_V2025_12_04_BETA
            }
        }
    }
}

/// Build the partial input manifest prefix (without root prefix).
///
/// Returns: `{farm_id}/{queue_id}/Inputs/{guid}`
///
/// # Arguments
/// * `farm_id` - Farm ID
/// * `queue_id` - Queue ID
pub fn build_partial_input_manifest_prefix(farm_id: &str, queue_id: &str) -> String {
    let guid: String = generate_random_guid();
    format!("{}/{}/Inputs/{}", farm_id, queue_id, guid)
}

// ============================================================================
// S3 Key Format Functions
// ============================================================================

/// Format the S3 prefix for job-level output manifests.
///
/// Prefix format: `{manifest_prefix}/{farm_id}/{queue_id}/{job_id}/`
///
/// # Arguments
/// * `manifest_prefix` - The manifest prefix (e.g., "DeadlineCloud/Manifests")
/// * `farm_id` - Farm ID
/// * `queue_id` - Queue ID
/// * `job_id` - Job ID
///
/// # Returns
/// S3 prefix for job-level output manifests.
pub fn format_job_output_prefix(
    manifest_prefix: &str,
    farm_id: &str,
    queue_id: &str,
    job_id: &str,
) -> String {
    format!("{}/{}/{}/{}/", manifest_prefix, farm_id, queue_id, job_id)
}

/// Format the S3 prefix for step-level output manifests.
///
/// Prefix format: `{manifest_prefix}/{farm_id}/{queue_id}/{job_id}/{step_id}/`
///
/// # Arguments
/// * `manifest_prefix` - The manifest prefix (e.g., "DeadlineCloud/Manifests")
/// * `farm_id` - Farm ID
/// * `queue_id` - Queue ID
/// * `job_id` - Job ID
/// * `step_id` - Step ID
///
/// # Returns
/// S3 prefix for step-level output manifests.
pub fn format_step_output_prefix(
    manifest_prefix: &str,
    farm_id: &str,
    queue_id: &str,
    job_id: &str,
    step_id: &str,
) -> String {
    format!(
        "{}/{}/{}/{}/{}/",
        manifest_prefix, farm_id, queue_id, job_id, step_id
    )
}

/// Format the S3 prefix for task-level output manifests.
///
/// Prefix format: `{manifest_prefix}/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/`
///
/// # Arguments
/// * `manifest_prefix` - The manifest prefix (e.g., "DeadlineCloud/Manifests")
/// * `farm_id` - Farm ID
/// * `queue_id` - Queue ID
/// * `job_id` - Job ID
/// * `step_id` - Step ID
/// * `task_id` - Task ID
///
/// # Returns
/// S3 prefix for task-level output manifests.
pub fn format_task_output_prefix(
    manifest_prefix: &str,
    farm_id: &str,
    queue_id: &str,
    job_id: &str,
    step_id: &str,
    task_id: &str,
) -> String {
    format!(
        "{}/{}/{}/{}/{}/{}/",
        manifest_prefix, farm_id, queue_id, job_id, step_id, task_id
    )
}

/// Format the full S3 key for an input manifest.
///
/// Key format: `{manifest_prefix}/{farm_id}/{queue_id}/Inputs/{guid}/{manifest_name}`
///
/// # Arguments
/// * `manifest_prefix` - The manifest prefix (e.g., "DeadlineCloud/Manifests")
/// * `farm_id` - Farm ID
/// * `queue_id` - Queue ID
/// * `guid` - Random GUID for uniqueness
/// * `manifest_name` - Manifest filename (e.g., "{hash}_input")
///
/// # Returns
/// Full S3 key for the input manifest.
pub fn format_input_manifest_s3_key(
    manifest_prefix: &str,
    farm_id: &str,
    queue_id: &str,
    guid: &str,
    manifest_name: &str,
) -> String {
    format!(
        "{}/{}/{}/Inputs/{}/{}",
        manifest_prefix, farm_id, queue_id, guid, manifest_name
    )
}

/// Format the full S3 key for a task-level output manifest.
///
/// Key format: `{manifest_prefix}/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/{timestamp}_{session_action_id}/{manifest_name}`
///
/// # Arguments
/// * `manifest_prefix` - The manifest prefix (e.g., "DeadlineCloud/Manifests")
/// * `farm_id` - Farm ID
/// * `queue_id` - Queue ID
/// * `job_id` - Job ID
/// * `step_id` - Step ID
/// * `task_id` - Task ID
/// * `timestamp_str` - ISO 8601 formatted timestamp
/// * `session_action_id` - Session action ID
/// * `manifest_name` - Manifest filename (e.g., "{hash}_output")
///
/// # Returns
/// Full S3 key for the task-level output manifest.
#[allow(clippy::too_many_arguments)]
pub fn format_task_output_manifest_s3_key(
    manifest_prefix: &str,
    farm_id: &str,
    queue_id: &str,
    job_id: &str,
    step_id: &str,
    task_id: &str,
    timestamp_str: &str,
    session_action_id: &str,
    manifest_name: &str,
) -> String {
    format!(
        "{}{}_{}/{}",
        format_task_output_prefix(manifest_prefix, farm_id, queue_id, job_id, step_id, task_id),
        timestamp_str,
        session_action_id,
        manifest_name
    )
}

/// Format the full S3 key for a step-level output manifest (no task).
///
/// Key format: `{manifest_prefix}/{farm_id}/{queue_id}/{job_id}/{step_id}/{timestamp}_{session_action_id}/{manifest_name}`
///
/// # Arguments
/// * `manifest_prefix` - The manifest prefix (e.g., "DeadlineCloud/Manifests")
/// * `farm_id` - Farm ID
/// * `queue_id` - Queue ID
/// * `job_id` - Job ID
/// * `step_id` - Step ID
/// * `timestamp_str` - ISO 8601 formatted timestamp
/// * `session_action_id` - Session action ID
/// * `manifest_name` - Manifest filename (e.g., "{hash}_output")
///
/// # Returns
/// Full S3 key for the step-level output manifest.
#[allow(clippy::too_many_arguments)]
pub fn format_step_output_manifest_s3_key(
    manifest_prefix: &str,
    farm_id: &str,
    queue_id: &str,
    job_id: &str,
    step_id: &str,
    timestamp_str: &str,
    session_action_id: &str,
    manifest_name: &str,
) -> String {
    format!(
        "{}{}_{}/{}",
        format_step_output_prefix(manifest_prefix, farm_id, queue_id, job_id, step_id),
        timestamp_str,
        session_action_id,
        manifest_name
    )
}

// ============================================================================
// Upload Functions
// ============================================================================

/// Upload an input manifest to S3.
///
/// Computes manifest hash, generates S3 key, sets content-type and metadata.
///
/// Key format: `{root_prefix}/Manifests/{farm_id}/{queue_id}/Inputs/{guid}/{hash}_input`
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `location` - Manifest location configuration
/// * `manifest` - The manifest to upload
/// * `asset_root` - The root path the manifest was created from
/// * `file_system_location_name` - Optional file system location name
///
/// # Returns
/// Upload result with S3 key and manifest hash.
pub async fn upload_input_manifest<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    manifest: &Manifest,
    asset_root: &str,
    file_system_location_name: Option<&str>,
) -> Result<ManifestUploadResult, StorageError> {
    // Encode manifest to JSON
    let manifest_bytes: Vec<u8> = manifest
        .encode()
        .map_err(|e| StorageError::Other {
            message: format!("Failed to encode manifest: {}", e),
        })?
        .into_bytes();

    // Compute manifest hash
    let manifest_hash: String = hash_bytes(&manifest_bytes);

    // Generate manifest name from source root hash
    let name_hash: String = compute_manifest_name_hash(asset_root);
    let manifest_name: String = format!("{}_input", name_hash);

    // Generate GUID for uniqueness
    let guid: String = generate_random_guid();

    // Build full S3 key using format function
    let full_key: String = format_input_manifest_s3_key(
        &location.manifest_prefix(),
        &location.farm_id,
        &location.queue_id,
        &guid,
        &manifest_name,
    );

    // Build metadata
    let s3_metadata: ManifestS3Metadata = match file_system_location_name {
        Some(loc) => ManifestS3Metadata::with_location(asset_root, loc),
        None => ManifestS3Metadata::new(asset_root),
    };
    let metadata: HashMap<String, String> = s3_metadata.to_s3_metadata();

    // Get content type
    let content_type: &str = get_manifest_content_type(manifest);

    // Upload to S3
    client
        .put_object(
            &location.bucket,
            &full_key,
            &manifest_bytes,
            Some(content_type),
            Some(&metadata),
        )
        .await?;

    Ok(ManifestUploadResult {
        s3_key: full_key,
        manifest_hash,
    })
}

/// Upload a task-level output manifest to S3.
///
/// Key format: `{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/{timestamp}_{session_action_id}/{hash}_output`
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `location` - Manifest location configuration
/// * `output_path` - Task output path identifiers
/// * `manifest` - The manifest to upload
/// * `asset_root` - The root path the manifest was created from
/// * `file_system_location_name` - Optional file system location name
///
/// # Returns
/// Upload result with S3 key and manifest hash.
pub async fn upload_task_output_manifest<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    output_path: &TaskOutputManifestPath,
    manifest: &Manifest,
    asset_root: &str,
    file_system_location_name: Option<&str>,
) -> Result<ManifestUploadResult, StorageError> {
    upload_output_manifest_impl(
        client,
        location,
        &output_path.job_id,
        &output_path.step_id,
        Some(&output_path.task_id),
        &output_path.session_action_id,
        output_path.timestamp,
        manifest,
        asset_root,
        file_system_location_name,
    )
    .await
}

/// Upload a step-level output manifest to S3 (no task).
///
/// Key format: `{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{timestamp}_{session_action_id}/{hash}_output`
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `location` - Manifest location configuration
/// * `output_path` - Step output path identifiers
/// * `manifest` - The manifest to upload
/// * `asset_root` - The root path the manifest was created from
/// * `file_system_location_name` - Optional file system location name
///
/// # Returns
/// Upload result with S3 key and manifest hash.
pub async fn upload_step_output_manifest<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    output_path: &StepOutputManifestPath,
    manifest: &Manifest,
    asset_root: &str,
    file_system_location_name: Option<&str>,
) -> Result<ManifestUploadResult, StorageError> {
    upload_output_manifest_impl(
        client,
        location,
        &output_path.job_id,
        &output_path.step_id,
        None,
        &output_path.session_action_id,
        output_path.timestamp,
        manifest,
        asset_root,
        file_system_location_name,
    )
    .await
}

/// Internal implementation for output manifest upload.
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `location` - Manifest location configuration
/// * `job_id` - Job ID
/// * `step_id` - Step ID
/// * `task_id` - Optional task ID (None for step-level manifests)
/// * `session_action_id` - Session action ID
/// * `timestamp` - Timestamp as seconds since epoch
/// * `manifest` - The manifest to upload
/// * `asset_root` - The root path the manifest was created from
/// * `file_system_location_name` - Optional file system location name
#[allow(clippy::too_many_arguments)]
async fn upload_output_manifest_impl<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    job_id: &str,
    step_id: &str,
    task_id: Option<&str>,
    session_action_id: &str,
    timestamp: f64,
    manifest: &Manifest,
    asset_root: &str,
    file_system_location_name: Option<&str>,
) -> Result<ManifestUploadResult, StorageError> {
    // Encode manifest to JSON
    let manifest_bytes: Vec<u8> = manifest
        .encode()
        .map_err(|e| StorageError::Other {
            message: format!("Failed to encode manifest: {}", e),
        })?
        .into_bytes();

    // Compute manifest hash for naming
    let name_hash: String = compute_root_path_hash(file_system_location_name, asset_root);
    let manifest_name: String = format!("{}_output", name_hash);

    // Build timestamp string
    let timestamp_str: String = float_to_iso_datetime_string(timestamp);

    // Build full S3 key using format functions
    let full_key: String = match task_id {
        Some(tid) => format_task_output_manifest_s3_key(
            &location.manifest_prefix(),
            &location.farm_id,
            &location.queue_id,
            job_id,
            step_id,
            tid,
            &timestamp_str,
            session_action_id,
            &manifest_name,
        ),
        None => format_step_output_manifest_s3_key(
            &location.manifest_prefix(),
            &location.farm_id,
            &location.queue_id,
            job_id,
            step_id,
            &timestamp_str,
            session_action_id,
            &manifest_name,
        ),
    };

    // Build metadata
    let s3_metadata: ManifestS3Metadata = match file_system_location_name {
        Some(loc) => ManifestS3Metadata::with_location(asset_root, loc),
        None => ManifestS3Metadata::new(asset_root),
    };
    let metadata: HashMap<String, String> = s3_metadata.to_s3_metadata();

    // Get content type
    let content_type: &str = get_manifest_content_type(manifest);

    // Compute manifest content hash for result
    let manifest_hash: String = hash_bytes(&manifest_bytes);

    // Upload to S3
    client
        .put_object(
            &location.bucket,
            &full_key,
            &manifest_bytes,
            Some(content_type),
            Some(&metadata),
        )
        .await?;

    Ok(ManifestUploadResult {
        s3_key: full_key,
        manifest_hash,
    })
}

// ============================================================================
// Download Functions
// ============================================================================

/// Download a manifest by its S3 key.
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `bucket` - S3 bucket name
/// * `s3_key` - Full S3 key of the manifest
///
/// # Returns
/// Tuple of (manifest, metadata) where metadata includes asset root and last modified.
pub async fn download_manifest<C: StorageClient>(
    client: &C,
    bucket: &str,
    s3_key: &str,
) -> Result<(Manifest, ManifestDownloadMetadata), StorageError> {
    // Download the manifest bytes
    let bytes: Vec<u8> = client.get_object(bucket, s3_key).await?;

    // Decode the manifest
    let content: String = String::from_utf8(bytes).map_err(|e| StorageError::Other {
        message: format!("Invalid UTF-8 in manifest: {}", e),
    })?;

    let manifest: Manifest = Manifest::decode(&content).map_err(|e| StorageError::Other {
        message: format!("Failed to decode manifest: {}", e),
    })?;

    // Get object metadata via HEAD request
    let metadata: ManifestDownloadMetadata =
        get_manifest_metadata(client, bucket, s3_key).await?;

    Ok((manifest, metadata))
}

/// Get manifest metadata from S3 without downloading the full object.
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `bucket` - S3 bucket name
/// * `s3_key` - Full S3 key of the manifest
///
/// # Returns
/// Manifest download metadata including asset root and last modified timestamp.
async fn get_manifest_metadata<C: StorageClient>(
    client: &C,
    bucket: &str,
    s3_key: &str,
) -> Result<ManifestDownloadMetadata, StorageError> {
    // Get extended metadata from HEAD request
    let obj_metadata: Option<ObjectMetadata> =
        client.head_object_with_metadata(bucket, s3_key).await?;

    match obj_metadata {
        Some(metadata) => {
            // Parse asset root from user metadata
            let s3_metadata: Option<ManifestS3Metadata> =
                ManifestS3Metadata::from_s3_metadata(&metadata.user_metadata);

            Ok(ManifestDownloadMetadata {
                asset_root: s3_metadata
                    .as_ref()
                    .map(|m| m.asset_root.clone())
                    .unwrap_or_default(),
                file_system_location_name: s3_metadata
                    .and_then(|m| m.file_system_location_name),
                content_type: metadata.content_type,
                last_modified: metadata.last_modified,
            })
        }
        None => Err(StorageError::NotFound {
            bucket: bucket.to_string(),
            key: s3_key.to_string(),
        }),
    }
}

/// Download a manifest and its metadata including asset root.
///
/// This function downloads the manifest and extracts the asset root from S3 metadata.
/// It requires a storage client that supports metadata retrieval.
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `bucket` - S3 bucket name
/// * `s3_key` - Full S3 key of the manifest
///
/// # Returns
/// Tuple of (manifest, metadata) with full metadata including asset root.
pub async fn download_manifest_with_metadata<C: StorageClient>(
    client: &C,
    bucket: &str,
    s3_key: &str,
) -> Result<(Manifest, ManifestDownloadMetadata), StorageError> {
    download_manifest(client, bucket, s3_key).await
}

/// Download an input manifest by its hash.
///
/// Note: This function requires knowing the GUID used when the manifest was uploaded.
/// For most use cases, use `download_manifest` with the full S3 key instead.
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `location` - Manifest location configuration
/// * `guid` - The GUID used when uploading the manifest
/// * `manifest_hash` - Hash of the manifest (used in the filename)
///
/// # Returns
/// Tuple of (manifest, metadata).
pub async fn download_input_manifest<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    guid: &str,
    manifest_hash: &str,
) -> Result<(Manifest, ManifestDownloadMetadata), StorageError> {
    let manifest_name: String = format!("{}_input", manifest_hash);
    let s3_key: String = format_input_manifest_s3_key(
        &location.manifest_prefix(),
        &location.farm_id,
        &location.queue_id,
        guid,
        &manifest_name,
    );

    download_manifest(client, &location.bucket, &s3_key).await
}

// ============================================================================
// Output Manifest Discovery - Primitives
// ============================================================================

/// Scope for output manifest discovery.
#[derive(Debug, Clone)]
pub enum OutputManifestScope {
    /// All outputs for a job.
    Job { job_id: String },
    /// All outputs for a step (includes all tasks).
    Step { job_id: String, step_id: String },
    /// All outputs for a specific task.
    Task {
        job_id: String,
        step_id: String,
        task_id: String,
    },
}

/// Build the S3 prefix for listing output manifests.
///
/// Returns prefix like:
/// - Job: `{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/`
/// - Step: `{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/`
/// - Task: `{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/`
///
/// # Arguments
/// * `location` - Manifest location configuration
/// * `scope` - Scope of discovery (job, step, or task)
///
/// # Returns
/// S3 prefix string for listing operations.
pub fn build_output_manifest_prefix(
    location: &ManifestLocation,
    scope: &OutputManifestScope,
) -> String {
    let manifest_prefix: &str = &location.manifest_prefix();

    match scope {
        OutputManifestScope::Job { job_id } => format_job_output_prefix(
            manifest_prefix,
            &location.farm_id,
            &location.queue_id,
            job_id,
        ),
        OutputManifestScope::Step { job_id, step_id } => format_step_output_prefix(
            manifest_prefix,
            &location.farm_id,
            &location.queue_id,
            job_id,
            step_id,
        ),
        OutputManifestScope::Task {
            job_id,
            step_id,
            task_id,
        } => format_task_output_prefix(
            manifest_prefix,
            &location.farm_id,
            &location.queue_id,
            job_id,
            step_id,
            task_id,
        ),
    }
}

/// Filter S3 objects to find output manifest files.
///
/// Output manifests match the pattern: `*_output` or `*_output.json`
/// This filters out non-manifest files that may exist in the prefix.
///
/// # Arguments
/// * `objects` - List of S3 objects from list operation
///
/// # Returns
/// References to objects that are output manifests.
pub fn filter_output_manifest_objects(objects: &[ObjectInfo]) -> Vec<&ObjectInfo> {
    objects
        .iter()
        .filter(|obj| {
            obj.key.ends_with("_output") || obj.key.ends_with("_output.json")
        })
        .collect()
}

/// Manifest key with parsed components.
#[derive(Debug, Clone)]
pub struct ParsedManifestKey {
    /// Full S3 key.
    pub key: String,
    /// Last modified timestamp (Unix seconds).
    pub last_modified: i64,
    /// Task ID if this is a task-level manifest, None for step-level (chunked).
    pub task_id: Option<String>,
    /// The timestamp_sessionaction folder name (e.g., "2025-05-22T12:00:00.000000Z_sessionaction-abc-123").
    pub session_folder: String,
    /// Session action ID extracted from the folder name (e.g., "sessionaction-abc-123").
    pub session_action_id: String,
}

/// Parse manifest keys to extract task IDs and session folders.
///
/// Handles both:
/// - Task-based: `.../step_id/task_id/timestamp_sessionaction/hash_output`
/// - Chunked (step-level): `.../step_id/timestamp_sessionaction/hash_output`
///
/// # Arguments
/// * `objects` - List of S3 objects that are output manifests
///
/// # Returns
/// Parsed manifest keys with extracted components.
pub fn parse_manifest_keys(objects: &[&ObjectInfo]) -> Vec<ParsedManifestKey> {
    // Regex to match task-based paths: .../step-xxx/task-xxx/timestamp_sessionaction-xxx/hash_output
    let task_pattern: Regex =
        Regex::new(r"step-[^/]+/(task-[^/]+)/([^/]+_sessionaction-[^/]+)/[^/]+_output")
            .expect("valid regex");

    // Regex to match step-level paths: .../step-xxx/timestamp_sessionaction-xxx/hash_output
    let step_pattern: Regex =
        Regex::new(r"step-[^/]+/([^/]+_sessionaction-[^/]+)/[^/]+_output")
            .expect("valid regex");

    // Regex to extract session action ID from folder name
    let session_action_re: Regex =
        Regex::new(r"(sessionaction-[^/]+)").expect("valid regex");

    objects
        .iter()
        .filter_map(|obj| {
            let last_modified: i64 = obj.last_modified.unwrap_or(0);

            // Try task-based pattern first
            if let Some(caps) = task_pattern.captures(&obj.key) {
                let task_id: String = caps.get(1)?.as_str().to_string();
                let session_folder: String = caps.get(2)?.as_str().to_string();
                let session_action_id: String = session_action_re
                    .captures(&session_folder)?
                    .get(1)?
                    .as_str()
                    .to_string();

                return Some(ParsedManifestKey {
                    key: obj.key.clone(),
                    last_modified,
                    task_id: Some(task_id),
                    session_folder,
                    session_action_id,
                });
            }

            // Try step-level pattern
            if let Some(caps) = step_pattern.captures(&obj.key) {
                let session_folder: String = caps.get(1)?.as_str().to_string();
                let session_action_id: String = session_action_re
                    .captures(&session_folder)?
                    .get(1)?
                    .as_str()
                    .to_string();

                return Some(ParsedManifestKey {
                    key: obj.key.clone(),
                    last_modified,
                    task_id: None,
                    session_folder,
                    session_action_id,
                });
            }

            None
        })
        .collect()
}

/// Group parsed manifest keys by task ID.
///
/// Returns a map where:
/// - Key is `Some(task_id)` for task-based manifests
/// - Key is `None` for step-level (chunked) manifests
///
/// # Arguments
/// * `parsed_keys` - Parsed manifest keys
///
/// # Returns
/// Map of task_id -> list of manifest keys.
pub fn group_manifests_by_task(
    parsed_keys: &[ParsedManifestKey],
) -> HashMap<Option<String>, Vec<&ParsedManifestKey>> {
    let mut grouped: HashMap<Option<String>, Vec<&ParsedManifestKey>> = HashMap::new();

    for key in parsed_keys {
        grouped
            .entry(key.task_id.clone())
            .or_default()
            .push(key);
    }

    grouped
}

/// Select the latest session action's manifests for each task.
///
/// For each task, keeps only manifests from the session folder with the
/// latest `last_modified` timestamp. Step-level manifests (chunked steps)
/// are always included.
///
/// # Why LastModified instead of folder name?
/// The timestamp in the folder name comes from WorkerAgent and shouldn't be
/// relied upon. S3's LastModified is authoritative.
///
/// # Arguments
/// * `grouped` - Manifests grouped by task ID
///
/// # Returns
/// S3 keys of the selected manifests.
pub fn select_latest_manifests_per_task(
    grouped: &HashMap<Option<String>, Vec<&ParsedManifestKey>>,
) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();

    for (task_id, manifests) in grouped {
        if task_id.is_none() {
            // Step-level manifests (chunked steps) - include all
            result.extend(manifests.iter().map(|m| m.key.clone()));
        } else {
            // Task-based manifests - select latest session folder by last_modified
            if manifests.is_empty() {
                continue;
            }

            // Find the session folder with the latest last_modified
            let latest_folder: &str = manifests
                .iter()
                .max_by_key(|m| m.last_modified)
                .map(|m| m.session_folder.as_str())
                .unwrap_or("");

            // Include all manifests from that session folder
            result.extend(
                manifests
                    .iter()
                    .filter(|m| m.session_folder == latest_folder)
                    .map(|m| m.key.clone()),
            );
        }
    }

    result
}

// ============================================================================
// Output Manifest Discovery - Composed Functions
// ============================================================================

/// Options for output manifest discovery.
#[derive(Debug, Clone)]
pub struct OutputManifestDiscoveryOptions {
    /// Scope of discovery (job, step, or task).
    pub scope: OutputManifestScope,
    /// If true, select only the latest session action per task.
    /// If false, return all manifests.
    pub select_latest_per_task: bool,
}

/// Discover output manifest keys from S3.
///
/// This composed function:
/// 1. Builds the S3 prefix for the given scope
/// 2. Lists all objects under that prefix
/// 3. Filters to only manifest files
/// 4. Parses keys to extract task IDs and session folders
/// 5. Optionally selects only the latest per task
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `location` - Manifest location configuration
/// * `options` - Discovery options
///
/// # Returns
/// List of S3 keys for discovered manifests.
pub async fn discover_output_manifest_keys<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    options: &OutputManifestDiscoveryOptions,
) -> Result<Vec<String>, StorageError> {
    // 1. Build prefix
    let prefix: String = build_output_manifest_prefix(location, &options.scope);

    // 2. List objects
    let objects: Vec<ObjectInfo> = client.list_objects(&location.bucket, &prefix).await?;

    // 3. Filter to manifests
    let manifest_objects: Vec<&ObjectInfo> = filter_output_manifest_objects(&objects);

    // 4. Parse keys
    let parsed: Vec<ParsedManifestKey> = parse_manifest_keys(&manifest_objects);

    // 5. Select latest if requested
    if options.select_latest_per_task {
        let grouped: HashMap<Option<String>, Vec<&ParsedManifestKey>> =
            group_manifests_by_task(&parsed);
        Ok(select_latest_manifests_per_task(&grouped))
    } else {
        Ok(parsed.iter().map(|p| p.key.clone()).collect())
    }
}

/// Downloaded manifest with metadata for merging.
#[derive(Debug, Clone)]
pub struct DownloadedManifest {
    /// The manifest content.
    pub manifest: Manifest,
    /// Asset root from S3 metadata.
    pub asset_root: String,
    /// Last modified timestamp (Unix seconds).
    pub last_modified: i64,
}

/// Default maximum concurrency for parallel manifest downloads.
/// Matches Python's S3_DOWNLOAD_MAX_CONCURRENCY.
pub const DEFAULT_MANIFEST_DOWNLOAD_CONCURRENCY: usize = 10;

/// Options for parallel manifest download operations.
#[derive(Debug, Clone)]
pub struct ManifestDownloadOptions {
    /// Maximum number of concurrent downloads.
    /// Default: 10 (matches Python's S3_DOWNLOAD_MAX_CONCURRENCY)
    pub max_concurrency: usize,
}

impl Default for ManifestDownloadOptions {
    fn default() -> Self {
        Self {
            max_concurrency: DEFAULT_MANIFEST_DOWNLOAD_CONCURRENCY,
        }
    }
}

impl ManifestDownloadOptions {
    /// Create options with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum concurrency for parallel downloads.
    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency;
        self
    }
}

/// Download output manifests and group by asset root.
///
/// This composed function:
/// 1. Discovers manifest keys using the given options
/// 2. Downloads all manifests in parallel (up to specified max_concurrency)
/// 3. Groups manifests by their asset root (from S3 metadata)
///
/// Note: Manifest merging should be done by the caller using the returned
/// manifests with their last_modified timestamps for chronological ordering.
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `location` - Manifest location configuration
/// * `discovery_options` - Discovery options (scope, latest per task)
/// * `download_options` - Download options (concurrency settings)
///
/// # Returns
/// Map of asset_root -> list of downloaded manifests with timestamps.
pub async fn download_output_manifests_by_asset_root<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    discovery_options: &OutputManifestDiscoveryOptions,
    download_options: &ManifestDownloadOptions,
) -> Result<HashMap<String, Vec<DownloadedManifest>>, StorageError> {
    // 1. Discover manifest keys
    let keys: Vec<String> =
        discover_output_manifest_keys(client, location, discovery_options).await?;

    // 2. Download all manifests in parallel
    let downloaded: Vec<DownloadedManifest> =
        download_manifests_parallel(client, &location.bucket, &keys, download_options).await?;

    // 3. Group by asset root
    let mut by_root: HashMap<String, Vec<DownloadedManifest>> = HashMap::new();
    for dm in downloaded {
        by_root.entry(dm.asset_root.clone()).or_default().push(dm);
    }

    Ok(by_root)
}

/// Download multiple manifests in parallel with concurrency limiting.
///
/// Downloads manifests concurrently up to the specified `max_concurrency`.
/// Uses `buffer_unordered` to limit the number of concurrent downloads.
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `bucket` - S3 bucket name
/// * `keys` - List of S3 keys to download
/// * `options` - Download options including max concurrency
///
/// # Returns
/// List of downloaded manifests with metadata.
///
/// # Errors
/// Returns the first error encountered during parallel downloads.
/// Returns `MissingAssetRoot` if any manifest is missing the asset-root metadata.
///
/// # Example
/// ```ignore
/// let options = ManifestDownloadOptions::default()
///     .with_max_concurrency(20);
/// let manifests = download_manifests_parallel(&client, "bucket", &keys, &options).await?;
/// ```
pub async fn download_manifests_parallel<C: StorageClient>(
    client: &C,
    bucket: &str,
    keys: &[String],
    options: &ManifestDownloadOptions,
) -> Result<Vec<DownloadedManifest>, StorageError> {
    use futures::stream::{self, StreamExt};

    if keys.is_empty() {
        return Ok(Vec::new());
    }

    // Use buffer_unordered to limit concurrent downloads
    let max_concurrency: usize = options.max_concurrency.max(1);

    let results: Vec<Result<DownloadedManifest, StorageError>> = stream::iter(keys.iter())
        .map(|key| async move {
            let (manifest, metadata) = download_manifest(client, bucket, key).await?;

            // Check for missing asset root (matches Python's MissingAssetRootError)
            if metadata.asset_root.is_empty() {
                return Err(StorageError::MissingAssetRoot {
                    s3_key: key.clone(),
                });
            }

            Ok::<DownloadedManifest, StorageError>(DownloadedManifest {
                manifest,
                asset_root: metadata.asset_root,
                last_modified: metadata.last_modified.unwrap_or(0),
            })
        })
        .buffer_unordered(max_concurrency)
        .collect()
        .await;

    // Collect successful results or return first error
    let mut downloaded: Vec<DownloadedManifest> = Vec::with_capacity(keys.len());
    for result in results {
        downloaded.push(result?);
    }

    Ok(downloaded)
}

// ============================================================================
// Matching Manifests to Job Attachment Roots
// ============================================================================

/// Job attachment root information needed for manifest matching.
#[derive(Debug, Clone)]
pub struct JobAttachmentRoot {
    /// The root path of the job attachment.
    pub root_path: String,
    /// Optional file system location name from storage profile.
    pub file_system_location_name: Option<String>,
}

/// Error type for manifest matching operations.
#[derive(Debug, Clone)]
pub enum ManifestMatchError {
    /// Invalid manifest key format.
    InvalidKey { key: String, reason: String },
    /// No matching root found for manifest.
    NoMatchingRoot { key: String },
}

impl std::fmt::Display for ManifestMatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestMatchError::InvalidKey { key, reason } => {
                write!(f, "Invalid manifest key '{}': {}", key, reason)
            }
            ManifestMatchError::NoMatchingRoot { key } => {
                write!(f, "No matching root path hash found for manifest: {}", key)
            }
        }
    }
}

impl std::error::Error for ManifestMatchError {}

/// Match manifest S3 keys to job attachment roots.
///
/// Given a list of manifest keys and job attachment roots, returns a mapping
/// from session action ID to manifest keys indexed by root position.
///
/// # Arguments
/// * `manifest_keys` - S3 keys of discovered manifests
/// * `job_attachment_roots` - Job attachment roots from job.attachments.manifests
///
/// # Returns
/// Map of session_action_id -> `Vec<Option<String>>` where the Vec is indexed
/// by root position (same order as job_attachment_roots).
pub fn match_manifests_to_roots(
    manifest_keys: &[String],
    job_attachment_roots: &[JobAttachmentRoot],
) -> Result<HashMap<String, Vec<Option<String>>>, ManifestMatchError> {
    // Regex to extract session action ID from manifest key
    let session_action_re: Regex =
        Regex::new(r"(sessionaction-[^/-]+-[^/-]+)/").expect("valid regex");

    // Compute hash for each root
    let root_hashes: Vec<String> = job_attachment_roots
        .iter()
        .map(|root| {
            compute_root_path_hash(root.file_system_location_name.as_deref(), &root.root_path)
        })
        .collect();

    let mut result: HashMap<String, Vec<Option<String>>> = HashMap::new();

    for key in manifest_keys {
        // Extract session action ID
        let session_action_id: String = session_action_re
            .captures(key)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| ManifestMatchError::InvalidKey {
                key: key.clone(),
                reason: "missing session action ID".into(),
            })?;

        // Find which root this manifest belongs to
        let root_index: usize = root_hashes
            .iter()
            .position(|hash| key.contains(hash))
            .ok_or_else(|| ManifestMatchError::NoMatchingRoot { key: key.clone() })?;

        // Initialize entry for this session action if needed
        let entry: &mut Vec<Option<String>> = result
            .entry(session_action_id)
            .or_insert_with(|| vec![None; job_attachment_roots.len()]);

        entry[root_index] = Some(key.clone());
    }

    Ok(result)
}

/// Find manifests for a specific session action ID.
///
/// When the session action ID is known (e.g., from GetTask API), we can
/// search for manifests containing that ID in their path. This is more
/// reliable than timestamp-based selection.
///
/// # Search Strategy
/// 1. First searches in task folder (`.../step_id/task_id/`)
/// 2. Falls back to step folder (`.../step_id/`) for chunked steps
///
/// # Arguments
/// * `client` - Storage client for S3 operations
/// * `location` - Manifest location configuration
/// * `job_id` - Job ID
/// * `step_id` - Step ID
/// * `task_id` - Task ID
/// * `session_action_id` - Session action ID to search for
///
/// # Returns
/// List of S3 keys for manifests belonging to the session action.
pub async fn find_manifests_by_session_action_id<C: StorageClient>(
    client: &C,
    location: &ManifestLocation,
    job_id: &str,
    step_id: &str,
    task_id: &str,
    session_action_id: &str,
) -> Result<Vec<String>, StorageError> {
    // Build regex pattern to match session action ID in path
    let pattern: Regex = Regex::new(&format!(r".*{}.*_output", regex::escape(session_action_id)))
        .map_err(|e| StorageError::Other {
            message: format!("Invalid session action ID pattern: {}", e),
        })?;

    // Try task-specific prefix first for efficiency
    let task_scope = OutputManifestScope::Task {
        job_id: job_id.to_string(),
        step_id: step_id.to_string(),
        task_id: task_id.to_string(),
    };
    let task_prefix: String = build_output_manifest_prefix(location, &task_scope);

    let task_objects: Vec<ObjectInfo> = client.list_objects(&location.bucket, &task_prefix).await?;
    let task_keys: Vec<String> = task_objects
        .iter()
        .filter(|obj| pattern.is_match(&obj.key))
        .map(|obj| obj.key.clone())
        .collect();

    if !task_keys.is_empty() {
        return Ok(task_keys);
    }

    // Fall back to step-level prefix for chunked steps
    let step_scope = OutputManifestScope::Step {
        job_id: job_id.to_string(),
        step_id: step_id.to_string(),
    };
    let step_prefix: String = build_output_manifest_prefix(location, &step_scope);

    let step_objects: Vec<ObjectInfo> = client.list_objects(&location.bucket, &step_prefix).await?;
    let step_keys: Vec<String> = step_objects
        .iter()
        .filter(|obj| pattern.is_match(&obj.key))
        .map(|obj| obj.key.clone())
        .collect();

    Ok(step_keys)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_float_to_iso_datetime_string() {
        // Test with a known timestamp
        let timestamp: f64 = 1716414026.409012;
        let result: String = float_to_iso_datetime_string(timestamp);

        // Should be in format YYYY-MM-DDTHH:MM:SS.ffffffZ
        assert!(result.ends_with('Z'));
        assert!(result.contains('T'));
        assert_eq!(result.len(), 27); // Fixed length for microsecond precision
    }

    #[test]
    fn test_float_to_iso_datetime_string_zero() {
        let result: String = float_to_iso_datetime_string(0.0);
        assert_eq!(result, "1970-01-01T00:00:00.000000Z");
    }

    #[test]
    fn test_generate_random_guid() {
        let guid1: String = generate_random_guid();
        let guid2: String = generate_random_guid();

        // Should be 32 hex characters
        assert_eq!(guid1.len(), 32);
        assert_eq!(guid2.len(), 32);

        // Should be different
        assert_ne!(guid1, guid2);

        // Should be valid hex
        assert!(guid1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_compute_manifest_name_hash() {
        let hash1: String = compute_manifest_name_hash("/path/to/assets");
        let hash2: String = compute_manifest_name_hash("/path/to/assets");
        let hash3: String = compute_manifest_name_hash("/different/path");

        // Same input should produce same hash
        assert_eq!(hash1, hash2);

        // Different input should produce different hash
        assert_ne!(hash1, hash3);

        // Should be 32 hex characters
        assert_eq!(hash1.len(), 32);
    }

    #[test]
    fn test_compute_root_path_hash() {
        // Without file system location
        let hash1: String = compute_root_path_hash(None, "/mnt/projects/job1");

        // With file system location
        let hash2: String = compute_root_path_hash(Some("ProjectShare"), "/mnt/projects/job1");

        // Should be different
        assert_ne!(hash1, hash2);

        // Should be 32 hex characters
        assert_eq!(hash1.len(), 32);
        assert_eq!(hash2.len(), 32);
    }

    #[test]
    fn test_manifest_s3_metadata_ascii() {
        let metadata: ManifestS3Metadata = ManifestS3Metadata::new("/ascii/path");
        let s3_meta: HashMap<String, String> = metadata.to_s3_metadata();

        assert_eq!(s3_meta.get(METADATA_KEY_ASSET_ROOT), Some(&"/ascii/path".to_string()));
        assert!(!s3_meta.contains_key(METADATA_KEY_ASSET_ROOT_JSON));
    }

    #[test]
    fn test_manifest_s3_metadata_non_ascii() {
        let metadata: ManifestS3Metadata = ManifestS3Metadata::new("/path/日本語");
        let s3_meta: HashMap<String, String> = metadata.to_s3_metadata();

        // Both fields should be populated for non-ASCII
        assert!(s3_meta.contains_key(METADATA_KEY_ASSET_ROOT));
        assert!(s3_meta.contains_key(METADATA_KEY_ASSET_ROOT_JSON));

        // Values should be JSON-encoded
        let json_value: &String = s3_meta.get(METADATA_KEY_ASSET_ROOT_JSON).unwrap();
        assert!(json_value.starts_with('"'));
    }

    #[test]
    fn test_manifest_s3_metadata_with_location() {
        let metadata: ManifestS3Metadata =
            ManifestS3Metadata::with_location("/path", "MyLocation");
        let s3_meta: HashMap<String, String> = metadata.to_s3_metadata();

        assert_eq!(
            s3_meta.get(METADATA_KEY_FILE_SYSTEM_LOCATION_NAME),
            Some(&"MyLocation".to_string())
        );
    }

    #[test]
    fn test_manifest_s3_metadata_roundtrip() {
        let original: ManifestS3Metadata =
            ManifestS3Metadata::with_location("/path/to/assets", "SharedDrive");
        let s3_meta: HashMap<String, String> = original.to_s3_metadata();
        let parsed: ManifestS3Metadata = ManifestS3Metadata::from_s3_metadata(&s3_meta).unwrap();

        assert_eq!(parsed.asset_root, original.asset_root);
        assert_eq!(
            parsed.file_system_location_name,
            original.file_system_location_name
        );
    }

    #[test]
    fn test_manifest_location_prefixes() {
        let location: ManifestLocation =
            ManifestLocation::new("my-bucket", "DeadlineCloud", "farm-123", "queue-456");

        assert_eq!(location.manifest_prefix(), "DeadlineCloud/Manifests");
        assert_eq!(
            location.input_manifest_prefix(),
            "DeadlineCloud/Manifests/farm-123/queue-456/Inputs"
        );
    }

    #[test]
    fn test_manifest_location_empty_prefix() {
        let location: ManifestLocation = ManifestLocation::new("my-bucket", "", "farm-123", "queue-456");

        assert_eq!(location.manifest_prefix(), "Manifests");
        assert_eq!(
            location.input_manifest_prefix(),
            "Manifests/farm-123/queue-456/Inputs"
        );
    }

    #[test]
    fn test_format_input_manifest_s3_key() {
        let key: String = format_input_manifest_s3_key(
            "DeadlineCloud/Manifests",
            "farm-123",
            "queue-456",
            "abcd1234",
            "e5f6g7h8_input",
        );

        assert_eq!(
            key,
            "DeadlineCloud/Manifests/farm-123/queue-456/Inputs/abcd1234/e5f6g7h8_input"
        );
    }

    #[test]
    fn test_format_input_manifest_s3_key_empty_prefix() {
        let key: String = format_input_manifest_s3_key(
            "Manifests",
            "farm-123",
            "queue-456",
            "guid1234",
            "hash_input",
        );

        assert_eq!(key, "Manifests/farm-123/queue-456/Inputs/guid1234/hash_input");
    }

    #[test]
    fn test_format_task_output_manifest_s3_key() {
        let key: String = format_task_output_manifest_s3_key(
            "DeadlineCloud/Manifests",
            "farm-123",
            "queue-456",
            "job-789",
            "step-abc",
            "task-def",
            "2025-05-22T22:17:03.409012Z",
            "sessionaction-xyz",
            "hash123_output",
        );

        assert_eq!(
            key,
            "DeadlineCloud/Manifests/farm-123/queue-456/job-789/step-abc/task-def/2025-05-22T22:17:03.409012Z_sessionaction-xyz/hash123_output"
        );
    }

    #[test]
    fn test_format_step_output_manifest_s3_key() {
        let key: String = format_step_output_manifest_s3_key(
            "DeadlineCloud/Manifests",
            "farm-123",
            "queue-456",
            "job-789",
            "step-abc",
            "2025-05-22T22:17:03.409012Z",
            "sessionaction-xyz",
            "hash123_output",
        );

        assert_eq!(
            key,
            "DeadlineCloud/Manifests/farm-123/queue-456/job-789/step-abc/2025-05-22T22:17:03.409012Z_sessionaction-xyz/hash123_output"
        );
    }

    #[test]
    fn test_format_task_vs_step_output_manifest_s3_key_difference() {
        // Task-level key should have task_id segment
        let task_key: String = format_task_output_manifest_s3_key(
            "Manifests",
            "farm",
            "queue",
            "job",
            "step",
            "task",
            "2025-01-01T00:00:00.000000Z",
            "session",
            "hash_output",
        );

        // Step-level key should NOT have task_id segment
        let step_key: String = format_step_output_manifest_s3_key(
            "Manifests",
            "farm",
            "queue",
            "job",
            "step",
            "2025-01-01T00:00:00.000000Z",
            "session",
            "hash_output",
        );

        // Task key should contain "task" segment
        assert!(task_key.contains("/task/"));

        // Step key should NOT contain "task" segment
        assert!(!step_key.contains("/task/"));

        // Both should end with the same manifest name
        assert!(task_key.ends_with("hash_output"));
        assert!(step_key.ends_with("hash_output"));
    }

    #[test]
    fn test_build_output_manifest_prefix_job() {
        let location: ManifestLocation =
            ManifestLocation::new("bucket", "DeadlineCloud", "farm-123", "queue-456");
        let scope = OutputManifestScope::Job {
            job_id: "job-789".to_string(),
        };

        let prefix: String = build_output_manifest_prefix(&location, &scope);
        assert_eq!(
            prefix,
            "DeadlineCloud/Manifests/farm-123/queue-456/job-789/"
        );
    }

    #[test]
    fn test_build_output_manifest_prefix_step() {
        let location: ManifestLocation =
            ManifestLocation::new("bucket", "DeadlineCloud", "farm-123", "queue-456");
        let scope = OutputManifestScope::Step {
            job_id: "job-789".to_string(),
            step_id: "step-abc".to_string(),
        };

        let prefix: String = build_output_manifest_prefix(&location, &scope);
        assert_eq!(
            prefix,
            "DeadlineCloud/Manifests/farm-123/queue-456/job-789/step-abc/"
        );
    }

    #[test]
    fn test_build_output_manifest_prefix_task() {
        let location: ManifestLocation =
            ManifestLocation::new("bucket", "DeadlineCloud", "farm-123", "queue-456");
        let scope = OutputManifestScope::Task {
            job_id: "job-789".to_string(),
            step_id: "step-abc".to_string(),
            task_id: "task-def".to_string(),
        };

        let prefix: String = build_output_manifest_prefix(&location, &scope);
        assert_eq!(
            prefix,
            "DeadlineCloud/Manifests/farm-123/queue-456/job-789/step-abc/task-def/"
        );
    }

    #[test]
    fn test_filter_output_manifest_objects() {
        use crate::traits::ObjectInfo;

        let objects: Vec<ObjectInfo> = vec![
            ObjectInfo {
                key: "path/to/hash123_output".to_string(),
                size: 100,
                last_modified: Some(1000),
                etag: None,
            },
            ObjectInfo {
                key: "path/to/hash456_output.json".to_string(),
                size: 200,
                last_modified: Some(2000),
                etag: None,
            },
            ObjectInfo {
                key: "path/to/some_other_file.txt".to_string(),
                size: 50,
                last_modified: Some(500),
                etag: None,
            },
            ObjectInfo {
                key: "path/to/hash789_input".to_string(),
                size: 150,
                last_modified: Some(1500),
                etag: None,
            },
        ];

        let filtered: Vec<&ObjectInfo> = filter_output_manifest_objects(&objects);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|o| o.key.contains("hash123_output")));
        assert!(filtered.iter().any(|o| o.key.contains("hash456_output")));
    }

    #[test]
    fn test_parse_manifest_keys_task_based() {
        use crate::traits::ObjectInfo;

        let objects: Vec<ObjectInfo> = vec![ObjectInfo {
            key: "prefix/job-123/step-abc/task-def/2025-01-01T00:00:00.000000Z_sessionaction-xyz-123/hash_output".to_string(),
            size: 100,
            last_modified: Some(1000),
            etag: None,
        }];

        let refs: Vec<&ObjectInfo> = objects.iter().collect();
        let parsed: Vec<ParsedManifestKey> = parse_manifest_keys(&refs);

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].task_id, Some("task-def".to_string()));
        assert_eq!(
            parsed[0].session_folder,
            "2025-01-01T00:00:00.000000Z_sessionaction-xyz-123"
        );
        assert_eq!(parsed[0].session_action_id, "sessionaction-xyz-123");
        assert_eq!(parsed[0].last_modified, 1000);
    }

    #[test]
    fn test_parse_manifest_keys_step_level() {
        use crate::traits::ObjectInfo;

        let objects: Vec<ObjectInfo> = vec![ObjectInfo {
            key: "prefix/job-123/step-abc/2025-01-01T00:00:00.000000Z_sessionaction-xyz-456/hash_output".to_string(),
            size: 100,
            last_modified: Some(2000),
            etag: None,
        }];

        let refs: Vec<&ObjectInfo> = objects.iter().collect();
        let parsed: Vec<ParsedManifestKey> = parse_manifest_keys(&refs);

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].task_id, None);
        assert_eq!(
            parsed[0].session_folder,
            "2025-01-01T00:00:00.000000Z_sessionaction-xyz-456"
        );
        assert_eq!(parsed[0].session_action_id, "sessionaction-xyz-456");
    }

    #[test]
    fn test_group_manifests_by_task() {
        use crate::traits::ObjectInfo;

        let objects: Vec<ObjectInfo> = vec![
            ObjectInfo {
                key: "prefix/step-abc/task-001/ts_sessionaction-a/hash_output".to_string(),
                size: 100,
                last_modified: Some(1000),
                etag: None,
            },
            ObjectInfo {
                key: "prefix/step-abc/task-001/ts_sessionaction-b/hash_output".to_string(),
                size: 100,
                last_modified: Some(2000),
                etag: None,
            },
            ObjectInfo {
                key: "prefix/step-abc/task-002/ts_sessionaction-c/hash_output".to_string(),
                size: 100,
                last_modified: Some(3000),
                etag: None,
            },
            ObjectInfo {
                key: "prefix/step-abc/ts_sessionaction-d/hash_output".to_string(),
                size: 100,
                last_modified: Some(4000),
                etag: None,
            },
        ];

        let refs: Vec<&ObjectInfo> = objects.iter().collect();
        let parsed: Vec<ParsedManifestKey> = parse_manifest_keys(&refs);
        let grouped: HashMap<Option<String>, Vec<&ParsedManifestKey>> =
            group_manifests_by_task(&parsed);

        // Should have 3 groups: task-001, task-002, and None (step-level)
        assert_eq!(grouped.len(), 3);
        assert_eq!(
            grouped.get(&Some("task-001".to_string())).map(|v| v.len()),
            Some(2)
        );
        assert_eq!(
            grouped.get(&Some("task-002".to_string())).map(|v| v.len()),
            Some(1)
        );
        assert_eq!(grouped.get(&None).map(|v| v.len()), Some(1));
    }

    #[test]
    fn test_select_latest_manifests_per_task() {
        use crate::traits::ObjectInfo;

        let objects: Vec<ObjectInfo> = vec![
            ObjectInfo {
                key: "prefix/step-abc/task-001/ts1_sessionaction-a/hash_output".to_string(),
                size: 100,
                last_modified: Some(1000), // older
                etag: None,
            },
            ObjectInfo {
                key: "prefix/step-abc/task-001/ts2_sessionaction-b/hash_output".to_string(),
                size: 100,
                last_modified: Some(2000), // newer - should be selected
                etag: None,
            },
            ObjectInfo {
                key: "prefix/step-abc/ts_sessionaction-c/hash_output".to_string(),
                size: 100,
                last_modified: Some(500), // step-level - always included
                etag: None,
            },
        ];

        let refs: Vec<&ObjectInfo> = objects.iter().collect();
        let parsed: Vec<ParsedManifestKey> = parse_manifest_keys(&refs);
        let grouped: HashMap<Option<String>, Vec<&ParsedManifestKey>> =
            group_manifests_by_task(&parsed);
        let selected: Vec<String> = select_latest_manifests_per_task(&grouped);

        // Should have 2 keys: the latest for task-001 and the step-level one
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|k| k.contains("sessionaction-b"))); // latest for task-001
        assert!(selected.iter().any(|k| k.contains("sessionaction-c"))); // step-level
        assert!(!selected.iter().any(|k| k.contains("sessionaction-a"))); // older, not selected
    }

    #[test]
    fn test_match_manifests_to_roots() {
        let roots: Vec<JobAttachmentRoot> = vec![
            JobAttachmentRoot {
                root_path: "/mnt/assets".to_string(),
                file_system_location_name: None,
            },
            JobAttachmentRoot {
                root_path: "/mnt/textures".to_string(),
                file_system_location_name: Some("TextureShare".to_string()),
            },
        ];

        // Compute expected hashes
        let hash1: String = compute_root_path_hash(None, "/mnt/assets");
        let hash2: String = compute_root_path_hash(Some("TextureShare"), "/mnt/textures");

        let manifest_keys: Vec<String> = vec![
            format!(
                "prefix/step-abc/task-def/ts_sessionaction-xyz-123/{}_output",
                hash1
            ),
            format!(
                "prefix/step-abc/task-def/ts_sessionaction-xyz-123/{}_output",
                hash2
            ),
        ];

        let matched: HashMap<String, Vec<Option<String>>> =
            match_manifests_to_roots(&manifest_keys, &roots).unwrap();

        assert_eq!(matched.len(), 1);
        let session_manifests: &Vec<Option<String>> =
            matched.get("sessionaction-xyz-123").unwrap();
        assert_eq!(session_manifests.len(), 2);
        assert!(session_manifests[0].is_some());
        assert!(session_manifests[1].is_some());
    }

    #[test]
    fn test_match_manifests_to_roots_missing_session_action() {
        let roots: Vec<JobAttachmentRoot> = vec![JobAttachmentRoot {
            root_path: "/mnt/assets".to_string(),
            file_system_location_name: None,
        }];

        let manifest_keys: Vec<String> = vec!["prefix/invalid/path/hash_output".to_string()];

        let result = match_manifests_to_roots(&manifest_keys, &roots);
        assert!(result.is_err());
    }
}
