//! Integration tests for the VFS read-only disk cache.
//!
//! Tests verify the ReadCache integration with MemoryPool and FileStore,
//! covering the read path, write-through behavior, and edge cases.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use rusty_attachments_model::HashAlgorithm;
use rusty_attachments_vfs::inode::{FileContent, INodeManager};
use rusty_attachments_vfs::memory_pool_v2::{MemoryPool, MemoryPoolConfig};
use rusty_attachments_vfs::write::{DirtyFileManager, MemoryWriteCache};
use rusty_attachments_vfs::{FileStore, ReadCache, ReadCacheOptions, VfsError};
use tempfile::TempDir;

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Mock file store that tracks retrieval calls.
///
/// Simulates S3 backend with call counting for verification.
#[derive(Debug)]
struct MockFileStore {
    /// Content stored by hash.
    content: std::sync::RwLock<HashMap<String, Vec<u8>>>,
    /// Number of retrieve calls (for verifying cache behavior).
    retrieve_count: AtomicU64,
}

impl MockFileStore {
    /// Create a new empty mock store.
    fn new() -> Self {
        Self {
            content: std::sync::RwLock::new(HashMap::new()),
            retrieve_count: AtomicU64::new(0),
        }
    }

    /// Add content to the mock store.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `data` - Content bytes
    fn insert(&self, hash: impl Into<String>, data: Vec<u8>) {
        self.content.write().unwrap().insert(hash.into(), data);
    }

    /// Get the number of retrieve calls made.
    fn retrieve_count(&self) -> u64 {
        self.retrieve_count.load(Ordering::Relaxed)
    }

    /// Reset the retrieve counter.
    fn reset_count(&self) {
        self.retrieve_count.store(0, Ordering::Relaxed);
    }
}

#[async_trait]
impl FileStore for MockFileStore {
    async fn retrieve(&self, hash: &str, _algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError> {
        self.retrieve_count.fetch_add(1, Ordering::Relaxed);

        self.content
            .read()
            .unwrap()
            .get(hash)
            .cloned()
            .ok_or_else(|| VfsError::ContentRetrievalFailed {
                hash: hash.to_string(),
                source: "Hash not found in mock store".into(),
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

/// Create a test environment with ReadCache enabled.
///
/// # Returns
/// Tuple of (DirtyFileManager, INodeManager, MockFileStore, ReadCache, TempDir)
fn create_test_env_with_cache() -> (
    Arc<DirtyFileManager>,
    Arc<INodeManager>,
    Arc<MockFileStore>,
    Arc<ReadCache>,
    TempDir,
) {
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = temp_dir.path().join("cas");

    let write_cache = Arc::new(MemoryWriteCache::new());
    let store = Arc::new(MockFileStore::new());
    let inodes = Arc::new(INodeManager::new());
    let pool = Arc::new(MemoryPool::new(MemoryPoolConfig::default()));

    let read_cache_options = ReadCacheOptions::with_cache_dir(cache_dir);
    let read_cache = Arc::new(ReadCache::new(read_cache_options).unwrap());

    let mut manager = DirtyFileManager::new(
        write_cache,
        store.clone(),
        inodes.clone(),
        pool,
    );
    manager.set_read_cache(read_cache.clone());

    (Arc::new(manager), inodes, store, read_cache, temp_dir)
}

/// Create a test environment without ReadCache.
///
/// # Returns
/// Tuple of (DirtyFileManager, INodeManager, MockFileStore)
fn create_test_env_no_cache() -> (
    Arc<DirtyFileManager>,
    Arc<INodeManager>,
    Arc<MockFileStore>,
) {
    let write_cache = Arc::new(MemoryWriteCache::new());
    let store = Arc::new(MockFileStore::new());
    let inodes = Arc::new(INodeManager::new());
    let pool = Arc::new(MemoryPool::new(MemoryPoolConfig::default()));

    let manager = DirtyFileManager::new(
        write_cache,
        store.clone(),
        inodes.clone(),
        pool,
    );

    (Arc::new(manager), inodes, store)
}

// ============================================================================
// TC-R1: First read stores to disk cache
// ============================================================================

mod first_read_caches {
    use super::*;

    /// Verify that first read of a file stores content in disk cache.
    #[tokio::test]
    async fn test_first_read_caches_to_disk() {
        let (manager, inodes, store, read_cache, _temp) = create_test_env_with_cache();

        // Setup: Add file to store
        let content: Vec<u8> = b"hello world from S3".to_vec();
        let hash: &str = "00000000000000000000000000001230";
        store.insert(hash, content.clone());

        // Add file to inode manager
        let ino: u64 = inodes.add_file(
            "test.txt",
            content.len() as u64,
            0,
            FileContent::SingleHash(hash.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Trigger COW to load content
        manager.cow_copy(ino).await.unwrap();

        // Read the file (triggers fetch from store)
        let data: Vec<u8> = manager.read(ino, 0, 100).await.unwrap();

        // Assert: Data returned correctly
        assert_eq!(data, content);

        // Assert: S3 was called once
        assert_eq!(store.retrieve_count(), 1);

        // Assert: Content is now in disk cache
        assert!(read_cache.contains(hash));
        let cached: Vec<u8> = read_cache.get(hash).unwrap().unwrap();
        assert_eq!(cached, content);
    }

    /// Verify cache file has correct content after read.
    #[tokio::test]
    async fn test_cache_file_content_matches() {
        let (manager, inodes, store, read_cache, temp) = create_test_env_with_cache();

        let content: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let hash: &str = "0000000000000000000000000000b1a0";
        store.insert(hash, content.clone());

        let ino: u64 = inodes.add_file(
            "binary.bin",
            content.len() as u64,
            0,
            FileContent::SingleHash(hash.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        manager.cow_copy(ino).await.unwrap();
        let _: Vec<u8> = manager.read(ino, 0, 10).await.unwrap();

        // Verify file exists on disk
        let cache_path = temp.path().join("cas").join(hash);
        assert!(cache_path.exists());

        // Verify content matches
        let disk_content: Vec<u8> = std::fs::read(&cache_path).unwrap();
        assert_eq!(disk_content, content);

        // Verify via ReadCache API
        assert!(read_cache.contains_with_size(hash, content.len() as u64));
    }
}

// ============================================================================
// TC-R2: Memory miss, disk hit - no S3 call
// ============================================================================

mod disk_cache_hit {
    use super::*;

    /// Verify that disk cache hit bypasses S3.
    #[tokio::test]
    async fn test_memory_miss_disk_hit_no_s3_call() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("cas");

        // Pre-populate disk cache
        std::fs::create_dir_all(&cache_dir).unwrap();
        let hash: &str = "0000000000000000000000000000cac0";
        let content: Vec<u8> = b"precached content".to_vec();
        std::fs::write(cache_dir.join(hash), &content).unwrap();

        // Create fresh environment (empty memory pool)
        let write_cache = Arc::new(MemoryWriteCache::new());
        let store = Arc::new(MockFileStore::new());
        let inodes = Arc::new(INodeManager::new());
        let pool = Arc::new(MemoryPool::new(MemoryPoolConfig::default()));

        let read_cache_options = ReadCacheOptions::with_cache_dir(cache_dir);
        let read_cache = Arc::new(ReadCache::new(read_cache_options).unwrap());

        // Store has the content (but we shouldn't need it)
        store.insert(hash, content.clone());

        let mut manager = DirtyFileManager::new(
            write_cache,
            store.clone(),
            inodes.clone(),
            pool,
        );
        manager.set_read_cache(read_cache.clone());
        let manager = Arc::new(manager);

        let ino: u64 = inodes.add_file(
            "cached.txt",
            content.len() as u64,
            0,
            FileContent::SingleHash(hash.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Reset counter to verify no S3 calls
        store.reset_count();

        // Trigger COW and read
        manager.cow_copy(ino).await.unwrap();
        let data: Vec<u8> = manager.read(ino, 0, 100).await.unwrap();

        // Assert: Data returned correctly from disk cache
        assert_eq!(data, content);

        // Assert: S3 was NOT called (disk cache hit)
        // Note: The current implementation may still call S3 if the memory pool
        // doesn't check disk cache first. This test documents expected behavior.
        // If this fails, it indicates the disk cache integration needs work.
        assert_eq!(
            store.retrieve_count(),
            0,
            "S3 should not be called when disk cache has content"
        );
    }
}

// ============================================================================
// TC-W1: Skip write if already cached with correct size
// ============================================================================

mod write_through_skip {
    use super::*;

    /// Verify that write-through skips if file already cached with correct size.
    #[tokio::test]
    async fn test_skip_write_if_cached_correct_size() {
        let (manager, inodes, store, read_cache, _temp) = create_test_env_with_cache();

        let content: Vec<u8> = b"original content".to_vec();
        let hash: &str = "000000000000000000000000000051c0";
        store.insert(hash, content.clone());

        // Pre-populate cache
        read_cache.put(hash, &content).unwrap();

        // Get original mtime
        let cache_path = read_cache.cache_dir().join(hash);
        let mtime_before = std::fs::metadata(&cache_path).unwrap().modified().unwrap();

        // Small delay
        std::thread::sleep(std::time::Duration::from_millis(10));

        let ino: u64 = inodes.add_file(
            "test.txt",
            content.len() as u64,
            0,
            FileContent::SingleHash(hash.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Trigger read (which would write-through if not cached)
        manager.cow_copy(ino).await.unwrap();
        let _: Vec<u8> = manager.read(ino, 0, 100).await.unwrap();

        // Assert: mtime unchanged (file not rewritten)
        let mtime_after = std::fs::metadata(&cache_path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "Cache file should not be rewritten");
    }
}

// ============================================================================
// TC-W2: Replace if size mismatch (via put)
// ============================================================================

mod write_through_replace {
    use super::*;

    /// Verify that ReadCache.put() replaces entry if size differs.
    ///
    /// Note: The current implementation trusts cached content on read.
    /// Size validation happens only during put() operations.
    /// Hash verification on read is a future enhancement (TC-I1).
    #[tokio::test]
    async fn test_put_replaces_if_size_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("cas");

        let options = ReadCacheOptions::with_cache_dir(cache_dir);
        let cache = ReadCache::new(options).unwrap();

        let hash: &str = "00000000000000000000000000005120";
        let short_content: Vec<u8> = b"short".to_vec();
        let long_content: Vec<u8> = b"longer content here".to_vec();

        // Put short content first
        cache.put(hash, &short_content).unwrap();
        assert!(cache.contains_with_size(hash, short_content.len() as u64));

        // Put longer content - should replace
        cache.put(hash, &long_content).unwrap();

        // Verify replacement
        assert!(cache.contains_with_size(hash, long_content.len() as u64));
        let cached: Vec<u8> = cache.get(hash).unwrap().unwrap();
        assert_eq!(cached, long_content);
    }

    /// Verify that same-size put is skipped (no rewrite).
    #[tokio::test]
    async fn test_put_skips_same_size() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("cas");

        let options = ReadCacheOptions::with_cache_dir(cache_dir.clone());
        let cache = ReadCache::new(options).unwrap();

        let hash: &str = "00000000000000000000000000005130";
        let content1: Vec<u8> = b"content1".to_vec(); // 8 bytes
        let content2: Vec<u8> = b"content2".to_vec(); // 8 bytes (same size)

        cache.put(hash, &content1).unwrap();

        let path = cache_dir.join(hash);
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Put same-size content - should skip
        cache.put(hash, &content2).unwrap();

        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "File should not be rewritten");

        // Original content preserved
        let cached: Vec<u8> = cache.get(hash).unwrap().unwrap();
        assert_eq!(cached, content1);
    }
}

// ============================================================================
// TC-W3: Atomic write - no partial files
// ============================================================================

mod atomic_write {
    use super::*;

    /// Verify atomic write uses temp file + rename pattern.
    #[tokio::test]
    async fn test_atomic_write_no_partial_files() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("cas");

        let read_cache_options = ReadCacheOptions::with_cache_dir(cache_dir.clone());
        let read_cache = ReadCache::new(read_cache_options).unwrap();

        let hash: &str = "00000000000000000000000000a10100";
        let content: Vec<u8> = b"atomic write content".to_vec();

        // Write content
        read_cache.put(hash, &content).unwrap();

        // Verify no temp files remain
        let entries: Vec<_> = std::fs::read_dir(&cache_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        for entry in &entries {
            let name: String = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name.ends_with(".tmp"),
                "Temp file should not remain: {}",
                name
            );
        }

        // Verify final file exists with correct content
        let final_path = cache_dir.join(hash);
        assert!(final_path.exists());
        assert_eq!(std::fs::read(&final_path).unwrap(), content);
    }
}

// ============================================================================
// TC-E1: Empty file (zero bytes)
// ============================================================================

mod empty_file {
    use super::*;

    /// Verify ReadCache handles empty files correctly.
    #[tokio::test]
    async fn test_empty_file_cached_directly() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("cas");

        let options = ReadCacheOptions::with_cache_dir(cache_dir);
        let cache = ReadCache::new(options).unwrap();

        let hash: &str = "00000000000000000000000000e10100";
        let content: Vec<u8> = Vec::new();

        // Put empty content
        cache.put(hash, &content).unwrap();

        // Verify cached
        assert!(cache.contains(hash));
        assert!(cache.contains_with_size(hash, 0));

        let cached: Vec<u8> = cache.get(hash).unwrap().unwrap();
        assert!(cached.is_empty());
    }

    /// Verify empty file read works through VFS (no hash to cache).
    ///
    /// Note: Empty files in manifests may not have a hash, so there's
    /// nothing to cache. This test verifies the VFS handles this gracefully.
    #[tokio::test]
    async fn test_empty_file_vfs_read() {
        let (manager, inodes, store, _read_cache, _temp) = create_test_env_with_cache();

        // Empty file with a hash (some manifests may include this)
        let content: Vec<u8> = Vec::new();
        let hash: &str = "00000000000000000000000000e10200";
        store.insert(hash, content.clone());

        let ino: u64 = inodes.add_file(
            "empty.txt",
            0,
            0,
            FileContent::SingleHash(hash.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        manager.cow_copy(ino).await.unwrap();
        let data: Vec<u8> = manager.read(ino, 0, 100).await.unwrap();

        // Empty data returned
        assert!(data.is_empty());
    }
}

// ============================================================================
// TC-E2: Large chunked file - each chunk cached independently
// ============================================================================

mod chunked_file {
    use super::*;

    /// Verify each chunk of a chunked file is cached independently.
    #[tokio::test]
    async fn test_chunked_file_chunks_cached_independently() {
        let (manager, inodes, store, read_cache, _temp) = create_test_env_with_cache();

        // Create multiple chunks
        let chunk0: Vec<u8> = b"chunk zero content here".to_vec();
        let chunk1: Vec<u8> = b"chunk one different data".to_vec();
        let hash0: &str = "00000000000000000000000000c00100";
        let hash1: &str = "00000000000000000000000000c00200";

        store.insert(hash0, chunk0.clone());
        store.insert(hash1, chunk1.clone());

        let total_size: u64 = (chunk0.len() + chunk1.len()) as u64;

        let ino: u64 = inodes.add_file(
            "chunked.bin",
            total_size,
            0,
            FileContent::Chunked(vec![hash0.to_string(), hash1.to_string()]),
            HashAlgorithm::Xxh128,
            false,
        );

        // Trigger COW
        manager.cow_copy(ino).await.unwrap();

        // Read only first chunk worth of data
        let data: Vec<u8> = manager.read(ino, 0, chunk0.len() as u32).await.unwrap();
        assert_eq!(data, chunk0);

        // Assert: First chunk cached
        assert!(read_cache.contains(hash0));

        // Note: Whether chunk1 is cached depends on implementation.
        // The design says chunks should be cached independently on access.
    }
}

// ============================================================================
// TC-I3: Handle missing cache directory
// ============================================================================

mod missing_cache_dir {
    use super::*;

    /// Verify cache directory is created automatically.
    #[tokio::test]
    async fn test_missing_cache_dir_created() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("nonexistent").join("nested").join("cas");

        // Directory doesn't exist yet
        assert!(!cache_dir.exists());

        // Create ReadCache - should create directory
        let options = ReadCacheOptions::with_cache_dir(cache_dir.clone());
        let cache = ReadCache::new(options).unwrap();

        // Directory should now exist
        assert!(cache_dir.exists());

        // Should be able to write
        cache.put("test_hash", b"test data").unwrap();
        assert!(cache.contains("test_hash"));
    }
}

// ============================================================================
// Cache size tracking
// ============================================================================

mod cache_size_tracking {
    use super::*;

    /// Verify current_size is tracked correctly across operations.
    #[tokio::test]
    async fn test_cache_size_tracking() {
        let (manager, inodes, store, read_cache, _temp) = create_test_env_with_cache();

        assert_eq!(read_cache.current_size(), 0);

        // Add first file
        let content1: Vec<u8> = b"first file".to_vec();
        let hash1: &str = "00000000000000000000000000000010";
        store.insert(hash1, content1.clone());

        let ino1: u64 = inodes.add_file(
            "file1.txt",
            content1.len() as u64,
            0,
            FileContent::SingleHash(hash1.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        manager.cow_copy(ino1).await.unwrap();
        let _: Vec<u8> = manager.read(ino1, 0, 100).await.unwrap();

        assert_eq!(read_cache.current_size(), content1.len() as u64);

        // Add second file
        let content2: Vec<u8> = b"second file content".to_vec();
        let hash2: &str = "00000000000000000000000000000020";
        store.insert(hash2, content2.clone());

        let ino2: u64 = inodes.add_file(
            "file2.txt",
            content2.len() as u64,
            0,
            FileContent::SingleHash(hash2.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        manager.cow_copy(ino2).await.unwrap();
        let _: Vec<u8> = manager.read(ino2, 0, 100).await.unwrap();

        let expected_size: u64 = (content1.len() + content2.len()) as u64;
        assert_eq!(read_cache.current_size(), expected_size);
    }
}

// ============================================================================
// No cache behavior
// ============================================================================

mod no_cache {
    use super::*;

    /// Verify VFS works correctly without read cache enabled.
    #[tokio::test]
    async fn test_works_without_read_cache() {
        let (manager, inodes, store) = create_test_env_no_cache();

        let content: Vec<u8> = b"content without cache".to_vec();
        let hash: &str = "00000000000000000000000000a0cac0";
        store.insert(hash, content.clone());

        let ino: u64 = inodes.add_file(
            "test.txt",
            content.len() as u64,
            0,
            FileContent::SingleHash(hash.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        manager.cow_copy(ino).await.unwrap();
        let data: Vec<u8> = manager.read(ino, 0, 100).await.unwrap();

        assert_eq!(data, content);
        assert_eq!(store.retrieve_count(), 1);
    }
}
