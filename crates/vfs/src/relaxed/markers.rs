//! Completion and failure marker types for relaxed consistency uploads.

use rusty_attachments_model::HashAlgorithm;
use serde::{Deserialize, Serialize};

/// Completion marker written by the upload agent after uploading a file.
/// Stored at the PendingUploads S3 key for the path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadCompletionMarker {
    /// Status field to distinguish from failure markers.
    pub status: String,
    /// The CAS hash of the uploaded file content.
    pub content_hash: String,
    /// Hash algorithm used.
    pub hash_algorithm: HashAlgorithm,
    /// File size in bytes.
    pub size: u64,
    /// Upload timestamp (epoch seconds).
    pub uploaded_at: f64,
    /// The original relative path (for debugging/auditing).
    pub relative_path: String,
    /// Chunk hashes if the file was chunked (>256MB).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_hashes: Option<Vec<String>>,
}

/// Failure marker written when the upload agent cannot find or upload the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFailureMarker {
    /// Status field to distinguish from completion markers.
    pub status: String,
    /// Reason for failure.
    pub reason: String,
    /// Timestamp of the failure (epoch seconds).
    pub failed_at: f64,
    /// The original relative path.
    pub relative_path: String,
}

/// Union type for reading markers from S3 — could be success or failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerEnvelope {
    /// "completed" or "failed".
    pub status: String,
    // Completion fields (present when status == "completed")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_algorithm: Option<HashAlgorithm>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uploaded_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_hashes: Option<Vec<String>>,
    // Failure fields (present when status == "failed")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<f64>,
}

/// SQS message body for a file upload request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadRequest {
    /// Version of the message format.
    pub version: String,
    /// The relaxed root this file belongs to.
    pub root_id: String,
    /// The source path on the submitter's machine.
    pub source_root_path: String,
    /// Relative path within the root.
    pub relative_path: String,
    /// The path key (XXH128 of relative_path).
    pub path_key: String,
    /// S3 bucket for CAS upload and completion marker.
    pub bucket: String,
    /// S3 root prefix (e.g., "DeadlineCloud").
    pub root_prefix: String,
    /// Job ID requesting this file.
    pub job_id: String,
    /// Timestamp of the request (epoch seconds).
    pub requested_at: f64,
    /// Priority hint.
    pub priority: String,
}

/// Configuration for a relaxed consistency root, passed to the VFS at mount time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaxedRootConfig {
    /// Stable identifier for this root (SHAKE-256 of source_path).
    pub root_id: String,
    /// Source path on the submitter's machine.
    pub source_path: String,
    /// Mount path within the VFS (after path mapping).
    /// - With storage profile: worker-side path from the profile (e.g., "/mnt/worker/assets")
    /// - Without storage profile: dynamic name (e.g., "assetroot-a1b2c3d4e5")
    pub mount_path: String,
    /// File system location name from storage profile (None if no profile).
    /// When set, the Deadline service uses this to resolve the worker-side mount path
    /// via storage profile path mapping rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_system_location_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completion_marker_roundtrip() {
        let marker = UploadCompletionMarker {
            status: "completed".to_string(),
            content_hash: "abc123def456".to_string(),
            hash_algorithm: HashAlgorithm::Xxh128,
            size: 1024,
            uploaded_at: 1711036800.0,
            relative_path: "project/file.png".to_string(),
            chunk_hashes: None,
        };

        let json: String = serde_json::to_string(&marker).unwrap();
        let parsed: UploadCompletionMarker = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.content_hash, "abc123def456");
        assert_eq!(parsed.size, 1024);
    }

    #[test]
    fn test_failure_marker_roundtrip() {
        let marker = UploadFailureMarker {
            status: "failed".to_string(),
            reason: "File not found".to_string(),
            failed_at: 1711036800.0,
            relative_path: "project/missing.png".to_string(),
        };

        let json: String = serde_json::to_string(&marker).unwrap();
        let parsed: UploadFailureMarker = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reason, "File not found");
    }

    #[test]
    fn test_envelope_completed() {
        let json: &str = r#"{
            "status": "completed",
            "contentHash": "abc123",
            "hashAlgorithm": "xxh128",
            "size": 2048,
            "uploadedAt": 1711036800.0,
            "relativePath": "tex/diffuse.png"
        }"#;

        let envelope: MarkerEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.status, "completed");
        assert_eq!(envelope.content_hash.as_deref(), Some("abc123"));
        assert_eq!(envelope.size, Some(2048));
    }

    #[test]
    fn test_envelope_failed() {
        let json: &str = r#"{
            "status": "failed",
            "reason": "Permission denied",
            "failedAt": 1711036800.0,
            "relativePath": "secret/file.dat"
        }"#;

        let envelope: MarkerEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.status, "failed");
        assert_eq!(envelope.reason.as_deref(), Some("Permission denied"));
    }

    #[test]
    fn test_upload_request_roundtrip() {
        let req = FileUploadRequest {
            version: "2026-03-21".to_string(),
            root_id: "a1b2c3d4e5".to_string(),
            source_root_path: "/mnt/shared".to_string(),
            relative_path: "project/file.png".to_string(),
            path_key: "7f3a9b2c1d4e5f6a".to_string(),
            bucket: "my-bucket".to_string(),
            root_prefix: "DeadlineCloud".to_string(),
            job_id: "job-123".to_string(),
            requested_at: 1711036800.0,
            priority: "high".to_string(),
        };

        let json: String = serde_json::to_string(&req).unwrap();
        let parsed: FileUploadRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.root_id, "a1b2c3d4e5");
        assert_eq!(parsed.relative_path, "project/file.png");
    }

    #[test]
    fn test_relaxed_root_config_roundtrip() {
        let config = RelaxedRootConfig {
            root_id: "a1b2c3d4e5".to_string(),
            source_path: "/mnt/shared".to_string(),
            mount_path: "shared".to_string(),
            file_system_location_name: None,
        };

        let json: String = serde_json::to_string(&config).unwrap();
        let parsed: RelaxedRootConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.root_id, "a1b2c3d4e5");
        assert_eq!(parsed.mount_path, "shared");
        assert!(parsed.file_system_location_name.is_none());
    }

    #[test]
    fn test_relaxed_root_config_with_storage_profile() {
        let config = RelaxedRootConfig {
            root_id: "a1b2c3d4e5".to_string(),
            source_path: "/mnt/shared/assets".to_string(),
            mount_path: "/mnt/worker/assets".to_string(),
            file_system_location_name: Some("StudioAssets".to_string()),
        };

        let json: String = serde_json::to_string(&config).unwrap();
        let parsed: RelaxedRootConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.mount_path, "/mnt/worker/assets");
        assert_eq!(
            parsed.file_system_location_name.as_deref(),
            Some("StudioAssets")
        );
    }

    #[test]
    fn test_relaxed_root_config_without_profile_omits_field() {
        // When file_system_location_name is None, it should be omitted from JSON
        let config = RelaxedRootConfig {
            root_id: "abc".to_string(),
            source_path: "/mnt/scratch".to_string(),
            mount_path: "assetroot-abc123".to_string(),
            file_system_location_name: None,
        };

        let json: String = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("fileSystemLocationName"));
    }

    #[test]
    fn test_relaxed_root_config_deserialize_missing_location_name() {
        // JSON without fileSystemLocationName should deserialize with None
        let json: &str = r#"{
            "rootId": "abc",
            "sourcePath": "/mnt/scratch",
            "mountPath": "assetroot-abc"
        }"#;

        let parsed: RelaxedRootConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.file_system_location_name.is_none());
    }
}
