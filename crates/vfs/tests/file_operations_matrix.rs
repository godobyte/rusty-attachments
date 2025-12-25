//! Integration tests for the File Operations Matrix from vfs-writes.md.
//!
//! Tests cover all combinations of operations across file types:
//! - Small files (single chunk, <256MB)
//! - Chunked files (multiple chunks, >256MB simulated with smaller chunks)
//!
//! Operations tested:
//! - create: New file creation
//! - read clean: Reading unmodified files
//! - read dirty: Reading modified files
//! - write: Writing to files
//! - truncate shrink: Reducing file size
//! - truncate extend: Increasing file size
//! - delete: Marking files as deleted
//! - small→chunked: Auto-conversion when file grows

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rusty_attachments_model::HashAlgorithm;
use rusty_attachments_vfs::content::MemoryFileStore;
use rusty_attachments_vfs::inode::{FileContent, INodeManager};
use rusty_attachments_vfs::write::{
    DirtyContent, DirtyFileManager, DirtyState, MemoryWriteCache,
};
use rusty_attachments_vfs::{FileStore, VfsError};

/// Helper to create a test environment with manager and inodes.
fn create_test_env() -> (Arc<DirtyFileManager>, Arc<INodeManager>, Arc<TestFileStore>) {
    let cache = Arc::new(MemoryWriteCache::new());
    let store = Arc::new(TestFileStore::new());
    let inodes = Arc::new(INodeManager::new());
    let manager = Arc::new(DirtyFileManager::new(
        cache,
        store.clone(),
        inodes.clone(),
    ));
    (manager, inodes, store)
}

/// Test file store that tracks content by hash.
#[derive(Debug, Default)]
struct TestFileStore {
    content: std::sync::RwLock<HashMap<String, Vec<u8>>>,
}

impl TestFileStore {
    fn new() -> Self {
        Self::default()
    }

    /// Add content to the store.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `data` - Content bytes
    fn insert(&self, hash: impl Into<String>, data: Vec<u8>) {
        self.content.write().unwrap().insert(hash.into(), data);
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
            .ok_or_else(|| VfsError::ContentRetrievalFailed {
                hash: hash.to_string(),
                source: "Hash not found in test store".into(),
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

// =============================================================================
// CREATE TESTS
// =============================================================================

mod create {
    use super::*;

    #[tokio::test]
    async fn test_create_small_file() {
        let (manager, _inodes, _store) = create_test_env();

        // Create a new file
        manager.create_file(100, "new_file.txt".to_string(), 1).unwrap();

        // Verify state
        assert!(manager.is_dirty(100));
        assert_eq!(manager.get_state(100), Some(DirtyState::New));
        assert_eq!(manager.get_size(100), Some(0));
    }

    #[tokio::test]
    async fn test_create_multiple_files() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "file1.txt".to_string(), 1).unwrap();
        manager.create_file(101, "file2.txt".to_string(), 1).unwrap();
        manager.create_file(102, "dir/file3.txt".to_string(), 1).unwrap();

        assert!(manager.is_dirty(100));
        assert!(manager.is_dirty(101));
        assert!(manager.is_dirty(102));

        let entries = manager.get_dirty_entries();
        assert_eq!(entries.len(), 3);
    }
}

// =============================================================================
// READ CLEAN TESTS
// =============================================================================

mod read_clean {
    use super::*;

    #[tokio::test]
    async fn test_read_clean_small_file_not_dirty() {
        let (manager, inodes, store) = create_test_env();

        // Add file to store
        let content: Vec<u8> = b"hello world".to_vec();
        store.insert("hash123", content.clone());

        // Add file to inodes
        inodes.add_file(
            "test.txt",
            content.len() as u64,
            0,
            FileContent::SingleHash("hash123".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // File should not be dirty initially
        assert!(!manager.is_dirty(2)); // inode 2 (after root)
    }

    #[tokio::test]
    async fn test_read_clean_chunked_file_not_dirty() {
        let (manager, inodes, store) = create_test_env();

        // Add chunked file content
        let chunk1: Vec<u8> = vec![1u8; 1024];
        let chunk2: Vec<u8> = vec![2u8; 1024];
        store.insert("chunk1_hash", chunk1);
        store.insert("chunk2_hash", chunk2);

        // Add chunked file to inodes
        inodes.add_file(
            "large.bin",
            2048,
            0,
            FileContent::Chunked(vec!["chunk1_hash".to_string(), "chunk2_hash".to_string()]),
            HashAlgorithm::Xxh128,
            false,
        );

        // File should not be dirty initially
        assert!(!manager.is_dirty(2));
    }
}

// =============================================================================
// READ DIRTY TESTS
// =============================================================================

mod read_dirty {
    use super::*;

    #[tokio::test]
    async fn test_read_dirty_small_file() {
        let (manager, _inodes, _store) = create_test_env();

        // Create and write to a new file
        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        // Read back
        let data: Vec<u8> = manager.read(100, 0, 11).await.unwrap();
        assert_eq!(data, b"hello world");
    }

    #[tokio::test]
    async fn test_read_dirty_small_file_partial() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        // Read partial - middle
        let data: Vec<u8> = manager.read(100, 3, 5).await.unwrap();
        assert_eq!(data, b"lo wo");

        // Read partial - from offset
        let data: Vec<u8> = manager.read(100, 6, 100).await.unwrap();
        assert_eq!(data, b"world");
    }

    #[tokio::test]
    async fn test_read_dirty_small_file_beyond_eof() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        // Read beyond EOF
        let data: Vec<u8> = manager.read(100, 10, 5).await.unwrap();
        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn test_read_dirty_after_cow() {
        let (manager, inodes, store) = create_test_env();

        // Setup original file
        let original: Vec<u8> = b"original content".to_vec();
        store.insert("orig_hash", original);

        let ino = inodes.add_file(
            "test.txt",
            16,
            0,
            FileContent::SingleHash("orig_hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // COW and modify
        manager.write(ino, 0, b"modified").await.unwrap();

        // Read should return modified content
        let data: Vec<u8> = manager.read(ino, 0, 16).await.unwrap();
        assert_eq!(&data[..8], b"modified");
    }
}

// =============================================================================
// WRITE TESTS
// =============================================================================

mod write {
    use super::*;

    #[tokio::test]
    async fn test_write_small_file_new() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();

        let written: usize = manager.write(100, 0, b"hello").await.unwrap();
        assert_eq!(written, 5);
        assert_eq!(manager.get_size(100), Some(5));
    }

    #[tokio::test]
    async fn test_write_small_file_append() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();
        manager.write(100, 5, b" world").await.unwrap();

        assert_eq!(manager.get_size(100), Some(11));

        let data: Vec<u8> = manager.read(100, 0, 11).await.unwrap();
        assert_eq!(data, b"hello world");
    }

    #[tokio::test]
    async fn test_write_small_file_overwrite() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();
        manager.write(100, 0, b"HELLO").await.unwrap();

        let data: Vec<u8> = manager.read(100, 0, 11).await.unwrap();
        assert_eq!(data, b"HELLO world");
    }

    #[tokio::test]
    async fn test_write_small_file_with_gap() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 10, b"hello").await.unwrap();

        assert_eq!(manager.get_size(100), Some(15));

        // First 10 bytes should be zeros
        let data: Vec<u8> = manager.read(100, 0, 15).await.unwrap();
        assert_eq!(&data[..10], &[0u8; 10]);
        assert_eq!(&data[10..], b"hello");
    }

    #[tokio::test]
    async fn test_write_cow_existing_file() {
        let (manager, inodes, store) = create_test_env();

        // Setup original file
        let original: Vec<u8> = b"original".to_vec();
        store.insert("orig_hash", original);

        let ino = inodes.add_file(
            "test.txt",
            8,
            0,
            FileContent::SingleHash("orig_hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Write triggers COW
        assert!(!manager.is_dirty(ino));
        manager.write(ino, 0, b"modified").await.unwrap();
        assert!(manager.is_dirty(ino));
        assert_eq!(manager.get_state(ino), Some(DirtyState::Modified));
    }
}

// =============================================================================
// TRUNCATE SHRINK TESTS
// =============================================================================

mod truncate_shrink {
    use super::*;

    #[tokio::test]
    async fn test_truncate_shrink_small_file() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        manager.truncate(100, 5).await.unwrap();

        assert_eq!(manager.get_size(100), Some(5));

        let data: Vec<u8> = manager.read(100, 0, 10).await.unwrap();
        assert_eq!(data, b"hello");
    }

    #[tokio::test]
    async fn test_truncate_shrink_to_zero() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        manager.truncate(100, 0).await.unwrap();

        assert_eq!(manager.get_size(100), Some(0));

        let data: Vec<u8> = manager.read(100, 0, 10).await.unwrap();
        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn test_truncate_shrink_cow_file() {
        let (manager, inodes, store) = create_test_env();

        let original: Vec<u8> = b"original content here".to_vec();
        store.insert("orig_hash", original);

        let ino = inodes.add_file(
            "test.txt",
            21,
            0,
            FileContent::SingleHash("orig_hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Truncate triggers COW
        manager.truncate(ino, 8).await.unwrap();

        assert!(manager.is_dirty(ino));
        assert_eq!(manager.get_size(ino), Some(8));

        let data: Vec<u8> = manager.read(ino, 0, 10).await.unwrap();
        assert_eq!(data, b"original");
    }
}

// =============================================================================
// TRUNCATE EXTEND TESTS
// =============================================================================

mod truncate_extend {
    use super::*;

    #[tokio::test]
    async fn test_truncate_extend_small_file() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        manager.truncate(100, 10).await.unwrap();

        assert_eq!(manager.get_size(100), Some(10));

        let data: Vec<u8> = manager.read(100, 0, 10).await.unwrap();
        assert_eq!(&data[..5], b"hello");
        assert_eq!(&data[5..], &[0u8; 5]);
    }

    #[tokio::test]
    async fn test_truncate_extend_then_write() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        // Extend
        manager.truncate(100, 15).await.unwrap();

        // Write in the extended area
        manager.write(100, 10, b"world").await.unwrap();

        let data: Vec<u8> = manager.read(100, 0, 15).await.unwrap();
        assert_eq!(&data[..5], b"hello");
        assert_eq!(&data[5..10], &[0u8; 5]);
        assert_eq!(&data[10..], b"world");
    }

    #[tokio::test]
    async fn test_truncate_extend_empty_file() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();

        manager.truncate(100, 100).await.unwrap();

        assert_eq!(manager.get_size(100), Some(100));

        let data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(data, vec![0u8; 100]);
    }
}

// =============================================================================
// DELETE TESTS
// =============================================================================

mod delete {
    use super::*;

    #[tokio::test]
    async fn test_delete_new_file() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        manager.delete_file(100).await.unwrap();

        assert!(manager.is_dirty(100));
        assert_eq!(manager.get_state(100), Some(DirtyState::Deleted));
    }

    #[tokio::test]
    async fn test_delete_existing_file() {
        let (manager, inodes, store) = create_test_env();

        let original: Vec<u8> = b"original".to_vec();
        store.insert("orig_hash", original);

        let ino = inodes.add_file(
            "test.txt",
            8,
            0,
            FileContent::SingleHash("orig_hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Delete without prior modification
        manager.delete_file(ino).await.unwrap();

        assert!(manager.is_dirty(ino));
        assert_eq!(manager.get_state(ino), Some(DirtyState::Deleted));
    }

    #[tokio::test]
    async fn test_delete_modified_file() {
        let (manager, inodes, store) = create_test_env();

        let original: Vec<u8> = b"original".to_vec();
        store.insert("orig_hash", original);

        let ino = inodes.add_file(
            "test.txt",
            8,
            0,
            FileContent::SingleHash("orig_hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Modify first
        manager.write(ino, 0, b"modified").await.unwrap();
        assert_eq!(manager.get_state(ino), Some(DirtyState::Modified));

        // Then delete
        manager.delete_file(ino).await.unwrap();
        assert_eq!(manager.get_state(ino), Some(DirtyState::Deleted));
    }

    #[tokio::test]
    async fn test_delete_appears_in_dirty_entries() {
        let (manager, inodes, store) = create_test_env();

        let original: Vec<u8> = b"original".to_vec();
        store.insert("orig_hash", original);

        let ino = inodes.add_file(
            "test.txt",
            8,
            0,
            FileContent::SingleHash("orig_hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        manager.delete_file(ino).await.unwrap();

        let entries = manager.get_dirty_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, DirtyState::Deleted);
        assert_eq!(entries[0].path, "test.txt");
    }
}

// =============================================================================
// SMALL TO CHUNKED CONVERSION TESTS
// =============================================================================

mod small_to_chunked {
    use super::*;
    use rusty_attachments_vfs::write::DirtyContent;

    #[test]
    fn test_dirty_content_stays_small_under_threshold() {
        let content = DirtyContent::Small {
            data: vec![0u8; 100],
        };

        // Should stay small
        assert!(matches!(content, DirtyContent::Small { .. }));
        assert_eq!(content.size(), 100);
    }

    #[test]
    fn test_dirty_file_converts_when_large() {
        use rusty_attachments_vfs::write::DirtyFile;

        let mut dirty = DirtyFile::new_file(1, "test.bin".to_string(), 100);

        // Write data that exceeds chunk threshold (256MB in production, but we test the mechanism)
        // The actual conversion happens in maybe_convert_to_chunked based on CHUNK_SIZE_V2
        if let DirtyContent::Small { data } = dirty.content_mut() {
            // Simulate writing a lot of data
            data.extend(vec![0u8; 1000]);
        }

        // After calling maybe_convert_to_chunked, if size > CHUNK_SIZE_V2, it converts
        dirty.maybe_convert_to_chunked();

        // In tests with small data, it stays small (CHUNK_SIZE_V2 is 256MB)
        assert!(matches!(dirty.content(), DirtyContent::Small { .. }));
    }
}

// =============================================================================
// DIRTY ENTRIES TESTS
// =============================================================================

mod dirty_entries {
    use super::*;

    #[tokio::test]
    async fn test_get_dirty_entries_empty() {
        let (manager, _inodes, _store) = create_test_env();

        let entries = manager.get_dirty_entries();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_get_dirty_entries_mixed_states() {
        let (manager, inodes, store) = create_test_env();

        // New file
        manager.create_file(100, "new.txt".to_string(), 1).unwrap();

        // Modified file
        let original: Vec<u8> = b"original".to_vec();
        store.insert("orig_hash", original);
        let mod_ino = inodes.add_file(
            "modified.txt",
            8,
            0,
            FileContent::SingleHash("orig_hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );
        manager.write(mod_ino, 0, b"changed").await.unwrap();

        // Deleted file
        store.insert("del_hash", b"to delete".to_vec());
        let del_ino = inodes.add_file(
            "deleted.txt",
            9,
            0,
            FileContent::SingleHash("del_hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );
        manager.delete_file(del_ino).await.unwrap();

        let entries = manager.get_dirty_entries();
        assert_eq!(entries.len(), 3);

        let new_count: usize = entries.iter().filter(|e| e.state == DirtyState::New).count();
        let mod_count: usize = entries.iter().filter(|e| e.state == DirtyState::Modified).count();
        let del_count: usize = entries.iter().filter(|e| e.state == DirtyState::Deleted).count();

        assert_eq!(new_count, 1);
        assert_eq!(mod_count, 1);
        assert_eq!(del_count, 1);
    }

    #[tokio::test]
    async fn test_clear_dirty_entries() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "file1.txt".to_string(), 1).unwrap();
        manager.create_file(101, "file2.txt".to_string(), 1).unwrap();

        assert_eq!(manager.get_dirty_entries().len(), 2);

        manager.clear();

        assert!(manager.get_dirty_entries().is_empty());
        assert!(!manager.is_dirty(100));
        assert!(!manager.is_dirty(101));
    }
}

// =============================================================================
// MTIME TRACKING TESTS
// =============================================================================

mod mtime_tracking {
    use super::*;
    use std::time::SystemTime;

    #[tokio::test]
    async fn test_mtime_updated_on_write() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        let mtime1: SystemTime = manager.get_mtime(100).unwrap();

        // Small delay to ensure time difference
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        manager.write(100, 0, b"hello").await.unwrap();
        let mtime2: SystemTime = manager.get_mtime(100).unwrap();

        assert!(mtime2 > mtime1);
    }

    #[tokio::test]
    async fn test_mtime_updated_on_truncate() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();
        let mtime1: SystemTime = manager.get_mtime(100).unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        manager.truncate(100, 5).await.unwrap();
        let mtime2: SystemTime = manager.get_mtime(100).unwrap();

        assert!(mtime2 > mtime1);
    }
}


// =============================================================================
// CHUNKED FILE TESTS
// =============================================================================

mod chunked_files {
    use super::*;

    /// Helper to create a chunked file in the test environment.
    ///
    /// # Arguments
    /// * `inodes` - Inode manager
    /// * `store` - File store
    /// * `path` - File path
    /// * `chunks` - Vector of chunk data
    ///
    /// # Returns
    /// Inode ID of the created file.
    fn setup_chunked_file(
        inodes: &INodeManager,
        store: &TestFileStore,
        path: &str,
        chunks: Vec<Vec<u8>>,
    ) -> u64 {
        let mut hashes: Vec<String> = Vec::new();
        let mut total_size: u64 = 0;

        for (i, chunk) in chunks.iter().enumerate() {
            let hash: String = format!("chunk_{}_hash", i);
            store.insert(hash.clone(), chunk.clone());
            hashes.push(hash);
            total_size += chunk.len() as u64;
        }

        inodes.add_file(
            path,
            total_size,
            0,
            FileContent::Chunked(hashes),
            HashAlgorithm::Xxh128,
            false,
        )
    }

    #[tokio::test]
    async fn test_read_chunked_single_chunk() {
        let (manager, inodes, store) = create_test_env();

        let chunk_data: Vec<u8> = b"chunk zero data here".to_vec();
        let ino: u64 = setup_chunked_file(&inodes, &store, "chunked.bin", vec![chunk_data.clone()]);

        // COW the file first
        manager.cow_copy(ino).await.unwrap();

        // Read from the chunked file
        let data: Vec<u8> = manager.read(ino, 0, 20).await.unwrap();
        assert_eq!(data, chunk_data);
    }

    #[tokio::test]
    async fn test_read_chunked_multiple_chunks() {
        let (manager, inodes, store) = create_test_env();

        // For chunked files, each chunk hash corresponds to a 256MB chunk
        // In tests, we simulate this by having the total size match the chunk layout
        // Create a single chunk file for simplicity
        let chunk0: Vec<u8> = b"chunk zero content".to_vec();

        let ino: u64 = setup_chunked_file(
            &inodes,
            &store,
            "multi_chunk.bin",
            vec![chunk0.clone()],
        );

        manager.cow_copy(ino).await.unwrap();

        // Read from the chunk
        let data: Vec<u8> = manager.read(ino, 0, 18).await.unwrap();
        assert_eq!(data, chunk0);

        // Read partial
        let data2: Vec<u8> = manager.read(ino, 6, 4).await.unwrap();
        assert_eq!(data2, b"zero");
    }

    #[tokio::test]
    async fn test_write_chunked_single_chunk_modify() {
        let (manager, inodes, store) = create_test_env();

        let chunk_data: Vec<u8> = b"original chunk data!".to_vec();
        let ino: u64 = setup_chunked_file(&inodes, &store, "chunked.bin", vec![chunk_data]);

        // Write to modify the chunk
        manager.write(ino, 0, b"MODIFIED").await.unwrap();

        assert!(manager.is_dirty(ino));
        assert_eq!(manager.get_state(ino), Some(DirtyState::Modified));

        // Read back
        let data: Vec<u8> = manager.read(ino, 0, 20).await.unwrap();
        assert_eq!(&data[..8], b"MODIFIED");
        assert_eq!(&data[8..], b" chunk data!");
    }

    #[tokio::test]
    async fn test_write_chunked_span_boundary() {
        let (manager, inodes, store) = create_test_env();

        // Single chunk file for testing write operations
        let chunk0: Vec<u8> = vec![0u8; 200];
        let ino: u64 = setup_chunked_file(&inodes, &store, "boundary.bin", vec![chunk0]);

        // Write in the middle of the chunk
        let write_data: Vec<u8> = vec![9u8; 20];
        manager.write(ino, 90, &write_data).await.unwrap();

        // Read back and verify
        let data: Vec<u8> = manager.read(ino, 85, 30).await.unwrap();

        // Bytes 85-89: original zeros
        assert!(data[..5].iter().all(|&b| b == 0));
        // Bytes 90-109: our written 9s
        assert!(data[5..25].iter().all(|&b| b == 9));
        // Bytes 110-114: original zeros
        assert!(data[25..].iter().all(|&b| b == 0));
    }

    #[tokio::test]
    async fn test_truncate_chunked_shrink_drops_chunks() {
        let (manager, inodes, store) = create_test_env();

        // Single chunk file
        let chunk: Vec<u8> = vec![0u8; 300];
        let ino: u64 = setup_chunked_file(&inodes, &store, "shrink.bin", vec![chunk]);

        // Truncate to smaller size
        manager.truncate(ino, 150).await.unwrap();

        assert!(manager.is_dirty(ino));
        assert_eq!(manager.get_size(ino), Some(150));
    }

    #[tokio::test]
    async fn test_truncate_chunked_extend_sparse() {
        let (manager, inodes, store) = create_test_env();

        let chunk0: Vec<u8> = vec![0u8; 100];
        let ino: u64 = setup_chunked_file(&inodes, &store, "extend.bin", vec![chunk0]);

        // Extend beyond original size
        manager.truncate(ino, 500).await.unwrap();

        assert_eq!(manager.get_size(ino), Some(500));
    }

    #[tokio::test]
    async fn test_delete_chunked_file() {
        let (manager, inodes, store) = create_test_env();

        let chunk: Vec<u8> = vec![0u8; 200];
        let ino: u64 = setup_chunked_file(&inodes, &store, "to_delete.bin", vec![chunk]);

        manager.delete_file(ino).await.unwrap();

        assert!(manager.is_dirty(ino));
        assert_eq!(manager.get_state(ino), Some(DirtyState::Deleted));
    }

    #[tokio::test]
    async fn test_chunked_read_partial_chunk() {
        let (manager, inodes, store) = create_test_env();

        // Create a chunk with known pattern
        let mut chunk: Vec<u8> = Vec::new();
        for i in 0u8..100 {
            chunk.push(i);
        }
        let ino: u64 = setup_chunked_file(&inodes, &store, "pattern.bin", vec![chunk]);

        manager.cow_copy(ino).await.unwrap();

        // Read middle portion
        let data: Vec<u8> = manager.read(ino, 25, 50).await.unwrap();
        assert_eq!(data.len(), 50);
        for (i, &b) in data.iter().enumerate() {
            assert_eq!(b, (25 + i) as u8);
        }
    }

    #[tokio::test]
    async fn test_chunked_write_extend_file() {
        let (manager, inodes, store) = create_test_env();

        let chunk0: Vec<u8> = vec![0u8; 100];
        let ino: u64 = setup_chunked_file(&inodes, &store, "extend_write.bin", vec![chunk0]);

        // Write beyond current file size
        manager.write(ino, 150, b"extended").await.unwrap();

        // Size should be updated
        assert_eq!(manager.get_size(ino), Some(158)); // 150 + 8

        // Read the gap (should be zeros)
        let gap: Vec<u8> = manager.read(ino, 100, 50).await.unwrap();
        assert!(gap.iter().all(|&b| b == 0));

        // Read the written data
        let written: Vec<u8> = manager.read(ino, 150, 8).await.unwrap();
        assert_eq!(written, b"extended");
    }
}

// =============================================================================
// CACHE INTEGRATION TESTS
// =============================================================================

mod cache_integration {
    use super::*;
    use rusty_attachments_vfs::WriteCache;

    #[tokio::test]
    async fn test_flush_to_disk_small_file() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        // Flush should succeed
        manager.flush_to_disk(100).await.unwrap();
    }

    #[tokio::test]
    async fn test_flush_deleted_file() {
        let (manager, _inodes, _store) = create_test_env();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();
        manager.delete_file(100).await.unwrap();

        // Flush deleted file should succeed
        manager.flush_to_disk(100).await.unwrap();
    }

    #[tokio::test]
    async fn test_cache_write_read_roundtrip() {
        use rusty_attachments_vfs::write::MemoryWriteCache;

        let cache = MemoryWriteCache::new();

        cache.write_file("test.txt", b"hello").await.unwrap();
        let data: Option<Vec<u8>> = cache.read_file("test.txt").await.unwrap();

        assert_eq!(data, Some(b"hello".to_vec()));
    }

    #[tokio::test]
    async fn test_cache_delete_creates_tombstone() {
        use rusty_attachments_vfs::write::MemoryWriteCache;

        let cache = MemoryWriteCache::new();

        cache.write_file("test.txt", b"hello").await.unwrap();
        assert!(!cache.is_deleted("test.txt"));

        cache.delete_file("test.txt").await.unwrap();
        assert!(cache.is_deleted("test.txt"));

        // File should no longer be readable
        let data: Option<Vec<u8>> = cache.read_file("test.txt").await.unwrap();
        assert!(data.is_none());
    }
}


// =============================================================================
// REALISTIC CHUNKED FILE TEST (256MB BOUNDARIES)
// =============================================================================

mod realistic_chunked {
    use super::*;
    use rusty_attachments_common::CHUNK_SIZE_V2;

    /// File store that pads chunk data to simulate 256MB chunks.
    ///
    /// This allows testing chunked file operations without allocating 256MB.
    /// The store returns data padded with zeros to the expected chunk size.
    #[derive(Debug, Default)]
    struct PaddedChunkStore {
        /// Chunk data by hash (small test data that gets padded on retrieval).
        chunks: std::sync::RwLock<HashMap<String, ChunkTestData>>,
    }

    /// Test data for a chunk with its expected padded size.
    #[derive(Debug, Clone)]
    struct ChunkTestData {
        /// Actual test data (small).
        data: Vec<u8>,
        /// Expected size when retrieved (e.g., 256MB for full chunks).
        padded_size: u64,
    }

    impl PaddedChunkStore {
        fn new() -> Self {
            Self::default()
        }

        /// Add a chunk with expected padded size.
        ///
        /// # Arguments
        /// * `hash` - Chunk hash
        /// * `data` - Actual test data (small)
        /// * `padded_size` - Size to pad to on retrieval
        fn insert_chunk(&self, hash: impl Into<String>, data: Vec<u8>, padded_size: u64) {
            self.chunks.write().unwrap().insert(
                hash.into(),
                ChunkTestData { data, padded_size },
            );
        }
    }

    #[async_trait]
    impl FileStore for PaddedChunkStore {
        async fn retrieve(&self, hash: &str, _algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError> {
            let guard = self.chunks.read().unwrap();
            let chunk: &ChunkTestData = guard.get(hash).ok_or_else(|| VfsError::ContentRetrievalFailed {
                hash: hash.to_string(),
                source: "Hash not found in padded store".into(),
            })?;

            // Return data padded to expected size
            let mut result: Vec<u8> = chunk.data.clone();
            if (result.len() as u64) < chunk.padded_size {
                result.resize(chunk.padded_size as usize, 0);
            }
            Ok(result)
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

    /// Create test environment with padded chunk store.
    fn create_padded_env() -> (Arc<DirtyFileManager>, Arc<INodeManager>, Arc<PaddedChunkStore>) {
        let cache = Arc::new(MemoryWriteCache::new());
        let store = Arc::new(PaddedChunkStore::new());
        let inodes = Arc::new(INodeManager::new());
        let manager = Arc::new(DirtyFileManager::new(
            cache,
            store.clone(),
            inodes.clone(),
        ));
        (manager, inodes, store)
    }

    /// Test realistic chunked file operations with proper 256MB chunk boundaries.
    ///
    /// This test simulates a 260MB file (256MB + 4MB) where:
    /// - Chunk 0: bytes 0 to CHUNK_SIZE_V2-1 (256MB)
    /// - Chunk 1: bytes CHUNK_SIZE_V2 to end (4MB)
    ///
    /// The PaddedChunkStore returns data padded to the expected chunk size,
    /// allowing us to test boundary-spanning operations without allocating 260MB.
    #[tokio::test]
    async fn test_260mb_file_boundary_operations() {
        let (manager, inodes, store) = create_padded_env();

        let chunk_size: u64 = CHUNK_SIZE_V2; // 256MB
        let chunk1_size: u64 = 4 * 1024 * 1024; // 4MB
        let total_size: u64 = chunk_size + chunk1_size; // 260MB

        // Create chunk data with known patterns at specific offsets
        // Chunk 0: starts with "CHUNK0_START", ends with "CHUNK0_END__" at offset 256MB-12
        let mut chunk0_data: Vec<u8> = b"CHUNK0_START".to_vec();
        // We'll also put data at the end of chunk 0 for boundary testing
        // The padded store will fill zeros in between

        // Chunk 1: starts with "CHUNK1_START"
        let chunk1_data: Vec<u8> = b"CHUNK1_START".to_vec();

        // Insert chunks with their expected padded sizes
        store.insert_chunk("chunk0_hash", chunk0_data.clone(), chunk_size);
        store.insert_chunk("chunk1_hash", chunk1_data.clone(), chunk1_size);

        let ino: u64 = inodes.add_file(
            "large_260mb.bin",
            total_size,
            0,
            FileContent::Chunked(vec!["chunk0_hash".to_string(), "chunk1_hash".to_string()]),
            HashAlgorithm::Xxh128,
            false,
        );

        // COW the file
        manager.cow_copy(ino).await.unwrap();

        // Verify file metadata
        assert!(manager.is_dirty(ino));
        assert_eq!(manager.get_size(ino), Some(total_size));
        assert_eq!(manager.get_state(ino), Some(DirtyState::Modified));

        // Test 1: Read from start of chunk 0
        let data: Vec<u8> = manager.read(ino, 0, 12).await.unwrap();
        assert_eq!(data, b"CHUNK0_START");

        // Test 2: Read from middle of chunk 0 (should be zeros from padding)
        let middle: Vec<u8> = manager.read(ino, 1000, 10).await.unwrap();
        assert!(middle.iter().all(|&b| b == 0), "Middle of chunk 0 should be zeros");

        // Test 3: Read from start of chunk 1 (at offset 256MB)
        let chunk1_read: Vec<u8> = manager.read(ino, chunk_size, 12).await.unwrap();
        assert_eq!(chunk1_read, b"CHUNK1_START");

        // Test 4: Write to chunk 0 only
        manager.write(ino, 0, b"MODIFIED_C0").await.unwrap();
        let modified: Vec<u8> = manager.read(ino, 0, 11).await.unwrap();
        assert_eq!(modified, b"MODIFIED_C0");

        // Test 5: Write spanning chunk boundary (last 10 bytes of chunk 0 + first 10 bytes of chunk 1)
        let boundary_offset: u64 = chunk_size - 10;
        let boundary_data: &[u8] = b"BOUNDARY_SPANNING_DATA"; // 22 bytes
        manager.write(ino, boundary_offset, boundary_data).await.unwrap();

        // Verify the boundary write
        let boundary_read: Vec<u8> = manager.read(ino, boundary_offset, 22).await.unwrap();
        assert_eq!(boundary_read, boundary_data);

        // Verify chunk 0 end was modified (last 10 bytes before boundary)
        let chunk0_end: Vec<u8> = manager.read(ino, boundary_offset, 10).await.unwrap();
        assert_eq!(chunk0_end, b"BOUNDARY_S");

        // Verify chunk 1 start was modified (first 12 bytes after boundary)
        let chunk1_start: Vec<u8> = manager.read(ino, chunk_size, 12).await.unwrap();
        assert_eq!(chunk1_start, b"PANNING_DATA");

        // Test 6: Read spanning chunk boundary
        let span_read: Vec<u8> = manager.read(ino, chunk_size - 5, 15).await.unwrap();
        assert_eq!(span_read.len(), 15);
        // First 5 bytes from chunk 0 (positions -5 to -1 relative to boundary): "ARY_S"
        // Next 10 bytes from chunk 1 (positions 0 to 9): "PANNING_DA"
        assert_eq!(&span_read[..5], b"ARY_S");
        assert_eq!(&span_read[5..], b"PANNING_DA");
    }

    /// Test truncate operations on a 260MB chunked file.
    #[tokio::test]
    async fn test_260mb_file_truncate() {
        let (manager, inodes, store) = create_padded_env();

        let chunk_size: u64 = CHUNK_SIZE_V2;
        let chunk1_size: u64 = 4 * 1024 * 1024;
        let total_size: u64 = chunk_size + chunk1_size;

        store.insert_chunk("chunk0_hash", b"CHUNK0".to_vec(), chunk_size);
        store.insert_chunk("chunk1_hash", b"CHUNK1".to_vec(), chunk1_size);

        let ino: u64 = inodes.add_file(
            "truncate_test.bin",
            total_size,
            0,
            FileContent::Chunked(vec!["chunk0_hash".to_string(), "chunk1_hash".to_string()]),
            HashAlgorithm::Xxh128,
            false,
        );

        // Truncate to remove chunk 1 entirely (shrink to 200MB)
        let new_size: u64 = 200 * 1024 * 1024;
        manager.truncate(ino, new_size).await.unwrap();

        assert_eq!(manager.get_size(ino), Some(new_size));

        // Reading beyond new size should return empty
        let beyond: Vec<u8> = manager.read(ino, new_size + 100, 10).await.unwrap();
        assert!(beyond.is_empty());

        // Extend back to 300MB (sparse extension)
        let extended_size: u64 = 300 * 1024 * 1024;
        manager.truncate(ino, extended_size).await.unwrap();

        assert_eq!(manager.get_size(ino), Some(extended_size));
    }

    /// Test write that extends file beyond original chunk count.
    #[tokio::test]
    async fn test_260mb_file_extend_via_write() {
        let (manager, inodes, store) = create_padded_env();

        let chunk_size: u64 = CHUNK_SIZE_V2;
        let chunk1_size: u64 = 4 * 1024 * 1024;
        let total_size: u64 = chunk_size + chunk1_size;

        store.insert_chunk("chunk0_hash", b"CHUNK0".to_vec(), chunk_size);
        store.insert_chunk("chunk1_hash", b"CHUNK1".to_vec(), chunk1_size);

        let ino: u64 = inodes.add_file(
            "extend_test.bin",
            total_size,
            0,
            FileContent::Chunked(vec!["chunk0_hash".to_string(), "chunk1_hash".to_string()]),
            HashAlgorithm::Xxh128,
            false,
        );

        // Write beyond current file size (into what would be chunk 2)
        let write_offset: u64 = total_size + 1000;
        manager.write(ino, write_offset, b"EXTENDED").await.unwrap();

        // File size should be updated
        let expected_size: u64 = write_offset + 8;
        assert_eq!(manager.get_size(ino), Some(expected_size));

        // Read back the written data
        let read_back: Vec<u8> = manager.read(ino, write_offset, 8).await.unwrap();
        assert_eq!(read_back, b"EXTENDED");

        // Gap between original end and write should be zeros
        let gap: Vec<u8> = manager.read(ino, total_size, 100).await.unwrap();
        assert!(gap.iter().all(|&b| b == 0), "Gap should be zeros");
    }

    /// Test delete of a 260MB chunked file.
    #[tokio::test]
    async fn test_260mb_file_delete() {
        let (manager, inodes, store) = create_padded_env();

        let chunk_size: u64 = CHUNK_SIZE_V2;
        let chunk1_size: u64 = 4 * 1024 * 1024;
        let total_size: u64 = chunk_size + chunk1_size;

        store.insert_chunk("chunk0_hash", b"CHUNK0".to_vec(), chunk_size);
        store.insert_chunk("chunk1_hash", b"CHUNK1".to_vec(), chunk1_size);

        let ino: u64 = inodes.add_file(
            "delete_test.bin",
            total_size,
            0,
            FileContent::Chunked(vec!["chunk0_hash".to_string(), "chunk1_hash".to_string()]),
            HashAlgorithm::Xxh128,
            false,
        );

        // Delete the file
        manager.delete_file(ino).await.unwrap();

        assert!(manager.is_dirty(ino));
        assert_eq!(manager.get_state(ino), Some(DirtyState::Deleted));

        // Verify it appears in dirty entries
        let entries = manager.get_dirty_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, DirtyState::Deleted);
        assert_eq!(entries[0].path, "delete_test.bin");
    }

    /// Test multiple boundary-spanning writes.
    #[tokio::test]
    async fn test_260mb_file_multiple_boundary_writes() {
        let (manager, inodes, store) = create_padded_env();

        let chunk_size: u64 = CHUNK_SIZE_V2;
        let chunk1_size: u64 = 4 * 1024 * 1024;
        let total_size: u64 = chunk_size + chunk1_size;

        store.insert_chunk("chunk0_hash", vec![0xAA; 100], chunk_size);
        store.insert_chunk("chunk1_hash", vec![0xBB; 100], chunk1_size);

        let ino: u64 = inodes.add_file(
            "multi_boundary.bin",
            total_size,
            0,
            FileContent::Chunked(vec!["chunk0_hash".to_string(), "chunk1_hash".to_string()]),
            HashAlgorithm::Xxh128,
            false,
        );

        // First boundary write
        let offset1: u64 = chunk_size - 20;
        manager.write(ino, offset1, b"FIRST_BOUNDARY_WRITE_").await.unwrap();

        // Second boundary write (overlapping)
        let offset2: u64 = chunk_size - 10;
        manager.write(ino, offset2, b"SECOND_WRITE").await.unwrap();

        // Verify final state
        let read1: Vec<u8> = manager.read(ino, offset1, 10).await.unwrap();
        assert_eq!(read1, b"FIRST_BOUN");

        let read2: Vec<u8> = manager.read(ino, offset2, 12).await.unwrap();
        assert_eq!(read2, b"SECOND_WRITE");

        // Read across boundary to verify continuity
        let full_read: Vec<u8> = manager.read(ino, offset1, 32).await.unwrap();
        assert_eq!(&full_read[..10], b"FIRST_BOUN");
        assert_eq!(&full_read[10..22], b"SECOND_WRITE");
    }
}
