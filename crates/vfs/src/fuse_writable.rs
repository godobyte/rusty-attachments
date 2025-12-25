//! Writable FUSE filesystem implementation.
//!
//! Extends the read-only DeadlineVfs with copy-on-write support.

#[cfg(feature = "fuse")]
mod impl_fuse {
    use std::collections::HashSet;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use async_trait::async_trait;
    use fuser::{
        FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
        ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request,
    };
    use rusty_attachments_model::Manifest;
    use tokio::runtime::Handle;

    use crate::builder::build_from_manifest;
    use crate::content::FileStore;
    use crate::inode::{INode, INodeManager, INodeType};
    use crate::memory_pool::MemoryPool;
    use crate::options::VfsOptions;
    use crate::write::{
        DiffManifestExporter, DirtyFileManager, DirtyState, DirtySummary, MaterializedCache,
        WriteCache, WritableVfsStatsCollector,
    };
    use crate::VfsError;

    /// Default TTL for FUSE attributes.
    const TTL: Duration = Duration::from_secs(1);

    /// Configuration for write behavior.
    #[derive(Debug, Clone)]
    pub struct WriteOptions {
        /// Directory for materialized cache.
        pub cache_dir: PathBuf,
        /// Whether to sync to disk on every write (slower but safer).
        pub sync_on_write: bool,
        /// Maximum dirty file size before forcing flush.
        pub max_dirty_size: u64,
    }

    impl Default for WriteOptions {
        fn default() -> Self {
            Self {
                cache_dir: PathBuf::from("/tmp/vfs-cache"),
                sync_on_write: true,
                max_dirty_size: 1024 * 1024 * 1024, // 1GB
            }
        }
    }

    /// Writable VFS with copy-on-write support.
    ///
    /// Extends the read-only DeadlineVfs with write operations.
    /// Modified files are stored both in memory (fast access) and
    /// on disk (persistence).
    pub struct WritableVfs {
        /// Inode manager.
        inodes: Arc<INodeManager>,
        /// Read-only file store.
        store: Arc<dyn FileStore>,
        /// Memory pool for caching read data.
        pool: Arc<MemoryPool>,
        /// Dirty file manager (COW layer).
        dirty_manager: Arc<DirtyFileManager>,
        /// Original directories from manifest (for diff tracking).
        #[allow(dead_code)]
        original_dirs: HashSet<String>,
        /// Options for VFS behavior.
        options: VfsOptions,
        /// Options for write behavior.
        #[allow(dead_code)]
        write_options: WriteOptions,
        /// Tokio runtime handle.
        runtime: Handle,
        /// Next file handle ID.
        next_handle: AtomicU64,
        /// VFS creation time for uptime tracking.
        start_time: Instant,
    }

    impl WritableVfs {
        /// Create a new writable VFS.
        ///
        /// # Arguments
        /// * `manifest` - Manifest to mount
        /// * `store` - File store for reading original content
        /// * `options` - VFS options
        /// * `write_options` - Write-specific options
        pub fn new(
            manifest: &Manifest,
            store: Arc<dyn FileStore>,
            options: VfsOptions,
            write_options: WriteOptions,
        ) -> Result<Self, VfsError> {
            let inodes: Arc<INodeManager> = Arc::new(build_from_manifest(manifest));

            let pool = Arc::new(MemoryPool::new(options.pool.clone()));

            let cache: Arc<dyn WriteCache> = Arc::new(
                MaterializedCache::new(write_options.cache_dir.clone())
                    .map_err(|e| VfsError::WriteCacheError(e.to_string()))?,
            );

            let dirty_manager = Arc::new(DirtyFileManager::new(
                cache,
                store.clone(),
                inodes.clone(),
            ));

            // Collect original directories for diff tracking
            let original_dirs: HashSet<String> = match manifest {
                Manifest::V2025_12_04_beta(m) => {
                    m.dirs.iter().map(|d| d.name.clone()).collect()
                }
                Manifest::V2023_03_03(_) => HashSet::new(),
            };

            let runtime: Handle = Handle::try_current()
                .map_err(|e| VfsError::MountFailed(format!("No tokio runtime: {}", e)))?;

            Ok(Self {
                inodes,
                store,
                pool,
                dirty_manager,
                original_dirs,
                options,
                write_options,
                runtime,
                next_handle: AtomicU64::new(1),
                start_time: Instant::now(),
            })
        }

        /// Get the dirty file manager.
        pub fn dirty_manager(&self) -> &Arc<DirtyFileManager> {
            &self.dirty_manager
        }

        /// Get a stats collector for this writable VFS.
        ///
        /// # Returns
        /// Collector that can be cloned and used from another thread to query stats.
        pub fn stats_collector(&self) -> WritableVfsStatsCollector {
            WritableVfsStatsCollector::new(
                self.pool.clone(),
                self.dirty_manager.clone(),
                self.inodes.inode_count(),
                self.start_time,
            )
        }

        /// Convert inode to FUSE file attributes.
        ///
        /// # Arguments
        /// * `inode` - Inode to convert
        fn to_file_attr(&self, inode: &dyn INode) -> FileAttr {
            let kind: FileType = match inode.inode_type() {
                INodeType::File => FileType::RegularFile,
                INodeType::Directory => FileType::Directory,
                INodeType::Symlink => FileType::Symlink,
            };

            let size: u64 = inode.size();
            let mtime: SystemTime = inode.mtime();

            FileAttr {
                ino: inode.id(),
                size,
                blocks: (size + 511) / 512,
                atime: mtime,
                mtime,
                ctime: mtime,
                crtime: UNIX_EPOCH,
                kind,
                perm: inode.permissions(),
                nlink: if kind == FileType::Directory { 2 } else { 1 },
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
                rdev: 0,
                blksize: 512,
                flags: 0,
            }
        }

        /// Get TTL for attributes.
        fn ttl(&self) -> Duration {
            Duration::from_secs(self.options.kernel_cache.attr_timeout_secs)
        }
    }


    impl Filesystem for WritableVfs {
        fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
            let name_str: &str = match name.to_str() {
                Some(n) => n,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            let parent_inode: Arc<dyn INode> = match self.inodes.get(parent) {
                Some(i) => i,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            if parent_inode.inode_type() != INodeType::Directory {
                reply.error(libc::ENOTDIR);
                return;
            }

            let path: String = if parent_inode.path().is_empty() {
                name_str.to_string()
            } else {
                format!("{}/{}", parent_inode.path(), name_str)
            };

            match self.inodes.get_by_path(&path) {
                Some(child) => {
                    let mut attr: FileAttr = self.to_file_attr(child.as_ref());

                    // Check if dirty and update attributes
                    if let Some(size) = self.dirty_manager.get_size(child.id()) {
                        attr.size = size;
                    }
                    if let Some(mtime) = self.dirty_manager.get_mtime(child.id()) {
                        attr.mtime = mtime;
                        attr.atime = mtime;
                    }

                    reply.entry(&self.ttl(), &attr, 0);
                }
                None => reply.error(libc::ENOENT),
            }
        }

        fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
            match self.inodes.get(ino) {
                Some(inode) => {
                    let mut attr: FileAttr = self.to_file_attr(inode.as_ref());

                    // Check if dirty and update attributes
                    if let Some(size) = self.dirty_manager.get_size(ino) {
                        attr.size = size;
                    }
                    if let Some(mtime) = self.dirty_manager.get_mtime(ino) {
                        attr.mtime = mtime;
                        attr.atime = mtime;
                    }

                    reply.attr(&self.ttl(), &attr);
                }
                None => reply.error(libc::ENOENT),
            }
        }

        fn readdir(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: i64,
            mut reply: ReplyDirectory,
        ) {
            let inode: Arc<dyn INode> = match self.inodes.get(ino) {
                Some(i) => i,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            if inode.inode_type() != INodeType::Directory {
                reply.error(libc::ENOTDIR);
                return;
            }

            let mut entries: Vec<(u64, FileType, String)> = vec![
                (ino, FileType::Directory, ".".to_string()),
                (inode.parent_id(), FileType::Directory, "..".to_string()),
            ];

            if let Some(children) = self.inodes.get_dir_children(ino) {
                for (name, cid) in children {
                    if let Some(c) = self.inodes.get(cid) {
                        // Skip deleted files
                        if self.dirty_manager.get_state(cid) == Some(DirtyState::Deleted) {
                            continue;
                        }

                        let k: FileType = match c.inode_type() {
                            INodeType::File => FileType::RegularFile,
                            INodeType::Directory => FileType::Directory,
                            INodeType::Symlink => FileType::Symlink,
                        };
                        entries.push((cid, k, name));
                    }
                }
            }

            for (i, (e_ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
                if reply.add(*e_ino, (i + 1) as i64, *kind, name) {
                    break;
                }
            }
            reply.ok();
        }

        fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
            let inode: Arc<dyn INode> = match self.inodes.get(ino) {
                Some(i) => i,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            if inode.inode_type() != INodeType::File {
                reply.error(libc::EISDIR);
                return;
            }

            // Check if deleted
            if self.dirty_manager.get_state(ino) == Some(DirtyState::Deleted) {
                reply.error(libc::ENOENT);
                return;
            }

            let fh: u64 = self.next_handle.fetch_add(1, Ordering::SeqCst);
            reply.opened(fh, 0);
        }

        fn read(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: i64,
            size: u32,
            _flags: i32,
            _lock: Option<u64>,
            reply: ReplyData,
        ) {
            // Check dirty layer first
            if self.dirty_manager.is_dirty(ino) {
                let rt: Handle = self.runtime.clone();
                let dm: Arc<DirtyFileManager> = self.dirty_manager.clone();

                match rt.block_on(dm.read(ino, offset as u64, size)) {
                    Ok(data) => {
                        reply.data(&data);
                        return;
                    }
                    Err(e) => {
                        tracing::error!("Read from dirty file failed: {}", e);
                        reply.error(libc::EIO);
                        return;
                    }
                }
            }

            // Fall back to read-only store
            let inode: Arc<dyn INode> = match self.inodes.get(ino) {
                Some(i) => i,
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            let file_content = match self.inodes.get_file_content(ino) {
                Some(c) => c,
                None => {
                    reply.error(libc::EIO);
                    return;
                }
            };

            let file_size: u64 = inode.size();
            let hash_alg = inode.hash_algorithm().unwrap_or(rusty_attachments_model::HashAlgorithm::Xxh128);

            // Read from store
            let rt: Handle = self.runtime.clone();
            let store: Arc<dyn FileStore> = self.store.clone();

            let result = rt.block_on(async {
                match &file_content {
                    crate::inode::FileContent::SingleHash(hash) => {
                        store.retrieve(hash, hash_alg).await
                    }
                    crate::inode::FileContent::Chunked(hashes) => {
                        // Read relevant chunks
                        let chunk_size: u64 = rusty_attachments_common::CHUNK_SIZE_V2;
                        let start_chunk: usize = (offset as u64 / chunk_size) as usize;
                        let end_offset: u64 = (offset as u64 + size as u64).min(file_size);
                        let end_chunk: usize = ((end_offset.saturating_sub(1)) / chunk_size) as usize;

                        let mut data: Vec<u8> = Vec::new();
                        for idx in start_chunk..=end_chunk {
                            if idx >= hashes.len() {
                                break;
                            }
                            let chunk: Vec<u8> = store.retrieve(&hashes[idx], hash_alg).await?;
                            data.extend(chunk);
                        }
                        Ok(data)
                    }
                }
            });

            match result {
                Ok(data) => {
                    let off: usize = offset as usize;
                    let end: usize = (off + size as usize).min(data.len());
                    if off < data.len() {
                        reply.data(&data[off..end]);
                    } else {
                        reply.data(&[]);
                    }
                }
                Err(e) => {
                    tracing::error!("Read failed: {}", e);
                    reply.error(libc::EIO);
                }
            }
        }

        fn write(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: i64,
            data: &[u8],
            _write_flags: u32,
            _flags: i32,
            _lock: Option<u64>,
            reply: ReplyWrite,
        ) {
            let rt: Handle = self.runtime.clone();
            let dm: Arc<DirtyFileManager> = self.dirty_manager.clone();

            match rt.block_on(dm.write(ino, offset as u64, data)) {
                Ok(written) => reply.written(written as u32),
                Err(e) => {
                    tracing::error!("Write failed for inode {}: {}", ino, e);
                    reply.error(libc::EIO);
                }
            }
        }

        fn setattr(
            &mut self,
            _req: &Request,
            ino: u64,
            _mode: Option<u32>,
            _uid: Option<u32>,
            _gid: Option<u32>,
            size: Option<u64>,
            _atime: Option<fuser::TimeOrNow>,
            _mtime: Option<fuser::TimeOrNow>,
            _ctime: Option<SystemTime>,
            _fh: Option<u64>,
            _crtime: Option<SystemTime>,
            _chgtime: Option<SystemTime>,
            _bkuptime: Option<SystemTime>,
            _flags: Option<u32>,
            reply: ReplyAttr,
        ) {
            // Handle truncate (size change)
            if let Some(new_size) = size {
                let rt: Handle = self.runtime.clone();
                let dm: Arc<DirtyFileManager> = self.dirty_manager.clone();

                if let Err(e) = rt.block_on(dm.truncate(ino, new_size)) {
                    tracing::error!("Truncate failed: {}", e);
                    reply.error(libc::EIO);
                    return;
                }
            }

            // Return updated attributes
            if let Some(inode) = self.inodes.get(ino) {
                let mut attr: FileAttr = self.to_file_attr(inode.as_ref());

                // Override with dirty state if applicable
                if let Some(dirty_size) = self.dirty_manager.get_size(ino) {
                    attr.size = dirty_size;
                }
                if let Some(mtime) = self.dirty_manager.get_mtime(ino) {
                    attr.mtime = mtime;
                    attr.atime = mtime;
                }

                reply.attr(&TTL, &attr);
            } else {
                reply.error(libc::ENOENT);
            }
        }

        fn create(
            &mut self,
            req: &Request,
            parent: u64,
            name: &OsStr,
            mode: u32,
            _umask: u32,
            _flags: i32,
            reply: ReplyCreate,
        ) {
            let name_str: &str = match name.to_str() {
                Some(s) => s,
                None => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };

            // Get parent path
            let parent_path: String = match self.inodes.get(parent) {
                Some(inode) => inode.path().to_string(),
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            // Build new file path
            let new_path: String = if parent_path.is_empty() {
                name_str.to_string()
            } else {
                format!("{}/{}", parent_path, name_str)
            };

            // Check if file already exists
            if self.inodes.get_by_path(&new_path).is_some() {
                reply.error(libc::EEXIST);
                return;
            }

            // Allocate new inode ID (use a high number to avoid conflicts)
            let new_ino: u64 = 0x8000_0000 + self.next_handle.fetch_add(1, Ordering::SeqCst);

            // Create dirty file entry
            if let Err(e) = self.dirty_manager.create_file(new_ino, new_path.clone()) {
                tracing::error!("Failed to create file: {}", e);
                reply.error(libc::EIO);
                return;
            }

            // Build file attributes
            let attr = FileAttr {
                ino: new_ino,
                size: 0,
                blocks: 0,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: FileType::RegularFile,
                perm: (mode & 0o7777) as u16,
                nlink: 1,
                uid: req.uid(),
                gid: req.gid(),
                rdev: 0,
                blksize: 512,
                flags: 0,
            };

            // Create file handle
            let fh: u64 = self.next_handle.fetch_add(1, Ordering::SeqCst);

            reply.created(&TTL, &attr, 0, fh, 0);
        }

        fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
            let name_str: &str = match name.to_str() {
                Some(s) => s,
                None => {
                    reply.error(libc::EINVAL);
                    return;
                }
            };

            // Get parent path
            let parent_path: String = match self.inodes.get(parent) {
                Some(inode) => inode.path().to_string(),
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            // Build file path
            let file_path: String = if parent_path.is_empty() {
                name_str.to_string()
            } else {
                format!("{}/{}", parent_path, name_str)
            };

            // Find inode
            let ino: u64 = match self.inodes.get_by_path(&file_path) {
                Some(inode) => inode.id(),
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            // Mark as deleted
            let rt: Handle = self.runtime.clone();
            let dm: Arc<DirtyFileManager> = self.dirty_manager.clone();

            if let Err(e) = rt.block_on(dm.delete_file(ino)) {
                tracing::error!("Failed to delete file: {}", e);
                reply.error(libc::EIO);
                return;
            }

            reply.ok();
        }

        fn fsync(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            _datasync: bool,
            reply: ReplyEmpty,
        ) {
            // Flush dirty file to disk
            let rt: Handle = self.runtime.clone();
            let dm: Arc<DirtyFileManager> = self.dirty_manager.clone();

            if let Err(e) = rt.block_on(dm.flush_to_disk(ino)) {
                tracing::error!("fsync failed: {}", e);
                reply.error(libc::EIO);
                return;
            }

            reply.ok();
        }

        fn release(
            &mut self,
            _req: &Request,
            _ino: u64,
            _fh: u64,
            _flags: i32,
            _lock: Option<u64>,
            _flush: bool,
            reply: ReplyEmpty,
        ) {
            reply.ok();
        }

        fn readlink(&mut self, _req: &Request, ino: u64, reply: ReplyData) {
            match self.inodes.get_symlink_target(ino) {
                Some(t) => reply.data(t.as_bytes()),
                None => reply.error(libc::EINVAL),
            }
        }
    }


    #[async_trait]
    impl DiffManifestExporter for WritableVfs {
        async fn export_diff_manifest(
            &self,
            _parent_manifest: &Manifest,
            _parent_encoded: &str,
        ) -> Result<Manifest, VfsError> {
            // TODO: Implement full diff manifest generation
            // This requires access to model types for creating manifest entries
            Err(VfsError::MountFailed(
                "Diff manifest export not yet implemented".to_string(),
            ))
        }

        fn clear_dirty(&self) -> Result<(), VfsError> {
            self.dirty_manager.clear();
            Ok(())
        }

        fn dirty_summary(&self) -> DirtySummary {
            let entries = self.dirty_manager.get_dirty_entries();
            let mut summary = DirtySummary::default();

            for entry in entries {
                match entry.state {
                    DirtyState::New => summary.new_count += 1,
                    DirtyState::Modified => summary.modified_count += 1,
                    DirtyState::Deleted => summary.deleted_count += 1,
                }
            }

            summary
        }
    }

    /// Mount a writable VFS.
    ///
    /// # Arguments
    /// * `vfs` - The writable VFS to mount
    /// * `mountpoint` - Path to mount at
    pub fn mount_writable(
        vfs: WritableVfs,
        mountpoint: &std::path::Path,
    ) -> Result<(), VfsError> {
        use fuser::MountOption;
        fuser::mount2(
            vfs,
            mountpoint,
            &[
                MountOption::FSName("deadline-vfs-rw".into()),
                MountOption::AutoUnmount,
            ],
        )
        .map_err(|e| VfsError::MountFailed(e.to_string()))
    }

    /// Spawn a writable VFS mount in the background.
    ///
    /// # Arguments
    /// * `vfs` - The writable VFS to mount
    /// * `mountpoint` - Path to mount at
    ///
    /// # Returns
    /// Background session handle.
    pub fn spawn_mount_writable(
        vfs: WritableVfs,
        mountpoint: &std::path::Path,
    ) -> Result<fuser::BackgroundSession, VfsError> {
        use fuser::MountOption;
        fuser::spawn_mount2(
            vfs,
            mountpoint,
            &[
                MountOption::FSName("deadline-vfs-rw".into()),
                MountOption::AutoUnmount,
            ],
        )
        .map_err(|e| VfsError::MountFailed(e.to_string()))
    }
}

#[cfg(feature = "fuse")]
pub use impl_fuse::{mount_writable, spawn_mount_writable, WritableVfs, WriteOptions};
