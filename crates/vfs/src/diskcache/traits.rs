//! Trait definitions for disk cache implementations.

use std::path::Path;

use async_trait::async_trait;

use super::DiskCacheError;

/// Trait for COW disk cache implementations.
///
/// Allows swapping cache backends for testing or alternative storage.
#[async_trait]
pub trait WriteCache: Send + Sync {
    /// Write file content to cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    /// * `data` - File content to write
    async fn write_file(&self, rel_path: &str, data: &[u8]) -> Result<(), DiskCacheError>;

    /// Read file content from cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    ///
    /// # Returns
    /// File content, or None if not in cache.
    async fn read_file(&self, rel_path: &str) -> Result<Option<Vec<u8>>, DiskCacheError>;

    /// Mark file as deleted (create tombstone).
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    async fn delete_file(&self, rel_path: &str) -> Result<(), DiskCacheError>;

    /// Check if file is marked as deleted.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    fn is_deleted(&self, rel_path: &str) -> bool;

    /// List all cached files (excluding deleted).
    ///
    /// # Returns
    /// Vector of relative paths.
    async fn list_files(&self) -> Result<Vec<String>, DiskCacheError>;

    /// Get cache directory path (for inspection/debugging).
    fn cache_dir(&self) -> &Path;
}
