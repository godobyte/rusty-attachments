//! AWS SDK S3 client implementation.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::Path;

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use rusty_attachments_storage::{
    ObjectInfo, ObjectMetadata, ProgressCallback, StorageClient, StorageError, StorageSettings,
};

/// StorageClient implementation using AWS SDK for Rust.
///
/// This client provides high-performance S3 operations with automatic retry,
/// connection pooling, and streaming support for large files.
pub struct CrtStorageClient {
    /// The underlying S3 client.
    s3_client: S3Client,
    /// Expected bucket owner for security validation.
    expected_bucket_owner: Option<String>,
}

impl CrtStorageClient {
    /// Create a new CRT storage client with default credential chain.
    ///
    /// # Arguments
    /// * `settings` - Storage settings including region and optional credentials
    ///
    /// # Returns
    /// A new CRT storage client.
    pub async fn new(settings: StorageSettings) -> Result<Self, StorageError> {
        let config_loader = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(settings.region.clone()));

        let config_loader = if let Some(ref creds) = settings.credentials {
            let credentials = Credentials::new(
                &creds.access_key_id,
                &creds.secret_access_key,
                creds.session_token.clone(),
                None,
                "rusty-attachments",
            );
            config_loader.credentials_provider(credentials)
        } else {
            config_loader
        };

        let sdk_config = config_loader.load().await;
        let s3_client = S3Client::new(&sdk_config);

        Ok(Self {
            s3_client,
            expected_bucket_owner: settings.expected_bucket_owner,
        })
    }

    /// Create a client from an existing S3Client (for testing).
    ///
    /// # Arguments
    /// * `s3_client` - Pre-configured S3 client
    /// * `expected_bucket_owner` - Optional expected bucket owner
    pub fn from_client(s3_client: S3Client, expected_bucket_owner: Option<String>) -> Self {
        Self {
            s3_client,
            expected_bucket_owner,
        }
    }
}

#[async_trait]
impl StorageClient for CrtStorageClient {
    fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

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

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        data: &[u8],
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
    ) -> Result<(), StorageError> {
        let body = ByteStream::from(data.to_vec());

        let mut request = self
            .s3_client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body);

        if let Some(ref owner) = self.expected_bucket_owner {
            request = request.expected_bucket_owner(owner);
        }

        if let Some(ct) = content_type {
            request = request.content_type(ct);
        }

        if let Some(meta) = metadata {
            for (k, v) in meta {
                request = request.metadata(k, v);
            }
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

    async fn put_object_from_file(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        content_type: Option<&str>,
        metadata: Option<&HashMap<String, String>>,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        let body = ByteStream::from_path(Path::new(file_path))
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        let mut request = self
            .s3_client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body);

        if let Some(ref owner) = self.expected_bucket_owner {
            request = request.expected_bucket_owner(owner);
        }

        if let Some(ct) = content_type {
            request = request.content_type(ct);
        }

        if let Some(meta) = metadata {
            for (k, v) in meta {
                request = request.metadata(k, v);
            }
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

    async fn put_object_from_file_range(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        offset: u64,
        length: u64,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        // Read the specific range from the file
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
        file.read_exact(&mut buffer)
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        let body = ByteStream::from(buffer);

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

    async fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError> {
        let mut request = self.s3_client.get_object().bucket(bucket).key(key);

        if let Some(ref owner) = self.expected_bucket_owner {
            request = request.expected_bucket_owner(owner);
        }

        let response = request.send().await.map_err(|err| {
            let service_err = err.into_service_error();
            if service_err.is_no_such_key() {
                StorageError::NotFound {
                    bucket: bucket.to_string(),
                    key: key.to_string(),
                }
            } else {
                StorageError::NetworkError {
                    message: service_err.to_string(),
                    retryable: true,
                }
            }
        })?;

        let data: Vec<u8> = response
            .body
            .collect()
            .await
            .map_err(|e| StorageError::NetworkError {
                message: e.to_string(),
                retryable: true,
            })?
            .into_bytes()
            .to_vec();

        Ok(data)
    }

    async fn get_object_to_file(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        let mut request = self.s3_client.get_object().bucket(bucket).key(key);

        if let Some(ref owner) = self.expected_bucket_owner {
            request = request.expected_bucket_owner(owner);
        }

        let response = request.send().await.map_err(|err| {
            let service_err = err.into_service_error();
            if service_err.is_no_such_key() {
                StorageError::NotFound {
                    bucket: bucket.to_string(),
                    key: key.to_string(),
                }
            } else {
                StorageError::NetworkError {
                    message: service_err.to_string(),
                    retryable: true,
                }
            }
        })?;

        // Create parent directories if needed
        if let Some(parent) = Path::new(file_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::IoError {
                    path: parent.display().to_string(),
                    message: e.to_string(),
                })?;
        }

        let mut file = File::create(file_path)
            .await
            .map_err(|e| StorageError::IoError {
                path: file_path.to_string(),
                message: e.to_string(),
            })?;

        let mut body = response.body;
        while let Some(chunk) = body
            .try_next()
            .await
            .map_err(|e| StorageError::NetworkError {
                message: e.to_string(),
                retryable: true,
            })?
        {
            file.write_all(&chunk)
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

    async fn get_object_to_file_offset(
        &self,
        bucket: &str,
        key: &str,
        file_path: &str,
        offset: u64,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        let mut request = self.s3_client.get_object().bucket(bucket).key(key);

        if let Some(ref owner) = self.expected_bucket_owner {
            request = request.expected_bucket_owner(owner);
        }

        let response = request.send().await.map_err(|err| {
            let service_err = err.into_service_error();
            if service_err.is_no_such_key() {
                StorageError::NotFound {
                    bucket: bucket.to_string(),
                    key: key.to_string(),
                }
            } else {
                StorageError::NetworkError {
                    message: service_err.to_string(),
                    retryable: true,
                }
            }
        })?;

        // Create parent directories if needed
        if let Some(parent) = Path::new(file_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::IoError {
                    path: parent.display().to_string(),
                    message: e.to_string(),
                })?;
        }

        // Open file for writing at offset (create if doesn't exist)
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

        let mut body = response.body;
        while let Some(chunk) = body
            .try_next()
            .await
            .map_err(|e| StorageError::NetworkError {
                message: e.to_string(),
                retryable: true,
            })?
        {
            file.write_all(&chunk)
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
    fn test_crt_client_expected_bucket_owner() {
        // This is a compile-time test to ensure the trait is implemented correctly
        fn assert_storage_client<T: StorageClient>() {}
        assert_storage_client::<CrtStorageClient>();
    }
}
