//! In-memory cache implementation for testing.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::RwLock;

use async_trait::async_trait;

use super::{DiskCacheError, WriteCache};

/// In-memory write cache for testing.
///
/// Stores all data in memory, no disk I/O.
pub struct MemoryWriteCache {
    /// Files by relative path.
    files: RwLock<HashMap<String, Vec<u8>>>,
    /// Deleted file paths.
    deleted: RwLock<HashSet<String>>,
}

impl MemoryWriteCache {
    /// Create a new empty memory cache.
    pub fn new() -> Self {
        Self {
            files: RwLock::new(HashMap::new()),
            deleted: RwLock::new(HashSet::new()),
        }
    }
}

impl Default for MemoryWriteCache {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WriteCache for MemoryWriteCache {
    async fn write_file(&self, rel_path: &str, data: &[u8]) -> Result<(), DiskCacheError> {
        self.deleted.write().unwrap().remove(rel_path);
        self.files
            .write()
            .unwrap()
            .insert(rel_path.to_string(), data.to_vec());
        Ok(())
    }

    async fn read_file(&self, rel_path: &str) -> Result<Option<Vec<u8>>, DiskCacheError> {
        Ok(self.files.read().unwrap().get(rel_path).cloned())
    }

    async fn delete_file(&self, rel_path: &str) -> Result<(), DiskCacheError> {
        self.files.write().unwrap().remove(rel_path);
        self.deleted.write().unwrap().insert(rel_path.to_string());
        Ok(())
    }

    fn is_deleted(&self, rel_path: &str) -> bool {
        self.deleted.read().unwrap().contains(rel_path)
    }

    async fn list_files(&self) -> Result<Vec<String>, DiskCacheError> {
        Ok(self.files.read().unwrap().keys().cloned().collect())
    }

    fn cache_dir(&self) -> &Path {
        Path::new("/dev/null")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_cache_write_read() {
        let cache = MemoryWriteCache::new();

        cache.write_file("test.txt", b"hello").await.unwrap();
        let data: Option<Vec<u8>> = cache.read_file("test.txt").await.unwrap();

        assert_eq!(data, Some(b"hello".to_vec()));
    }

    #[tokio::test]
    async fn test_memory_cache_delete() {
        let cache = MemoryWriteCache::new();

        cache.write_file("test.txt", b"hello").await.unwrap();
        assert!(!cache.is_deleted("test.txt"));

        cache.delete_file("test.txt").await.unwrap();
        assert!(cache.is_deleted("test.txt"));
        assert!(cache.read_file("test.txt").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_memory_cache_list_files() {
        let cache = MemoryWriteCache::new();

        cache.write_file("a.txt", b"a").await.unwrap();
        cache.write_file("b.txt", b"b").await.unwrap();

        let files: Vec<String> = cache.list_files().await.unwrap();
        assert_eq!(files.len(), 2);
    }
}
