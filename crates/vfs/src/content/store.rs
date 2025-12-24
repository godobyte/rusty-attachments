//! FileStore trait for content retrieval.

use async_trait::async_trait;

use rusty_attachments_model::HashAlgorithm;

use crate::VfsError;

/// Trait for types that can retrieve file content.
///
/// Implement this trait to integrate with different storage backends
/// (S3 CAS, local disk, memory, etc.).
#[async_trait]
pub trait FileStore: Send + Sync {
    /// Retrieve the entire content for a hash.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm used
    ///
    /// # Returns
    /// The raw bytes of the content.
    async fn retrieve(&self, hash: &str, algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError>;

    /// Retrieve a range of content for a hash.
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
    ) -> Result<Vec<u8>, VfsError>;

    /// Build the CAS key for a hash.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm used
    ///
    /// # Returns
    /// The CAS key (e.g., "Data/abc123.xxh128").
    fn cas_key(&self, hash: &str, algorithm: HashAlgorithm) -> String {
        format!("{}/{}.{}", self.cas_prefix(), hash, algorithm.extension())
    }

    /// Get the CAS prefix (e.g., "Data").
    fn cas_prefix(&self) -> &str {
        "Data"
    }
}

/// In-memory file store for testing.
#[derive(Debug, Default)]
pub struct MemoryFileStore {
    /// Content by hash.
    content: std::collections::HashMap<String, Vec<u8>>,
}

impl MemoryFileStore {
    /// Create a new empty memory store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add content to the store.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `data` - Content bytes
    pub fn insert(&mut self, hash: impl Into<String>, data: Vec<u8>) {
        self.content.insert(hash.into(), data);
    }
}

#[async_trait]
impl FileStore for MemoryFileStore {
    async fn retrieve(&self, hash: &str, _algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError> {
        self.content
            .get(hash)
            .cloned()
            .ok_or_else(|| VfsError::ContentRetrievalFailed {
                hash: hash.to_string(),
                source: "Hash not found in memory store".into(),
            })
    }

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

    #[tokio::test]
    async fn test_memory_store_retrieve() {
        let mut store: MemoryFileStore = MemoryFileStore::new();
        store.insert("hash123", vec![1, 2, 3, 4, 5]);

        let data: Vec<u8> = store.retrieve("hash123", HashAlgorithm::Xxh128).await.unwrap();
        assert_eq!(data, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn test_memory_store_retrieve_range() {
        let mut store: MemoryFileStore = MemoryFileStore::new();
        store.insert("hash123", vec![1, 2, 3, 4, 5]);

        let data: Vec<u8> = store
            .retrieve_range("hash123", HashAlgorithm::Xxh128, 1, 3)
            .await
            .unwrap();
        assert_eq!(data, vec![2, 3, 4]);
    }

    #[tokio::test]
    async fn test_memory_store_not_found() {
        let store: MemoryFileStore = MemoryFileStore::new();
        let result: Result<Vec<u8>, VfsError> =
            store.retrieve("nonexistent", HashAlgorithm::Xxh128).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_cas_key() {
        let store: MemoryFileStore = MemoryFileStore::new();
        let key: String = store.cas_key("abc123", HashAlgorithm::Xxh128);
        assert_eq!(key, "Data/abc123.xxh128");
    }
}
