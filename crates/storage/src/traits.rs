//! Storage traits/interfaces for S3 operations.

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use rusty_attachments_model::HashAlgorithm;

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

/// Extended object metadata from HEAD operations.
#[derive(Debug, Clone, Default)]
pub struct ObjectMetadata {
    /// Object size in bytes.
    pub size: u64,
    /// Last modified timestamp (Unix epoch seconds).
    pub last_modified: Option<i64>,
    /// Content type.
    pub content_type: Option<String>,
    /// ETag (usually MD5 hash for non-multipart uploads).
    pub etag: Option<String>,
    /// User-defined metadata (x-amz-meta-* headers).
    pub user_metadata: HashMap<String, String>,
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

    /// Get extended object metadata including user-defined metadata.
    ///
    /// Returns None if object doesn't exist.
    /// Implementations should include ExpectedBucketOwner if configured.
    ///
    /// # Arguments
    /// * `bucket` - S3 bucket name
    /// * `key` - S3 object key
    ///
    /// # Returns
    /// Object metadata including size, last_modified, content_type, and user metadata.
    async fn head_object_with_metadata(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<Option<ObjectMetadata>, StorageError>;

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

/// Content-addressable data cache abstraction.
///
/// Provides a unified interface for storing and retrieving content
/// by hash, regardless of the underlying storage backend (S3 or filesystem).
#[async_trait]
pub trait ContentAddressedDataCache: Send + Sync {
    /// Get the storage key for a given hash.
    ///
    /// # Arguments
    /// * `hash` - Content hash value
    /// * `algorithm` - Hash algorithm used
    ///
    /// # Returns
    /// Storage key string (e.g., "Data/abc123.xxh128" for S3, "abc123.xxh128" for filesystem).
    fn get_object_key(&self, hash: &str, algorithm: HashAlgorithm) -> String;

    /// Check if an object exists in the cache.
    ///
    /// # Arguments
    /// * `hash` - Content hash value
    /// * `algorithm` - Hash algorithm used
    ///
    /// # Returns
    /// True if the object exists.
    async fn object_exists(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<bool, StorageError>;

    /// Get object size if it exists.
    ///
    /// # Arguments
    /// * `hash` - Content hash value
    /// * `algorithm` - Hash algorithm used
    ///
    /// # Returns
    /// Object size in bytes, or None if not found.
    async fn object_size(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<Option<u64>, StorageError>;

    /// Upload content to the cache.
    ///
    /// # Arguments
    /// * `hash` - Content hash value
    /// * `algorithm` - Hash algorithm used
    /// * `data` - Content bytes to upload
    async fn put_object(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        data: &[u8],
    ) -> Result<(), StorageError>;

    /// Upload content from a file.
    ///
    /// # Arguments
    /// * `hash` - Content hash value
    /// * `algorithm` - Hash algorithm used
    /// * `file_path` - Path to file to upload
    /// * `progress` - Optional progress callback
    async fn put_object_from_file(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        file_path: &Path,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;

    /// Download content from the cache.
    ///
    /// # Arguments
    /// * `hash` - Content hash value
    /// * `algorithm` - Hash algorithm used
    ///
    /// # Returns
    /// Content bytes.
    async fn get_object(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<Vec<u8>, StorageError>;

    /// Download content to a file.
    ///
    /// # Arguments
    /// * `hash` - Content hash value
    /// * `algorithm` - Hash algorithm used
    /// * `file_path` - Destination file path
    /// * `progress` - Optional progress callback
    async fn get_object_to_file(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        file_path: &Path,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError>;
}
