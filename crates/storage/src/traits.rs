//! Storage traits/interfaces for S3 operations.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::error::StorageError;
use crate::types::TransferProgress;

/// Callback trait for progress reporting.
pub trait ProgressCallback: Send + Sync {
    /// Called with progress updates.
    /// Returns false to cancel the operation.
    fn on_progress(&self, progress: &TransferProgress) -> bool;
}

/// Information about an S3 object from list/head operations.
#[derive(Debug, Clone)]
pub struct ObjectInfo {
    /// S3 object key.
    pub key: String,
    /// Object size in bytes.
    pub size: u64,
    /// Last modified timestamp (Unix epoch seconds).
    pub last_modified: Option<i64>,
    /// ETag (usually MD5 hash for non-multipart uploads).
    pub etag: Option<String>,
}

/// Low-level S3 operations - implemented by each backend.
#[async_trait]
pub trait StorageClient: Send + Sync {
    /// Get the expected bucket owner account ID for security validation.
    /// When set, all S3 operations should include ExpectedBucketOwner parameter.
    fn expected_bucket_owner(&self) -> Option<&str>;

    /// Check if an object exists and return its size.
    /// Returns None if object doesn't exist.
    /// Implementations should include ExpectedBucketOwner if configured.
    async fn head_object(&self, bucket: &str, key: &str) -> Result<Option<u64>, StorageError>;

    /// Upload bytes to S3.
    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        data: &[u8],
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
    ) -> Result<(), StorageError>;

    /// Upload from file path to S3 (for large files, enables streaming).
    async fn put_object_from_file(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;

    /// Upload a byte range from file to S3 (for chunked uploads).
    async fn put_object_from_file_range(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        offset: u64,
        length: u64,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;

    /// Download object to bytes.
    async fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError>;

    /// Download object to file path (for large files, enables streaming).
    async fn get_object_to_file(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;

    /// Download object to file at specific offset (for chunked downloads).
    async fn get_object_to_file_offset(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        offset: u64,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;

    /// List objects with prefix.
    async fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> Result<Vec<ObjectInfo>, StorageError>;
}
