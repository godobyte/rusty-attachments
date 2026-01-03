//! VFS callbacks coordination layer.

use std::sync::Arc;

use rusty_attachments_model::HashAlgorithm;
use rusty_attachments_vfs::write::DirtyDirManager;
use rusty_attachments_vfs::{DirtyFileManager, FileStore, MemoryPool};

use crate::callbacks::background::{BackgroundTask, BackgroundTaskRunner};
use crate::projection::types::ProjectedFileInfo;
use crate::projection::ManifestProjection;

/// Coordination layer between virtualizer and projection.
///
/// Tracks modified paths and coordinates background operations.
pub struct VfsCallbacks {
    /// Manifest projection (backing store).
    projection: Arc<ManifestProjection>,
    /// Background task queue.
    background_tasks: BackgroundTaskRunner,
    /// Storage client for S3 CAS access.
    storage: Arc<dyn FileStore>,
    /// Memory pool for caching fetched content.
    memory_pool: Arc<MemoryPool>,
    /// Dirty file manager (COW layer).
    #[allow(dead_code)]
    dirty_files: Arc<DirtyFileManager>,
    /// Dirty directory manager.
    #[allow(dead_code)]
    dirty_dirs: Arc<DirtyDirManager>,
}

impl VfsCallbacks {
    /// Create new VFS callbacks.
    ///
    /// # Arguments
    /// * `projection` - Manifest projection
    /// * `storage` - Storage client for S3 CAS
    /// * `memory_pool` - Memory pool for content caching
    /// * `dirty_files` - Dirty file manager
    /// * `dirty_dirs` - Dirty directory manager
    pub fn new(
        projection: Arc<ManifestProjection>,
        storage: Arc<dyn FileStore>,
        memory_pool: Arc<MemoryPool>,
        dirty_files: Arc<DirtyFileManager>,
        dirty_dirs: Arc<DirtyDirManager>,
    ) -> Self {
        let background_tasks = BackgroundTaskRunner::new(|task| {
            // Handle background tasks (logging, metrics, etc.)
            tracing::debug!("Background task: {:?}", task);
        });

        Self {
            projection,
            background_tasks,
            storage,
            memory_pool,
            dirty_files,
            dirty_dirs,
        }
    }

    /// Get projected items for enumeration.
    ///
    /// Fast path - all data is in memory.
    ///
    /// # Arguments
    /// * `relative_path` - Directory path
    ///
    /// # Returns
    /// Arc-wrapped slice of items to enumerate (shared, no cloning).
    pub fn get_projected_items(&self, relative_path: &str) -> Option<Arc<[ProjectedFileInfo]>> {
        // Get base items from projection
        let items: Arc<[ProjectedFileInfo]> = self.projection.get_projected_items(relative_path)?;

        // TODO: Apply dirty state (new files, deleted files, modified sizes)
        // For now, return projection items directly
        Some(items)
    }

    /// Check if path is projected.
    ///
    /// # Arguments
    /// * `relative_path` - Path to check
    ///
    /// # Returns
    /// (canonical_name, is_folder) if exists.
    pub fn is_path_projected(&self, relative_path: &str) -> Option<(String, bool)> {
        // Check manifest projection first
        if let Some(result) = self.projection.is_path_projected(relative_path) {
            // TODO: Check if deleted in dirty managers
            return Some(result);
        }

        // TODO: Check new files/dirs from dirty managers
        None
    }

    /// Get file info for placeholder creation.
    ///
    /// # Arguments
    /// * `relative_path` - Path to file
    ///
    /// # Returns
    /// File info if found.
    pub fn get_file_info(&self, relative_path: &str) -> Option<ProjectedFileInfo> {
        self.projection.get_file_info(relative_path)
    }

    /// Fetch file content from S3 CAS.
    ///
    /// # Arguments
    /// * `content_hash` - Hash of content to fetch
    /// * `_offset` - Byte offset (reserved for future use)
    /// * `_length` - Bytes to read (reserved for future use)
    ///
    /// # Returns
    /// File content bytes.
    pub async fn fetch_file_content(
        &self,
        content_hash: &str,
        _offset: u64,
        _length: u32,
    ) -> Result<Vec<u8>, rusty_attachments_vfs::VfsError> {
        // TODO: Check dirty manager first (COW files)
        // TODO: Check memory pool
        // Finally fetch from S3
        let hash_alg: HashAlgorithm = self.projection.hash_algorithm();
        self.storage.retrieve(content_hash, hash_alg).await
    }

    /// Get reference to projection.
    pub fn projection(&self) -> &Arc<ManifestProjection> {
        &self.projection
    }

    /// Get reference to memory pool.
    pub fn memory_pool(&self) -> &Arc<MemoryPool> {
        &self.memory_pool
    }

    // --- Notification handlers (queued to background) ---

    /// Called when a new file is created.
    ///
    /// # Arguments
    /// * `relative_path` - Path of created file
    pub fn on_file_created(&self, relative_path: &str) {
        self.background_tasks
            .enqueue(BackgroundTask::FileCreated(relative_path.to_string()));
    }

    /// Called when a file is modified.
    ///
    /// # Arguments
    /// * `relative_path` - Path of modified file
    pub fn on_file_modified(&self, relative_path: &str) {
        self.background_tasks
            .enqueue(BackgroundTask::FileModified(relative_path.to_string()));
    }

    /// Called when a file is deleted.
    ///
    /// # Arguments
    /// * `relative_path` - Path of deleted file
    pub fn on_file_deleted(&self, relative_path: &str) {
        self.background_tasks
            .enqueue(BackgroundTask::FileDeleted(relative_path.to_string()));
    }

    /// Called when a file is renamed.
    ///
    /// # Arguments
    /// * `old_path` - Original path
    /// * `new_path` - New path
    pub fn on_file_renamed(&self, old_path: &str, new_path: &str) {
        self.background_tasks.enqueue(BackgroundTask::FileRenamed {
            old_path: old_path.to_string(),
            new_path: new_path.to_string(),
        });
    }

    /// Called when a file is hydrated.
    ///
    /// # Arguments
    /// * `relative_path` - Path of hydrated file
    pub fn on_file_hydrated(&self, relative_path: &str) {
        tracing::debug!("File hydrated: {}", relative_path);
    }
}
