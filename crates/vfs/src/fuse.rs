//! FUSE filesystem implementation.

#[cfg(feature = "fuse")]
mod impl_fuse {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, RwLock};
    use std::time::{Duration, Instant, UNIX_EPOCH};

    use fuser::{
        FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
        ReplyEntry, ReplyOpen, Request,
    };
    use rusty_attachments_model::{HashAlgorithm, Manifest};
    use tokio::runtime::Handle;

    use crate::builder::build_from_manifest;
    use crate::content::FileStore;
    use crate::inode::{FileContent, INode, INodeManager, INodeType};
    use crate::memory_pool::{BlockKey, MemoryPool, MemoryPoolError, MemoryPoolStats};
    use crate::options::VfsOptions;
    use crate::VfsError;

    use rusty_attachments_common::CHUNK_SIZE_V2;

    /// Information about an open file handle.
    #[derive(Debug, Clone)]
    pub struct OpenFileInfo {
        /// Inode ID of the open file.
        pub inode: u64,
        /// File path.
        pub path: String,
        /// File size in bytes.
        pub size: u64,
        /// File handle ID.
        pub handle_id: u64,
    }

    /// Statistics snapshot from the VFS.
    #[derive(Debug, Clone)]
    pub struct VfsStats {
        /// Number of inodes in the filesystem.
        pub inode_count: usize,
        /// Number of currently open file handles.
        pub open_files: usize,
        /// List of currently open files with details.
        pub open_file_list: Vec<OpenFileInfo>,
        /// Memory pool statistics.
        pub pool_stats: MemoryPoolStats,
        /// Total cache hits.
        pub cache_hits: u64,
        /// Total cache allocations (misses that required fetch).
        pub cache_allocations: u64,
        /// Cache hit rate percentage.
        pub cache_hit_rate: f64,
        /// Time since VFS was created.
        pub uptime_secs: u64,
    }

    struct OpenHandle {
        inode: u64,
        path: String,
        content: FileContent,
        size: u64,
    }

    /// Shared state for stats collection.
    pub struct VfsStatsCollector {
        pool: Arc<MemoryPool>,
        handles: Arc<RwLock<HashMap<u64, OpenHandle>>>,
        inode_count: usize,
        start_time: Instant,
    }

    impl VfsStatsCollector {
        /// Collect current VFS statistics.
        pub fn collect(&self) -> VfsStats {
            let handles_guard = self.handles.read().unwrap();
            let open_file_list: Vec<OpenFileInfo> = handles_guard
                .iter()
                .map(|(&handle_id, h)| OpenFileInfo {
                    inode: h.inode,
                    path: h.path.clone(),
                    size: h.size,
                    handle_id,
                })
                .collect();
            let open_files: usize = open_file_list.len();
            drop(handles_guard);

            let pool_stats: MemoryPoolStats = self.pool.stats();
            let cache_hits: u64 = self.pool.hit_count();
            let cache_allocations: u64 = self.pool.allocation_count();
            let cache_hit_rate: f64 = self.pool.hit_rate();
            let uptime_secs: u64 = self.start_time.elapsed().as_secs();

            VfsStats {
                inode_count: self.inode_count,
                open_files,
                open_file_list,
                pool_stats,
                cache_hits,
                cache_allocations,
                cache_hit_rate,
                uptime_secs,
            }
        }
    }

    pub struct DeadlineVfs {
        inodes: INodeManager,
        store: Arc<dyn FileStore>,
        pool: Arc<MemoryPool>,
        hash_algorithm: HashAlgorithm,
        handles: Arc<RwLock<HashMap<u64, OpenHandle>>>,
        next_handle: AtomicU64,
        options: VfsOptions,
        runtime: Handle,
        start_time: Instant,
    }

    impl DeadlineVfs {
        /// Create a new VFS from a manifest.
        pub fn new(manifest: &Manifest, store: Arc<dyn FileStore>, options: VfsOptions) -> Result<Self, VfsError> {
            let inodes = build_from_manifest(manifest);
            let hash_algorithm = manifest.hash_alg();
            let pool = Arc::new(MemoryPool::new(options.pool.clone()));
            let runtime = Handle::try_current().map_err(|e| VfsError::MountFailed(format!("No tokio runtime: {}", e)))?;
            Ok(Self {
                inodes,
                store,
                pool,
                hash_algorithm,
                handles: Arc::new(RwLock::new(HashMap::new())),
                next_handle: AtomicU64::new(1),
                options,
                runtime,
                start_time: Instant::now(),
            })
        }

        /// Get a stats collector that can be used from another thread.
        pub fn stats_collector(&self) -> VfsStatsCollector {
            VfsStatsCollector {
                pool: self.pool.clone(),
                handles: self.handles.clone(),
                inode_count: self.inodes.inode_count(),
                start_time: self.start_time,
            }
        }

        /// Get current VFS statistics.
        pub fn stats(&self) -> VfsStats {
            self.stats_collector().collect()
        }

        fn to_file_attr(&self, inode: &dyn INode) -> FileAttr {
            let kind = match inode.inode_type() { INodeType::File => FileType::RegularFile, INodeType::Directory => FileType::Directory, INodeType::Symlink => FileType::Symlink };
            let size = inode.size();
            let mtime = inode.mtime();
            FileAttr { ino: inode.id(), size, blocks: (size + 511) / 512, atime: mtime, mtime, ctime: mtime, crtime: UNIX_EPOCH, kind, perm: inode.permissions(), nlink: if kind == FileType::Directory { 2 } else { 1 }, uid: unsafe { libc::getuid() }, gid: unsafe { libc::getgid() }, rdev: 0, blksize: 512, flags: 0 }
        }

        fn ttl(&self) -> Duration { Duration::from_secs(self.options.kernel_cache.attr_timeout_secs) }

        fn read_file_data(&self, content: &FileContent, file_size: u64, offset: i64, size: u32) -> Result<Vec<u8>, VfsError> {
            let off = offset as u64;
            if off >= file_size { return Ok(Vec::new()); }
            let actual = (size as u64).min(file_size - off);
            match content { FileContent::SingleHash(hash) => self.read_single_hash(hash, off, actual), FileContent::Chunked(hashes) => self.read_chunked(hashes, off, actual) }
        }

        fn read_single_hash(&self, hash: &str, offset: u64, size: u64) -> Result<Vec<u8>, VfsError> {
            let key = BlockKey::single(hash);
            let h = hash.to_string();
            let s = self.store.clone();
            let alg = self.hash_algorithm;
            let p = self.pool.clone();
            let handle = self.runtime.block_on(async move { p.acquire(&key, move || { let store = s; let hash = h; async move { store.retrieve(&hash, alg).await.map_err(|e| MemoryPoolError::RetrievalFailed(e.to_string())) } }).await });
            match handle { Ok(blk) => { let data = blk.data(); let start = offset as usize; let end = (offset + size).min(data.len() as u64) as usize; Ok(data[start..end].to_vec()) } Err(e) => Err(VfsError::ContentRetrievalFailed { hash: hash.to_string(), source: e.to_string().into() }) }
        }

        fn read_chunked(&self, hashes: &[String], offset: u64, size: u64) -> Result<Vec<u8>, VfsError> {
            let chunk_size = CHUNK_SIZE_V2;
            let start_chunk = (offset / chunk_size) as usize;
            let end_off = offset + size;
            let end_chunk = ((end_off.saturating_sub(1)) / chunk_size) as usize;
            let mut result = Vec::with_capacity(size as usize);
            for idx in start_chunk..=end_chunk {
                if idx >= hashes.len() { break; }
                let hash = hashes[idx].clone();
                let key = BlockKey::new(&hash, idx as u32);
                let h = hash.clone();
                let s = self.store.clone();
                let alg = self.hash_algorithm;
                let p = self.pool.clone();
                let handle = self.runtime.block_on(async move { p.acquire(&key, move || { let store = s; let hash = h; async move { store.retrieve(&hash, alg).await.map_err(|e| MemoryPoolError::RetrievalFailed(e.to_string())) } }).await });
                match handle { Ok(blk) => { let data = blk.data(); let chunk_start = idx as u64 * chunk_size; let rs = if idx == start_chunk { (offset - chunk_start) as usize } else { 0 }; let re = if idx == end_chunk { ((end_off - chunk_start) as usize).min(data.len()) } else { data.len() }; if rs < data.len() { result.extend_from_slice(&data[rs..re]); } } Err(e) => return Err(VfsError::ContentRetrievalFailed { hash, source: e.to_string().into() }) }
            }
            Ok(result)
        }
    }

    impl Filesystem for DeadlineVfs {
        fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
            let name_str = match name.to_str() { Some(n) => n, None => { reply.error(libc::ENOENT); return; } };
            let parent_inode = match self.inodes.get(parent) { Some(i) => i, None => { reply.error(libc::ENOENT); return; } };
            if parent_inode.inode_type() != INodeType::Directory { reply.error(libc::ENOTDIR); return; }
            let path = if parent_inode.path().is_empty() { name_str.to_string() } else { format!("{}/{}", parent_inode.path(), name_str) };
            match self.inodes.get_by_path(&path) { Some(child) => reply.entry(&self.ttl(), &self.to_file_attr(child.as_ref()), 0), None => reply.error(libc::ENOENT) }
        }
        fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) { match self.inodes.get(ino) { Some(inode) => reply.attr(&self.ttl(), &self.to_file_attr(inode.as_ref())), None => reply.error(libc::ENOENT) } }
        fn readdir(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
            let inode = match self.inodes.get(ino) { Some(i) => i, None => { reply.error(libc::ENOENT); return; } };
            if inode.inode_type() != INodeType::Directory { reply.error(libc::ENOTDIR); return; }
            let mut entries = vec![(ino, FileType::Directory, ".".to_string()), (inode.parent_id(), FileType::Directory, "..".to_string())];
            if let Some(children) = self.inodes.get_dir_children(ino) { for (name, cid) in children { if let Some(c) = self.inodes.get(cid) { let k = match c.inode_type() { INodeType::File => FileType::RegularFile, INodeType::Directory => FileType::Directory, INodeType::Symlink => FileType::Symlink }; entries.push((cid, k, name)); } } }
            for (i, (e_ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) { if reply.add(*e_ino, (i + 1) as i64, *kind, name) { break; } }
            reply.ok();
        }
        fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
            let inode = match self.inodes.get(ino) { Some(i) => i, None => { reply.error(libc::ENOENT); return; } };
            if inode.inode_type() != INodeType::File { reply.error(libc::EISDIR); return; }
            if flags & libc::O_WRONLY != 0 || flags & libc::O_RDWR != 0 { reply.error(libc::EROFS); return; }
            let content = match self.inodes.get_file_content(ino) { Some(c) => c, None => { reply.error(libc::EIO); return; } };
            let fh = self.next_handle.fetch_add(1, Ordering::SeqCst);
            let path = inode.path().to_string();
            let size = inode.size();
            self.handles.write().unwrap().insert(fh, OpenHandle { inode: ino, path, content, size });
            reply.opened(fh, 0);
        }
        fn read(&mut self, _req: &Request, ino: u64, fh: u64, offset: i64, size: u32, _flags: i32, _lock: Option<u64>, reply: ReplyData) {
            let (content, file_size) = { let handles = self.handles.read().unwrap(); match handles.get(&fh) { Some(h) if h.inode == ino => (h.content.clone(), h.size), _ => { reply.error(libc::EBADF); return; } } };
            match self.read_file_data(&content, file_size, offset, size) { Ok(data) => reply.data(&data), Err(_) => reply.error(libc::EIO) }
        }
        fn release(&mut self, _req: &Request, _ino: u64, fh: u64, _flags: i32, _lock: Option<u64>, _flush: bool, reply: ReplyEmpty) { self.handles.write().unwrap().remove(&fh); reply.ok(); }
        fn readlink(&mut self, _req: &Request, ino: u64, reply: ReplyData) { match self.inodes.get_symlink_target(ino) { Some(t) => reply.data(t.as_bytes()), None => reply.error(libc::EINVAL) } }
    }

    pub fn mount(vfs: DeadlineVfs, mountpoint: &std::path::Path) -> Result<(), VfsError> { use fuser::MountOption; fuser::mount2(vfs, mountpoint, &[MountOption::RO, MountOption::FSName("deadline-vfs".into()), MountOption::AutoUnmount]).map_err(|e| VfsError::MountFailed(e.to_string())) }
    pub fn spawn_mount(vfs: DeadlineVfs, mountpoint: &std::path::Path) -> Result<fuser::BackgroundSession, VfsError> { use fuser::MountOption; fuser::spawn_mount2(vfs, mountpoint, &[MountOption::RO, MountOption::FSName("deadline-vfs".into()), MountOption::AutoUnmount]).map_err(|e| VfsError::MountFailed(e.to_string())) }
}

#[cfg(feature = "fuse")]
pub use impl_fuse::{mount, spawn_mount, DeadlineVfs, OpenFileInfo, VfsStats, VfsStatsCollector};
