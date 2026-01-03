//! Integration tests for ProjFS VFS.

use std::sync::Arc;

use rusty_attachments_model::v2023_03_03::{AssetManifest, PathEntry};
use rusty_attachments_model::{HashAlgorithm, Manifest};
use rusty_attachments_storage::{S3Location, StorageSettings};
use rusty_attachments_storage_crt::CrtStorageClient;
use rusty_attachments_vfs::StorageClientAdapter;
use rusty_attachments_vfs_projfs::{ProjFsOptions, ProjFsWriteOptions, WritableProjFs};
use tempfile::TempDir;

/// Create a test manifest with sample files.
fn create_test_manifest() -> Manifest {
    let manifest = AssetManifest {
        hash_alg: HashAlgorithm::Xxh128,
        total_size: 600,
        paths: vec![
            PathEntry {
                path: "file1.txt".to_string(),
                hash: "hash1".to_string(),
                size: 100,
                mtime: 1000,
            },
            PathEntry {
                path: "dir1/file2.txt".to_string(),
                hash: "hash2".to_string(),
                size: 200,
                mtime: 2000,
            },
            PathEntry {
                path: "dir1/subdir/file3.txt".to_string(),
                hash: "hash3".to_string(),
                size: 300,
                mtime: 3000,
            },
        ],
    };
    Manifest::V2023_03_03(manifest)
}

/// Create a mock storage client for testing.
fn create_mock_storage() -> Arc<dyn rusty_attachments_vfs::FileStore> {
    let storage_settings = StorageSettings {
        s3_location: S3Location {
            bucket_name: "test-bucket".to_string(),
            key_prefix: "cas/".to_string(),
        },
        ..Default::default()
    };

    let crt_client = CrtStorageClient::new(storage_settings).unwrap();
    Arc::new(StorageClientAdapter::new(Arc::new(crt_client)))
}

#[test]
fn test_create_virtualizer() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options);
    assert!(vfs.is_ok());

    let vfs = vfs.unwrap();
    assert!(!vfs.is_started());
}

#[test]
fn test_virtualizer_lifecycle() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();

    assert!(!vfs.is_started());

    // Note: start() will fail on non-Windows or without ProjFS enabled
    // This test verifies the API structure works correctly
    let start_result = vfs.start();

    #[cfg(not(target_os = "windows"))]
    {
        // On non-Windows, start should fail gracefully
        assert!(start_result.is_err());
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows, may succeed or fail depending on ProjFS availability
        if start_result.is_ok() {
            assert!(vfs.is_started());
            let stop_result = vfs.stop();
            assert!(stop_result.is_ok());
            assert!(!vfs.is_started());
        }
    }
}

#[test]
fn test_projection_enumeration() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();

    // Test enumeration through callbacks
    let callbacks = vfs.callbacks();

    // Root directory should have file1.txt and dir1
    let root_items = callbacks.get_projected_items("");
    assert!(root_items.is_some());
    let root_items = root_items.unwrap();
    assert_eq!(root_items.len(), 2);

    // dir1 should have file2.txt and subdir
    let dir1_items = callbacks.get_projected_items("dir1");
    assert!(dir1_items.is_some());
    let dir1_items = dir1_items.unwrap();
    assert_eq!(dir1_items.len(), 2);

    // dir1/subdir should have file3.txt
    let subdir_items = callbacks.get_projected_items("dir1/subdir");
    assert!(subdir_items.is_some());
    let subdir_items = subdir_items.unwrap();
    assert_eq!(subdir_items.len(), 1);
    assert_eq!(subdir_items[0].name.as_ref(), "file3.txt");
}

#[test]
fn test_path_lookup() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();
    let callbacks = vfs.callbacks();

    // Test file lookup
    let (name, is_folder) = callbacks.is_path_projected("file1.txt").unwrap();
    assert_eq!(name, "file1.txt");
    assert!(!is_folder);

    // Test directory lookup
    let (name, is_folder) = callbacks.is_path_projected("dir1").unwrap();
    assert_eq!(name, "dir1");
    assert!(is_folder);

    // Test nested file lookup
    let (name, is_folder) = callbacks.is_path_projected("dir1/file2.txt").unwrap();
    assert_eq!(name, "file2.txt");
    assert!(!is_folder);

    // Test non-existent path
    assert!(callbacks.is_path_projected("nonexistent.txt").is_none());
}

#[test]
fn test_file_info_retrieval() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();
    let callbacks = vfs.callbacks();

    // Get file info
    let info = callbacks.get_file_info("file1.txt");
    assert!(info.is_some());
    let info = info.unwrap();
    assert_eq!(info.name.as_ref(), "file1.txt");
    assert_eq!(info.size, 100);
    assert!(!info.is_folder);
    assert!(info.content_hash.is_some());

    // Get nested file info
    let info = callbacks.get_file_info("dir1/subdir/file3.txt");
    assert!(info.is_some());
    let info = info.unwrap();
    assert_eq!(info.name.as_ref(), "file3.txt");
    assert_eq!(info.size, 300);

    // Non-existent file
    assert!(callbacks.get_file_info("nonexistent.txt").is_none());

    // Directory (not a file)
    assert!(callbacks.get_file_info("dir1").is_none());
}

#[test]
fn test_case_insensitive_paths() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();
    let callbacks = vfs.callbacks();

    // Windows is case-insensitive
    assert!(callbacks.is_path_projected("FILE1.TXT").is_some());
    assert!(callbacks.is_path_projected("Dir1").is_some());
    assert!(callbacks.is_path_projected("DIR1/FILE2.TXT").is_some());
}

#[test]
fn test_options_builder() {
    let temp_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf())
        .with_worker_threads(8)
        .with_notifications(rusty_attachments_vfs_projfs::NotificationMask::for_readonly());

    assert_eq!(options.worker_threads, 8);
    assert!(!options.notifications.new_file_created);
}

#[test]
fn test_write_options_builder() {
    let cache_dir = TempDir::new().unwrap();

    let write_options = ProjFsWriteOptions::default()
        .with_cache_dir(cache_dir.path().to_path_buf())
        .with_disk_cache(false);

    assert_eq!(write_options.cache_dir, cache_dir.path());
    assert!(!write_options.use_disk_cache);
}

#[test]
fn test_multiple_virtualizers() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();

    let temp_dir1 = TempDir::new().unwrap();
    let temp_dir2 = TempDir::new().unwrap();
    let cache_dir1 = TempDir::new().unwrap();
    let cache_dir2 = TempDir::new().unwrap();

    let options1 = ProjFsOptions::new(temp_dir1.path().to_path_buf());
    let write_options1 =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir1.path().to_path_buf());

    let options2 = ProjFsOptions::new(temp_dir2.path().to_path_buf());
    let write_options2 =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir2.path().to_path_buf());

    let vfs1 = WritableProjFs::new(&manifest, storage.clone(), options1, write_options1);
    let vfs2 = WritableProjFs::new(&manifest, storage, options2, write_options2);

    assert!(vfs1.is_ok());
    assert!(vfs2.is_ok());
}

#[test]
fn test_empty_manifest() {
    let manifest = AssetManifest {
        hash_alg: HashAlgorithm::Xxh128,
        total_size: 0,
        paths: vec![],
    };
    let manifest = Manifest::V2023_03_03(manifest);

    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options);
    assert!(vfs.is_ok());

    let vfs = vfs.unwrap();
    let callbacks = vfs.callbacks();

    // Root should be empty
    let root_items = callbacks.get_projected_items("");
    assert!(root_items.is_some());
    assert_eq!(root_items.unwrap().len(), 0);
}

#[test]
fn test_deeply_nested_paths() {
    let manifest = AssetManifest {
        hash_alg: HashAlgorithm::Xxh128,
        total_size: 100,
        paths: vec![PathEntry {
            path: "a/b/c/d/e/f/g/file.txt".to_string(),
            hash: "hash".to_string(),
            size: 100,
            mtime: 1000,
        }],
    };
    let manifest = Manifest::V2023_03_03(manifest);

    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();
    let callbacks = vfs.callbacks();

    // Verify each level exists
    assert!(callbacks.is_path_projected("a").is_some());
    assert!(callbacks.is_path_projected("a/b").is_some());
    assert!(callbacks.is_path_projected("a/b/c").is_some());
    assert!(callbacks.is_path_projected("a/b/c/d").is_some());
    assert!(callbacks.is_path_projected("a/b/c/d/e").is_some());
    assert!(callbacks.is_path_projected("a/b/c/d/e/f").is_some());
    assert!(callbacks.is_path_projected("a/b/c/d/e/f/g").is_some());
    assert!(callbacks
        .is_path_projected("a/b/c/d/e/f/g/file.txt")
        .is_some());
}
