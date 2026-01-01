//! Dirty file tracking for copy-on-write VFS.
//!
//! This module provides the core data structures for tracking modified files
//! in the writable VFS layer.
//!
//! # Architecture
//!
//! The dirty file system uses a unified memory pool for content storage:
//! - `DirtyFileMetadata` - Tracks file state, size, and chunk info (no content)
//! - `MemoryPool` - Stores actual chunk data with LRU eviction
//!
//! This separation allows:
//! - Global memory limit enforcement across read and write caches
//! - Automatic eviction of dirty blocks after flush
//! - Invalidation of stale read-only blocks when files are modified

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use rusty_attachments_common::CHUNK_SIZE_V2;
use rusty_attachments_model::HashAlgorithm;

use crate::content::FileStore;
use crate::inode::{FileContent, INode, INodeId, INodeManager, INodeType};
use crate::memory_pool_v2::MemoryPool;
use crate::VfsError;

use crate::diskcache::WriteCache;

/// State of a dirty file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyState {
    /// File has been modified from original.
    Modified,
    /// File is newly created (not in original manifest).
    New,
    /// File has been deleted.
    Deleted,
}

/// Metadata about a dirty file (content stored in memory pool).
///
/// This struct tracks file state and chunk information without storing
/// the actual content. Content is stored in the unified `MemoryPool`.
#[derive(Debug)]
pub struct DirtyFileMetadata {
    /// Inode ID of this file.
    inode_id: INodeId,
    /// Relative path within the VFS.
    rel_path: String,
    /// Parent inode ID (only set for new files).
    parent_inode: Option<INodeId>,
    /// Original chunk hashes (for invalidation when modified).
    original_hashes: Vec<String>,
    /// Current state of the file.
    state: DirtyState,
    /// Modification time (updated on each write).
    mtime: SystemTime,
    /// Whether file is executable (from original or set via chmod).
    executable: bool,
    /// Current file size in bytes.
    size: u64,
    /// Chunk size (256MB for V2).
    chunk_size: u64,
    /// Number of chunks (1 for small files).
    chunk_count: u32,
    /// Which chunks have been modified (need flush).
    dirty_chunks: HashSet<u32>,
}

impl DirtyFileMetadata {
    /// Create metadata for a COW copy of an existing file.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the file
    /// * `rel_path` - Relative path within VFS
    /// * `file_content` - Original file content reference
    /// * `total_size` - Total file size
    /// * `executable` - Whether file is executable
    pub fn from_cow(
        inode_id: INodeId,
        rel_path: String,
        file_content: &FileContent,
        total_size: u64,
        executable: bool,
    ) -> Self {
        let (original_hashes, chunk_count): (Vec<String>, u32) = match file_content {
            FileContent::SingleHash(hash) => (vec![hash.clone()], 1),
            FileContent::Chunked(hashes) => {
                let count: u32 = hashes.len() as u32;
                (hashes.clone(), count)
            }
        };

        Self {
            inode_id,
            rel_path,
            parent_inode: None,
            original_hashes,
            state: DirtyState::Modified,
            mtime: SystemTime::now(),
            executable,
            size: total_size,
            chunk_size: CHUNK_SIZE_V2,
            chunk_count,
            dirty_chunks: HashSet::new(),
        }
    }

    /// Create metadata for a new file (not from COW).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the file
    /// * `rel_path` - Relative path within VFS
    /// * `parent_inode` - Parent directory inode ID
    pub fn new_file(inode_id: INodeId, rel_path: String, parent_inode: INodeId) -> Self {
        Self {
            inode_id,
            rel_path,
            parent_inode: Some(parent_inode),
            original_hashes: Vec::new(),
            state: DirtyState::New,
            mtime: SystemTime::now(),
            executable: false,
            size: 0,
            chunk_size: CHUNK_SIZE_V2,
            chunk_count: 1,
            dirty_chunks: HashSet::new(),
        }
    }

    /// Get the inode ID.
    pub fn inode_id(&self) -> INodeId {
        self.inode_id
    }

    /// Get relative path.
    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }

    /// Get parent inode ID (only set for new files).
    pub fn parent_inode(&self) -> Option<INodeId> {
        self.parent_inode
    }

    /// Get the file name (last component of path).
    pub fn file_name(&self) -> &str {
        self.rel_path
            .rsplit('/')
            .next()
            .unwrap_or(&self.rel_path)
    }

    /// Get current state.
    pub fn state(&self) -> DirtyState {
        self.state
    }

    /// Get modification time.
    pub fn mtime(&self) -> SystemTime {
        self.mtime
    }

    /// Get current file size.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Check if file is executable.
    pub fn is_executable(&self) -> bool {
        self.executable
    }

    /// Get original hashes for invalidation.
    pub fn original_hashes(&self) -> &[String] {
        &self.original_hashes
    }

    /// Get chunk size.
    pub fn chunk_size(&self) -> u64 {
        self.chunk_size
    }

    /// Get chunk count.
    pub fn chunk_count(&self) -> u32 {
        self.chunk_count
    }

    /// Check if a chunk is dirty.
    ///
    /// # Arguments
    /// * `chunk_index` - Index of the chunk to check
    pub fn is_chunk_dirty(&self, chunk_index: u32) -> bool {
        self.dirty_chunks.contains(&chunk_index)
    }

    /// Get all dirty chunk indices.
    pub fn dirty_chunks(&self) -> &HashSet<u32> {
        &self.dirty_chunks
    }

    /// Mark a chunk as dirty.
    ///
    /// # Arguments
    /// * `chunk_index` - Index of the chunk to mark
    pub fn mark_chunk_dirty(&mut self, chunk_index: u32) {
        self.dirty_chunks.insert(chunk_index);
    }

    /// Clear dirty flag for a chunk (after flush).
    ///
    /// # Arguments
    /// * `chunk_index` - Index of the chunk to mark as flushed
    pub fn mark_chunk_flushed(&mut self, chunk_index: u32) {
        self.dirty_chunks.remove(&chunk_index);
    }

    /// Clear all dirty flags (after full flush).
    pub fn mark_all_flushed(&mut self) {
        self.dirty_chunks.clear();
    }

    /// Update modification time.
    pub fn touch(&mut self) {
        self.mtime = SystemTime::now();
    }

    /// Update file size.
    ///
    /// # Arguments
    /// * `size` - New file size in bytes
    pub fn set_size(&mut self, size: u64) {
        self.size = size;
        // Recalculate chunk count
        self.chunk_count = if size == 0 {
            1
        } else {
            ((size - 1) / self.chunk_size + 1) as u32
        };
    }

    /// Mark file as deleted.
    pub fn mark_deleted(&mut self) {
        self.state = DirtyState::Deleted;
        self.size = 0;
        self.chunk_count = 0;
        self.dirty_chunks.clear();
    }

    /// Check if this is a small file (single chunk).
    pub fn is_small_file(&self) -> bool {
        self.chunk_count <= 1 && self.size <= self.chunk_size
    }

    /// Get the original hash for a chunk (if available).
    ///
    /// # Arguments
    /// * `chunk_index` - Index of the chunk
    ///
    /// # Returns
    /// Original hash if available, None for new chunks.
    pub fn original_hash(&self, chunk_index: u32) -> Option<&str> {
        self.original_hashes.get(chunk_index as usize).map(|s| s.as_str())
    }

    /// Calculate chunk index for a byte offset.
    ///
    /// # Arguments
    /// * `offset` - Byte offset within the file
    pub fn chunk_index_for_offset(&self, offset: u64) -> u32 {
        (offset / self.chunk_size) as u32
    }

    /// Calculate byte range within a chunk for a file offset.
    ///
    /// # Arguments
    /// * `chunk_index` - Chunk index
    /// * `file_offset` - Byte offset within the file
    /// * `size` - Number of bytes
    ///
    /// # Returns
    /// (start_in_chunk, end_in_chunk) byte offsets within the chunk.
    pub fn chunk_byte_range(&self, chunk_index: u32, file_offset: u64, size: u64) -> (usize, usize) {
        let chunk_start: u64 = chunk_index as u64 * self.chunk_size;
        let start_in_chunk: usize = (file_offset.saturating_sub(chunk_start)) as usize;
        let end_in_chunk: usize = ((file_offset + size).saturating_sub(chunk_start))
            .min(self.chunk_size) as usize;
        (start_in_chunk, end_in_chunk)
    }
}


/// Summary of a dirty file for export.
#[derive(Debug, Clone)]
pub struct DirtyEntry {
    /// Inode ID.
    pub inode_id: INodeId,
    /// Relative path.
    pub path: String,
    /// Current state.
    pub state: DirtyState,
    /// File size.
    pub size: u64,
    /// Modification time.
    pub mtime: SystemTime,
    /// Whether executable.
    pub executable: bool,
}

/// Manages dirty (modified) files with copy-on-write semantics.
///
/// Coordinates between in-memory dirty files and disk cache.
/// Content is stored in the unified memory pool for global memory management.
///
/// # Lock Safety
///
/// This struct uses `RwLock` for internal synchronization. Lock operations use
/// `unwrap()` because:
/// - Lock poisoning only occurs if a thread panics while holding the lock
/// - The operations inside locks are infallible (simple HashMap operations)
/// - A poisoned lock indicates corrupted internal state with no meaningful recovery
/// - This follows the standard library's recommended pattern for non-recoverable locks
pub struct DirtyFileManager {
    /// Map of inode ID to dirty file metadata (content in pool).
    dirty_metadata: RwLock<HashMap<INodeId, DirtyFileMetadata>>,
    /// Unified memory pool for content storage.
    pool: Arc<MemoryPool>,
    /// Disk cache for persistence (trait object for testability).
    cache: Arc<dyn WriteCache>,
    /// Reference to read-only file store (for COW source).
    read_store: Arc<dyn FileStore>,
    /// Optional read cache for immutable content.
    read_cache: Option<Arc<crate::diskcache::ReadCache>>,
    /// Reference to inode manager (for path lookup).
    inodes: Arc<INodeManager>,
}

impl DirtyFileManager {
    /// Create a new dirty file manager with unified memory pool.
    ///
    /// # Arguments
    /// * `cache` - Write cache implementation (disk or memory)
    /// * `read_store` - Read-only file store for COW source
    /// * `inodes` - Inode manager for metadata
    /// * `pool` - Unified memory pool for content storage
    pub fn new(
        cache: Arc<dyn WriteCache>,
        read_store: Arc<dyn FileStore>,
        inodes: Arc<INodeManager>,
        pool: Arc<MemoryPool>,
    ) -> Self {
        Self {
            dirty_metadata: RwLock::new(HashMap::new()),
            pool,
            cache,
            read_store,
            read_cache: None,
            inodes,
        }
    }

    /// Set the read cache for immutable content.
    ///
    /// # Arguments
    /// * `read_cache` - Read cache instance
    pub fn set_read_cache(&mut self, read_cache: Arc<crate::diskcache::ReadCache>) {
        self.read_cache = Some(read_cache);
    }

    /// Get reference to the memory pool.
    pub fn pool(&self) -> &Arc<MemoryPool> {
        &self.pool
    }

    /// Check if a file is dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    pub fn is_dirty(&self, inode_id: INodeId) -> bool {
        self.dirty_metadata.read().unwrap().contains_key(&inode_id)
    }

    /// Get dirty file size if dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// File size if dirty, None otherwise.
    pub fn get_size(&self, inode_id: INodeId) -> Option<u64> {
        let guard = self.dirty_metadata.read().unwrap();
        guard.get(&inode_id).map(|meta| meta.size())
    }

    /// Get dirty file mtime if dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// Modification time if dirty, None otherwise.
    pub fn get_mtime(&self, inode_id: INodeId) -> Option<SystemTime> {
        let guard = self.dirty_metadata.read().unwrap();
        guard.get(&inode_id).map(|meta| meta.mtime())
    }

    /// Get dirty file state if dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// State if dirty, None otherwise.
    pub fn get_state(&self, inode_id: INodeId) -> Option<DirtyState> {
        let guard = self.dirty_metadata.read().unwrap();
        guard.get(&inode_id).map(|meta| meta.state())
    }

    /// Perform copy-on-write for a file before modification.
    ///
    /// If file is already dirty, returns immediately.
    /// Otherwise, creates a sparse COW entry (no data fetched yet).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to COW
    pub async fn cow_copy(&self, inode_id: INodeId) -> Result<(), VfsError> {
        // Check if already dirty (includes new files created via create_file)
        if self.is_dirty(inode_id) {
            return Ok(());
        }

        // Get file metadata from inode manager (only for manifest files)
        let inode: Arc<dyn INode> = self
            .inodes
            .get(inode_id)
            .ok_or(VfsError::InodeNotFound(inode_id))?;

        if inode.inode_type() != INodeType::File {
            return Err(VfsError::NotAFile(inode_id));
        }

        let file_content: FileContent = self
            .inodes
            .get_file_content(inode_id)
            .ok_or(VfsError::NotAFile(inode_id))?;

        // Get executable flag by downcasting
        let executable: bool = inode
            .as_any()
            .downcast_ref::<crate::inode::INodeFile>()
            .map(|f| f.is_executable())
            .unwrap_or(false);

        let metadata = DirtyFileMetadata::from_cow(
            inode_id,
            inode.path().to_string(),
            &file_content,
            inode.size(),
            executable,
        );

        // Invalidate any cached read-only blocks for original hashes
        for hash in metadata.original_hashes() {
            self.pool.invalidate_hash(hash);
        }

        self.dirty_metadata.write().unwrap().insert(inode_id, metadata);

        Ok(())
    }


    /// Read from a dirty file.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the file
    /// * `offset` - Byte offset to read from
    /// * `size` - Maximum bytes to read
    ///
    /// # Returns
    /// Data read from the file.
    pub async fn read(
        &self,
        inode_id: INodeId,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, VfsError> {
        // Get file metadata
        let (file_size, chunk_size, chunk_count): (u64, u64, u32) = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            (meta.size(), meta.chunk_size(), meta.chunk_count())
        };

        // Handle empty file or read past end
        if offset >= file_size {
            return Ok(Vec::new());
        }

        let read_end: u64 = (offset + size as u64).min(file_size);
        let actual_size: u64 = read_end - offset;

        // Small file optimization: single chunk read
        if chunk_count <= 1 {
            return self.read_single_chunk(inode_id, offset, actual_size as u32).await;
        }

        // Multi-chunk read
        let start_chunk: u32 = (offset / chunk_size) as u32;
        let end_chunk: u32 = ((read_end - 1) / chunk_size) as u32;

        let mut result: Vec<u8> = Vec::with_capacity(actual_size as usize);

        for chunk_idx in start_chunk..=end_chunk {
            // Ensure chunk is loaded into pool
            self.ensure_chunk_in_pool(inode_id, chunk_idx).await?;

            // Read from pool
            if let Some(handle) = self.pool.get_dirty(inode_id, chunk_idx) {
                let chunk_start_offset: u64 = chunk_idx as u64 * chunk_size;

                handle.with_data(|chunk_data: &[u8]| {
                    let read_start: usize = if chunk_idx == start_chunk {
                        (offset - chunk_start_offset) as usize
                    } else {
                        0
                    };
                    let read_end_in_chunk: usize = if chunk_idx == end_chunk {
                        (read_end - chunk_start_offset) as usize
                    } else {
                        chunk_data.len()
                    };

                    let actual_end: usize = read_end_in_chunk.min(chunk_data.len());
                    if read_start < actual_end {
                        result.extend_from_slice(&chunk_data[read_start..actual_end]);
                    }
                });
            }
        }

        Ok(result)
    }

    /// Read a single chunk (optimized for small files).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `offset` - Byte offset within the file
    /// * `size` - Bytes to read
    async fn read_single_chunk(
        &self,
        inode_id: INodeId,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, VfsError> {
        // Get file size from metadata
        let file_size: u64 = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            meta.size()
        };

        // Ensure chunk 0 is loaded
        self.ensure_chunk_in_pool(inode_id, 0).await?;

        // Read from pool - get a copy of the data
        let pool_data: Vec<u8> = if let Some(handle) = self.pool.get_dirty(inode_id, 0) {
            handle.data()
        } else {
            // New file with no content yet - treat as empty
            Vec::new()
        };

        // Calculate read range
        let start: usize = offset as usize;
        let end: usize = (offset + size as u64).min(file_size) as usize;

        if start >= end {
            return Ok(Vec::new());
        }

        let read_len: usize = end - start;
        let mut result: Vec<u8> = Vec::with_capacity(read_len);

        // Handle case where file was extended via truncate (pool data < file size)
        for i in start..end {
            if i < pool_data.len() {
                result.push(pool_data[i]);
            } else {
                // Extended region - return zeros
                result.push(0);
            }
        }

        Ok(result)
    }

    /// Ensure a chunk is loaded into the pool.
    ///
    /// Checks pool first, then disk cache, then fetches from S3.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `chunk_index` - Chunk index to load
    async fn ensure_chunk_in_pool(&self, inode_id: INodeId, chunk_index: u32) -> Result<(), VfsError> {
        // Check if already in pool
        if self.pool.has_dirty(inode_id, chunk_index) {
            return Ok(());
        }

        // Get original hash for this chunk (if any)
        let original_hash: Option<String> = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            meta.original_hash(chunk_index).map(|s| s.to_string())
        };

        // Fetch content if we have an original hash
        let chunk_data: Vec<u8> = if let Some(hash) = original_hash {
            self.fetch_with_cache(&hash).await?
        } else {
            // New chunk - start empty
            Vec::new()
        };

        // Insert into pool (not dirty yet - just loaded)
        self.pool.insert_dirty(inode_id, chunk_index, chunk_data)
            .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

        // Mark as not needing flush (just loaded, not modified)
        self.pool.mark_flushed(inode_id, chunk_index);

        Ok(())
    }

    /// Fetch content with disk cache support.
    ///
    /// Checks disk cache first, then fetches from S3 and writes through.
    ///
    /// # Arguments
    /// * `hash` - Content hash to fetch
    async fn fetch_with_cache(&self, hash: &str) -> Result<Vec<u8>, VfsError> {
        // Check disk cache first
        if let Some(ref cache) = self.read_cache {
            if let Ok(Some(data)) = cache.get(hash) {
                return Ok(data);
            }
        }

        // Fetch from S3
        let data: Vec<u8> = self.read_store
            .retrieve(hash, HashAlgorithm::Xxh128)
            .await?;

        // Write through to disk cache (ignore errors - cache is best-effort)
        if let Some(ref cache) = self.read_cache {
            let _ = cache.put(hash, &data);
        }

        Ok(data)
    }


    /// Write to a dirty file, performing COW if needed.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file
    /// * `offset` - Byte offset to write at
    /// * `data` - Data to write
    ///
    /// # Returns
    /// Number of bytes written.
    pub async fn write(
        &self,
        inode_id: INodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, VfsError> {
        // Ensure file is dirty (COW if needed)
        self.cow_copy(inode_id).await?;

        // Get file metadata
        let (chunk_size, chunk_count): (u64, u32) = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            (meta.chunk_size(), meta.chunk_count())
        };

        let write_end: u64 = offset + data.len() as u64;

        // Small file optimization: single chunk
        let bytes_written: usize = if chunk_count <= 1 && write_end <= chunk_size {
            self.write_single_chunk(inode_id, offset, data).await?
        } else {
            self.write_multi_chunk(inode_id, offset, data).await?
        };

        // Note: We don't flush to disk here - that happens on fsync() or release()
        // This is critical for performance: FUSE sends many small writes (4KB-128KB)
        // and flushing after each one would be extremely slow for large files.

        Ok(bytes_written)
    }

    /// Write to an already-dirty file synchronously.
    ///
    /// This is a fast path for files that are already dirty and have their
    /// chunks loaded in the pool. Returns `None` if async path is needed.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file
    /// * `offset` - Byte offset to write at
    /// * `data` - Data to write
    ///
    /// # Returns
    /// `Some(Ok(bytes_written))` if sync write succeeded,
    /// `Some(Err(...))` if sync write failed,
    /// `None` if async path is needed (file not dirty or chunks not in pool).
    pub fn write_sync(
        &self,
        inode_id: INodeId,
        offset: u64,
        data: &[u8],
    ) -> Option<Result<usize, VfsError>> {
        // Fast path: check if file is dirty
        if !self.is_dirty(inode_id) {
            return None; // Need async COW
        }

        // Get metadata
        let (chunk_size, chunk_count): (u64, u32) = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard.get(&inode_id)?;
            (meta.chunk_size(), meta.chunk_count())
        };

        let write_end: u64 = offset + data.len() as u64;

        // Small file optimization: single chunk
        if chunk_count <= 1 && write_end <= chunk_size {
            // Check if chunk 0 is in pool
            if !self.pool.has_dirty(inode_id, 0) {
                return None; // Need async to load chunk
            }
            return Some(self.write_single_chunk_sync(inode_id, offset, data));
        }

        // Multi-chunk: check if all chunks are in pool
        let start_chunk: u32 = (offset / chunk_size) as u32;
        let end_chunk: u32 = if data.is_empty() {
            start_chunk
        } else {
            ((write_end - 1) / chunk_size) as u32
        };

        // Check all required chunks are in pool
        for chunk_idx in start_chunk..=end_chunk {
            if !self.pool.has_dirty(inode_id, chunk_idx) {
                return None; // Need async to load chunk
            }
        }

        // All chunks in pool - do sync write
        Some(self.write_multi_chunk_sync(inode_id, offset, data, chunk_size))
    }

    /// Synchronous single-chunk write (no async, no Vec clone).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `offset` - Byte offset within the file
    /// * `data` - Data to write
    ///
    /// # Returns
    /// Number of bytes written.
    fn write_single_chunk_sync(
        &self,
        inode_id: INodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, VfsError> {
        let write_end: u64 = offset + data.len() as u64;
        let offset_usize: usize = offset as usize;

        // Modify in place - pass slice directly, no Vec clone
        self.pool
            .modify_dirty_in_place_with_slice(inode_id, 0, offset_usize, data)
            .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

        // Update metadata
        {
            let mut guard = self.dirty_metadata.write().unwrap();
            if let Some(meta) = guard.get_mut(&inode_id) {
                meta.mark_chunk_dirty(0);
                if write_end > meta.size() {
                    meta.set_size(write_end);
                }
                meta.touch();
            }
        }

        Ok(data.len())
    }

    /// Synchronous multi-chunk write.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `offset` - Byte offset to write at
    /// * `data` - Data to write
    /// * `chunk_size` - Size of each chunk
    ///
    /// # Returns
    /// Number of bytes written.
    fn write_multi_chunk_sync(
        &self,
        inode_id: INodeId,
        offset: u64,
        data: &[u8],
        chunk_size: u64,
    ) -> Result<usize, VfsError> {
        let write_end: u64 = offset + data.len() as u64;
        let start_chunk: u32 = (offset / chunk_size) as u32;
        let end_chunk: u32 = ((write_end - 1) / chunk_size) as u32;

        let mut data_offset: usize = 0;

        for chunk_idx in start_chunk..=end_chunk {
            let chunk_start_offset: u64 = chunk_idx as u64 * chunk_size;
            let write_start_in_chunk: usize = if chunk_idx == start_chunk {
                (offset - chunk_start_offset) as usize
            } else {
                0
            };
            let write_end_in_chunk: usize = if chunk_idx == end_chunk {
                (write_end - chunk_start_offset) as usize
            } else {
                chunk_size as usize
            };
            let write_len: usize = write_end_in_chunk - write_start_in_chunk;

            // Get slice of data for this chunk
            let chunk_data: &[u8] = &data[data_offset..data_offset + write_len];

            // Modify chunk in place with slice
            self.pool
                .modify_dirty_in_place_with_slice(inode_id, chunk_idx, write_start_in_chunk, chunk_data)
                .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

            // Mark chunk as dirty
            {
                let mut guard = self.dirty_metadata.write().unwrap();
                if let Some(meta) = guard.get_mut(&inode_id) {
                    meta.mark_chunk_dirty(chunk_idx);
                }
            }

            data_offset += write_len;
        }

        // Update file size
        {
            let mut guard = self.dirty_metadata.write().unwrap();
            if let Some(meta) = guard.get_mut(&inode_id) {
                if write_end > meta.size() {
                    meta.set_size(write_end);
                }
                meta.touch();
            }
        }

        Ok(data.len())
    }

    /// Write to a single chunk (optimized for small files).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `offset` - Byte offset within the file
    /// * `data` - Data to write
    ///
    /// # Returns
    /// Number of bytes written.
    async fn write_single_chunk(
        &self,
        inode_id: INodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, VfsError> {
        // Ensure chunk 0 is loaded
        self.ensure_chunk_in_pool(inode_id, 0).await?;

        let write_end: u64 = offset + data.len() as u64;
        let offset_usize: usize = offset as usize;

        // Use slice-based method - no Vec clone needed
        self.pool
            .modify_dirty_in_place_with_slice(inode_id, 0, offset_usize, data)
            .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

        // Update metadata
        {
            let mut guard = self.dirty_metadata.write().unwrap();
            if let Some(meta) = guard.get_mut(&inode_id) {
                meta.mark_chunk_dirty(0);
                if write_end > meta.size() {
                    meta.set_size(write_end);
                }
                meta.touch();
            }
        }

        Ok(data.len())
    }

    /// Write across multiple chunks.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file
    /// * `offset` - Byte offset to write at
    /// * `data` - Data to write
    ///
    /// # Returns
    /// Number of bytes written.
    async fn write_multi_chunk(
        &self,
        inode_id: INodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, VfsError> {
        let chunk_size: u64 = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            meta.chunk_size()
        };

        let write_end: u64 = offset + data.len() as u64;
        let start_chunk: u32 = (offset / chunk_size) as u32;
        let end_chunk: u32 = if data.is_empty() {
            start_chunk
        } else {
            ((write_end - 1) / chunk_size) as u32
        };

        let mut data_offset: usize = 0;

        for chunk_idx in start_chunk..=end_chunk {
            // Ensure chunk is loaded into pool
            self.ensure_chunk_in_pool(inode_id, chunk_idx).await?;

            // Calculate write range within this chunk
            let chunk_start_offset: u64 = chunk_idx as u64 * chunk_size;
            let write_start_in_chunk: usize = if chunk_idx == start_chunk {
                (offset - chunk_start_offset) as usize
            } else {
                0
            };
            let write_end_in_chunk: usize = if chunk_idx == end_chunk {
                (write_end - chunk_start_offset) as usize
            } else {
                chunk_size as usize
            };
            let write_len: usize = write_end_in_chunk - write_start_in_chunk;

            // Get slice of data for this chunk - no Vec clone
            let chunk_data: &[u8] = &data[data_offset..data_offset + write_len];

            // Modify chunk in place with slice
            self.pool
                .modify_dirty_in_place_with_slice(inode_id, chunk_idx, write_start_in_chunk, chunk_data)
                .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;

            // Mark chunk as dirty in metadata
            {
                let mut guard = self.dirty_metadata.write().unwrap();
                if let Some(meta) = guard.get_mut(&inode_id) {
                    meta.mark_chunk_dirty(chunk_idx);
                }
            }

            data_offset += write_len;
        }

        // Update file size in metadata
        {
            let mut guard = self.dirty_metadata.write().unwrap();
            if let Some(meta) = guard.get_mut(&inode_id) {
                if write_end > meta.size() {
                    meta.set_size(write_end);
                }
                meta.touch();
            }
        }

        Ok(data.len())
    }


    /// Create a new file (not COW).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID for new file
    /// * `rel_path` - Relative path for new file
    /// * `parent_inode` - Parent directory inode ID
    pub fn create_file(
        &self,
        inode_id: INodeId,
        rel_path: String,
        parent_inode: INodeId,
    ) -> Result<(), VfsError> {
        let metadata = DirtyFileMetadata::new_file(inode_id, rel_path, parent_inode);
        self.dirty_metadata.write().unwrap().insert(inode_id, metadata);
        Ok(())
    }

    /// Get new files in a directory.
    ///
    /// # Arguments
    /// * `parent_inode` - Parent directory inode ID
    ///
    /// # Returns
    /// Vector of (inode_id, file_name) for new files in the directory.
    pub fn get_new_files_in_dir(&self, parent_inode: INodeId) -> Vec<(INodeId, String)> {
        let guard = self.dirty_metadata.read().unwrap();
        guard
            .values()
            .filter(|meta| meta.state() == DirtyState::New && meta.parent_inode() == Some(parent_inode))
            .map(|meta| (meta.inode_id(), meta.file_name().to_string()))
            .collect()
    }

    /// Look up a new file by parent inode and name.
    ///
    /// # Arguments
    /// * `parent_inode` - Parent directory inode ID
    /// * `name` - File name to look up
    ///
    /// # Returns
    /// Inode ID if found, None otherwise.
    pub fn lookup_new_file(&self, parent_inode: INodeId, name: &str) -> Option<INodeId> {
        let guard = self.dirty_metadata.read().unwrap();
        guard
            .values()
            .find(|meta| {
                meta.state() == DirtyState::New
                    && meta.parent_inode() == Some(parent_inode)
                    && meta.file_name() == name
            })
            .map(|meta| meta.inode_id())
    }

    /// Check if an inode is a new file (created, not from manifest).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// True if this is a new file, false otherwise.
    pub fn is_new_file(&self, inode_id: INodeId) -> bool {
        let guard = self.dirty_metadata.read().unwrap();
        guard
            .get(&inode_id)
            .map(|meta| meta.state() == DirtyState::New)
            .unwrap_or(false)
    }

    /// Mark a file as deleted.
    ///
    /// For new files (created in this session), removes them entirely.
    /// For manifest files, marks them as deleted.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to delete
    pub async fn delete_file(&self, inode_id: INodeId) -> Result<(), VfsError> {
        // Check if it's a new file - if so, just remove it entirely
        if self.is_new_file(inode_id) {
            let rel_path: Option<String> = self.get_rel_path(inode_id);

            // Remove from pool
            self.pool.remove_inode_blocks(inode_id);

            // Remove from metadata
            self.dirty_metadata.write().unwrap().remove(&inode_id);

            // Remove from disk cache if it was written
            if let Some(path) = rel_path {
                let _ = self.cache.delete_file(&path).await;
            }

            return Ok(());
        }

        // If not dirty, create a deleted entry for manifest file
        if !self.is_dirty(inode_id) {
            let inode: Arc<dyn INode> = self
                .inodes
                .get(inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;

            if inode.inode_type() != INodeType::File {
                return Err(VfsError::NotAFile(inode_id));
            }

            let file_content: FileContent = self
                .inodes
                .get_file_content(inode_id)
                .ok_or(VfsError::NotAFile(inode_id))?;

            let mut metadata = DirtyFileMetadata::from_cow(
                inode_id,
                inode.path().to_string(),
                &file_content,
                inode.size(),
                false,
            );
            metadata.mark_deleted();
            self.dirty_metadata.write().unwrap().insert(inode_id, metadata);
        } else {
            // Mark existing dirty entry as deleted
            let mut guard = self.dirty_metadata.write().unwrap();
            if let Some(meta) = guard.get_mut(&inode_id) {
                meta.mark_deleted();
            }
        }

        // Remove blocks from pool
        self.pool.remove_inode_blocks(inode_id);

        // Remove from disk cache
        self.flush_to_disk(inode_id).await?;
        Ok(())
    }

    /// Get the relative path for an inode.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to look up
    ///
    /// # Returns
    /// Relative path if found, None otherwise.
    fn get_rel_path(&self, inode_id: INodeId) -> Option<String> {
        let guard = self.dirty_metadata.read().unwrap();
        guard.get(&inode_id).map(|meta| meta.rel_path().to_string())
    }


    /// Truncate a dirty file.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to truncate
    /// * `new_size` - New file size
    pub async fn truncate(&self, inode_id: INodeId, new_size: u64) -> Result<(), VfsError> {
        // Ensure file is dirty
        self.cow_copy(inode_id).await?;

        let (old_size, chunk_size): (u64, u64) = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            (meta.size(), meta.chunk_size())
        };

        if new_size >= old_size {
            // Extending - just update metadata
            let mut guard = self.dirty_metadata.write().unwrap();
            if let Some(meta) = guard.get_mut(&inode_id) {
                meta.set_size(new_size);
                meta.touch();
            }
        } else {
            // Shrinking - truncate last chunk if needed
            let new_chunk_count: u32 = if new_size == 0 {
                0
            } else {
                ((new_size - 1) / chunk_size + 1) as u32
            };

            // Truncate last chunk if needed
            if new_chunk_count > 0 {
                let last_chunk_idx: u32 = new_chunk_count - 1;
                let last_chunk_end: u64 = new_size - (last_chunk_idx as u64 * chunk_size);

                if let Some(handle) = self.pool.get_dirty(inode_id, last_chunk_idx) {
                    let chunk_data: Vec<u8> = handle.data();
                    drop(handle);

                    if (last_chunk_end as usize) < chunk_data.len() {
                        let mut truncated_data: Vec<u8> = chunk_data;
                        truncated_data.truncate(last_chunk_end as usize);
                        self.pool.update_dirty(inode_id, last_chunk_idx, truncated_data)
                            .map_err(|e| VfsError::MemoryPoolError(e.to_string()))?;
                    }
                }
            }

            // Update metadata
            {
                let mut guard = self.dirty_metadata.write().unwrap();
                if let Some(meta) = guard.get_mut(&inode_id) {
                    meta.set_size(new_size);
                    meta.touch();
                }
            }
        }

        // Flush to disk
        self.flush_to_disk(inode_id).await?;

        Ok(())
    }

    /// Flush dirty file to disk cache.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to flush
    pub async fn flush_to_disk(&self, inode_id: INodeId) -> Result<(), VfsError> {
        let (rel_path, state, size, chunk_count, dirty_chunks): (String, DirtyState, u64, u32, HashSet<u32>) = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            (
                meta.rel_path().to_string(),
                meta.state(),
                meta.size(),
                meta.chunk_count(),
                meta.dirty_chunks().clone(),
            )
        };

        if state == DirtyState::Deleted {
            self.cache
                .delete_file(&rel_path)
                .await
                .map_err(|e| VfsError::WriteCacheError(e.to_string()))?;
            return Ok(());
        }

        // Assemble file content from pool
        let mut assembled: Vec<u8> = Vec::with_capacity(size as usize);

        for chunk_idx in 0..chunk_count {
            if let Some(handle) = self.pool.get_dirty(inode_id, chunk_idx) {
                handle.with_data(|data: &[u8]| {
                    assembled.extend_from_slice(data);
                });
            } else if dirty_chunks.contains(&chunk_idx) {
                return Err(VfsError::ChunkNotLoaded {
                    path: rel_path,
                    chunk_index: chunk_idx,
                });
            }
        }

        // Truncate to actual size (last chunk may be larger)
        assembled.truncate(size as usize);

        // Write to cache
        if !assembled.is_empty() || size == 0 {
            self.cache
                .write_file(&rel_path, &assembled)
                .await
                .map_err(|e| VfsError::WriteCacheError(e.to_string()))?;
        }

        // Mark all chunks as flushed in pool
        for chunk_idx in 0..chunk_count {
            self.pool.mark_flushed(inode_id, chunk_idx);
        }

        // Clear dirty flags in metadata
        {
            let mut guard = self.dirty_metadata.write().unwrap();
            if let Some(meta) = guard.get_mut(&inode_id) {
                meta.mark_all_flushed();
            }
        }

        Ok(())
    }


    /// Remove all new files under a directory path.
    ///
    /// Used when a new directory is deleted to clean up any new files
    /// that were created inside it.
    ///
    /// # Arguments
    /// * `dir_path` - Directory path prefix to match
    ///
    /// # Returns
    /// Number of files removed.
    pub fn remove_new_files_under_path(&self, dir_path: &str) -> usize {
        let prefix: String = if dir_path.is_empty() {
            String::new()
        } else {
            format!("{}/", dir_path)
        };

        let mut guard = self.dirty_metadata.write().unwrap();
        let to_remove: Vec<INodeId> = guard
            .iter()
            .filter(|(_, meta)| {
                meta.state() == DirtyState::New
                    && (meta.rel_path().starts_with(&prefix) || meta.rel_path() == dir_path)
            })
            .map(|(id, _)| *id)
            .collect();

        let count: usize = to_remove.len();

        // Remove blocks from pool
        for id in &to_remove {
            self.pool.remove_inode_blocks(*id);
        }

        for id in to_remove {
            guard.remove(&id);
        }

        count
    }

    /// Get all dirty entries for diff manifest generation.
    ///
    /// # Returns
    /// Vector of dirty entry summaries.
    pub fn get_dirty_entries(&self) -> Vec<DirtyEntry> {
        let guard = self.dirty_metadata.read().unwrap();
        guard
            .values()
            .map(|meta| DirtyEntry {
                inode_id: meta.inode_id(),
                path: meta.rel_path().to_string(),
                state: meta.state(),
                size: meta.size(),
                mtime: meta.mtime(),
                executable: meta.is_executable(),
            })
            .collect()
    }

    /// Clear all dirty state.
    pub fn clear(&self) {
        // Clear pool blocks for all dirty inodes
        let guard = self.dirty_metadata.read().unwrap();
        for inode_id in guard.keys() {
            self.pool.remove_inode_blocks(*inode_id);
        }
        drop(guard);

        self.dirty_metadata.write().unwrap().clear();
    }

    /// Get reference to the cache.
    pub fn cache(&self) -> &Arc<dyn WriteCache> {
        &self.cache
    }

    /// Get reference to the read store.
    pub fn read_store(&self) -> &Arc<dyn FileStore> {
        &self.read_store
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::MemoryFileStore;
    use crate::diskcache::MemoryWriteCache;
    use crate::memory_pool_v2::MemoryPoolConfig;

    fn create_test_manager() -> (DirtyFileManager, Arc<INodeManager>, Arc<MemoryPool>) {
        let cache = Arc::new(MemoryWriteCache::new());
        let store = Arc::new(MemoryFileStore::new());
        let inodes = Arc::new(INodeManager::new());
        let pool = Arc::new(MemoryPool::new(MemoryPoolConfig::default()));
        let manager = DirtyFileManager::new(cache, store, inodes.clone(), pool.clone());
        (manager, inodes, pool)
    }

    // ========================================================================
    // DirtyFileMetadata Tests
    // ========================================================================

    #[test]
    fn test_dirty_file_metadata_new_file() {
        let meta = DirtyFileMetadata::new_file(42, "test/file.txt".to_string(), 1);

        assert_eq!(meta.inode_id(), 42);
        assert_eq!(meta.rel_path(), "test/file.txt");
        assert_eq!(meta.file_name(), "file.txt");
        assert_eq!(meta.parent_inode(), Some(1));
        assert_eq!(meta.state(), DirtyState::New);
        assert_eq!(meta.size(), 0);
        assert_eq!(meta.chunk_count(), 1);
        assert!(meta.original_hashes().is_empty());
        assert!(!meta.is_executable());
    }

    #[test]
    fn test_dirty_file_metadata_from_cow_small() {
        let file_content = FileContent::SingleHash("abc123".to_string());
        let meta = DirtyFileMetadata::from_cow(
            42,
            "test/file.txt".to_string(),
            &file_content,
            1024,
            true,
        );

        assert_eq!(meta.inode_id(), 42);
        assert_eq!(meta.state(), DirtyState::Modified);
        assert_eq!(meta.size(), 1024);
        assert_eq!(meta.chunk_count(), 1);
        assert_eq!(meta.original_hashes(), &["abc123".to_string()]);
        assert!(meta.is_executable());
        assert!(meta.is_small_file());
    }

    #[test]
    fn test_dirty_file_metadata_from_cow_chunked() {
        let file_content = FileContent::Chunked(vec![
            "hash1".to_string(),
            "hash2".to_string(),
            "hash3".to_string(),
        ]);
        let meta = DirtyFileMetadata::from_cow(
            42,
            "large_file.bin".to_string(),
            &file_content,
            CHUNK_SIZE_V2 * 2 + 1000,
            false,
        );

        assert_eq!(meta.chunk_count(), 3);
        assert_eq!(meta.original_hashes().len(), 3);
        assert_eq!(meta.original_hash(0), Some("hash1"));
        assert_eq!(meta.original_hash(1), Some("hash2"));
        assert_eq!(meta.original_hash(2), Some("hash3"));
        assert_eq!(meta.original_hash(3), None);
        assert!(!meta.is_small_file());
    }

    #[test]
    fn test_dirty_file_metadata_dirty_chunks() {
        let mut meta = DirtyFileMetadata::new_file(42, "test.txt".to_string(), 1);

        assert!(!meta.is_chunk_dirty(0));
        assert!(meta.dirty_chunks().is_empty());

        meta.mark_chunk_dirty(0);
        meta.mark_chunk_dirty(2);

        assert!(meta.is_chunk_dirty(0));
        assert!(!meta.is_chunk_dirty(1));
        assert!(meta.is_chunk_dirty(2));
        assert_eq!(meta.dirty_chunks().len(), 2);

        meta.mark_chunk_flushed(0);
        assert!(!meta.is_chunk_dirty(0));

        meta.mark_all_flushed();
        assert!(meta.dirty_chunks().is_empty());
    }

    #[test]
    fn test_dirty_file_metadata_set_size() {
        let mut meta = DirtyFileMetadata::new_file(42, "test.txt".to_string(), 1);

        meta.set_size(100);
        assert_eq!(meta.size(), 100);
        assert_eq!(meta.chunk_count(), 1);

        meta.set_size(CHUNK_SIZE_V2 + 1);
        assert_eq!(meta.size(), CHUNK_SIZE_V2 + 1);
        assert_eq!(meta.chunk_count(), 2);

        meta.set_size(0);
        assert_eq!(meta.size(), 0);
        assert_eq!(meta.chunk_count(), 1);
    }

    #[test]
    fn test_dirty_file_metadata_mark_deleted() {
        let mut meta = DirtyFileMetadata::new_file(42, "test.txt".to_string(), 1);
        meta.set_size(1000);
        meta.mark_chunk_dirty(0);

        meta.mark_deleted();

        assert_eq!(meta.state(), DirtyState::Deleted);
        assert_eq!(meta.size(), 0);
        assert_eq!(meta.chunk_count(), 0);
        assert!(meta.dirty_chunks().is_empty());
    }

    #[test]
    fn test_dirty_file_metadata_chunk_calculations() {
        let file_content = FileContent::Chunked(vec!["h1".to_string(), "h2".to_string()]);
        let meta = DirtyFileMetadata::from_cow(
            42,
            "file.bin".to_string(),
            &file_content,
            CHUNK_SIZE_V2 + 1000,
            false,
        );

        // Test chunk_index_for_offset
        assert_eq!(meta.chunk_index_for_offset(0), 0);
        assert_eq!(meta.chunk_index_for_offset(CHUNK_SIZE_V2 - 1), 0);
        assert_eq!(meta.chunk_index_for_offset(CHUNK_SIZE_V2), 1);
        assert_eq!(meta.chunk_index_for_offset(CHUNK_SIZE_V2 + 500), 1);

        // Test chunk_byte_range
        let (start, end): (usize, usize) = meta.chunk_byte_range(0, 100, 50);
        assert_eq!(start, 100);
        assert_eq!(end, 150);

        let (start, end): (usize, usize) = meta.chunk_byte_range(1, CHUNK_SIZE_V2 + 100, 50);
        assert_eq!(start, 100);
        assert_eq!(end, 150);
    }

    #[test]
    fn test_dirty_state() {
        assert_eq!(DirtyState::Modified, DirtyState::Modified);
        assert_ne!(DirtyState::New, DirtyState::Deleted);
    }


    // ========================================================================
    // DirtyFileManager Tests
    // ========================================================================

    #[tokio::test]
    async fn test_manager_create_file() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "new_file.txt".to_string(), 1).unwrap();

        assert!(manager.is_dirty(100));
        assert_eq!(manager.get_state(100), Some(DirtyState::New));
        assert_eq!(manager.get_size(100), Some(0));
    }

    #[tokio::test]
    async fn test_manager_write_and_read() {
        let (manager, _inodes, pool) = create_test_manager();

        // Create a new file
        manager.create_file(100, "test.txt".to_string(), 1).unwrap();

        // Write data
        let data: &[u8] = b"hello world";
        let written: usize = manager.write(100, 0, data).await.unwrap();
        assert_eq!(written, 11);

        // Verify size updated
        assert_eq!(manager.get_size(100), Some(11));

        // Verify data in pool
        assert!(pool.has_dirty(100, 0));

        // Read data back
        let read_data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(read_data, b"hello world");
    }

    #[tokio::test]
    async fn test_manager_partial_read() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        // Read partial data
        let read_data: Vec<u8> = manager.read(100, 6, 5).await.unwrap();
        assert_eq!(read_data, b"world");
    }

    #[tokio::test]
    async fn test_manager_overwrite() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        // Overwrite middle
        manager.write(100, 6, b"WORLD").await.unwrap();

        let read_data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(read_data, b"hello WORLD");
    }

    #[tokio::test]
    async fn test_manager_extend() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        // Write beyond current end
        manager.write(100, 10, b"world").await.unwrap();

        assert_eq!(manager.get_size(100), Some(15));

        let read_data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(read_data.len(), 15);
        assert_eq!(&read_data[0..5], b"hello");
        assert_eq!(&read_data[10..15], b"world");
    }

    #[tokio::test]
    async fn test_manager_delete_new_file() {
        let (manager, _inodes, pool) = create_test_manager();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        assert!(manager.is_dirty(100));
        assert!(pool.has_dirty(100, 0));

        // Delete the new file
        manager.delete_file(100).await.unwrap();

        // Should be completely removed
        assert!(!manager.is_dirty(100));
        assert!(!pool.has_dirty(100, 0));
    }

    #[tokio::test]
    async fn test_manager_truncate_shrink() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello world").await.unwrap();

        assert_eq!(manager.get_size(100), Some(11));

        // Truncate to smaller size
        manager.truncate(100, 5).await.unwrap();

        assert_eq!(manager.get_size(100), Some(5));

        let read_data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert_eq!(read_data, b"hello");
    }

    #[tokio::test]
    async fn test_manager_truncate_extend() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        // Truncate to larger size (extend)
        manager.truncate(100, 10).await.unwrap();

        assert_eq!(manager.get_size(100), Some(10));
    }

    #[test]
    fn test_remove_new_files_under_path() {
        let (manager, _inodes, _pool) = create_test_manager();

        // Create files in different directories
        manager.create_file(100, "root.txt".to_string(), 1).unwrap();
        manager.create_file(101, "dir/file1.txt".to_string(), 1).unwrap();
        manager.create_file(102, "dir/file2.txt".to_string(), 1).unwrap();
        manager.create_file(103, "dir/subdir/file3.txt".to_string(), 1).unwrap();
        manager.create_file(104, "other/file4.txt".to_string(), 1).unwrap();

        assert_eq!(manager.get_dirty_entries().len(), 5);

        // Remove files under "dir"
        let removed: usize = manager.remove_new_files_under_path("dir");
        assert_eq!(removed, 3); // file1.txt, file2.txt, subdir/file3.txt

        // Verify remaining files
        assert!(manager.is_dirty(100)); // root.txt
        assert!(!manager.is_dirty(101)); // dir/file1.txt - removed
        assert!(!manager.is_dirty(102)); // dir/file2.txt - removed
        assert!(!manager.is_dirty(103)); // dir/subdir/file3.txt - removed
        assert!(manager.is_dirty(104)); // other/file4.txt

        assert_eq!(manager.get_dirty_entries().len(), 2);
    }

    #[test]
    fn test_remove_new_files_under_path_no_match() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "file.txt".to_string(), 1).unwrap();
        manager.create_file(101, "other/file.txt".to_string(), 1).unwrap();

        let removed: usize = manager.remove_new_files_under_path("nonexistent");
        assert_eq!(removed, 0);
        assert_eq!(manager.get_dirty_entries().len(), 2);
    }

    #[test]
    fn test_lookup_new_file() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "dir/file.txt".to_string(), 1).unwrap();
        manager.create_file(101, "dir/other.txt".to_string(), 1).unwrap();

        assert_eq!(manager.lookup_new_file(1, "file.txt"), Some(100));
        assert_eq!(manager.lookup_new_file(1, "other.txt"), Some(101));
        assert_eq!(manager.lookup_new_file(1, "missing.txt"), None);
        assert_eq!(manager.lookup_new_file(2, "file.txt"), None);
    }

    #[test]
    fn test_get_new_files_in_dir() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "dir/file1.txt".to_string(), 1).unwrap();
        manager.create_file(101, "dir/file2.txt".to_string(), 1).unwrap();
        manager.create_file(102, "other/file3.txt".to_string(), 2).unwrap();

        let files: Vec<(INodeId, String)> = manager.get_new_files_in_dir(1);
        assert_eq!(files.len(), 2);

        let names: Vec<&str> = files.iter().map(|(_, n)| n.as_str()).collect();
        assert!(names.contains(&"file1.txt"));
        assert!(names.contains(&"file2.txt"));
    }

    #[test]
    fn test_clear() {
        let (manager, _inodes, pool) = create_test_manager();

        manager.create_file(100, "file1.txt".to_string(), 1).unwrap();
        manager.create_file(101, "file2.txt".to_string(), 1).unwrap();

        // Insert some data into pool
        pool.insert_dirty(100, 0, vec![1, 2, 3]).unwrap();
        pool.insert_dirty(101, 0, vec![4, 5, 6]).unwrap();

        assert!(manager.is_dirty(100));
        assert!(manager.is_dirty(101));
        assert!(pool.has_dirty(100, 0));
        assert!(pool.has_dirty(101, 0));

        manager.clear();

        assert!(!manager.is_dirty(100));
        assert!(!manager.is_dirty(101));
        assert!(!pool.has_dirty(100, 0));
        assert!(!pool.has_dirty(101, 0));
        assert_eq!(manager.get_dirty_entries().len(), 0);
    }

    #[test]
    fn test_is_new_file() {
        let (manager, inodes, _pool) = create_test_manager();

        // Create a new file
        manager.create_file(100, "new.txt".to_string(), 1).unwrap();
        assert!(manager.is_new_file(100));

        // Add a manifest file
        let _ino: INodeId = inodes.add_file(
            "existing.txt",
            100,
            0,
            FileContent::SingleHash("hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Non-existent file
        assert!(!manager.is_new_file(999));
    }

    #[tokio::test]
    async fn test_read_empty_file() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "empty.txt".to_string(), 1).unwrap();

        let data: Vec<u8> = manager.read(100, 0, 100).await.unwrap();
        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn test_read_past_end() {
        let (manager, _inodes, _pool) = create_test_manager();

        manager.create_file(100, "test.txt".to_string(), 1).unwrap();
        manager.write(100, 0, b"hello").await.unwrap();

        // Read starting past end of file
        let data: Vec<u8> = manager.read(100, 100, 50).await.unwrap();
        assert!(data.is_empty());
    }
}
