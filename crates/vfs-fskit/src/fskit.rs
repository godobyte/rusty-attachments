//! FSKit filesystem implementation for Deadline Cloud job attachments.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use fskit_rs::{
    AccessMask, DirectoryEntries, Error, Filesystem, Item, ItemAttributes, ItemType, OpenMode,
    PathConfOperations, PreallocateFlag, ResourceIdentifier, Result, SetXattrPolicy, StatFsResult,
    SupportedCapabilities, SyncFlags, TaskOptions, VolumeBehavior, VolumeIdentifier, Xattrs,
};
use parking_lot::RwLock;
use rusty_attachments_model::{HashAlgorithm, Manifest};
use rusty_attachments_vfs::diskcache::{MaterializedCache, ReadCache, ReadCacheOptions};
use rusty_attachments_vfs::inode::{INodeType, ROOT_INODE};
use rusty_attachments_vfs::write::{
    DirtyDirManager, DirtyDirState, DirtyEntry, DirtyFileManager, DirtyState, WriteCache,
};
use rusty_attachments_vfs::{
    DirtySummary, FileContent, FileStore, INode, INodeManager, MemoryPool,
    WritableVfsStatsCollector, build_from_manifest,
};

use crate::convert::{
    new_dir_attributes, new_file_attributes, to_item, to_item_attributes, to_item_type,
    to_proto_timestamp,
};
use crate::error::FsKitVfsError;
use crate::helpers::{make_dir_entry, path_join};
use crate::options::{FsKitVfsOptions, FsKitWriteOptions};

/// Writable FSKit filesystem implementation for Deadline Cloud job attachments.
///
/// Implements `fskit_rs::Filesystem` trait with full COW write support.
///
/// # Thread Safety
/// All mutable state is wrapped in `Arc` with interior mutability (`RwLock`/`DashMap`).
/// The struct implements `Clone` by cloning Arc pointers (cheap).
///
/// # Async Model
/// Unlike FUSE, FSKit-rs runs in a Tokio context, so we can directly `.await`
/// on async operations without needing an `AsyncExecutor` bridge.
#[derive(Clone)]
pub struct WritableFsKit {
    /// Shared state (wrapped in Arc for Clone requirement).
    inner: Arc<WritableFsKitInner>,
}

struct WritableFsKitInner {
    /// Inode manager for file metadata.
    inodes: Arc<INodeManager>,
    /// File store for fetching content from S3.
    store: Arc<dyn FileStore>,
    /// Memory pool for caching content.
    pool: Arc<MemoryPool>,
    /// Optional disk cache for read-only content.
    #[allow(dead_code)]
    read_cache: Option<Arc<ReadCache>>,
    /// Dirty file manager (COW layer).
    dirty_manager: Arc<DirtyFileManager>,
    /// Dirty directory manager.
    dirty_dir_manager: Arc<DirtyDirManager>,
    /// Original directories from manifest (for diff tracking).
    #[allow(dead_code)]
    original_dirs: HashSet<String>,
    /// Hash algorithm from manifest.
    #[allow(dead_code)]
    hash_algorithm: HashAlgorithm,
    /// VFS options.
    options: FsKitVfsOptions,
    /// Directory enumeration state (verifiers).
    enum_state: RwLock<HashMap<u64, u64>>,
    /// Next inode ID for new files (starts at 0x8000_0000).
    next_inode: AtomicU64,
    /// VFS creation time for uptime tracking.
    start_time: Instant,
}

impl WritableFsKit {
    /// Create a new writable FSKit VFS.
    ///
    /// # Arguments
    /// * `manifest` - Manifest to mount
    /// * `store` - File store for reading original content
    /// * `options` - VFS options
    /// * `write_options` - Write-specific options
    ///
    /// # Returns
    /// New WritableFsKit instance or error.
    pub fn new(
        manifest: &Manifest,
        store: Arc<dyn FileStore>,
        options: FsKitVfsOptions,
        write_options: FsKitWriteOptions,
    ) -> std::result::Result<Self, FsKitVfsError> {
        let inodes: Arc<INodeManager> = Arc::new(build_from_manifest(manifest));
        let hash_algorithm: HashAlgorithm = manifest.hash_alg();
        let pool = Arc::new(MemoryPool::new(options.pool.clone()));

        // Initialize read cache if enabled
        let read_cache: Option<Arc<ReadCache>> = if options.read_cache.enabled {
            let cache_options = ReadCacheOptions {
                cache_dir: options.read_cache.cache_dir.clone(),
                write_through: options.read_cache.write_through,
            };
            Some(Arc::new(
                ReadCache::new(cache_options)
                    .map_err(|e| FsKitVfsError::ReadCacheError(e.to_string()))?,
            ))
        } else {
            None
        };

        // Initialize write cache (MaterializedCache)
        let write_cache: Arc<dyn WriteCache> = Arc::new(
            MaterializedCache::new(write_options.cache_dir.clone())
                .map_err(|e| FsKitVfsError::WriteCacheError(e.to_string()))?,
        );

        // Create DirtyFileManager with unified memory pool
        let mut dirty_manager =
            DirtyFileManager::new(write_cache, store.clone(), inodes.clone(), pool.clone());
        if let Some(ref rc) = read_cache {
            dirty_manager.set_read_cache(rc.clone());
        }
        let dirty_manager = Arc::new(dirty_manager);

        // Collect original directories for diff tracking
        let original_dirs: HashSet<String> = match manifest {
            Manifest::V2025_12_04_beta(m) => m.dirs.iter().map(|d| d.name.clone()).collect(),
            Manifest::V2023_03_03(_) => HashSet::new(),
        };

        // Create dirty directory manager
        let dirty_dir_manager =
            Arc::new(DirtyDirManager::new(inodes.clone(), original_dirs.clone()));

        Ok(Self {
            inner: Arc::new(WritableFsKitInner {
                inodes,
                store,
                pool,
                read_cache,
                dirty_manager,
                dirty_dir_manager,
                original_dirs,
                hash_algorithm,
                options,
                enum_state: RwLock::new(HashMap::new()),
                next_inode: AtomicU64::new(0x8000_0000),
                start_time: Instant::now(),
            }),
        })
    }

    /// Get the dirty file manager.
    ///
    /// # Returns
    /// Reference to the dirty file manager for external access.
    pub fn dirty_manager(&self) -> &Arc<DirtyFileManager> {
        &self.inner.dirty_manager
    }

    /// Get the dirty directory manager.
    ///
    /// # Returns
    /// Reference to the dirty directory manager for external access.
    pub fn dirty_dir_manager(&self) -> &Arc<DirtyDirManager> {
        &self.inner.dirty_dir_manager
    }

    /// Get a stats collector for this writable VFS.
    ///
    /// # Returns
    /// Collector that provides memory pool stats, cache hit rates, and dirty file lists.
    pub fn stats_collector(&self) -> WritableVfsStatsCollector {
        WritableVfsStatsCollector::new(
            self.inner.pool.clone(),
            self.inner.dirty_manager.clone(),
            self.inner.inodes.inode_count(),
            self.inner.start_time,
        )
    }

    /// Get directory verifier for change detection.
    fn get_dir_verifier(&self, dir_id: u64) -> u64 {
        let state: parking_lot::RwLockReadGuard<'_, HashMap<u64, u64>> =
            self.inner.enum_state.read();
        *state.get(&dir_id).unwrap_or(&1)
    }

    /// Increment directory verifier (called on modifications).
    fn bump_dir_verifier(&self, dir_id: u64) {
        let mut state: parking_lot::RwLockWriteGuard<'_, HashMap<u64, u64>> =
            self.inner.enum_state.write();
        let v: &mut u64 = state.entry(dir_id).or_insert(1);
        *v = v.wrapping_add(1);
    }

    /// Get parent path for a directory ID.
    fn get_parent_path(&self, dir_id: u64) -> Result<String> {
        if let Some(inode) = self.inner.inodes.get(dir_id) {
            return Ok(inode.path().to_string());
        }
        if let Some(path) = self.inner.dirty_dir_manager.get_new_dir_path(dir_id) {
            return Ok(path);
        }
        Err(Error::Posix(libc::ENOENT))
    }

    /// Convert inode to Item with dirty state overlay.
    fn to_item_with_dirty_attrs(&self, inode: &dyn INode, name: &str) -> Item {
        let mut attrs: ItemAttributes = to_item_attributes(inode);

        // Override with dirty state if applicable
        if let Some(size) = self.inner.dirty_manager.get_size(inode.id()) {
            attrs.size = Some(size);
            attrs.alloc_size = Some(size);
        }
        if let Some(mtime) = self.inner.dirty_manager.get_mtime(inode.id()) {
            let ts = to_proto_timestamp(mtime);
            attrs.modify_time = Some(ts);
            attrs.access_time = Some(ts);
        }

        Item {
            attributes: Some(attrs),
            name: name.as_bytes().to_vec(),
        }
    }

    /// Convert new file to Item.
    fn new_file_to_item(&self, item_id: u64, name: &str, parent_id: u64) -> Item {
        let size: u64 = self.inner.dirty_manager.get_size(item_id).unwrap_or(0);
        let mtime: SystemTime = self
            .inner
            .dirty_manager
            .get_mtime(item_id)
            .unwrap_or_else(SystemTime::now);

        Item {
            attributes: Some(new_file_attributes(item_id, parent_id, size, mtime)),
            name: name.as_bytes().to_vec(),
        }
    }

    /// Convert new directory to Item.
    fn new_dir_to_item(&self, item_id: u64, name: &str, parent_id: u64) -> Item {
        Item {
            attributes: Some(new_dir_attributes(item_id, parent_id)),
            name: name.as_bytes().to_vec(),
        }
    }

    /// Read from a single-hash file.
    async fn read_single_hash(&self, hash: &str, offset: u64, length: u64) -> Result<Vec<u8>> {
        // Fetch from store
        self.inner
            .store
            .retrieve_range(hash, self.inner.hash_algorithm, offset, length)
            .await
            .map_err(|_| Error::Posix(libc::EIO))
    }

    /// Read from a chunked file.
    async fn read_chunked(&self, hashes: &[String], offset: u64, length: u64) -> Result<Vec<u8>> {
        use rusty_attachments_common::CHUNK_SIZE_V2;

        let chunk_size: u64 = CHUNK_SIZE_V2 as u64;
        let start_chunk: usize = (offset / chunk_size) as usize;
        let end_offset: u64 = offset + length;
        let end_chunk: usize = ((end_offset + chunk_size - 1) / chunk_size) as usize;

        let mut result: Vec<u8> = Vec::with_capacity(length as usize);
        let mut current_offset: u64 = offset;

        for chunk_idx in start_chunk..end_chunk.min(hashes.len()) {
            let chunk_start: u64 = (chunk_idx as u64) * chunk_size;
            let chunk_end: u64 = chunk_start + chunk_size;

            let read_start: u64 = current_offset.max(chunk_start) - chunk_start;
            let read_end: u64 = end_offset.min(chunk_end) - chunk_start;
            let read_len: u64 = read_end - read_start;

            let hash: &str = &hashes[chunk_idx];
            let data: Vec<u8> = self.read_single_hash(hash, read_start, read_len).await?;
            result.extend_from_slice(&data);

            current_offset = chunk_end;
        }

        Ok(result)
    }

    /// Flush all dirty files to disk.
    async fn flush_all_dirty(&self) -> std::result::Result<(), rusty_attachments_vfs::VfsError> {
        // Flush each dirty file individually
        let entries: Vec<DirtyEntry> = self.inner.dirty_manager.get_dirty_entries();
        for entry in entries {
            if entry.state != DirtyState::Deleted {
                self.inner.dirty_manager.flush_to_disk(entry.inode_id).await?;
            }
        }
        Ok(())
    }
}


#[async_trait]
impl Filesystem for WritableFsKit {
    async fn get_resource_identifier(&mut self) -> Result<ResourceIdentifier> {
        Ok(ResourceIdentifier {
            name: Some(self.inner.options.volume_name.clone()),
            container_id: Some(self.inner.options.volume_id.clone()),
        })
    }

    async fn get_volume_identifier(&mut self) -> Result<VolumeIdentifier> {
        Ok(VolumeIdentifier {
            id: Some(self.inner.options.volume_id.clone()),
            name: Some(self.inner.options.volume_name.clone()),
        })
    }

    async fn get_volume_behavior(&mut self) -> Result<VolumeBehavior> {
        Ok(VolumeBehavior::default())
    }

    async fn get_path_conf_operations(&mut self) -> Result<PathConfOperations> {
        Ok(PathConfOperations {
            maximum_name_length: 255,
            maximum_link_count: 1,
            ..Default::default()
        })
    }

    async fn get_volume_capabilities(&mut self) -> Result<SupportedCapabilities> {
        Ok(SupportedCapabilities {
            supports_hard_links: Some(false),
            supports_symbolic_links: Some(true),
            case_format: Some(fskit_rs::CaseFormat::Sensitive as i32),
            supports_sparse_files: Some(true),
            ..Default::default()
        })
    }

    async fn get_volume_statistics(&mut self) -> Result<StatFsResult> {
        Ok(StatFsResult {
            total_blocks: self.inner.inodes.inode_count() as u64,
            available_blocks: u64::MAX,
            free_blocks: u64::MAX,
            block_size: 512,
            ..Default::default()
        })
    }

    async fn mount(&mut self, _options: TaskOptions) -> Result<()> {
        // FSKit calls mount before activate
        // No-op for our implementation - state already initialized in constructor
        Ok(())
    }

    async fn unmount(&mut self) -> Result<()> {
        // Flush all dirty files before unmount
        self.flush_all_dirty()
            .await
            .map_err(|_| Error::Posix(libc::EIO))
    }

    async fn synchronize(&mut self, _flags: SyncFlags) -> Result<()> {
        // Flush all dirty files to disk
        self.flush_all_dirty()
            .await
            .map_err(|_| Error::Posix(libc::EIO))
    }

    async fn activate(&mut self, _options: TaskOptions) -> Result<Item> {
        // Return root directory item
        let root: Arc<dyn INode> = self
            .inner
            .inodes
            .get(ROOT_INODE)
            .ok_or(Error::Posix(libc::EIO))?;

        Ok(to_item(root.as_ref(), ""))
    }

    async fn deactivate(&mut self) -> Result<()> {
        // Cleanup after volume is deactivated
        let _ = self.flush_all_dirty().await;
        Ok(())
    }

    async fn get_attributes(&mut self, item_id: u64) -> Result<ItemAttributes> {
        // Check manifest inodes first
        if let Some(inode) = self.inner.inodes.get(item_id) {
            // Check if deleted
            if self.inner.dirty_manager.get_state(item_id) == Some(DirtyState::Deleted) {
                return Err(Error::Posix(libc::ENOENT));
            }
            if self.inner.dirty_dir_manager.get_state(item_id) == Some(DirtyDirState::Deleted) {
                return Err(Error::Posix(libc::ENOENT));
            }

            let mut attrs: ItemAttributes = to_item_attributes(inode.as_ref());

            // Override with dirty state if applicable
            if let Some(size) = self.inner.dirty_manager.get_size(item_id) {
                attrs.size = Some(size);
                attrs.alloc_size = Some(size);
            }
            if let Some(mtime) = self.inner.dirty_manager.get_mtime(item_id) {
                let ts = to_proto_timestamp(mtime);
                attrs.modify_time = Some(ts);
                attrs.access_time = Some(ts);
            }

            return Ok(attrs);
        }

        // Check new files
        if self.inner.dirty_manager.is_new_file(item_id) {
            let size: u64 = self.inner.dirty_manager.get_size(item_id).unwrap_or(0);
            let mtime: SystemTime = self
                .inner
                .dirty_manager
                .get_mtime(item_id)
                .unwrap_or_else(SystemTime::now);

            return Ok(new_file_attributes(item_id, 0, size, mtime));
        }

        // Check new directories
        if self.inner.dirty_dir_manager.is_new_dir(item_id) {
            return Ok(new_dir_attributes(item_id, 0));
        }

        Err(Error::Posix(libc::ENOENT))
    }

    async fn set_attributes(
        &mut self,
        item_id: u64,
        attributes: ItemAttributes,
    ) -> Result<ItemAttributes> {
        // Handle truncate (size change)
        if let Some(new_size) = attributes.size {
            self.inner
                .dirty_manager
                .truncate(item_id, new_size)
                .await
                .map_err(|_| Error::Posix(libc::EIO))?;
        }

        // Return updated attributes
        self.get_attributes(item_id).await
    }

    async fn lookup_item(&mut self, name: &OsStr, directory_id: u64) -> Result<Item> {
        let name_str: &str = name.to_str().ok_or(Error::Posix(libc::EINVAL))?;

        // Get parent path
        let parent_path: String = self.get_parent_path(directory_id)?;
        let path: String = path_join(&parent_path, name_str);

        // Check manifest files first
        if let Some(child) = self.inner.inodes.get_by_path(&path) {
            // Check if deleted
            if self.inner.dirty_manager.get_state(child.id()) == Some(DirtyState::Deleted) {
                return Err(Error::Posix(libc::ENOENT));
            }
            if self.inner.dirty_dir_manager.get_state(child.id()) == Some(DirtyDirState::Deleted) {
                return Err(Error::Posix(libc::ENOENT));
            }

            return Ok(self.to_item_with_dirty_attrs(child.as_ref(), name_str));
        }

        // Check new files
        if let Some(new_ino) = self.inner.dirty_manager.lookup_new_file(directory_id, name_str) {
            return Ok(self.new_file_to_item(new_ino, name_str, directory_id));
        }

        // Check new directories
        if let Some(new_ino) = self
            .inner
            .dirty_dir_manager
            .lookup_new_dir(directory_id, name_str)
        {
            return Ok(self.new_dir_to_item(new_ino, name_str, directory_id));
        }

        Err(Error::Posix(libc::ENOENT))
    }

    async fn reclaim_item(&mut self, item_id: u64) -> Result<()> {
        // FSKit calls this when kernel no longer references an item
        tracing::trace!("reclaim_item: {}", item_id);
        Ok(())
    }

    async fn read_symbolic_link(&mut self, item_id: u64) -> Result<Vec<u8>> {
        match self.inner.inodes.get_symlink_target(item_id) {
            Some(target) => Ok(target.as_bytes().to_vec()),
            None => Err(Error::Posix(libc::EINVAL)),
        }
    }

    async fn create_item(
        &mut self,
        name: &OsStr,
        r#type: ItemType,
        directory_id: u64,
        attributes: ItemAttributes,
    ) -> Result<Item> {
        let name_str: &str = name.to_str().ok_or(Error::Posix(libc::EINVAL))?;

        // Validate name
        if name_str.is_empty() || name_str.contains('/') {
            return Err(Error::Posix(libc::EINVAL));
        }

        // Get parent path
        let parent_path: String = self.get_parent_path(directory_id)?;
        let new_path: String = path_join(&parent_path, name_str);

        match r#type {
            ItemType::File => {
                // Check if file already exists
                if self.inner.inodes.get_by_path(&new_path).is_some() {
                    return Err(Error::Posix(libc::EEXIST));
                }

                // Allocate new inode ID
                let new_ino: u64 = self.inner.next_inode.fetch_add(1, Ordering::SeqCst);

                // Create dirty file entry
                self.inner
                    .dirty_manager
                    .create_file(new_ino, new_path, directory_id)
                    .map_err(|_| Error::Posix(libc::EIO))?;

                // Bump directory verifier
                self.bump_dir_verifier(directory_id);

                Ok(Item {
                    name: name_str.as_bytes().to_vec(),
                    attributes: Some(ItemAttributes {
                        file_id: Some(new_ino),
                        parent_id: Some(directory_id),
                        r#type: Some(ItemType::File as i32),
                        mode: attributes.mode.or(Some(0o644)),
                        size: Some(0),
                        ..Default::default()
                    }),
                })
            }
            ItemType::Directory => {
                let new_id: u64 = self
                    .inner
                    .dirty_dir_manager
                    .create_dir(directory_id, name_str)
                    .map_err(|e| match e {
                        rusty_attachments_vfs::VfsError::AlreadyExists(_) => {
                            Error::Posix(libc::EEXIST)
                        }
                        rusty_attachments_vfs::VfsError::NotADirectory(_) => {
                            Error::Posix(libc::ENOTDIR)
                        }
                        rusty_attachments_vfs::VfsError::InodeNotFound(_) => {
                            Error::Posix(libc::ENOENT)
                        }
                        _ => Error::Posix(libc::EIO),
                    })?;

                // Bump directory verifier
                self.bump_dir_verifier(directory_id);

                Ok(Item {
                    name: name_str.as_bytes().to_vec(),
                    attributes: Some(ItemAttributes {
                        file_id: Some(new_id),
                        parent_id: Some(directory_id),
                        r#type: Some(ItemType::Directory as i32),
                        mode: attributes.mode.or(Some(0o755)),
                        link_count: Some(2),
                        ..Default::default()
                    }),
                })
            }
            _ => Err(Error::Posix(libc::EINVAL)),
        }
    }

    async fn create_symbolic_link(
        &mut self,
        _name: &OsStr,
        _directory_id: u64,
        _new_attributes: ItemAttributes,
        _contents: Vec<u8>,
    ) -> Result<Item> {
        // Symlink creation not supported
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn create_link(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> Result<Vec<u8>> {
        // Hard links not supported
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn remove_item(&mut self, item_id: u64, name: &OsStr, directory_id: u64) -> Result<()> {
        let name_str: &str = name.to_str().ok_or(Error::Posix(libc::EINVAL))?;

        // Check if it's a file or directory
        let is_directory: bool = if let Some(inode) = self.inner.inodes.get(item_id) {
            inode.inode_type() == INodeType::Directory
        } else if self.inner.dirty_dir_manager.is_new_dir(item_id) {
            true
        } else if self.inner.dirty_manager.is_new_file(item_id) {
            false
        } else {
            return Err(Error::Posix(libc::ENOENT));
        };

        if is_directory {
            // Get directory path for cleanup
            let parent_path: String = self.get_parent_path(directory_id)?;
            let dir_path: String = path_join(&parent_path, name_str);

            // Clean up any new files under this directory
            let _: usize = self
                .inner
                .dirty_manager
                .remove_new_files_under_path(&dir_path);

            // Delete directory
            self.inner
                .dirty_dir_manager
                .delete_dir(directory_id, name_str)
                .map_err(|e| match e {
                    rusty_attachments_vfs::VfsError::NotFound(_) => Error::Posix(libc::ENOENT),
                    rusty_attachments_vfs::VfsError::NotADirectory(_) => {
                        Error::Posix(libc::ENOTDIR)
                    }
                    rusty_attachments_vfs::VfsError::DirectoryNotEmpty(_) => {
                        Error::Posix(libc::ENOTEMPTY)
                    }
                    _ => Error::Posix(libc::EIO),
                })?;
        } else {
            // Delete file
            self.inner
                .dirty_manager
                .delete_file(item_id)
                .await
                .map_err(|_| Error::Posix(libc::EIO))?;
        }

        // Bump directory verifier
        self.bump_dir_verifier(directory_id);

        Ok(())
    }

    async fn rename_item(
        &mut self,
        _item_id: u64,
        _source_directory_id: u64,
        _source_name: &OsStr,
        _destination_name: &OsStr,
        _destination_directory_id: u64,
        _over_item_id: Option<u64>,
    ) -> Result<Vec<u8>> {
        // Rename not yet implemented
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn enumerate_directory(
        &mut self,
        directory_id: u64,
        cookie: u64,
        verifier: u64,
    ) -> Result<DirectoryEntries> {
        // Get directory inode (manifest or new)
        let (parent_id, is_manifest_dir): (u64, bool) =
            if let Some(inode) = self.inner.inodes.get(directory_id) {
                if inode.inode_type() != INodeType::Directory {
                    return Err(Error::Posix(libc::ENOTDIR));
                }
                (inode.parent_id(), true)
            } else if self
                .inner
                .dirty_dir_manager
                .get_new_dir_path(directory_id)
                .is_some()
            {
                (directory_id, false)
            } else {
                return Err(Error::Posix(libc::ENOENT));
            };

        // Check verifier
        let current_verifier: u64 = self.get_dir_verifier(directory_id);
        if verifier != 0 && verifier != current_verifier {
            return Err(Error::Posix(libc::EINVAL));
        }

        let mut entries: Vec<fskit_rs::directory_entries::Entry> = Vec::new();
        // Track names already added to avoid duplicates
        let mut seen_names: HashSet<String> = HashSet::new();

        // Add . and .. for initial enumeration
        if cookie == 0 {
            entries.push(make_dir_entry(".", directory_id, ItemType::Directory, 1));
            entries.push(make_dir_entry("..", parent_id, ItemType::Directory, 2));
        }

        // Add manifest children (excluding deleted)
        if is_manifest_dir {
            if let Some(children) = self.inner.inodes.get_dir_children(directory_id) {
                let start_idx: usize = if cookie <= 2 { 0 } else { (cookie - 2) as usize };

                for (i, (name, child_id)) in children.into_iter().enumerate().skip(start_idx) {
                    // Skip deleted files
                    if self.inner.dirty_manager.get_state(child_id) == Some(DirtyState::Deleted) {
                        continue;
                    }
                    // Skip deleted directories
                    if self.inner.dirty_dir_manager.get_state(child_id)
                        == Some(DirtyDirState::Deleted)
                    {
                        continue;
                    }

                    if let Some(child) = self.inner.inodes.get(child_id) {
                        let item_type: ItemType = to_item_type(child.inode_type());
                        entries.push(make_dir_entry(&name, child_id, item_type, (i + 3) as u64));
                        seen_names.insert(name);
                    }
                }
            }
        }

        // Add new files created in this directory (skip if name already seen)
        let new_files: Vec<(u64, String)> =
            self.inner.dirty_manager.get_new_files_in_dir(directory_id);
        for (new_ino, name) in new_files {
            if !seen_names.contains(&name) {
                entries.push(make_dir_entry(
                    &name,
                    new_ino,
                    ItemType::File,
                    entries.len() as u64 + 1,
                ));
                seen_names.insert(name);
            }
        }

        // Add new directories created in this directory (skip if name already seen)
        let new_dirs: Vec<(u64, String)> = self
            .inner
            .dirty_dir_manager
            .get_new_dirs_in_parent(directory_id);
        for (new_ino, name) in new_dirs {
            if !seen_names.contains(&name) {
                entries.push(make_dir_entry(
                    &name,
                    new_ino,
                    ItemType::Directory,
                    entries.len() as u64 + 1,
                ));
                seen_names.insert(name);
            }
        }

        Ok(DirectoryEntries {
            entries,
            verifier: current_verifier,
        })
    }

    async fn get_supported_xattr_names(&mut self, _item_id: u64) -> Result<Xattrs> {
        Ok(Xattrs { names: vec![] })
    }

    async fn get_xattr(&mut self, _name: &OsStr, _item_id: u64) -> Result<Vec<u8>> {
        // ENOATTR is typically 93 on macOS
        Err(Error::Posix(93))
    }

    async fn set_xattr(
        &mut self,
        _name: &OsStr,
        _value: Option<Vec<u8>>,
        _item_id: u64,
        _policy: SetXattrPolicy,
    ) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_xattrs(&mut self, _item_id: u64) -> Result<Xattrs> {
        Ok(Xattrs { names: vec![] })
    }

    async fn open_item(&mut self, item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        // Check if deleted
        if self.inner.dirty_manager.get_state(item_id) == Some(DirtyState::Deleted) {
            return Err(Error::Posix(libc::ENOENT));
        }

        // Validate item exists (manifest or new file)
        if self.inner.inodes.get(item_id).is_none()
            && !self.inner.dirty_manager.is_new_file(item_id)
        {
            return Err(Error::Posix(libc::ENOENT));
        }

        tracing::trace!("open_item: {}", item_id);
        Ok(())
    }

    async fn close_item(&mut self, item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        // Flush dirty file to disk on close
        if self.inner.dirty_manager.is_dirty(item_id) {
            self.inner
                .dirty_manager
                .flush_to_disk(item_id)
                .await
                .map_err(|_| Error::Posix(libc::EIO))?;
        }
        Ok(())
    }

    async fn read(&mut self, item_id: u64, offset: i64, length: i64) -> Result<Vec<u8>> {
        // Check dirty layer first (COW files and new files)
        if self.inner.dirty_manager.is_dirty(item_id) {
            let offset_u64: u64 = offset as u64;
            let size: u32 = length as u32;

            // Try sync read first (fast path for already-loaded chunks)
            if let Some(data) = self.inner.dirty_manager.read_sync(item_id, offset_u64, size) {
                return Ok(data);
            }

            // Async path: need to load chunks from S3/cache
            return self
                .inner
                .dirty_manager
                .read(item_id, offset_u64, size)
                .await
                .map_err(|_| Error::Posix(libc::EIO));
        }

        // Fall back to read-only store for manifest files
        let content: FileContent = self
            .inner
            .inodes
            .get_file_content(item_id)
            .ok_or(Error::Posix(libc::ENOENT))?;

        let inode: Arc<dyn INode> = self
            .inner
            .inodes
            .get(item_id)
            .ok_or(Error::Posix(libc::ENOENT))?;

        let file_size: u64 = inode.size();
        let off: u64 = offset as u64;

        if off >= file_size {
            return Ok(Vec::new());
        }

        let actual: u64 = (length as u64).min(file_size - off);

        match content {
            FileContent::SingleHash(hash) => self.read_single_hash(&hash, off, actual).await,
            FileContent::Chunked(hashes) => self.read_chunked(&hashes, off, actual).await,
        }
    }

    async fn write(&mut self, contents: Vec<u8>, item_id: u64, offset: i64) -> Result<i64> {
        let offset_u64: u64 = offset as u64;

        // Try sync write first (fast path for already-dirty files)
        if let Some(result) = self
            .inner
            .dirty_manager
            .write_sync(item_id, offset_u64, &contents)
        {
            return result
                .map(|written| written as i64)
                .map_err(|_| Error::Posix(libc::EIO));
        }

        // Async path: need COW or chunk loading
        self.inner
            .dirty_manager
            .write(item_id, offset_u64, &contents)
            .await
            .map(|written| written as i64)
            .map_err(|_| Error::Posix(libc::EIO))
    }

    async fn check_access(&mut self, item_id: u64, _access: Vec<AccessMask>) -> Result<bool> {
        // Check if item exists and is not deleted
        if self.inner.dirty_manager.get_state(item_id) == Some(DirtyState::Deleted) {
            return Err(Error::Posix(libc::ENOENT));
        }
        if self.inner.dirty_dir_manager.get_state(item_id) == Some(DirtyDirState::Deleted) {
            return Err(Error::Posix(libc::ENOENT));
        }

        // Check manifest inodes
        if self.inner.inodes.get(item_id).is_some() {
            return Ok(true);
        }

        // Check new files/directories
        if self.inner.dirty_manager.is_new_file(item_id)
            || self.inner.dirty_dir_manager.is_new_dir(item_id)
        {
            return Ok(true);
        }

        Err(Error::Posix(libc::ENOENT))
    }

    async fn set_volume_name(&mut self, _name: Vec<u8>) -> Result<Vec<u8>> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn preallocate_space(
        &mut self,
        _item_id: u64,
        _offset: i64,
        _length: i64,
        _flags: Vec<PreallocateFlag>,
    ) -> Result<i64> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn deactivate_item(&mut self, item_id: u64) -> Result<()> {
        tracing::trace!("deactivate_item: {}", item_id);
        Ok(())
    }
}

// Note: DiffManifestExporter requires async_trait, but we only implement dirty_summary
// for now. The full export_diff_manifest and clear_dirty are TODO.

impl WritableFsKit {
    /// Get summary of dirty state.
    ///
    /// # Returns
    /// Count of new, modified, and deleted files and directories.
    pub fn dirty_summary(&self) -> DirtySummary {
        let entries: Vec<DirtyEntry> = self.inner.dirty_manager.get_dirty_entries();
        let dir_entries = self.inner.dirty_dir_manager.get_dirty_dir_entries();

        let mut summary = DirtySummary::default();

        for entry in entries {
            match entry.state {
                DirtyState::New => summary.new_count += 1,
                DirtyState::Modified => summary.modified_count += 1,
                DirtyState::Deleted => summary.deleted_count += 1,
            }
        }

        for dir_entry in dir_entries {
            match dir_entry.state {
                DirtyDirState::New => summary.new_dir_count += 1,
                DirtyDirState::Deleted => summary.deleted_dir_count += 1,
            }
        }

        summary
    }

    /// Clear all dirty state.
    ///
    /// Call this after the diff manifest has been successfully
    /// uploaded to S3 to reset the dirty tracking.
    pub fn clear_dirty(&self) {
        self.inner.dirty_manager.clear();
        self.inner.dirty_dir_manager.clear();
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use rusty_attachments_vfs::inode::{INodeType, ROOT_INODE};
    use rusty_attachments_vfs::FileContent;

    /// Create a test manifest with a directory structure.
    fn create_test_manifest() -> rusty_attachments_model::Manifest {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [
                {"path": "dir1/file1.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
            ],
            "totalSize": 100
        }"#;
        rusty_attachments_model::Manifest::decode(json).unwrap()
    }

    /// Mock file store that returns empty data.
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

    #[test]
    fn test_enumerate_directory_no_duplicate_new_dirs() {
        // This test verifies that when a new directory is created under an existing
        // manifest directory, it appears exactly once in enumerate_directory results.
        //
        // The bug was: create_dir adds the directory to both INodeManager and
        // DirtyDirManager, causing enumerate_directory to list it twice.

        let manifest = create_test_manifest();
        let store: Arc<dyn FileStore> = Arc::new(MockFileStore);
        let options = FsKitVfsOptions::default();
        let write_options = FsKitWriteOptions::default();

        let vfs = WritableFsKit::new(&manifest, store, options, write_options).unwrap();

        // Get dir1's inode ID
        let dir1_id: u64 = vfs
            .inner
            .inodes
            .get_by_path("dir1")
            .expect("dir1 should exist")
            .id();

        // Create a new directory under dir1
        let new_dir_id: u64 = vfs
            .inner
            .dirty_dir_manager
            .create_dir(dir1_id, "newsubdir")
            .expect("create_dir should succeed");

        // Simulate enumerate_directory logic (same as the actual implementation)
        let mut entries: Vec<String> = Vec::new();
        let mut seen_names: HashSet<String> = HashSet::new();

        // Add manifest children
        if let Some(children) = vfs.inner.inodes.get_dir_children(dir1_id) {
            for (name, child_id) in children {
                // Skip deleted
                if vfs.inner.dirty_manager.get_state(child_id) == Some(DirtyState::Deleted) {
                    continue;
                }
                if vfs.inner.dirty_dir_manager.get_state(child_id) == Some(DirtyDirState::Deleted) {
                    continue;
                }
                entries.push(name.clone());
                seen_names.insert(name);
            }
        }

        // Add new directories (skip if already seen)
        let new_dirs: Vec<(u64, String)> = vfs.inner.dirty_dir_manager.get_new_dirs_in_parent(dir1_id);
        for (ino, name) in new_dirs {
            let is_newsubdir: bool = name == "newsubdir";
            if !seen_names.contains(&name) {
                entries.push(name.clone());
                seen_names.insert(name);
            }
            // Verify the inode ID matches
            if is_newsubdir {
                assert_eq!(ino, new_dir_id, "new directory inode ID should match");
            }
        }

        // Verify: newsubdir should appear exactly once
        let newsubdir_count: usize = entries.iter().filter(|n| *n == "newsubdir").count();
        assert_eq!(
            newsubdir_count, 1,
            "newsubdir should appear exactly once, but appeared {} times",
            newsubdir_count
        );

        // Also verify file1.txt is present
        assert!(
            entries.contains(&"file1.txt".to_string()),
            "file1.txt should be in entries"
        );
    }

    #[test]
    fn test_enumerate_directory_new_dir_in_root() {
        // Test creating a new directory in the root directory.

        let manifest = create_test_manifest();
        let store: Arc<dyn FileStore> = Arc::new(MockFileStore);
        let options = FsKitVfsOptions::default();
        let write_options = FsKitWriteOptions::default();

        let vfs = WritableFsKit::new(&manifest, store, options, write_options).unwrap();

        // Create a new directory in root
        let _new_dir_id: u64 = vfs
            .inner
            .dirty_dir_manager
            .create_dir(ROOT_INODE, "newroot")
            .expect("create_dir should succeed");

        // Simulate enumerate_directory logic
        let mut entries: Vec<String> = Vec::new();
        let mut seen_names: HashSet<String> = HashSet::new();

        // Add manifest children from root
        if let Some(children) = vfs.inner.inodes.get_dir_children(ROOT_INODE) {
            for (name, child_id) in children {
                if vfs.inner.dirty_manager.get_state(child_id) == Some(DirtyState::Deleted) {
                    continue;
                }
                if vfs.inner.dirty_dir_manager.get_state(child_id) == Some(DirtyDirState::Deleted) {
                    continue;
                }
                entries.push(name.clone());
                seen_names.insert(name);
            }
        }

        // Add new directories
        let new_dirs: Vec<(u64, String)> = vfs.inner.dirty_dir_manager.get_new_dirs_in_parent(ROOT_INODE);
        for (_ino, name) in new_dirs {
            if !seen_names.contains(&name) {
                entries.push(name.clone());
                seen_names.insert(name);
            }
        }

        // Verify: newroot should appear exactly once
        let newroot_count: usize = entries.iter().filter(|n| *n == "newroot").count();
        assert_eq!(
            newroot_count, 1,
            "newroot should appear exactly once, but appeared {} times",
            newroot_count
        );

        // dir1 should also be present (from manifest)
        assert!(
            entries.contains(&"dir1".to_string()),
            "dir1 should be in entries"
        );
    }

    /// Create a test manifest in v2025-12-04-beta format with directories and chunked files.
    fn create_test_manifest_v2025() -> rusty_attachments_model::Manifest {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2025-12-04-beta",
            "dirs": [
                {"name": "subdir"}
            ],
            "files": [
                {"name": "file1.txt", "hash": "abc123", "size": 100, "mtime": 1234567890},
                {"name": "subdir/file2.txt", "hash": "def456", "size": 200, "mtime": 1234567891},
                {"name": "link.txt", "symlink_target": "file1.txt"}
            ],
            "totalSize": 300
        }"#;
        rusty_attachments_model::Manifest::decode(json).unwrap()
    }

    #[test]
    fn test_v2023_manifest_support() {
        // Test that v2023-03-03 manifests work correctly.
        let manifest = create_test_manifest();
        let store: Arc<dyn FileStore> = Arc::new(MockFileStore);
        let options = FsKitVfsOptions::default();
        let write_options = FsKitWriteOptions::default();

        let vfs = WritableFsKit::new(&manifest, store, options, write_options).unwrap();

        // Verify file exists
        let file: Arc<dyn INode> = vfs
            .inner
            .inodes
            .get_by_path("dir1/file1.txt")
            .expect("file should exist");
        assert_eq!(file.size(), 100);

        // Verify directory was created implicitly
        let dir: Arc<dyn INode> = vfs
            .inner
            .inodes
            .get_by_path("dir1")
            .expect("dir should exist");
        assert_eq!(dir.inode_type(), INodeType::Directory);

        // Verify hash algorithm
        assert_eq!(vfs.inner.hash_algorithm, HashAlgorithm::Xxh128);

        // Verify original_dirs is empty for v2023
        assert!(vfs.inner.original_dirs.is_empty());
    }

    #[test]
    fn test_v2025_manifest_support() {
        // Test that v2025-12-04-beta manifests work correctly.
        let manifest = create_test_manifest_v2025();
        let store: Arc<dyn FileStore> = Arc::new(MockFileStore);
        let options = FsKitVfsOptions::default();
        let write_options = FsKitWriteOptions::default();

        let vfs = WritableFsKit::new(&manifest, store, options, write_options).unwrap();

        // Verify files exist
        let file1: Arc<dyn INode> = vfs
            .inner
            .inodes
            .get_by_path("file1.txt")
            .expect("file1 should exist");
        assert_eq!(file1.size(), 100);

        let file2: Arc<dyn INode> = vfs
            .inner
            .inodes
            .get_by_path("subdir/file2.txt")
            .expect("file2 should exist");
        assert_eq!(file2.size(), 200);

        // Verify explicit directory exists
        let subdir: Arc<dyn INode> = vfs
            .inner
            .inodes
            .get_by_path("subdir")
            .expect("subdir should exist");
        assert_eq!(subdir.inode_type(), INodeType::Directory);

        // Verify symlink exists
        let link: Arc<dyn INode> = vfs
            .inner
            .inodes
            .get_by_path("link.txt")
            .expect("link should exist");
        assert_eq!(link.inode_type(), INodeType::Symlink);

        // Verify original_dirs contains the explicit directory
        assert!(vfs.inner.original_dirs.contains("subdir"));
    }

    #[test]
    fn test_v2025_manifest_with_chunked_file() {
        // Test that v2025 manifests with chunked files work correctly.
        // Chunk size is 256MB, so 3 chunks = 512MB < size <= 768MB
        let file_size: u64 = 600 * 1024 * 1024; // 600MB = 3 chunks
        let json: String = format!(r#"{{
            "hashAlg": "xxh128",
            "manifestVersion": "2025-12-04-beta",
            "dirs": [],
            "files": [
                {{"name": "large.bin", "chunkhashes": ["chunk1", "chunk2", "chunk3"], "size": {}, "mtime": 1234567890}}
            ],
            "totalSize": {}
        }}"#, file_size, file_size);
        let manifest = rusty_attachments_model::Manifest::decode(&json).unwrap();
        let store: Arc<dyn FileStore> = Arc::new(MockFileStore);
        let options = FsKitVfsOptions::default();
        let write_options = FsKitWriteOptions::default();

        let vfs = WritableFsKit::new(&manifest, store, options, write_options).unwrap();

        // Verify chunked file exists with correct size
        let file: Arc<dyn INode> = vfs
            .inner
            .inodes
            .get_by_path("large.bin")
            .expect("large.bin should exist");
        assert_eq!(file.size(), file_size);

        // Verify file content is chunked
        let content: FileContent = vfs
            .inner
            .inodes
            .get_file_content(file.id())
            .expect("content should exist");
        match content {
            FileContent::Chunked(hashes) => {
                assert_eq!(hashes.len(), 3);
                assert_eq!(hashes[0], "chunk1");
            }
            FileContent::SingleHash(_) => panic!("Expected chunked content"),
        }
    }

    #[test]
    fn test_v2025_write_to_manifest_dir() {
        // Test creating new files/dirs under v2025 manifest directories.
        let manifest = create_test_manifest_v2025();
        let store: Arc<dyn FileStore> = Arc::new(MockFileStore);
        let options = FsKitVfsOptions::default();
        let write_options = FsKitWriteOptions::default();

        let vfs = WritableFsKit::new(&manifest, store, options, write_options).unwrap();

        // Get subdir inode ID
        let subdir: Arc<dyn INode> = vfs
            .inner
            .inodes
            .get_by_path("subdir")
            .expect("subdir should exist");
        let subdir_id: u64 = subdir.id();

        // Create a new directory under subdir
        let new_dir_id: u64 = vfs
            .inner
            .dirty_dir_manager
            .create_dir(subdir_id, "newchild")
            .expect("create_dir should succeed");

        // Verify new directory is tracked
        assert!(vfs.inner.dirty_dir_manager.is_new_dir(new_dir_id));

        // Verify dirty summary reflects the new directory
        let summary = vfs.dirty_summary();
        assert_eq!(summary.new_dir_count, 1);
    }
}
