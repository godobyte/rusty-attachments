//! Edge case tests for dirty file operations.
//!
//! These tests cover specific bugs found during the unified memory pool cleanup:
//! 1. Truncate extend not returning zeros for extended region
//! 2. Reading from new files that have been extended but not written
//! 3. Pool data smaller than file size after truncate extend

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rusty_attachments_model::HashAlgorithm;
use rusty_attachments_vfs::inode::{FileContent, INodeManager};
use rusty_attachments_vfs::memory_pool_v2::{MemoryPool, MemoryPoolConfig};
use rusty_attachments_vfs::write::{DirtyFileManager, MemoryWriteCache};
use rusty_attachments_vfs::{FileStore, VfsError};

/// Test file store that tracks content by hash.
#[derive(Debug, Default)]
struct TestFileStore {
    content: std::sync::RwLock<HashMap<String, Vec<u8>>>,
}

impl TestFileStore {
    fn new() -> Self {
        Self::default()
    }

    fn add_content(&self, hash: &str, data: Vec<u8>) {
        self.content.write().unwrap().insert(hash.to_string(), data);
    }
}

#[async_trait]
impl FileStore for TestFileStore {
    async fn retrieve(&self, hash: &str, _algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError> {
        self.content
            .read()
            .unwrap()
            .get(hash)
            .cloned()
            .ok_or_else(|| VfsError::NotFound(hash.to_string()))
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

/// Helper to create a test environment with manager and inodes.
fn create_test_env() -> (Arc<DirtyFileManager>, Arc<INodeManager>, Arc<TestFileStore>) {
    let cache = Arc::new(MemoryWriteCache::new());
    let store = Arc::new(TestFileStore::new());
    let inodes = Arc::new(INodeManager::new());
    let pool = Arc::new(MemoryPool::new(MemoryPoolConfig::default()));
    let manager = Arc::new(DirtyFileManager::new(
        cache,
        store.clone(),
        inodes.clone(),
        pool,
    ));
    (manager, inodes, store)
}

// =============================================================================
// BUG FIX #1: Truncate extend returns zeros for extended region
// =============================================================================
//
// Previously, when a file was extended via truncate(), the read() function
// would return an empty vector because the pool data was smaller than the
// file size. The fix ensures that reads beyond the pool data return zeros
// up to the file size.

mod truncate_extend_zeros {
    use super::*;

    /// Test that truncate extend on empty file returns zeros.
    ///
    /// Bug: read() returned empty vec instead of zeros when file was
    /// extended via truncate but pool had no data.
    #[tokio::test]
    async fn test_truncate_extend_empty_file_returns_zeros() {
        let (manager, _inodes, _store) = create_test_env();

        // Create empty file
        manager
            .create_file(100, "empty.txt".to_string(), 1)
            .unwrap();
        assert_eq!(manager.get_size(100), Some(0));

        // Extend via truncate
        manager.truncate(100, 50).await.unwrap();
        assert_eq!(manager.get_size(100), Some(50));

        // Read should return 50 zeros
        let data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(data.len(), 50, "Should return 50 bytes");
        assert!(data.iter().all(|&b| b == 0), "All bytes should be zero");
    }

    /// Test that truncate extend preserves existing data and pads with zeros.
    ///
    /// Bug: read() would return only the existing data, not the extended zeros.
    #[tokio::test]
    async fn test_truncate_extend_preserves_data_and_pads_zeros() {
        let (manager, _inodes, _store) = create_test_env();

        // Create file with some data
        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();
        assert_eq!(manager.get_size(100), Some(5));

        // Extend via truncate
        manager.truncate(100, 15).await.unwrap();
        assert_eq!(manager.get_size(100), Some(15));

        // Read entire file
        let data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(data.len(), 15, "Should return 15 bytes");
        assert_eq!(&data[0..5], b"hello", "First 5 bytes should be 'hello'");
        assert!(
            data[5..].iter().all(|&b| b == 0),
            "Extended region should be zeros"
        );
    }

    /// Test reading only the extended (zero) region.
    ///
    /// Bug: read() at offset beyond pool data would return empty.
    #[tokio::test]
    async fn test_read_only_extended_region() {
        let (manager, _inodes, _store) = create_test_env();

        // Create file with some data
        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        // Extend via truncate
        manager.truncate(100, 20).await.unwrap();

        // Read only the extended region (offset 5-20)
        let data: Vec<u8> = manager.read(100, 5, 15).await.unwrap();
        assert_eq!(data.len(), 15, "Should return 15 bytes");
        assert!(data.iter().all(|&b| b == 0), "All bytes should be zero");
    }

    /// Test reading across the boundary of existing data and extended region.
    ///
    /// Bug: read() spanning pool data and extended region would be truncated.
    #[tokio::test]
    async fn test_read_across_data_and_extended_boundary() {
        let (manager, _inodes, _store) = create_test_env();

        // Create file with some data
        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        // Extend via truncate
        manager.truncate(100, 10).await.unwrap();

        // Read from offset 3 (in data) to offset 10 (in extended region)
        let data: Vec<u8> = manager.read(100, 3, 10).await.unwrap();
        assert_eq!(data.len(), 7, "Should return 7 bytes (3-10)");
        assert_eq!(&data[0..2], b"lo", "First 2 bytes should be 'lo'");
        assert!(
            data[2..].iter().all(|&b| b == 0),
            "Remaining bytes should be zeros"
        );
    }
}

// =============================================================================
// BUG FIX #2: New file with no pool data
// =============================================================================
//
// When a new file is created and extended via truncate (without any writes),
// the pool has no data for it. The read function must handle this case by
// returning zeros up to the file size.

mod new_file_no_pool_data {
    use super::*;

    /// Test reading from new file that was never written to.
    #[tokio::test]
    async fn test_read_new_file_never_written() {
        let (manager, _inodes, _store) = create_test_env();

        // Create file but don't write anything
        manager.create_file(100, "new.txt".to_string(), 1).unwrap();

        // Extend via truncate
        manager.truncate(100, 100).await.unwrap();

        // Read should return zeros
        let data: Vec<u8> = manager.read(100, 0, 200).await.unwrap();
        assert_eq!(data.len(), 100, "Should return file size bytes");
        assert!(data.iter().all(|&b| b == 0), "All bytes should be zero");
    }

    /// Test partial read from new file that was never written to.
    #[tokio::test]
    async fn test_partial_read_new_file_never_written() {
        let (manager, _inodes, _store) = create_test_env();

        // Create file but don't write anything
        manager.create_file(100, "new.txt".to_string(), 1).unwrap();

        // Extend via truncate
        manager.truncate(100, 100).await.unwrap();

        // Read middle portion
        let data: Vec<u8> = manager.read(100, 25, 50).await.unwrap();
        assert_eq!(data.len(), 50, "Should return 50 bytes");
        assert!(data.iter().all(|&b| b == 0), "All bytes should be zero");
    }

    /// Test that write after truncate extend works correctly.
    #[tokio::test]
    async fn test_write_after_truncate_extend() {
        let (manager, _inodes, _store) = create_test_env();

        // Create file
        manager.create_file(100, "new.txt".to_string(), 1).unwrap();

        // Extend via truncate first
        manager.truncate(100, 20).await.unwrap();

        // Write in the middle of the extended region
        manager.write(100, 10, b"world").await.unwrap();

        // Read entire file
        let data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(data.len(), 20, "File should be 20 bytes");
        assert!(
            data[0..10].iter().all(|&b| b == 0),
            "First 10 bytes should be zeros"
        );
        assert_eq!(&data[10..15], b"world", "Middle should be 'world'");
        assert!(
            data[15..].iter().all(|&b| b == 0),
            "Last 5 bytes should be zeros"
        );
    }
}

// =============================================================================
// BUG FIX #3: COW file truncate extend
// =============================================================================
//
// When a COW file (from manifest) is extended via truncate, the original
// content should be preserved and the extended region should be zeros.

mod cow_file_truncate_extend {
    use super::*;

    /// Test COW file truncate extend preserves original content.
    #[tokio::test]
    async fn test_cow_file_truncate_extend() {
        let (manager, inodes, store) = create_test_env();

        // Add original content to store
        let original_content: Vec<u8> = b"original content".to_vec();
        // Use a proper 32-character hex hash (128-bit)
        let hash: &str = "abc123def456789012345678abcdef12";
        store.add_content(hash, original_content.clone());

        // Add file to inode manager (simulating manifest file)
        let ino: u64 = inodes.add_file(
            "manifest_file.txt",
            original_content.len() as u64,
            0,
            FileContent::SingleHash(hash.to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Trigger COW by writing (this loads original content)
        manager.write(ino, 0, b"").await.unwrap(); // Empty write to trigger COW

        // Extend via truncate
        manager.truncate(ino, 30).await.unwrap();
        assert_eq!(manager.get_size(ino), Some(30));

        // Read entire file
        let data: Vec<u8> = manager.read(ino, 0, 100).await.unwrap();
        assert_eq!(data.len(), 30, "File should be 30 bytes");
        assert_eq!(
            &data[0..16],
            b"original content",
            "Original content preserved"
        );
        assert!(
            data[16..].iter().all(|&b| b == 0),
            "Extended region should be zeros"
        );
    }
}

// =============================================================================
// BUG FIX #4: Multiple truncate operations
// =============================================================================
//
// Ensure that multiple truncate operations (extend then shrink, or vice versa)
// work correctly.

mod multiple_truncate_operations {
    use super::*;

    /// Test extend then shrink.
    #[tokio::test]
    async fn test_extend_then_shrink() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        // Extend
        manager.truncate(100, 20).await.unwrap();
        assert_eq!(manager.get_size(100), Some(20));

        // Shrink back
        manager.truncate(100, 3).await.unwrap();
        assert_eq!(manager.get_size(100), Some(3));

        let data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(data, b"hel");
    }

    /// Test shrink then extend.
    #[tokio::test]
    async fn test_shrink_then_extend() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        // Shrink
        manager.truncate(100, 5).await.unwrap();
        assert_eq!(manager.get_size(100), Some(5));

        // Extend
        manager.truncate(100, 10).await.unwrap();
        assert_eq!(manager.get_size(100), Some(10));

        let data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(data.len(), 10);
        assert_eq!(&data[0..5], b"hello");
        assert!(data[5..].iter().all(|&b| b == 0));
    }

    /// Test multiple extends.
    #[tokio::test]
    async fn test_multiple_extends() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();

        // Multiple extends without writes
        manager.truncate(100, 10).await.unwrap();
        manager.truncate(100, 20).await.unwrap();
        manager.truncate(100, 50).await.unwrap();

        assert_eq!(manager.get_size(100), Some(50));

        let data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(data.len(), 50);
        assert!(data.iter().all(|&b| b == 0));
    }
}

// =============================================================================
// BUG FIX #5: New file in new directory
// =============================================================================
//
// When a new directory is created and then a file is created inside it,
// the file operations (write, read, delete) must work correctly. The bug
// was that FUSE handlers only looked up parent paths from the inode manager,
// which doesn't contain new directories - they're only in dirty_dir_manager.

mod new_file_in_new_directory {
    use super::*;
    use rusty_attachments_vfs::write::{DirtyDirManager, DirtyDirState};
    use std::collections::HashSet;

    /// Helper to create test environment with both file and directory managers.
    fn create_dir_test_env() -> (
        Arc<DirtyFileManager>,
        Arc<DirtyDirManager>,
        Arc<INodeManager>,
    ) {
        let cache = Arc::new(MemoryWriteCache::new());
        let store = Arc::new(TestFileStore::new());
        let inodes = Arc::new(INodeManager::new());
        let pool = Arc::new(MemoryPool::new(MemoryPoolConfig::default()));
        let file_manager = Arc::new(DirtyFileManager::new(cache, store, inodes.clone(), pool));
        let dir_manager = Arc::new(DirtyDirManager::new(inodes.clone(), HashSet::new()));
        (file_manager, dir_manager, inodes)
    }

    /// Test creating a file in a new directory and writing to it.
    ///
    /// Bug: FUSE create handler couldn't find parent path for new directories.
    #[tokio::test]
    async fn test_create_file_in_new_dir_and_write() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory to inodes (required for create_dir)
        let _root_ino: u64 = inodes.add_directory("");

        // Create new directory (simulating mkdir)
        let dir_ino: u64 = dir_manager.create_dir(1, "newdir").unwrap();

        // Verify directory was created
        assert!(dir_manager.is_new_dir(dir_ino));
        assert_eq!(
            dir_manager.get_new_dir_path(dir_ino),
            Some("newdir".to_string())
        );

        // Create file in new directory (simulating touch/create)
        let file_ino: u64 = 0x8000_0002;
        file_manager
            .create_file(file_ino, "newdir/file.txt".to_string(), dir_ino)
            .unwrap();

        // Verify file was created
        assert!(file_manager.is_new_file(file_ino));
        assert_eq!(file_manager.get_size(file_ino), Some(0));

        // Write to the file
        let written: usize = file_manager
            .write(file_ino, 0, b"hello world")
            .await
            .unwrap();
        assert_eq!(written, 11);

        // Verify write succeeded
        assert_eq!(file_manager.get_size(file_ino), Some(11));

        // Read back the data
        let data: Vec<u8> = file_manager.read(file_ino, 0, 100).await.unwrap();
        assert_eq!(data, b"hello world");
    }

    /// Test looking up a file in a new directory.
    ///
    /// Bug: FUSE lookup handler couldn't find parent path for new directories.
    #[tokio::test]
    async fn test_lookup_file_in_new_dir() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        // Create new directory
        let dir_ino: u64 = dir_manager.create_dir(1, "lookupdir").unwrap();

        // Create file in new directory
        let file_ino: u64 = 0x8000_0002;
        file_manager
            .create_file(file_ino, "lookupdir/target.txt".to_string(), dir_ino)
            .unwrap();
        file_manager
            .write(file_ino, 0, b"lookup test")
            .await
            .unwrap();

        // Simulate lookup: find file by name in parent directory
        let found: Option<(u64, String)> = file_manager
            .get_new_files_in_dir(dir_ino)
            .into_iter()
            .find(|(_, name)| name == "target.txt");

        assert!(found.is_some());
        let (found_ino, _): (u64, String) = found.unwrap();
        assert_eq!(found_ino, file_ino);

        // Verify we can read the file we looked up
        let data: Vec<u8> = file_manager.read(found_ino, 0, 100).await.unwrap();
        assert_eq!(data, b"lookup test");
    }

    /// Test multiple writes to a file in a new directory.
    #[tokio::test]
    async fn test_multiple_writes_to_file_in_new_dir() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        // Create new directory
        let dir_ino: u64 = dir_manager.create_dir(1, "mydir").unwrap();

        // Create file in new directory
        let file_ino: u64 = 0x8000_0002;
        file_manager
            .create_file(file_ino, "mydir/data.txt".to_string(), dir_ino)
            .unwrap();

        // Multiple writes
        file_manager.write(file_ino, 0, b"first").await.unwrap();
        file_manager.write(file_ino, 5, b" second").await.unwrap();
        file_manager.write(file_ino, 12, b" third").await.unwrap();

        // Verify final content
        let data: Vec<u8> = file_manager.read(file_ino, 0, 100).await.unwrap();
        assert_eq!(data, b"first second third");
    }

    /// Test overwriting data in a file in a new directory.
    #[tokio::test]
    async fn test_overwrite_file_in_new_dir() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        // Create new directory
        let dir_ino: u64 = dir_manager.create_dir(1, "testdir").unwrap();

        // Create file
        let file_ino: u64 = 0x8000_0002;
        file_manager
            .create_file(file_ino, "testdir/test.txt".to_string(), dir_ino)
            .unwrap();

        // Write initial content
        file_manager
            .write(file_ino, 0, b"hello world")
            .await
            .unwrap();

        // Overwrite middle portion
        file_manager.write(file_ino, 6, b"WORLD").await.unwrap();

        // Verify
        let data: Vec<u8> = file_manager.read(file_ino, 0, 100).await.unwrap();
        assert_eq!(data, b"hello WORLD");
    }

    /// Test truncating a file in a new directory.
    #[tokio::test]
    async fn test_truncate_file_in_new_dir() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        // Create new directory
        let dir_ino: u64 = dir_manager.create_dir(1, "truncdir").unwrap();

        // Create file and write
        let file_ino: u64 = 0x8000_0002;
        file_manager
            .create_file(file_ino, "truncdir/file.txt".to_string(), dir_ino)
            .unwrap();
        file_manager
            .write(file_ino, 0, b"hello world")
            .await
            .unwrap();

        // Truncate to smaller size
        file_manager.truncate(file_ino, 5).await.unwrap();
        assert_eq!(file_manager.get_size(file_ino), Some(5));

        let data: Vec<u8> = file_manager.read(file_ino, 0, 100).await.unwrap();
        assert_eq!(data, b"hello");
    }

    /// Test extending a file in a new directory via truncate.
    #[tokio::test]
    async fn test_extend_file_in_new_dir_via_truncate() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        // Create new directory
        let dir_ino: u64 = dir_manager.create_dir(1, "extdir").unwrap();

        // Create file and write
        let file_ino: u64 = 0x8000_0002;
        file_manager
            .create_file(file_ino, "extdir/file.txt".to_string(), dir_ino)
            .unwrap();
        file_manager.write(file_ino, 0, b"hello").await.unwrap();

        // Extend via truncate
        file_manager.truncate(file_ino, 15).await.unwrap();
        assert_eq!(file_manager.get_size(file_ino), Some(15));

        let data: Vec<u8> = file_manager.read(file_ino, 0, 100).await.unwrap();
        assert_eq!(data.len(), 15);
        assert_eq!(&data[0..5], b"hello");
        assert!(data[5..].iter().all(|&b| b == 0));
    }

    /// Test deleting a file in a new directory.
    #[tokio::test]
    async fn test_delete_file_in_new_dir() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        // Create new directory
        let dir_ino: u64 = dir_manager.create_dir(1, "deldir").unwrap();

        // Create file and write
        let file_ino: u64 = 0x8000_0002;
        file_manager
            .create_file(file_ino, "deldir/todelete.txt".to_string(), dir_ino)
            .unwrap();
        file_manager.write(file_ino, 0, b"delete me").await.unwrap();

        // Verify file exists
        assert!(file_manager.is_dirty(file_ino));
        assert!(file_manager.is_new_file(file_ino));

        // Delete the file
        file_manager.delete_file(file_ino).await.unwrap();

        // Verify file is gone (new files are completely removed, not marked deleted)
        assert!(!file_manager.is_dirty(file_ino));
    }

    /// Test creating multiple files in a new directory.
    #[tokio::test]
    async fn test_multiple_files_in_new_dir() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        // Create new directory
        let dir_ino: u64 = dir_manager.create_dir(1, "multidir").unwrap();

        // Create multiple files
        let file1_ino: u64 = 0x8000_0002;
        let file2_ino: u64 = 0x8000_0003;
        let file3_ino: u64 = 0x8000_0004;

        file_manager
            .create_file(file1_ino, "multidir/file1.txt".to_string(), dir_ino)
            .unwrap();
        file_manager
            .create_file(file2_ino, "multidir/file2.txt".to_string(), dir_ino)
            .unwrap();
        file_manager
            .create_file(file3_ino, "multidir/file3.txt".to_string(), dir_ino)
            .unwrap();

        // Write to each file
        file_manager.write(file1_ino, 0, b"content1").await.unwrap();
        file_manager.write(file2_ino, 0, b"content2").await.unwrap();
        file_manager.write(file3_ino, 0, b"content3").await.unwrap();

        // Verify each file
        let data1: Vec<u8> = file_manager.read(file1_ino, 0, 100).await.unwrap();
        let data2: Vec<u8> = file_manager.read(file2_ino, 0, 100).await.unwrap();
        let data3: Vec<u8> = file_manager.read(file3_ino, 0, 100).await.unwrap();

        assert_eq!(data1, b"content1");
        assert_eq!(data2, b"content2");
        assert_eq!(data3, b"content3");

        // Verify listing
        let files: Vec<(u64, String)> = file_manager.get_new_files_in_dir(dir_ino);
        assert_eq!(files.len(), 3);
    }

    /// Test file in nested new directories.
    #[tokio::test]
    async fn test_file_in_nested_new_dirs() {
        let (file_manager, dir_manager, inodes) = create_dir_test_env();

        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        // Create nested directories
        let dir1_ino: u64 = dir_manager.create_dir(1, "level1").unwrap();
        let dir2_ino: u64 = dir_manager.create_dir(dir1_ino, "level2").unwrap();

        // Verify nested directory path
        assert_eq!(
            dir_manager.get_new_dir_path(dir2_ino),
            Some("level1/level2".to_string())
        );

        // Create file in nested directory
        let file_ino: u64 = 0x8000_0003;
        file_manager
            .create_file(file_ino, "level1/level2/deep.txt".to_string(), dir2_ino)
            .unwrap();

        // Write and read
        file_manager
            .write(file_ino, 0, b"deep content")
            .await
            .unwrap();
        let data: Vec<u8> = file_manager.read(file_ino, 0, 100).await.unwrap();
        assert_eq!(data, b"deep content");
    }

    /// Test that get_new_dir_path returns correct path.
    #[test]
    fn test_get_new_dir_path() {
        let inodes = Arc::new(INodeManager::new());
        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        let dir_manager = Arc::new(DirtyDirManager::new(inodes, HashSet::new()));

        // Create directory
        let dir_ino: u64 = dir_manager.create_dir(1, "testdir").unwrap();

        // Verify path lookup
        assert_eq!(
            dir_manager.get_new_dir_path(dir_ino),
            Some("testdir".to_string())
        );

        // Non-existent directory returns None
        assert_eq!(dir_manager.get_new_dir_path(999), None);
    }

    /// Test that deleted directories don't return paths.
    #[test]
    fn test_deleted_dir_no_path() {
        let inodes = Arc::new(INodeManager::new());
        // Add root directory
        let _root_ino: u64 = inodes.add_directory("");

        let dir_manager = Arc::new(DirtyDirManager::new(inodes, HashSet::new()));

        // Create and delete directory
        let _dir_ino: u64 = dir_manager.create_dir(1, "tempdir").unwrap();
        dir_manager.delete_dir(1, "tempdir").unwrap();

        // Deleted directory should not return path via get_new_dir_path
        // (it only returns paths for New state directories)
        // Note: After delete, the directory entry is removed for new dirs
        assert_eq!(dir_manager.get_new_dir_path(_dir_ino), None);
    }

    /// Test that new directories don't appear twice in readdir listing.
    ///
    /// Bug: create_dir adds to both INodeManager and DirtyDirManager, causing
    /// readdir to list the directory twice (once from each source).
    #[test]
    fn test_new_dir_not_duplicated_in_listing() {
        let inodes = Arc::new(INodeManager::new());
        let _root_ino: u64 = inodes.add_directory("");

        let dir_manager = Arc::new(DirtyDirManager::new(inodes.clone(), HashSet::new()));

        // Create new directory
        let new_dir_ino: u64 = dir_manager.create_dir(1, "newdir").unwrap();

        // Simulate readdir logic: collect entries from both sources
        let mut entries: Vec<(u64, String)> = Vec::new();

        // Source 1: INodeManager children (skip new dirs to avoid duplicates)
        if let Some(children) = inodes.get_dir_children(1) {
            for (name, cid) in children {
                // Skip new directories - they'll be added from dirty_dir_manager
                if dir_manager.get_state(cid) == Some(DirtyDirState::New) {
                    continue;
                }
                entries.push((cid, name));
            }
        }

        // Source 2: DirtyDirManager new directories
        let new_dirs: Vec<(u64, String)> = dir_manager.get_new_dirs_in_parent(1);
        for (ino, name) in new_dirs {
            entries.push((ino, name));
        }

        // Verify: directory should appear exactly once
        let newdir_count: usize = entries.iter().filter(|(_, n)| n == "newdir").count();
        assert_eq!(newdir_count, 1, "newdir should appear exactly once");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], (new_dir_ino, "newdir".to_string()));
    }
}
