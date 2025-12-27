//! S3 storage adapter for the VFS FileStore trait.
//!
//! This module provides an adapter that bridges the `StorageClient` trait
//! from the storage crate to the `FileStore` trait used by the VFS.
//!
//! # Example
//!
//! ```ignore
//! use rusty_attachments_storage::{S3Location, StorageSettings};
//! use rusty_attachments_storage_crt::CrtStorageClient;
//! use rusty_attachments_vfs::content::StorageClientAdapter;
//!
//! let settings = StorageSettings::default();
//! let client = CrtStorageClient::new(settings).await?;
//! let location = S3Location::new("bucket", "root", "Data", "Manifests");
//!
//! let store = StorageClientAdapter::new(client, location);
//! ```

use async_trait::async_trait;
use rusty_attachments_model::HashAlgorithm;
use rusty_attachments_storage::{S3Location, StorageClient};

use super::FileStore;
use crate::VfsError;

/// Adapter that wraps a `StorageClient` to implement `FileStore`.
///
/// This bridges the storage crate's `StorageClient` trait with the VFS
/// crate's `FileStore` trait, following the pyramid architecture pattern.
///
/// # Type Parameters
/// * `C` - A type implementing `StorageClient`
pub struct StorageClientAdapter<C: StorageClient> {
    /// The underlying storage client.
    client: C,
    /// S3 location configuration.
    location: S3Location,
}

impl<C: StorageClient> StorageClientAdapter<C> {
    /// Create a new adapter wrapping a storage client.
    ///
    /// # Arguments
    /// * `client` - Storage client implementing S3 operations
    /// * `location` - S3 location with bucket and prefix configuration
    pub fn new(client: C, location: S3Location) -> Self {
        Self { client, location }
    }

    /// Get a reference to the underlying storage client.
    pub fn client(&self) -> &C {
        &self.client
    }

    /// Get a reference to the S3 location configuration.
    pub fn location(&self) -> &S3Location {
        &self.location
    }
}

#[async_trait]
impl<C: StorageClient + 'static> FileStore for StorageClientAdapter<C> {
    /// Retrieve the entire content for a hash from S3 CAS.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm used
    ///
    /// # Returns
    /// The raw bytes of the content.
    async fn retrieve(&self, hash: &str, algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError> {
        let key: String = self.location.cas_key(hash, algorithm);

        self.client
            .get_object(&self.location.bucket, &key)
            .await
            .map_err(|e| VfsError::ContentRetrievalFailed {
                hash: hash.to_string(),
                source: format!("StorageClient get_object failed: {}", e).into(),
            })
    }

    /// Retrieve a range of content for a hash from S3 CAS.
    ///
    /// Note: This implementation fetches the full content and slices it.
    /// A more optimized version could use S3 range requests.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm used
    /// * `offset` - Start offset in bytes
    /// * `size` - Number of bytes to retrieve
    ///
    /// # Returns
    /// The raw bytes of the requested range.
    async fn retrieve_range(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, VfsError> {
        let data: Vec<u8> = self.retrieve(hash, algorithm).await?;
        let start: usize = offset as usize;
        let end: usize = (offset + size).min(data.len() as u64) as usize;
        Ok(data[start..end].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Full integration tests require the storage-crt crate
    // which is a dev-dependency. Unit tests here focus on the adapter logic.

    #[test]
    fn test_adapter_accessors() {
        // This test would require a mock StorageClient
        // For now, we verify the module compiles correctly
    }
}
