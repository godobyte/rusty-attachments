//! ProjFS virtualizer implementation for Windows.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use rusty_attachments_model::Manifest;
use rusty_attachments_vfs::write::DirtyDirManager;
use rusty_attachments_vfs::{AsyncExecutor, DirtyFileManager, FileStore, INodeManager, MemoryPool};

use crate::callbacks::VfsCallbacks;
use crate::error::ProjFsError;
use crate::options::{ProjFsOptions, ProjFsWriteOptions};
use crate::projection::ManifestProjection;

/// Writable ProjFS virtualizer.
///
/// Provides a Windows-native virtual filesystem using ProjFS with
/// copy-on-write support for modifications.
pub struct WritableProjFs {
    /// VFS callbacks for coordination.
    callbacks: Arc<VfsCallbacks>,
    /// Async executor for bridging sync callbacks to async I/O.
    executor: Arc<AsyncExecutor>,
    /// Configuration options.
    options: ProjFsOptions,
    /// Whether virtualization is started.
    started: RwLock<bool>,
}

impl WritableProjFs {
    /// Create new writable ProjFS virtualizer.
    ///
    /// # Arguments
    /// * `manifest` - Manifest to project
    /// * `storage` - Storage client for S3 CAS
    /// * `options` - ProjFS configuration options
    /// * `write_options` - Write cache options
    ///
    /// # Returns
    /// New virtualizer instance.
    pub fn new(
        manifest: &Manifest,
        storage: Arc<dyn FileStore>,
        options: ProjFsOptions,
        write_options: ProjFsWriteOptions,
    ) -> Result<Self, ProjFsError> {
        // Build projection from manifest
        let projection = Arc::new(ManifestProjection::from_manifest(manifest)?);

        // Create memory pool
        let memory_pool = Arc::new(MemoryPool::new(options.memory_pool.clone()));

        // Create inode manager (for dirty file tracking)
        let inodes = Arc::new(INodeManager::new());

        // Create write cache
        let write_cache: Arc<dyn rusty_attachments_vfs::WriteCache> = if write_options.use_disk_cache {
            let cache = rusty_attachments_vfs::MaterializedCache::new(
                write_options.cache_dir.clone(),
            ).map_err(ProjFsError::Io)?;
            Arc::new(cache)
        } else {
            Arc::new(rusty_attachments_vfs::MemoryWriteCache::new())
        };

        // Create dirty file manager
        let dirty_files = Arc::new(DirtyFileManager::new(
            write_cache,
            storage.clone(),
            inodes.clone(),
            memory_pool.clone(),
        ));

        // Create dirty directory manager
        let original_dirs: HashSet<String> = HashSet::new(); // TODO: Extract from manifest
        let dirty_dirs = Arc::new(DirtyDirManager::new(inodes.clone(), original_dirs));

        // Create callbacks layer
        let callbacks = Arc::new(VfsCallbacks::new(
            projection,
            storage,
            memory_pool,
            dirty_files,
            dirty_dirs,
        ));

        // Create async executor
        let executor = Arc::new(AsyncExecutor::new(options.executor_config()));

        Ok(Self {
            callbacks,
            executor,
            options,
            started: RwLock::new(false),
        })
    }

    /// Start virtualization.
    ///
    /// # Returns
    /// Ok on success, error if already started or ProjFS fails.
    pub fn start(&self) -> Result<(), ProjFsError> {
        let mut started = self.started.write();
        if *started {
            return Err(ProjFsError::AlreadyStarted);
        }

        // Ensure directory exists
        std::fs::create_dir_all(&self.options.root_path)?;

        // TODO: Call ProjFS APIs to start virtualization
        // This requires Windows-specific implementation

        *started = true;
        Ok(())
    }

    /// Stop virtualization.
    ///
    /// # Returns
    /// Ok on success.
    pub fn stop(&self) -> Result<(), ProjFsError> {
        let mut started = self.started.write();
        if !*started {
            return Err(ProjFsError::NotStarted);
        }

        // Cancel any in-flight async operations
        self.executor.cancel_all();

        // TODO: Call ProjFS APIs to stop virtualization

        *started = false;
        Ok(())
    }

    /// Check if virtualization is started.
    pub fn is_started(&self) -> bool {
        *self.started.read()
    }

    /// Get reference to callbacks.
    pub fn callbacks(&self) -> &Arc<VfsCallbacks> {
        &self.callbacks
    }

    /// Get reference to executor.
    pub fn executor(&self) -> &Arc<AsyncExecutor> {
        &self.executor
    }

    /// Get virtualization root path.
    pub fn root_path(&self) -> &PathBuf {
        &self.options.root_path
    }
}

impl Drop for WritableProjFs {
    fn drop(&mut self) {
        if *self.started.read() {
            let _ = self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rusty_attachments_model::v2023_03_03::{AssetManifest, ManifestPath};
    use rusty_attachments_model::{HashAlgorithm, ManifestVersion};
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

    fn create_test_manifest() -> Manifest {
        let manifest = AssetManifest {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2023_03_03,
            total_size: 100,
            paths: vec![ManifestPath {
                path: "test.txt".to_string(),
                hash: "testhash".to_string(),
                size: 100,
                mtime: 1000,
            }],
        };
        Manifest::V2023_03_03(manifest)
    }

    #[test]
    fn test_writable_projfs_new() {
        let manifest = create_test_manifest();
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();

        let storage: Arc<dyn FileStore> = Arc::new(MockFileStore);

        let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
        let write_options =
            ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

        let vfs = WritableProjFs::new(&manifest, storage, options, write_options);
        assert!(vfs.is_ok());
    }

    #[test]
    fn test_writable_projfs_lifecycle() {
        let manifest = create_test_manifest();
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();

        let storage: Arc<dyn FileStore> = Arc::new(MockFileStore);

        let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
        let write_options = ProjFsWriteOptions::default()
            .with_cache_dir(cache_dir.path().to_path_buf());

        let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();
        assert!(!vfs.is_started());
    }


}