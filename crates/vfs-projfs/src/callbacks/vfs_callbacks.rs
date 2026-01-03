//! VFS callbacks coordination layer.
//!
//! Coordinates between the ProjFS virtualizer and the manifest projection,
//! handling dirty state tracking and content fetching.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::SystemTime;

use rusty_attachments_model::HashAlgorithm;
use rusty_attachments_vfs::memory_pool_v2::BlockKey;
use rusty_attachments_vfs::write::DirtyDirManager;
use rusty_attachments_vfs::{DirtyFileManager, FileStore, MemoryPool, VfsError};

use crate::callbacks::background::{BackgroundTask, BackgroundTaskRunner};
use crate::callbacks::modified_paths::{ModificationSummary, ModifiedPathsDatabase};
use crate::callbacks::path_registry::PathRegistry;
use crate::projection::types::ProjectedFileInfo;
use crate::projection::ManifestProjection;
use crate::util::prj_file_name_compare;

/// Coordination layer between virtualizer and projection.
///
/// Tracks modified paths and coordinates background operations.
/// Integrates dirty file/directory managers for write support.
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
    dirty_files: Arc<DirtyFileManager>,
    /// Dirty directory manager.
    dirty_dirs: Arc<DirtyDirManager>,
    /// Path↔inode registry for dirty state lookup.
    path_registry: Arc<PathRegistry>,
    /// Modified paths database for diff manifest.
    modified_paths: Arc<ModifiedPathsDatabase>,
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
        // Create and populate path registry from projection
        let path_registry = Arc::new(PathRegistry::new());
        path_registry.populate_from_projection(&projection);

        // Create modified paths database
        let modified_paths = Arc::new(ModifiedPathsDatabase::new());
        let paths_clone: Arc<ModifiedPathsDatabase> = modified_paths.clone();

        // Wire background task handler to update modified paths
        let background_tasks = BackgroundTaskRunner::new(move |task: BackgroundTask| {
            match task {
                BackgroundTask::FileCreated(path) => {
                    tracing::debug!("Background: file created {}", path);
                    paths_clone.file_created(&path);
                }
                BackgroundTask::FileModified(path) => {
                    tracing::debug!("Background: file modified {}", path);
                    paths_clone.file_modified(&path);
                }
                BackgroundTask::FileDeleted(path) => {
                    tracing::debug!("Background: file deleted {}", path);
                    paths_clone.file_deleted(&path);
                }
                BackgroundTask::FileRenamed { old_path, new_path } => {
                    tracing::debug!("Background: file renamed {} -> {}", old_path, new_path);
                    paths_clone.file_renamed(&old_path, &new_path);
                }
                BackgroundTask::FolderCreated(path) => {
                    tracing::debug!("Background: folder created {}", path);
                    paths_clone.dir_created(&path);
                }
                BackgroundTask::FolderDeleted(path) => {
                    tracing::debug!("Background: folder deleted {}", path);
                    paths_clone.dir_deleted(&path);
                }
            }
        });

        Self {
            projection,
            background_tasks,
            storage,
            memory_pool,
            dirty_files,
            dirty_dirs,
            path_registry,
            modified_paths,
        }
    }

    /// Get projected items for enumeration with dirty state applied.
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
        let base_items: Arc<[ProjectedFileInfo]> = self.projection.get_projected_items(relative_path)?;

        // Fast path: no dirty state for this directory
        if !self.has_dirty_state_in_dir(relative_path) {
            return Some(base_items);
        }

        // Slow path: apply dirty state
        let modified: Vec<ProjectedFileInfo> = self.apply_dirty_state(&base_items, relative_path);
        Some(Arc::from(modified))
    }

    /// Check if directory has any dirty state that needs to be applied.
    ///
    /// # Arguments
    /// * `_dir_path` - Directory path to check
    ///
    /// # Returns
    /// True if dirty state needs to be applied.
    fn has_dirty_state_in_dir(&self, _dir_path: &str) -> bool {
        // For now, check if any dirty state exists globally
        // TODO: Optimize with per-directory dirty tracking
        let pool_stats = self.memory_pool.stats();
        pool_stats.dirty_blocks > 0 || !self.dirty_dirs.get_dirty_dir_entries().is_empty()
    }

    /// Apply dirty state to enumeration items.
    ///
    /// # Arguments
    /// * `items` - Base items from projection
    /// * `parent_path` - Parent directory path
    ///
    /// # Returns
    /// Modified items with dirty state applied.
    fn apply_dirty_state(
        &self,
        items: &[ProjectedFileInfo],
        parent_path: &str,
    ) -> Vec<ProjectedFileInfo> {
        let mut result: Vec<ProjectedFileInfo> = Vec::with_capacity(items.len());
        let mut seen_names: HashSet<String> = HashSet::new();

        for item in items {
            let item_path: String = self.join_path(parent_path, &item.name);
            let name_lower: String = item.name.to_lowercase();

            // Skip deleted items
            if self.is_path_deleted(&item_path) {
                continue;
            }

            // Check for modifications (updated size/mtime)
            if let Some(inode) = self.path_registry.get(&item_path) {
                if let Some(size) = self.dirty_files.get_size(inode) {
                    let mtime: SystemTime = self.dirty_files.get_mtime(inode).unwrap_or(item.mtime);
                    result.push(ProjectedFileInfo::file(
                        item.name.to_string(),
                        size,
                        item.content_hash.as_ref().map(|s| s.to_string()).unwrap_or_default(),
                        mtime,
                        item.executable,
                    ));
                    seen_names.insert(name_lower);
                    continue;
                }
            }

            result.push(item.clone());
            seen_names.insert(name_lower);
        }

        // Add new files from dirty manager
        self.add_new_files_to_result(&mut result, &mut seen_names, parent_path);

        // Add new directories from dirty manager
        self.add_new_dirs_to_result(&mut result, &mut seen_names, parent_path);

        // Re-sort using ProjFS collation order
        result.sort_by(|a, b| prj_file_name_compare(&a.name, &b.name));

        result
    }

    /// Add new files from dirty manager to result.
    ///
    /// # Arguments
    /// * `result` - Result vector to add to
    /// * `seen_names` - Set of already-seen names (lowercase)
    /// * `parent_path` - Parent directory path
    fn add_new_files_to_result(
        &self,
        result: &mut Vec<ProjectedFileInfo>,
        seen_names: &mut HashSet<String>,
        parent_path: &str,
    ) {
        // Get new files in this directory from dirty file manager
        let new_files: Vec<(String, u64, SystemTime)> = self.get_new_files_in_dir(parent_path);

        for (name, size, mtime) in new_files {
            let name_lower: String = name.to_lowercase();
            if !seen_names.contains(&name_lower) {
                result.push(ProjectedFileInfo::file(
                    name,
                    size,
                    String::new(), // New files don't have content hash yet
                    mtime,
                    false,
                ));
                seen_names.insert(name_lower);
            }
        }
    }

    /// Add new directories from dirty manager to result.
    ///
    /// # Arguments
    /// * `result` - Result vector to add to
    /// * `seen_names` - Set of already-seen names (lowercase)
    /// * `parent_path` - Parent directory path
    fn add_new_dirs_to_result(
        &self,
        result: &mut Vec<ProjectedFileInfo>,
        seen_names: &mut HashSet<String>,
        parent_path: &str,
    ) {
        // Get new directories in this parent from dirty dir manager
        let new_dirs: Vec<String> = self.get_new_dirs_in_parent(parent_path);

        for name in new_dirs {
            let name_lower: String = name.to_lowercase();
            if !seen_names.contains(&name_lower) {
                result.push(ProjectedFileInfo::folder(name));
                seen_names.insert(name_lower);
            }
        }
    }

    /// Get new files in a directory from dirty file manager.
    ///
    /// # Arguments
    /// * `parent_path` - Parent directory path
    ///
    /// # Returns
    /// Vector of (name, size, mtime) for new files.
    fn get_new_files_in_dir(&self, parent_path: &str) -> Vec<(String, u64, SystemTime)> {
        // Get all dirty entries and filter for new files in this directory
        let entries = self.dirty_files.get_dirty_entries();
        let parent_normalized: String = parent_path.to_lowercase().replace('\\', "/");

        entries
            .into_iter()
            .filter(|e| {
                if e.state != rusty_attachments_vfs::write::DirtyState::New {
                    return false;
                }
                let path_normalized: String = e.path.to_lowercase().replace('\\', "/");
                let file_parent: &str = path_normalized.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                file_parent == parent_normalized
            })
            .map(|e| {
                let name: String = e.path.rsplit('/').next().unwrap_or(&e.path).to_string();
                (name, e.size, e.mtime)
            })
            .collect()
    }

    /// Get new directories in a parent from dirty dir manager.
    ///
    /// # Arguments
    /// * `parent_path` - Parent directory path
    ///
    /// # Returns
    /// Vector of directory names.
    fn get_new_dirs_in_parent(&self, parent_path: &str) -> Vec<String> {
        let entries = self.dirty_dirs.get_dirty_dir_entries();
        let parent_normalized: String = parent_path.to_lowercase().replace('\\', "/");

        entries
            .into_iter()
            .filter(|e| {
                if e.state != rusty_attachments_vfs::write::DirtyDirState::New {
                    return false;
                }
                let path_normalized: String = e.path.to_lowercase().replace('\\', "/");
                let dir_parent: &str = path_normalized.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                dir_parent == parent_normalized
            })
            .map(|e| e.path.rsplit('/').next().unwrap_or(&e.path).to_string())
            .collect()
    }

    /// Check if a path has been deleted.
    ///
    /// # Arguments
    /// * `path` - Path to check
    ///
    /// # Returns
    /// True if path is deleted.
    fn is_path_deleted(&self, path: &str) -> bool {
        if let Some(inode) = self.path_registry.get(path) {
            if let Some(state) = self.dirty_files.get_state(inode) {
                return state == rusty_attachments_vfs::write::DirtyState::Deleted;
            }
            if let Some(state) = self.dirty_dirs.get_state(inode) {
                return state == rusty_attachments_vfs::write::DirtyDirState::Deleted;
            }
        }
        false
    }

    /// Join parent path and name.
    ///
    /// # Arguments
    /// * `parent` - Parent path
    /// * `name` - Child name
    ///
    /// # Returns
    /// Joined path.
    fn join_path(&self, parent: &str, name: &str) -> String {
        if parent.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent, name)
        }
    }

    /// Check if path is projected (with dirty state).
    ///
    /// # Arguments
    /// * `relative_path` - Path to check
    ///
    /// # Returns
    /// (canonical_name, is_folder) if exists.
    pub fn is_path_projected(&self, relative_path: &str) -> Option<(String, bool)> {
        // Check if deleted
        if self.is_path_deleted(relative_path) {
            return None;
        }

        // Check manifest projection first
        if let Some(result) = self.projection.is_path_projected(relative_path) {
            return Some(result);
        }

        // Check new files from dirty manager
        if let Some(inode) = self.path_registry.get(relative_path) {
            if let Some(state) = self.dirty_files.get_state(inode) {
                if state == rusty_attachments_vfs::write::DirtyState::New {
                    let name: String = relative_path
                        .rsplit('/')
                        .next()
                        .unwrap_or(relative_path)
                        .to_string();
                    return Some((name, false));
                }
            }
            if self.dirty_dirs.is_new_dir(inode) {
                let name: String = relative_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(relative_path)
                    .to_string();
                return Some((name, true));
            }
        }

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
        // Check if deleted
        if self.is_path_deleted(relative_path) {
            return None;
        }

        // Check dirty files first for updated info
        if let Some(inode) = self.path_registry.get(relative_path) {
            if let Some(size) = self.dirty_files.get_size(inode) {
                let mtime: SystemTime = self.dirty_files.get_mtime(inode).unwrap_or(SystemTime::now());
                let name: String = relative_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(relative_path)
                    .to_string();

                // For dirty files, we may not have a content hash
                return Some(ProjectedFileInfo::file(name, size, String::new(), mtime, false));
            }
        }

        // Fall back to projection
        self.projection.get_file_info(relative_path)
    }

    /// Fetch file content from storage with caching.
    ///
    /// Handles both single-hash and chunked files. For chunked files,
    /// automatically fetches the correct chunks based on offset/length.
    ///
    /// # Arguments
    /// * `relative_path` - Path to file (for chunk lookup)
    /// * `offset` - Byte offset
    /// * `length` - Bytes to read
    /// * `file_size` - Total file size
    ///
    /// # Returns
    /// File content bytes.
    pub async fn fetch_file_content_for_path(
        &self,
        relative_path: &str,
        offset: u64,
        length: u32,
        file_size: u64,
    ) -> Result<Vec<u8>, VfsError> {
        // Get content hash info (may be single or chunked)
        let content_hash = self.projection.get_content_hash(relative_path)
            .ok_or_else(|| VfsError::NotFound(relative_path.to_string()))?;

        match content_hash {
            crate::projection::types::ContentHash::Single(hash) => {
                self.fetch_single_hash(&hash, offset, length).await
            }
            crate::projection::types::ContentHash::Chunked(hashes) => {
                self.fetch_chunked(&hashes, file_size, offset, length).await
            }
        }
    }

    /// Fetch file content from storage with caching (single hash).
    ///
    /// Checks: 1) Memory pool cache, 2) S3 CAS
    ///
    /// # Arguments
    /// * `content_hash` - Hash of content to fetch
    /// * `offset` - Byte offset
    /// * `length` - Bytes to read
    ///
    /// # Returns
    /// File content bytes.
    pub async fn fetch_file_content(
        &self,
        content_hash: &str,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, VfsError> {
        self.fetch_single_hash(content_hash, offset, length).await
    }

    /// Fetch content for a single hash with caching.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `offset` - Byte offset within the content
    /// * `length` - Bytes to read
    ///
    /// # Returns
    /// Requested byte range.
    async fn fetch_single_hash(
        &self,
        hash: &str,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, VfsError> {
        let key = BlockKey::from_hash_hex(hash, 0);

        // Try cache first (synchronous)
        if let Some(handle) = self.memory_pool.try_get(&key) {
            let data: Vec<u8> = self.extract_range(handle.data(), offset, length);
            return Ok(data);
        }

        // Fetch from S3 with caching via acquire
        let hash_alg: HashAlgorithm = self.projection.hash_algorithm();
        let storage: Arc<dyn FileStore> = self.storage.clone();
        let hash_clone: String = hash.to_string();

        let handle = self.memory_pool.acquire(&key, move || {
            let storage = storage.clone();
            let hash = hash_clone.clone();
            async move {
                storage.retrieve(&hash, hash_alg).await
                    .map_err(|e| rusty_attachments_vfs::memory_pool_v2::MemoryPoolError::RetrievalFailed(e.to_string()))
            }
        }).await.map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

        // Extract requested range
        let data: Vec<u8> = self.extract_range(handle.data(), offset, length);
        Ok(data)
    }

    /// Fetch content spanning multiple chunks.
    ///
    /// V2 manifests support chunked files (>256MB). Each chunk is 256MB.
    /// This method fetches the required chunks and assembles the result.
    ///
    /// # Arguments
    /// * `chunk_hashes` - Hashes for each chunk
    /// * `file_size` - Total file size
    /// * `offset` - Byte offset within the file
    /// * `length` - Bytes to read
    ///
    /// # Returns
    /// Requested byte range assembled from chunks.
    async fn fetch_chunked(
        &self,
        chunk_hashes: &[String],
        file_size: u64,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, VfsError> {
        const CHUNK_SIZE: u64 = 256 * 1024 * 1024; // 256MB per chunk

        // Calculate which chunks we need
        let start_chunk: usize = (offset / CHUNK_SIZE) as usize;
        let end_offset: u64 = (offset + length as u64).min(file_size);
        let end_chunk: usize = if end_offset == 0 {
            0
        } else {
            ((end_offset - 1) / CHUNK_SIZE) as usize
        };

        if start_chunk >= chunk_hashes.len() {
            return Err(VfsError::NotFound(format!(
                "Invalid offset {} for file size {}",
                offset, file_size
            )));
        }

        let mut result: Vec<u8> = Vec::with_capacity(length as usize);
        let last_chunk: usize = end_chunk.min(chunk_hashes.len() - 1);

        for (chunk_idx, chunk_hash) in chunk_hashes.iter().enumerate().skip(start_chunk).take(last_chunk - start_chunk + 1) {
            let chunk_start: u64 = chunk_idx as u64 * CHUNK_SIZE;
            let chunk_end: u64 = ((chunk_idx + 1) as u64 * CHUNK_SIZE).min(file_size);

            // Calculate read range within this chunk
            let read_start: u64 = if chunk_idx == start_chunk {
                offset - chunk_start
            } else {
                0
            };
            let read_end: u64 = if chunk_idx == end_chunk {
                end_offset - chunk_start
            } else {
                chunk_end - chunk_start
            };

            let chunk_data: Vec<u8> = self.fetch_single_hash(
                chunk_hash,
                read_start,
                (read_end - read_start) as u32,
            ).await?;

            result.extend_from_slice(&chunk_data);
        }

        Ok(result)
    }

    /// Extract a byte range from data.
    ///
    /// # Arguments
    /// * `data` - Source data
    /// * `offset` - Byte offset
    /// * `length` - Bytes to read
    ///
    /// # Returns
    /// Extracted byte range.
    fn extract_range(&self, data: &[u8], offset: u64, length: u32) -> Vec<u8> {
        let start: usize = offset as usize;
        let end: usize = (offset + length as u64).min(data.len() as u64) as usize;

        if start >= data.len() {
            return Vec::new();
        }

        data[start..end].to_vec()
    }

    /// Get reference to projection.
    pub fn projection(&self) -> &Arc<ManifestProjection> {
        &self.projection
    }

    /// Get reference to memory pool.
    pub fn memory_pool(&self) -> &Arc<MemoryPool> {
        &self.memory_pool
    }

    /// Get reference to path registry.
    pub fn path_registry(&self) -> &Arc<PathRegistry> {
        &self.path_registry
    }

    /// Get modification summary for diff manifest.
    ///
    /// # Returns
    /// Summary of all modifications during this session.
    pub fn get_modification_summary(&self) -> ModificationSummary {
        self.modified_paths.get_summary()
    }

    // --- Notification handlers (queued to background) ---

    /// Called when a new file is created.
    ///
    /// # Arguments
    /// * `relative_path` - Path of created file
    pub fn on_file_created(&self, relative_path: &str) {
        self.path_registry.get_or_create(relative_path);
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
        self.path_registry.rename(old_path, new_path);
        self.background_tasks.enqueue(BackgroundTask::FileRenamed {
            old_path: old_path.to_string(),
            new_path: new_path.to_string(),
        });
    }

    /// Called when a directory is created.
    ///
    /// # Arguments
    /// * `relative_path` - Path of created directory
    pub fn on_dir_created(&self, relative_path: &str) {
        self.path_registry.get_or_create(relative_path);
        self.background_tasks
            .enqueue(BackgroundTask::FolderCreated(relative_path.to_string()));
    }

    /// Called when a directory is deleted.
    ///
    /// # Arguments
    /// * `relative_path` - Path of deleted directory
    pub fn on_dir_deleted(&self, relative_path: &str) {
        self.background_tasks
            .enqueue(BackgroundTask::FolderDeleted(relative_path.to_string()));
    }

    /// Called when a file is hydrated.
    ///
    /// # Arguments
    /// * `relative_path` - Path of hydrated file
    pub fn on_file_hydrated(&self, relative_path: &str) {
        tracing::debug!("File hydrated: {}", relative_path);
    }
}
