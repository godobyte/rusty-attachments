//! Shared data structures for storage operations.

use rusty_attachments_model::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::error::TransferError;

/// Chunk size for manifest v2 format (256MB).
/// Files larger than this are split into chunks with individual hashes.
pub const CHUNK_SIZE_V2: u64 = 256 * 1024 * 1024; // 268435456 bytes

/// No chunking (for v1 format or when chunking is disabled).
pub const CHUNK_SIZE_NONE: u64 = 0;

/// Configuration settings for storage operations.
#[derive(Debug, Clone)]
pub struct StorageSettings {
    /// AWS region.
    pub region: String,
    /// AWS credentials (access key, secret key, session token).
    pub credentials: Option<AwsCredentials>,
    /// Expected bucket owner account ID for security validation.
    /// When set, all S3 operations will include ExpectedBucketOwner parameter.
    pub expected_bucket_owner: Option<String>,
    /// Chunk size for large files (use CHUNK_SIZE_V2 or CHUNK_SIZE_NONE).
    pub chunk_size: u64,
    /// Upload retry settings.
    pub upload_retry: RetrySettings,
    /// Download retry settings.
    pub download_retry: RetrySettings,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            region: "us-west-2".into(),
            credentials: None,
            expected_bucket_owner: None,
            chunk_size: CHUNK_SIZE_V2,
            upload_retry: RetrySettings::default(),
            download_retry: RetrySettings::default(),
        }
    }
}

/// AWS credentials.
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Retry settings for transfer operations.
#[derive(Debug, Clone)]
pub struct RetrySettings {
    /// Maximum number of retry attempts.
    pub max_attempts: u32,
    /// Initial backoff delay in milliseconds.
    pub initial_backoff_ms: u64,
    /// Maximum backoff delay in milliseconds.
    pub max_backoff_ms: u64,
    /// Backoff multiplier (exponential backoff).
    pub backoff_multiplier: f64,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 30_000,
            backoff_multiplier: 2.0,
        }
    }
}

/// S3 bucket and prefix configuration for CAS storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Location {
    /// S3 bucket name.
    pub bucket: String,
    /// Root prefix for all operations (e.g., "DeadlineCloud").
    pub root_prefix: String,
    /// CAS data prefix (e.g., "Data").
    pub cas_prefix: String,
    /// Manifest prefix (e.g., "Manifests").
    pub manifest_prefix: String,
}

impl S3Location {
    /// Create a new S3 location configuration.
    pub fn new(
        bucket: impl Into<String>,
        root_prefix: impl Into<String>,
        cas_prefix: impl Into<String>,
        manifest_prefix: impl Into<String>,
    ) -> Self {
        Self {
            bucket: bucket.into(),
            root_prefix: root_prefix.into(),
            cas_prefix: cas_prefix.into(),
            manifest_prefix: manifest_prefix.into(),
        }
    }

    /// Generate the full S3 key for a CAS object.
    /// Returns: "{root_prefix}/{cas_prefix}/{hash}.{algorithm}"
    pub fn cas_key(&self, hash: &str, algorithm: HashAlgorithm) -> String {
        let ext = algorithm.extension();
        if self.root_prefix.is_empty() {
            format!("{}/{}.{}", self.cas_prefix, hash, ext)
        } else {
            format!("{}/{}/{}.{}", self.root_prefix, self.cas_prefix, hash, ext)
        }
    }

    /// Generate the full S3 key for a manifest.
    /// Returns: "{root_prefix}/{manifest_prefix}/{path}"
    pub fn manifest_key(&self, path: &str) -> String {
        if self.root_prefix.is_empty() {
            format!("{}/{}", self.manifest_prefix, path)
        } else {
            format!("{}/{}/{}", self.root_prefix, self.manifest_prefix, path)
        }
    }

    /// Get the full CAS prefix.
    pub fn full_cas_prefix(&self) -> String {
        if self.root_prefix.is_empty() {
            self.cas_prefix.clone()
        } else {
            format!("{}/{}", self.root_prefix, self.cas_prefix)
        }
    }

    /// Get the full manifest prefix.
    pub fn full_manifest_prefix(&self) -> String {
        if self.root_prefix.is_empty() {
            self.manifest_prefix.clone()
        } else {
            format!("{}/{}", self.root_prefix, self.manifest_prefix)
        }
    }
}

/// Request to upload a single object to CAS.
#[derive(Debug, Clone)]
pub struct CasUploadRequest {
    /// Content hash (becomes the S3 key).
    pub hash: String,
    /// Hash algorithm used.
    pub algorithm: HashAlgorithm,
    /// Expected size in bytes.
    pub size: u64,
    /// Data source - either file path or in-memory bytes.
    pub source: DataSource,
}

/// Request to download a single object from CAS.
#[derive(Debug, Clone)]
pub struct CasDownloadRequest {
    /// Content hash (the S3 key).
    pub hash: String,
    /// Hash algorithm used.
    pub algorithm: HashAlgorithm,
    /// Expected size in bytes (for validation).
    pub expected_size: u64,
    /// Destination - either file path or in-memory buffer.
    pub destination: DataDestination,
}

/// Source of data for upload.
#[derive(Debug, Clone)]
pub enum DataSource {
    /// Read from file at path.
    FilePath(String),
    /// Read from file at path, specific byte range (for chunked uploads).
    FileRange {
        path: String,
        offset: u64,
        length: u64,
    },
    /// In-memory bytes.
    Bytes(Vec<u8>),
}

/// Destination for downloaded data.
#[derive(Debug, Clone)]
pub enum DataDestination {
    /// Write to file at path.
    FilePath(String),
    /// Write to file at path, specific offset (for chunked downloads).
    FileOffset { path: String, offset: u64 },
    /// Return as in-memory bytes.
    Memory,
}

/// Result of a single upload operation.
#[derive(Debug, Clone)]
pub struct UploadResult {
    /// The hash/key that was uploaded.
    pub hash: String,
    /// Bytes transferred (0 if skipped due to existence check).
    pub bytes_transferred: u64,
    /// Whether the object was actually uploaded (false if already existed).
    pub was_uploaded: bool,
}

/// Result of a single download operation.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    /// The hash/key that was downloaded.
    pub hash: String,
    /// Bytes transferred.
    pub bytes_transferred: u64,
    /// Downloaded data (only populated if destination was Memory).
    pub data: Option<Vec<u8>>,
}

/// Aggregated statistics for batch operations.
#[derive(Debug, Clone, Default)]
pub struct TransferStatistics {
    /// Total files processed.
    pub files_processed: u64,
    /// Files actually transferred (not skipped).
    pub files_transferred: u64,
    /// Files skipped (already existed or unchanged).
    pub files_skipped: u64,
    /// Total bytes transferred.
    pub bytes_transferred: u64,
    /// Total bytes skipped.
    pub bytes_skipped: u64,
    /// Errors encountered (non-fatal).
    pub errors: Vec<TransferError>,
}

impl TransferStatistics {
    /// Create statistics for a skipped file.
    pub fn skipped(size: u64) -> Self {
        Self {
            files_processed: 1,
            files_skipped: 1,
            bytes_skipped: size,
            ..Default::default()
        }
    }

    /// Create statistics for an uploaded file.
    pub fn uploaded(size: u64) -> Self {
        Self {
            files_processed: 1,
            files_transferred: 1,
            bytes_transferred: size,
            ..Default::default()
        }
    }

    /// Merge another statistics into this one.
    pub fn merge(&mut self, other: Self) {
        self.files_processed += other.files_processed;
        self.files_transferred += other.files_transferred;
        self.files_skipped += other.files_skipped;
        self.bytes_transferred += other.bytes_transferred;
        self.bytes_skipped += other.bytes_skipped;
        self.errors.extend(other.errors);
    }
}

/// Progress update for transfer operations.
#[derive(Debug, Clone)]
pub struct TransferProgress {
    /// Current operation type.
    pub operation: OperationType,
    /// Current file/object being processed.
    pub current_key: String,
    /// Bytes transferred so far for current object.
    pub current_bytes: u64,
    /// Total bytes for current object.
    pub current_total: u64,
    /// Overall progress: objects completed.
    pub overall_completed: u64,
    /// Overall progress: total objects.
    pub overall_total: u64,
    /// Overall bytes transferred.
    pub overall_bytes: u64,
    /// Overall total bytes.
    pub overall_total_bytes: u64,
}

/// Type of operation in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    CheckingExistence,
    Uploading,
    Downloading,
    Hashing,
}

/// How to handle conflicts when downloading files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictResolution {
    /// Skip files that already exist locally.
    Skip,
    /// Overwrite existing files.
    #[default]
    Overwrite,
    /// Create copy with suffix: "file (1).ext".
    CreateCopy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s3_location_cas_key() {
        let loc = S3Location::new("bucket", "DeadlineCloud", "Data", "Manifests");
        assert_eq!(
            loc.cas_key("abc123", HashAlgorithm::Xxh128),
            "DeadlineCloud/Data/abc123.xxh128"
        );
    }

    #[test]
    fn test_s3_location_cas_key_empty_prefix() {
        let loc = S3Location::new("bucket", "", "Data", "Manifests");
        assert_eq!(
            loc.cas_key("abc123", HashAlgorithm::Xxh128),
            "Data/abc123.xxh128"
        );
    }

    #[test]
    fn test_s3_location_manifest_key() {
        let loc = S3Location::new("bucket", "DeadlineCloud", "Data", "Manifests");
        assert_eq!(
            loc.manifest_key("farm-123/queue-456/input.manifest"),
            "DeadlineCloud/Manifests/farm-123/queue-456/input.manifest"
        );
    }

    #[test]
    fn test_transfer_statistics_merge() {
        let mut stats1 = TransferStatistics::uploaded(100);
        let stats2 = TransferStatistics::skipped(200);
        stats1.merge(stats2);

        assert_eq!(stats1.files_processed, 2);
        assert_eq!(stats1.files_transferred, 1);
        assert_eq!(stats1.files_skipped, 1);
        assert_eq!(stats1.bytes_transferred, 100);
        assert_eq!(stats1.bytes_skipped, 200);
    }
}
