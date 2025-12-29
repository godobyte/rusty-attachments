//! Serializable types for Tauri IPC.

use serde::{Deserialize, Serialize};

use rusty_attachments_storage::{ManifestLocation, S3Location};

/// AWS and S3 configuration for job attachments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobAttachmentConfig {
    /// AWS region (e.g., "us-west-2").
    pub region: String,
    /// S3 bucket name.
    pub bucket: String,
    /// Root prefix in S3 (e.g., "DeadlineCloud").
    pub root_prefix: String,
    /// Farm ID for manifest location.
    pub farm_id: String,
    /// Queue ID for manifest location.
    pub queue_id: String,
}

impl JobAttachmentConfig {
    /// Build S3Location for CAS operations.
    pub fn to_s3_location(&self) -> S3Location {
        S3Location::new(&self.bucket, &self.root_prefix, "Data", "Manifests")
    }

    /// Build ManifestLocation for manifest uploads.
    pub fn to_manifest_location(&self) -> ManifestLocation {
        ManifestLocation::new(&self.bucket, &self.root_prefix, &self.farm_id, &self.queue_id)
    }
}

/// Options for creating a directory snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotConfig {
    /// Root directory to snapshot.
    pub root_path: String,
    /// Specific files to include (if empty, scans entire directory).
    pub input_files: Vec<String>,
    /// Glob patterns to include (e.g., ["**/*.exr", "**/*.blend"]).
    /// Empty means include all files.
    pub include_patterns: Vec<String>,
    /// Glob patterns to exclude (e.g., ["**/*.tmp", "**/__pycache__/**"]).
    pub exclude_patterns: Vec<String>,
    /// Manifest version: "v2023-03-03" or "v2025-12-04-beta".
    pub manifest_version: String,
}

/// Result of a snapshot operation (no upload).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResult {
    /// Manifest version created.
    pub version: String,
    /// Total files in manifest.
    pub file_count: usize,
    /// Total size in bytes.
    pub total_size: u64,
    /// Encoded manifest JSON (for preview/debugging).
    pub manifest_json: String,
}

/// Result of a full bundle submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResult {
    /// Attachments JSON for CreateJob API.
    pub attachments_json: String,
    /// Hashing statistics.
    pub hashing_stats: StatsInfo,
    /// Upload statistics.
    pub upload_stats: StatsInfo,
}

/// Statistics for an operation phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsInfo {
    pub files_processed: u64,
    pub bytes_processed: u64,
    pub files_transferred: u64,
    pub bytes_transferred: u64,
    pub files_skipped: u64,
}

impl From<ja_deadline_utils::SummaryStatistics> for StatsInfo {
    fn from(stats: ja_deadline_utils::SummaryStatistics) -> Self {
        Self {
            files_processed: stats.processed_files,
            bytes_processed: stats.processed_bytes,
            files_transferred: stats.files_transferred,
            bytes_transferred: stats.bytes_transferred,
            files_skipped: stats.files_skipped,
        }
    }
}

/// A directory entry for the file browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified: Option<u64>,
}

/// An S3 object entry for the bucket browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3ObjectEntry {
    /// Object key (full path).
    pub key: String,
    /// Display name (last segment).
    pub name: String,
    /// Whether this is a "folder" (common prefix).
    pub is_prefix: bool,
    /// Size in bytes (None for prefixes).
    pub size: Option<u64>,
    /// Last modified timestamp (seconds since epoch).
    pub last_modified: Option<u64>,
    /// Whether this looks like a manifest file.
    pub is_manifest: bool,
}

/// A file entry from a parsed manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFileEntry {
    /// Relative path within the manifest.
    pub path: String,
    /// File size in bytes.
    pub size: Option<u64>,
    /// Modification time (microseconds since epoch).
    pub mtime: Option<i64>,
    /// Content hash.
    pub hash: Option<String>,
    /// Whether file is executable.
    pub executable: bool,
    /// Entry type: "file", "symlink", "directory", "deleted".
    pub entry_type: String,
    /// Symlink target (if symlink).
    pub symlink_target: Option<String>,
    /// Number of chunks (for large files).
    pub chunk_count: Option<usize>,
}

/// Parsed manifest with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedManifest {
    /// Manifest version.
    pub version: String,
    /// Hash algorithm.
    pub hash_alg: String,
    /// Total size in bytes.
    pub total_size: u64,
    /// File count.
    pub file_count: usize,
    /// Directory count (v2 only).
    pub dir_count: usize,
    /// Asset root from S3 metadata.
    pub asset_root: Option<String>,
    /// File entries.
    pub files: Vec<ManifestFileEntry>,
}
