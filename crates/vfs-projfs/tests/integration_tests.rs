//! Integration tests for ProjFS VFS.

use std::sync::Arc;

use async_trait::async_trait;
use rusty_attachments_model::v2023_03_03::{AssetManifest, ManifestPath};
use rusty_attachments_model::{HashAlgorithm, Manifest};
use rusty_attachments_vfs::FileStore;
use rusty_attachments_vfs_projfs::{ProjFsOptions, ProjFsWriteOptions, WritableProjFs};
use tempfile::TempDir;

/// Mock file store that returns empty data for testing.
struct MockFileStore;

#[async_trait]
impl FileStore for MockFileStore {
    async fn retrieve(
        &self,
        _hash: &str,
        _algorithm: HashAlgorithm,
    ) -> std::result::Result<Vec<u8>, rusty_attachments_vfs::VfsError> {
        Ok(vec![0u8; 100])
    }

    async fn retrieve_range(
        &self,
        _hash: &str,
        _algorithm: HashAlgorithm,
        _offset: u64,
        _length: u64,
    ) -> std::result::Result<Vec<u8>, rusty_attachments_vfs::VfsError> {
        Ok(vec![0u8; 100])
    }
}

/// Create a test manifest with sample files.
fn create_test_manifest() -> Manifest {
    let manifest = AssetManifest {
        hash_alg: HashAlgorithm::Xxh128,
        manifest_version: rusty_attachments_model::ManifestVersion::V2023_03_03,
        total_size: 600,
        paths: vec![
            ManifestPath {
                path: "file1.txt".to_string(),
                hash: "hash1".to_string(),
                size: 100,
                mtime: 1000,
            },
            ManifestPath {
                path: "dir1/file2.txt".to_string(),
                hash: "hash2".to_string(),
                size: 200,
                mtime: 2000,
            },
            ManifestPath {
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
fn create_mock_storage() -> Arc<dyn FileStore> {
    Arc::new(MockFileStore)
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

    let result = WritableProjFs::new(&manifest, storage, options, write_options);
    assert!(result.is_ok());
}

#[test]
fn test_virtualizer_not_started() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();
    assert!(!vfs.is_started());
}

#[test]
fn test_virtualizer_root_path() {
    let manifest = create_test_manifest();
    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();
    assert_eq!(vfs.root_path(), temp_dir.path());
}

#[test]
fn test_extract_directories_from_manifest() {
    let manifest = AssetManifest {
        hash_alg: HashAlgorithm::Xxh128,
        manifest_version: rusty_attachments_model::ManifestVersion::V2023_03_03,
        total_size: 300,
        paths: vec![
            ManifestPath {
                path: "file.txt".to_string(),
                hash: "hash1".to_string(),
                size: 100,
                mtime: 1000,
            },
            ManifestPath {
                path: "dir1/file.txt".to_string(),
                hash: "hash2".to_string(),
                size: 100,
                mtime: 1000,
            },
            ManifestPath {
                path: "dir1/dir2/file.txt".to_string(),
                hash: "hash3".to_string(),
                size: 100,
                mtime: 1000,
            },
        ],
    };
    let manifest = Manifest::V2023_03_03(manifest);

    let storage = create_mock_storage();
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
    let write_options =
        ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

    // Just verify it creates successfully - directory extraction is internal
    let result = WritableProjFs::new(&manifest, storage, options, write_options);
    assert!(result.is_ok());
}

#[test]
fn test_deeply_nested_paths() {
    let manifest = AssetManifest {
        hash_alg: HashAlgorithm::Xxh128,
        manifest_version: rusty_attachments_model::ManifestVersion::V2023_03_03,
        total_size: 100,
        paths: vec![ManifestPath {
            path: "a/b/c/d/e/file.txt".to_string(),
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

    let result = WritableProjFs::new(&manifest, storage, options, write_options);
    assert!(result.is_ok());
}
