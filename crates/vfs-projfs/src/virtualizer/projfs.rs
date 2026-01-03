//! ProjFS virtualizer implementation for Windows.
//!
//! This module provides the main `WritableProjFs` struct that manages
//! the lifecycle of a ProjFS virtualization instance.

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use rusty_attachments_model::Manifest;
use rusty_attachments_vfs::write::DirtyDirManager;
use rusty_attachments_vfs::{AsyncExecutor, DirtyFileManager, FileStore, INodeManager, MemoryPool};
use windows::core::PCWSTR;
use windows::Win32::Storage::ProjectedFileSystem::{
    PrjMarkDirectoryAsPlaceholder, PrjStartVirtualizing, PrjStopVirtualizing,
    PRJ_CALLBACKS, PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PRJ_NOTIFICATION_MAPPING,
    PRJ_STARTVIRTUALIZING_OPTIONS,
    PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED, PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED,
    PRJ_NOTIFY_FILE_RENAMED, PRJ_NOTIFY_NEW_FILE_CREATED,
    PRJ_NOTIFY_PRE_DELETE, PRJ_NOTIFY_PRE_RENAME,
};

use crate::callbacks::VfsCallbacks;
use crate::error::ProjFsError;
use crate::options::{NotificationMask, ProjFsOptions, ProjFsWriteOptions};
use crate::projection::ManifestProjection;
use crate::util::wstr::string_to_wide;
use crate::virtualizer::callbacks::{build_callbacks, CallbackContext};

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
    /// ProjFS namespace virtualization context (set after start).
    namespace_context: RwLock<Option<PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT>>,
    /// Callback context (must outlive virtualization).
    /// Stored as raw pointer because ProjFS holds a reference to it.
    callback_context_ptr: RwLock<Option<*mut CallbackContext>>,
}

// Safety: WritableProjFs can be sent between threads.
// The callback_context_ptr is only accessed while holding the lock.
unsafe impl Send for WritableProjFs {}
unsafe impl Sync for WritableProjFs {}

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
    /// New virtualizer instance or error.
    pub fn new(
        manifest: &Manifest,
        storage: Arc<dyn FileStore>,
        options: ProjFsOptions,
        write_options: ProjFsWriteOptions,
    ) -> Result<Self, ProjFsError> {
        let projection: Arc<ManifestProjection> =
            Arc::new(ManifestProjection::from_manifest(manifest)?);
        let memory_pool: Arc<MemoryPool> = Arc::new(MemoryPool::new(options.memory_pool.clone()));
        let inodes: Arc<INodeManager> = Arc::new(INodeManager::new());

        let write_cache: Arc<dyn rusty_attachments_vfs::WriteCache> = if write_options.use_disk_cache
        {
            let cache = rusty_attachments_vfs::MaterializedCache::new(write_options.cache_dir.clone())
                .map_err(ProjFsError::Io)?;
            Arc::new(cache)
        } else {
            Arc::new(rusty_attachments_vfs::MemoryWriteCache::new())
        };

        let dirty_files: Arc<DirtyFileManager> = Arc::new(DirtyFileManager::new(
            write_cache,
            storage.clone(),
            inodes.clone(),
            memory_pool.clone(),
        ));

        // Extract original directories from manifest for dirty dir tracking
        let original_dirs: HashSet<String> = extract_directories_from_manifest(manifest);
        let dirty_dirs: Arc<DirtyDirManager> =
            Arc::new(DirtyDirManager::new(inodes.clone(), original_dirs));

        let callbacks: Arc<VfsCallbacks> = Arc::new(VfsCallbacks::new(
            projection,
            storage,
            memory_pool,
            dirty_files,
            dirty_dirs,
        ));

        let executor: Arc<AsyncExecutor> = Arc::new(AsyncExecutor::new(options.executor_config()));

        Ok(Self {
            callbacks,
            executor,
            options,
            started: RwLock::new(false),
            namespace_context: RwLock::new(None),
            callback_context_ptr: RwLock::new(None),
        })
    }

    /// Start virtualization.
    ///
    /// Marks the root directory as a placeholder and starts the ProjFS
    /// virtualization instance with configured callbacks.
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

        // Step 1: Mark root directory as placeholder
        mark_directory_as_placeholder(&self.options.root_path, &self.options.instance_guid)?;

        // Step 2: Build callbacks structure
        let callbacks: PRJ_CALLBACKS = build_callbacks();

        // Step 3: Create callback context (must outlive virtualization)
        let ctx = Box::new(CallbackContext::new(
            self.callbacks.clone(),
            self.executor.clone(),
        ));
        let ctx_ptr: *mut CallbackContext = Box::into_raw(ctx);

        // Step 4: Build notification mappings
        let mut notification_mappings: Vec<PRJ_NOTIFICATION_MAPPING> =
            build_notification_mappings(&self.options.notifications);

        // Step 5: Start virtualization
        let namespace_context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT =
            start_virtualizing(
                &self.options.root_path,
                &callbacks,
                ctx_ptr as *const c_void,
                &self.options,
                &mut notification_mappings,
            )?;

        // Store context for stop()
        *self.namespace_context.write() = Some(namespace_context);
        *self.callback_context_ptr.write() = Some(ctx_ptr);
        *started = true;

        tracing::info!(
            "ProjFS virtualization started at {:?}",
            self.options.root_path
        );

        Ok(())
    }

    /// Stop virtualization.
    ///
    /// Stops the ProjFS virtualization instance and cleans up resources.
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

        // Stop ProjFS virtualization
        if let Some(ctx) = self.namespace_context.write().take() {
            unsafe {
                PrjStopVirtualizing(ctx);
            }
            tracing::info!("ProjFS virtualization stopped");
        }

        // Clean up callback context
        if let Some(ctx_ptr) = self.callback_context_ptr.write().take() {
            // Safety: We created this with Box::into_raw, so we can reclaim it
            unsafe {
                let _ = Box::from_raw(ctx_ptr);
            }
        }

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

// ============================================================================
// Helper Functions (Primitives)
// ============================================================================

/// Extract directory paths from manifest.
///
/// # Arguments
/// * `manifest` - Manifest to extract directories from
///
/// # Returns
/// Set of directory paths.
fn extract_directories_from_manifest(manifest: &Manifest) -> HashSet<String> {
    let mut dirs: HashSet<String> = HashSet::new();

    // Get file paths based on manifest version
    let file_paths: Vec<&str> = match manifest {
        Manifest::V2023_03_03(m) => m.paths.iter().map(|p| p.path.as_str()).collect(),
        // V2 uses 'name' field instead of 'path'
        Manifest::V2025_12_04_beta(m) => m.files.iter().map(|f| f.name.as_str()).collect(),
    };

    for path in file_paths {
        // Extract parent directories
        let mut current: &str = path;
        while let Some(idx) = current.rfind('/') {
            current = &current[..idx];
            if !current.is_empty() {
                dirs.insert(current.to_string());
            }
        }
    }

    dirs
}

/// Mark a directory as a ProjFS placeholder.
///
/// # Arguments
/// * `root_path` - Path to the directory
/// * `instance_guid` - Unique GUID for this virtualization instance
///
/// # Returns
/// Ok on success, ProjFsError on failure.
fn mark_directory_as_placeholder(
    root_path: &PathBuf,
    instance_guid: &windows::core::GUID,
) -> Result<(), ProjFsError> {
    let root_path_str: String = root_path
        .to_str()
        .ok_or_else(|| ProjFsError::InvalidRootPath(format!("{:?}", root_path)))?
        .to_string();

    let root_path_wide: Vec<u16> = string_to_wide(&root_path_str);

    unsafe {
        PrjMarkDirectoryAsPlaceholder(
            PCWSTR::from_raw(root_path_wide.as_ptr()),
            PCWSTR::null(), // target_path_name (None for root)
            None,           // version_info
            instance_guid,
        )
        .map_err(|e| ProjFsError::ProjFsApi {
            operation: "PrjMarkDirectoryAsPlaceholder".to_string(),
            hresult: e.code().0,
        })?;
    }

    Ok(())
}

/// Build notification mappings from NotificationMask.
///
/// # Arguments
/// * `mask` - Notification mask configuration
///
/// # Returns
/// Vector of PRJ_NOTIFICATION_MAPPING structures.
fn build_notification_mappings(mask: &NotificationMask) -> Vec<PRJ_NOTIFICATION_MAPPING> {
    let mut notification_bits: u32 = 0;

    if mask.new_file_created {
        notification_bits |= PRJ_NOTIFY_NEW_FILE_CREATED.0;
    }
    if mask.file_modified {
        notification_bits |= PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED.0;
    }
    if mask.file_deleted {
        notification_bits |= PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED.0;
    }
    if mask.file_renamed {
        notification_bits |= PRJ_NOTIFY_FILE_RENAMED.0;
    }
    if mask.pre_delete {
        notification_bits |= PRJ_NOTIFY_PRE_DELETE.0;
    }
    if mask.pre_rename {
        notification_bits |= PRJ_NOTIFY_PRE_RENAME.0;
    }

    if notification_bits == 0 {
        return vec![];
    }

    vec![PRJ_NOTIFICATION_MAPPING {
        NotificationBitMask: windows::Win32::Storage::ProjectedFileSystem::PRJ_NOTIFY_TYPES(
            notification_bits,
        ),
        NotificationRoot: PCWSTR::null(), // Root = all paths
    }]
}

/// Start ProjFS virtualization.
///
/// # Arguments
/// * `root_path` - Virtualization root directory
/// * `callbacks` - ProjFS callbacks structure
/// * `instance_context` - Context pointer passed to callbacks
/// * `options` - ProjFS options
/// * `notification_mappings` - Notification mappings (mutable for ProjFS API)
///
/// # Returns
/// Namespace virtualization context on success.
fn start_virtualizing(
    root_path: &PathBuf,
    callbacks: &PRJ_CALLBACKS,
    instance_context: *const c_void,
    options: &ProjFsOptions,
    notification_mappings: &mut [PRJ_NOTIFICATION_MAPPING],
) -> Result<PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, ProjFsError> {
    let root_path_str: String = root_path
        .to_str()
        .ok_or_else(|| ProjFsError::InvalidRootPath(format!("{:?}", root_path)))?
        .to_string();

    let root_path_wide: Vec<u16> = string_to_wide(&root_path_str);

    let start_options = PRJ_STARTVIRTUALIZING_OPTIONS {
        Flags: windows::Win32::Storage::ProjectedFileSystem::PRJ_STARTVIRTUALIZING_FLAGS(0),
        PoolThreadCount: options.pool_thread_count,
        ConcurrentThreadCount: options.concurrent_thread_count,
        NotificationMappings: if notification_mappings.is_empty() {
            std::ptr::null_mut()
        } else {
            notification_mappings.as_mut_ptr()
        },
        NotificationMappingsCount: notification_mappings.len() as u32,
    };

    unsafe {
        PrjStartVirtualizing(
            PCWSTR::from_raw(root_path_wide.as_ptr()),
            callbacks,
            Some(instance_context),
            Some(&start_options),
        )
        .map_err(|e| ProjFsError::ProjFsApi {
            operation: "PrjStartVirtualizing".to_string(),
            hresult: e.code().0,
        })
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
    fn test_writable_projfs_not_started() {
        let manifest = create_test_manifest();
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();

        let storage: Arc<dyn FileStore> = Arc::new(MockFileStore);

        let options = ProjFsOptions::new(temp_dir.path().to_path_buf());
        let write_options =
            ProjFsWriteOptions::default().with_cache_dir(cache_dir.path().to_path_buf());

        let vfs = WritableProjFs::new(&manifest, storage, options, write_options).unwrap();
        assert!(!vfs.is_started());
    }

    #[test]
    fn test_extract_directories_from_manifest() {
        let manifest = AssetManifest {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2023_03_03,
            total_size: 200,
            paths: vec![
                ManifestPath {
                    path: "dir1/dir2/file.txt".to_string(),
                    hash: "hash1".to_string(),
                    size: 100,
                    mtime: 1000,
                },
                ManifestPath {
                    path: "dir1/other.txt".to_string(),
                    hash: "hash2".to_string(),
                    size: 100,
                    mtime: 1000,
                },
            ],
        };
        let manifest = Manifest::V2023_03_03(manifest);

        let dirs: HashSet<String> = extract_directories_from_manifest(&manifest);

        assert!(dirs.contains("dir1"));
        assert!(dirs.contains("dir1/dir2"));
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn test_build_notification_mappings_writable() {
        let mask = NotificationMask::for_writable();
        let mappings: Vec<PRJ_NOTIFICATION_MAPPING> = build_notification_mappings(&mask);

        assert_eq!(mappings.len(), 1);
        // Should have new_file_created, file_modified, file_deleted, file_renamed
        let bits: u32 = mappings[0].NotificationBitMask.0;
        assert!(bits & PRJ_NOTIFY_NEW_FILE_CREATED.0 != 0);
        assert!(bits & PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED.0 != 0);
        assert!(bits & PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED.0 != 0);
        assert!(bits & PRJ_NOTIFY_FILE_RENAMED.0 != 0);
    }

    #[test]
    fn test_build_notification_mappings_readonly() {
        let mask = NotificationMask::for_readonly();
        let mappings: Vec<PRJ_NOTIFICATION_MAPPING> = build_notification_mappings(&mask);

        assert!(mappings.is_empty());
    }
}
