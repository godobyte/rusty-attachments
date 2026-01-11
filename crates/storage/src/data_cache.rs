//! Content-addressable data cache implementations.
//!
//! Provides S3 and filesystem backends for the `ContentAddressedDataCache` trait.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use rusty_attachments_model::HashAlgorithm;

use crate::error::StorageError;
use crate::s3_check_cache::S3CheckCache;
use crate::traits::{ContentAddressedDataCache, ProgressCallback, StorageClient};

/// S3-backed content-addressable data cache.
///
/// Stores content in S3 with keys formatted as `{prefix}/{hash}.{algorithm}`.
pub struct S3DataCache<'a, C: StorageClient> {
    client: &'a C,
    bucket: String,
    key_prefix: String,
    s3_check_cache: Option<&'a S3CheckCache>,
}

impl<'a, C: StorageClient> S3DataCache<'a, C> {
    /// Create a new S3 data cache.
    ///
    /// # Arguments
    /// * `client` - S3 storage client
    /// * `bucket` - S3 bucket name
    /// * `key_prefix` - Prefix for all keys (e.g., "Data")
    pub fn new(client: &'a C, bucket: impl Into<String>, key_prefix: impl Into<String>) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            key_prefix: key_prefix.into(),
            s3_check_cache: None,
        }
    }

    /// Add an S3 check cache for existence lookups.
    pub fn with_check_cache(mut self, cache: &'a S3CheckCache) -> Self {
        self.s3_check_cache = Some(cache);
        self
    }
}

#[async_trait]
impl<C: StorageClient> ContentAddressedDataCache for S3DataCache<'_, C> {
    fn get_object_key(&self, hash: &str, algorithm: HashAlgorithm) -> String {
        format!("{}/{}.{}", self.key_prefix, hash, algorithm.extension())
    }

    async fn object_exists(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<bool, StorageError> {
        let key: String = self.get_object_key(hash, algorithm);

        // Check local cache first
        if let Some(cache) = self.s3_check_cache {
            if cache.exists(&self.bucket, &key).await {
                return Ok(true);
            }
        }

        // Check S3
        let exists: bool = self.client.head_object(&self.bucket, &key).await?.is_some();

        // Update cache if exists
        if exists {
            if let Some(cache) = self.s3_check_cache {
                cache.mark_uploaded(&self.bucket, &key).await;
            }
        }

        Ok(exists)
    }

    async fn object_size(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<Option<u64>, StorageError> {
        let key: String = self.get_object_key(hash, algorithm);
        self.client.head_object(&self.bucket, &key).await
    }

    async fn put_object(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        data: &[u8],
    ) -> Result<(), StorageError> {
        let key: String = self.get_object_key(hash, algorithm);
        self.client
            .put_object(&self.bucket, &key, data, None, None)
            .await?;

        // Update cache
        if let Some(cache) = self.s3_check_cache {
            cache.mark_uploaded(&self.bucket, &key).await;
        }

        Ok(())
    }

    async fn put_object_from_file(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        file_path: &Path,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        let key: String = self.get_object_key(hash, algorithm);
        let file_path_str: &str = file_path.to_str().ok_or_else(|| StorageError::IoError {
            path: file_path.display().to_string(),
            message: "Path contains invalid UTF-8".to_string(),
        })?;

        self.client
            .put_object_from_file(&self.bucket, &key, file_path_str, None, None, progress)
            .await?;

        // Update cache
        if let Some(cache) = self.s3_check_cache {
            cache.mark_uploaded(&self.bucket, &key).await;
        }

        Ok(())
    }

    async fn get_object(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<Vec<u8>, StorageError> {
        let key: String = self.get_object_key(hash, algorithm);
        self.client.get_object(&self.bucket, &key).await
    }

    async fn get_object_to_file(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        file_path: &Path,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        let key: String = self.get_object_key(hash, algorithm);
        let file_path_str: &str = file_path.to_str().ok_or_else(|| StorageError::IoError {
            path: file_path.display().to_string(),
            message: "Path contains invalid UTF-8".to_string(),
        })?;

        self.client
            .get_object_to_file(&self.bucket, &key, file_path_str, progress)
            .await
    }
}

/// Local filesystem content-addressable data cache.
///
/// Stores content in a directory with files named `{hash}.{algorithm}`.
pub struct FileSystemDataCache {
    root_path: PathBuf,
}

impl FileSystemDataCache {
    /// Create a new filesystem data cache.
    ///
    /// # Arguments
    /// * `root_path` - Root directory for the cache (must be absolute)
    ///
    /// # Errors
    /// Returns error if path is not absolute.
    pub fn new(root_path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root: PathBuf = root_path.into();
        if !root.is_absolute() {
            return Err(StorageError::InvalidConfig {
                message: format!(
                    "FileSystemDataCache root must be absolute: {}",
                    root.display()
                ),
            });
        }
        Ok(Self { root_path: root })
    }

    /// Get the full path for a cached object.
    fn get_full_path(&self, hash: &str, algorithm: HashAlgorithm) -> PathBuf {
        self.root_path
            .join(format!("{}.{}", hash, algorithm.extension()))
    }
}

#[async_trait]
impl ContentAddressedDataCache for FileSystemDataCache {
    fn get_object_key(&self, hash: &str, algorithm: HashAlgorithm) -> String {
        format!("{}.{}", hash, algorithm.extension())
    }

    async fn object_exists(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<bool, StorageError> {
        let path: PathBuf = self.get_full_path(hash, algorithm);
        Ok(path.exists())
    }

    async fn object_size(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<Option<u64>, StorageError> {
        let path: PathBuf = self.get_full_path(hash, algorithm);
        match std::fs::metadata(&path) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StorageError::IoError {
                path: path.display().to_string(),
                message: e.to_string(),
            }),
        }
    }

    async fn put_object(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        data: &[u8],
    ) -> Result<(), StorageError> {
        let path: PathBuf = self.get_full_path(hash, algorithm);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::IoError {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }

        std::fs::write(&path, data).map_err(|e| StorageError::IoError {
            path: path.display().to_string(),
            message: e.to_string(),
        })
    }

    async fn put_object_from_file(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        file_path: &Path,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        let dest_path: PathBuf = self.get_full_path(hash, algorithm);

        // Ensure parent directory exists
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::IoError {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }

        std::fs::copy(file_path, &dest_path).map_err(|e| StorageError::IoError {
            path: file_path.display().to_string(),
            message: e.to_string(),
        })?;

        Ok(())
    }

    async fn get_object(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
    ) -> Result<Vec<u8>, StorageError> {
        let path: PathBuf = self.get_full_path(hash, algorithm);
        std::fs::read(&path).map_err(|e| StorageError::IoError {
            path: path.display().to_string(),
            message: e.to_string(),
        })
    }

    async fn get_object_to_file(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        file_path: &Path,
        _progress: Option<&dyn ProgressCallback>,
    ) -> Result<(), StorageError> {
        let src_path: PathBuf = self.get_full_path(hash, algorithm);

        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::IoError {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }

        std::fs::copy(&src_path, file_path).map_err(|e| StorageError::IoError {
            path: src_path.display().to_string(),
            message: e.to_string(),
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_filesystem_cache_put_get() {
        let dir: TempDir = TempDir::new().unwrap();
        let cache: FileSystemDataCache =
            FileSystemDataCache::new(dir.path().to_path_buf()).unwrap();

        let hash: &str = "abc123";
        let data: &[u8] = b"test content";

        // Put object
        cache
            .put_object(hash, HashAlgorithm::Xxh128, data)
            .await
            .unwrap();

        // Check exists
        assert!(cache
            .object_exists(hash, HashAlgorithm::Xxh128)
            .await
            .unwrap());

        // Get object
        let retrieved: Vec<u8> = cache.get_object(hash, HashAlgorithm::Xxh128).await.unwrap();
        assert_eq!(retrieved, data);
    }

    #[tokio::test]
    async fn test_filesystem_cache_object_size() {
        let dir: TempDir = TempDir::new().unwrap();
        let cache: FileSystemDataCache =
            FileSystemDataCache::new(dir.path().to_path_buf()).unwrap();

        let hash: &str = "def456";
        let data: &[u8] = b"some test data here";

        cache
            .put_object(hash, HashAlgorithm::Xxh128, data)
            .await
            .unwrap();

        let size: Option<u64> = cache
            .object_size(hash, HashAlgorithm::Xxh128)
            .await
            .unwrap();
        assert_eq!(size, Some(data.len() as u64));
    }

    #[tokio::test]
    async fn test_filesystem_cache_not_exists() {
        let dir: TempDir = TempDir::new().unwrap();
        let cache: FileSystemDataCache =
            FileSystemDataCache::new(dir.path().to_path_buf()).unwrap();

        assert!(!cache
            .object_exists("nonexistent", HashAlgorithm::Xxh128)
            .await
            .unwrap());
        assert!(cache
            .object_size("nonexistent", HashAlgorithm::Xxh128)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_filesystem_cache_requires_absolute_path() {
        let result: Result<FileSystemDataCache, StorageError> =
            FileSystemDataCache::new("relative/path");
        assert!(result.is_err());
    }

    #[test]
    fn test_get_object_key() {
        let dir: TempDir = TempDir::new().unwrap();
        let cache: FileSystemDataCache =
            FileSystemDataCache::new(dir.path().to_path_buf()).unwrap();

        let key: String = cache.get_object_key("abc123", HashAlgorithm::Xxh128);
        assert_eq!(key, "abc123.xxh128");
    }
}
