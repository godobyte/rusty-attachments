//! AWS S3 Transfer Manager client implementation.
//!
//! Provides high-performance S3 operations using the AWS S3 Transfer Manager,
//! which automatically handles multipart uploads and parallel byte-range downloads.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::Path;

use async_trait::async_trait;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3_transfer_manager::io::InputStream;
use aws_sdk_s3_transfer_manager::Client as TransferManager;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use rusty_attachments_storage::{
    ObjectInfo, ObjectMetadata, ProgressCallback, StorageClient, StorageError, StorageSettings,
};

use crate::config::S3Config;

/// StorageClient implementation using AWS S3 Transfer Manager.
///
/// Provides high-performance S3 operations with:
/// - Automatic multipart uploads (no manual chunking needed)
/// - Parallel byte-range downloads for faster throughput
/// - Optimized memory usage for large files
///
/// For operations not optimized by the Transfer Manager (HEAD, LIST, range uploads),
/// delegates to the underlying S3 client directly.
pub struct TransferManagerClient {
    /// Underlying S3 client for simple operations (HEAD, LIST, range uploads).
    s3_client: S3Client,
    /// Transfer manager for high-performance uploads/downloads.
    transfer_manager: TransferManager,
    /// Expected bucket owner for security validation.
    expected_bucket_owner: Option<String>,
}

impl TransferManagerClient {
    /// Create a new transfer manager client.
    ///
    /// # Arguments
    /// * `settings` - Storage settings including region and credentials
    ///
    /// # Returns
    /// A new transfer manager client configured for high-performance S3 operations.
    pub async fn new(settings: StorageSettings) -> Result<Self, StorageError> {
        let config = S3Config::from_settings(settings).await?;
        let s3_client = S3Client::new(&config.sdk_config);

        let tm_config: aws_sdk_s3_transfer_manager::Config =
            aws_sdk_s3_transfer_manager::from_env().load().await;
        let transfer_manager = TransferManager::new(tm_config);

        Ok(Self {
            s3_client,
            transfer_manager,
            expected_bucket_owner: config.expected_bucket_owner,
        })
    }

    /// Create a client from existing S3 client and transfer manager (for testing).
    ///
    /// # Arguments
    /// * `s3_client` - Pre-configured S3 client
    /// * `transfer_manager` - Pre-configured transfer manager
    /// * `expected_bucket_owner` - Optional expected bucket owner
    pub fn from_components(
        s3_client: S3Client,
        transfer_manager: TransferManager,
        expected_bucket_owner: Option<String>,
    ) -> Self {
        Self {
            s3_client,
            transfer_manager,
            expected_bucket_owner,
        }
    }
}

#[async_trait]
impl StorageClient for TransferManagerClient {
    fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

    /// Check if an object exists and return its size (delegates to S3 client).
    async fn head_object(&self, bucket: &str, key: &str) -> Result<Option<u64>, StorageError> {
        let mut request = self.s3_client.head_object().bucket(bucket).key(key);

        if let Some(ref owner) = self.expected_bucket_owner {
            request = request.expected_bucket_owner(owner);
        }

        match request.send().await {
            Ok(output) => Ok(output.content_length().map(|l| l as u64)),
            Err(err) => {
                let service_err = err.into_service_error();
                if service_err.is_not_found() {
                    Ok(None)
                } else {
                    Err(StorageError::NetworkError {
                        message: service_err.to_string(),
                        retryable: false,
                    })
                }
            }
        }
    }

    /// Get extended object metadata (delegates to S3 client).
    async fn head_object_with_metadata(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<Option<ObjectMetadata>, StorageError> {
        let mut request = self.s3_client.head_object().bucket(bucket).key(key);

        if let Some(ref owner) = self.expected_bucket_owner {
            request = request.expected_bucket_owner(owner);
        }

        match request.send().await {
            Ok(output) => {
                let user_metadata: HashMap<String, String> = output
                    .metadata()
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();

                let last_modified: Option<i64> = output
                    .last_modified()
                    .and_then(|dt| dt.to_millis().ok())
                    .map(|ms| ms / 1000);

                Ok(Some(ObjectMetadata {
                    size: output.content_length().map(|l| l as u64).unwrap_or(0),
                    last_modified,
                    content_type: output.content_type().map(|s| s.to_string()),
                    etag: output.e_tag().map(|s| s.to_string()),
                    user_metadata,
                }))
            }
            Err(err) => {
                let service_err = err.into_service_error();
                if service_err.is_not_found() {
                    Ok(None)
                } else {
                    Err(StorageError::NetworkError {
                        message: service_err.to_string(),
                        retryable: false,
                    })
                }
            }
        }
    }

    /// Upload bytes to S3 using Transfer Manager (auto multipart for large payloads).
    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        data: &[u8],
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
    ) -> Result<(), StorageError> {
        let stream = InputStream::from(bytes::Bytes::copy_from_slice(data));

        let mut upload = self
            .transfer_manager
            .upload()
            .bucket(bucket)
            .key(key)
            .body(stream);

        if let Some(ref owner) = self.expected_bucket_owner {
            upload = upload.expected_bucket_owner(owner);
        }

        if let Some(ct) = content_type {
            upload = upload.content_type(ct);
        }

        if let Some(meta) = metadata {
            for (k, v) in meta {
                upload = upload.metadata(k, v);
            }
        }

        let handle = upload
            .initiate()
            .map_err(|e| StorageError::NetworkError {
                message: format!("Failed to initiate upload: {}", e),
                retryable: true,
            })?;

        handle
            .join()
            .await
            .map_err(|e| StorageError::NetworkError {
                message: format!("Upload failed: {}", e),
                retryable: true,
            })?;

        Ok(())
    }

    /// Upload from file path using Transfer Manager (streaming, auto multipart).
    async fn put_object_from_file(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        let stream = InputStream::from_path(file_path).map_err(|e| StorageError::IoError {
            path: file_path.to_string(),
            message: e.to_string(),
        })?;

        let mut upload = self
            .transfer_manager
            .upload()
            .bucket(bucket)
            .key(key)
            .body(stream);

        if let Some(ref owner) = self.expected_bucket_owner {
            upload = upload.expected_bucket_owner(owner);
        }

        if let Some(ct) = content_type {
            upload = upload.content_type(ct);
        }

        if let Some(meta) = metadata {
            for (k, v) in meta {
                upload = upload.metadata(k, v);
            }
        }

        let handle = upload
            .initiate()
            .map_err(|e| StorageError::NetworkError {
                message: format!("Failed to initiate file upload: {}", e),
                retryable: true,
            })?;

        handle
            .join()
            .await
            .map_err(|e| StorageError::NetworkError {
                message: format!("File upload failed: {}", e),
                retryable: true,
            })?;

        Ok(())
    }

    /// Upload a byte range from file (delegates to S3 client, not optimized by TM).
    async fn put_object_from_file_range(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        offset: u64,
        length: u64,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        // Transfer Manager doesn't optimize range uploads, use S3 client directly
        let mut file = File::open(file_path)
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        let mut buffer: Vec<u8> = vec![0u8; length as usize];
        tokio::io::AsyncReadExt::read_exact(&mut file, &mut buffer)
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        let body = aws_sdk_s3::primitives::ByteStream::from(buffer);

        let mut request = self
            .s3_client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body);

        if let Some(ref owner) = self.expected_bucket_owner {
            request = request.expected_bucket_owner(owner);
        }

        request
            .send()
            .await
            .map_err(|err| StorageError::NetworkError {
                message: err.to_string(),
                retryable: true,
            })?;

        Ok(())
    }

    /// Download object to bytes using Transfer Manager (parallel byte-range).
    async fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError> {
        let mut download = self
            .transfer_manager
            .download()
            .bucket(bucket)
            .key(key);

        if let Some(ref owner) = self.expected_bucket_owner {
            download = download.expected_bucket_owner(owner);
        }

        let mut handle = download
            .initiate()
            .map_err(|e| StorageError::NetworkError {
                message: format!("Failed to initiate download: {}", e),
                retryable: true,
            })?;

        let mut data: Vec<u8> = Vec::new();
        while let Some(chunk_result) = handle.body_mut().next().await {
            let chunk = chunk_result.map_err(|e| StorageError::NetworkError {
                message: format!("Download chunk failed: {}", e),
                retryable: true,
            })?;
            data.extend_from_slice(&chunk.data.into_bytes());
        }

        Ok(data)
    }

    /// Download object to file using Transfer Manager (streaming).
    async fn get_object_to_file(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        // Create parent directories if needed
        if let Some(parent) = Path::new(file_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::IoError {
                    path: parent.display().to_string(),
                    message: e.to_string(),
                })?;
        }

        let mut download = self
            .transfer_manager
            .download()
            .bucket(bucket)
            .key(key);

        if let Some(ref owner) = self.expected_bucket_owner {
            download = download.expected_bucket_owner(owner);
        }

        let mut handle = download
            .initiate()
            .map_err(|e| StorageError::NetworkError {
                message: format!("Failed to initiate download: {}", e),
                retryable: true,
            })?;

        let mut file = File::create(file_path)
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        while let Some(chunk_result) = handle.body_mut().next().await {
            let chunk = chunk_result.map_err(|e| StorageError::NetworkError {
                message: format!("Download chunk failed: {}", e),
                retryable: true,
            })?;
            file.write_all(&chunk.data.into_bytes())
                .await
                .map_err(|e| StorageError::IoError {
                    path: file_path.to_string(),
                    message: e.to_string(),
                })?;
        }

        file.flush().await.map_err(|e| StorageError::IoError {
            path: file_path.to_string(),
            message: e.to_string(),
        })?;

        Ok(())
    }

    /// Download object to file at specific offset using Transfer Manager.
    async fn get_object_to_file_offset(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        offset: u64,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        // Create parent directories if needed
        if let Some(parent) = Path::new(file_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::IoError {
                    path: parent.display().to_string(),
                    message: e.to_string(),
                })?;
        }

        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(file_path)
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        let mut download = self
            .transfer_manager
            .download()
            .bucket(bucket)
            .key(key);

        if let Some(ref owner) = self.expected_bucket_owner {
            download = download.expected_bucket_owner(owner);
        }

        let mut handle = download
            .initiate()
            .map_err(|e| StorageError::NetworkError {
                message: format!("Failed to initiate download: {}", e),
                retryable: true,
            })?;

        while let Some(chunk_result) = handle.body_mut().next().await {
            let chunk = chunk_result.map_err(|e| StorageError::NetworkError {
                message: format!("Download chunk failed: {}", e),
                retryable: true,
            })?;
            file.write_all(&chunk.data.into_bytes())
                .await
                .map_err(|e| StorageError::IoError {
                    path: file_path.to_string(),
                    message: e.to_string(),
                })?;
        }

        file.flush().await.map_err(|e| StorageError::IoError {
            path: file_path.to_string(),
            message: e.to_string(),
        })?;

        Ok(())
    }

    /// List objects with prefix (delegates to S3 client).
    async fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> Result<Vec<ObjectInfo>, StorageError> {
        let mut objects: Vec<ObjectInfo> = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut request = self
                .s3_client
                .list_objects_v2()
                .bucket(bucket)
                .prefix(prefix);

            if let Some(ref owner) = self.expected_bucket_owner {
                request = request.expected_bucket_owner(owner);
            }

            if let Some(ref token) = continuation_token {
                request = request.continuation_token(token);
            }

            let response = request
                .send()
                .await
                .map_err(|err| StorageError::NetworkError {
                    message: err.to_string(),
                    retryable: true,
                })?;

            if let Some(ref contents) = response.contents {
                for obj in contents {
                    let last_modified: Option<i64> = obj
                        .last_modified()
                        .and_then(|dt| dt.to_millis().ok())
                        .map(|ms| ms / 1000);

                    objects.push(ObjectInfo {
                        key: obj.key().unwrap_or_default().to_string(),
                        size: obj.size().map(|s| s as u64).unwrap_or(0),
                        last_modified,
                        etag: obj.e_tag().map(|s| s.to_string()),
                    });
                }
            }

            if response.is_truncated() == Some(true) {
                continuation_token = response.next_continuation_token.clone();
            } else {
                break;
            }
        }

        Ok(objects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transfer_manager_client_implements_storage_client() {
        fn assert_storage_client<T: StorageClient>() {}
        assert_storage_client::<TransferManagerClient>();
    }
}
