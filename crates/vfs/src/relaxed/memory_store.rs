//! In-memory implementation of RelaxedFileStore for testing.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use rusty_attachments_model::HashAlgorithm;

use crate::VfsError;

use super::store::RelaxedFileStore;
use super::types::{RelaxedFileKey, RelaxedResolution, RequestPriority};

/// In-memory relaxed file store for testing.
///
/// Files can be pre-populated as "available" or left absent to simulate
/// the pending/polling flow.
#[derive(Debug)]
pub struct MemoryRelaxedStore {
    /// Map of path_key → resolution.
    resolutions: RwLock<HashMap<String, RelaxedResolution>>,
    /// Track which keys have been requested (for assertions in tests).
    requests: RwLock<Vec<(String, RequestPriority)>>,
}

impl MemoryRelaxedStore {
    /// Create a new empty store (all lookups return Pending).
    pub fn new() -> Self {
        Self {
            resolutions: RwLock::new(HashMap::new()),
            requests: RwLock::new(Vec::new()),
        }
    }

    /// Pre-populate a file as available.
    ///
    /// # Arguments
    /// * `path_key` - The path key of the file.
    /// * `content_hash` - The CAS content hash.
    /// * `size` - File size in bytes.
    pub fn insert_available(&self, path_key: &str, content_hash: &str, size: u64) {
        let mut resolutions = self.resolutions.write().unwrap();
        resolutions.insert(
            path_key.to_string(),
            RelaxedResolution::Available {
                content_hash: content_hash.to_string(),
                hash_algorithm: HashAlgorithm::Xxh128,
                size,
                chunk_hashes: None,
            },
        );
    }

    /// Mark a file as failed.
    ///
    /// # Arguments
    /// * `path_key` - The path key of the file.
    /// * `reason` - Failure reason.
    pub fn insert_failed(&self, path_key: &str, reason: &str) {
        let mut resolutions = self.resolutions.write().unwrap();
        resolutions.insert(
            path_key.to_string(),
            RelaxedResolution::Failed {
                reason: reason.to_string(),
            },
        );
    }

    /// Get all requests that have been made (for test assertions).
    pub fn get_requests(&self) -> Vec<(String, RequestPriority)> {
        self.requests.read().unwrap().clone()
    }
}

impl Default for MemoryRelaxedStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RelaxedFileStore for MemoryRelaxedStore {
    async fn resolve(
        &self,
        key: &RelaxedFileKey,
        priority: RequestPriority,
    ) -> Result<RelaxedResolution, VfsError> {
        // Record the request
        {
            let mut requests = self.requests.write().unwrap();
            requests.push((key.path_key.clone(), priority));
        }

        let resolutions = self.resolutions.read().unwrap();
        match resolutions.get(&key.path_key) {
            Some(resolution) => Ok(resolution.clone()),
            None => Ok(RelaxedResolution::Pending),
        }
    }

    async fn poll(&self, key: &RelaxedFileKey) -> Result<RelaxedResolution, VfsError> {
        let resolutions = self.resolutions.read().unwrap();
        match resolutions.get(&key.path_key) {
            Some(resolution) => Ok(resolution.clone()),
            None => Ok(RelaxedResolution::Pending),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_store_pending_by_default() {
        let store = MemoryRelaxedStore::new();
        let key = RelaxedFileKey {
            root_id: "root1".to_string(),
            relative_path: "file.txt".to_string(),
            path_key: "abc123".to_string(),
        };

        let result: RelaxedResolution = store.resolve(&key, RequestPriority::High).await.unwrap();
        assert!(matches!(result, RelaxedResolution::Pending));
    }

    #[tokio::test]
    async fn test_memory_store_available() {
        let store = MemoryRelaxedStore::new();
        store.insert_available("abc123", "hash_of_content", 4096);

        let key = RelaxedFileKey {
            root_id: "root1".to_string(),
            relative_path: "file.txt".to_string(),
            path_key: "abc123".to_string(),
        };

        let result: RelaxedResolution = store.resolve(&key, RequestPriority::High).await.unwrap();
        match result {
            RelaxedResolution::Available {
                content_hash,
                size,
                ..
            } => {
                assert_eq!(content_hash, "hash_of_content");
                assert_eq!(size, 4096);
            }
            _ => panic!("Expected Available"),
        }
    }

    #[tokio::test]
    async fn test_memory_store_failed() {
        let store = MemoryRelaxedStore::new();
        store.insert_failed("abc123", "File not found");

        let key = RelaxedFileKey {
            root_id: "root1".to_string(),
            relative_path: "file.txt".to_string(),
            path_key: "abc123".to_string(),
        };

        let result: RelaxedResolution = store.resolve(&key, RequestPriority::High).await.unwrap();
        match result {
            RelaxedResolution::Failed { reason } => {
                assert_eq!(reason, "File not found");
            }
            _ => panic!("Expected Failed"),
        }
    }

    #[tokio::test]
    async fn test_memory_store_tracks_requests() {
        let store = MemoryRelaxedStore::new();
        let key = RelaxedFileKey {
            root_id: "root1".to_string(),
            relative_path: "file.txt".to_string(),
            path_key: "abc123".to_string(),
        };

        store.resolve(&key, RequestPriority::High).await.unwrap();
        store
            .resolve(&key, RequestPriority::AsyncEventual)
            .await
            .unwrap();

        let requests: Vec<(String, RequestPriority)> = store.get_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], ("abc123".to_string(), RequestPriority::High));
        assert_eq!(
            requests[1],
            ("abc123".to_string(), RequestPriority::AsyncEventual)
        );
    }

    #[tokio::test]
    async fn test_poll_returns_same_as_resolve() {
        let store = MemoryRelaxedStore::new();
        store.insert_available("abc123", "hash1", 100);

        let key = RelaxedFileKey {
            root_id: "root1".to_string(),
            relative_path: "file.txt".to_string(),
            path_key: "abc123".to_string(),
        };

        let result: RelaxedResolution = store.poll(&key).await.unwrap();
        assert!(matches!(result, RelaxedResolution::Available { .. }));
    }
}
