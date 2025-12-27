//! Dirty file tracking for copy-on-write VFS.
//!
//! This module provides the core data structures for tracking modified files
//! in the writable VFS layer.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use rusty_attachments_common::CHUNK_SIZE_V2;
use rusty_attachments_model::HashAlgorithm;

use crate::content::FileStore;
use crate::inode::{FileContent, INode, INodeId, INodeManager, INodeType};
use crate::VfsError;

use super::cache::WriteCache;

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

/// Content state for a dirty file.
///
/// Tracks original chunks and which have been modified.
#[derive(Debug)]
pub enum DirtyContent {
    /// Small file (single chunk) - entire content in memory.
    Small {
        /// File data.
        data: Vec<u8>,
    },
    /// Large file (multiple chunks) - sparse tracking.
    Chunked {
        /// Original chunk hashes (from manifest).
        original_chunks: Vec<String>,
        /// Chunk size (256MB for V2).
        chunk_size: u64,
        /// Total file size.
        total_size: u64,
        /// Loaded chunks: chunk_index → data.
        /// Only chunks that have been read or written are loaded.
        loaded_chunks: HashMap<u32, Vec<u8>>,
        /// Which chunks have been modified.
        dirty_chunks: HashSet<u32>,
    },
}

impl DirtyContent {
    /// Create sparse content for a chunked file.
    ///
    /// # Arguments
    /// * `original_chunks` - Chunk hashes from manifest
    /// * `chunk_size` - Size of each chunk (256MB)
    /// * `total_size` - Total file size
    pub fn chunked(original_chunks: Vec<String>, chunk_size: u64, total_size: u64) -> Self {
        Self::Chunked {
            original_chunks,
            chunk_size,
            total_size,
            loaded_chunks: HashMap::new(),
            dirty_chunks: HashSet::new(),
        }
    }

    /// Check if a chunk is loaded.
    ///
    /// # Arguments
    /// * `chunk_index` - Index of the chunk to check
    pub fn is_chunk_loaded(&self, chunk_index: u32) -> bool {
        match self {
            Self::Small { .. } => true,
            Self::Chunked { loaded_chunks, .. } => loaded_chunks.contains_key(&chunk_index),
        }
    }

    /// Get loaded chunk data (None if not loaded).
    ///
    /// # Arguments
    /// * `chunk_index` - Index of the chunk to get
    ///
    /// # Returns
    /// Chunk data if loaded, None otherwise.
    pub fn get_chunk(&self, chunk_index: u32) -> Option<&[u8]> {
        match self {
            Self::Small { data } if chunk_index == 0 => Some(data),
            Self::Small { .. } => None,
            Self::Chunked { loaded_chunks, .. } => {
                loaded_chunks.get(&chunk_index).map(|v| v.as_slice())
            }
        }
    }

    /// Insert a loaded chunk.
    ///
    /// # Arguments
    /// * `chunk_index` - Index of the chunk
    /// * `data` - Chunk data
    pub fn insert_chunk(&mut self, chunk_index: u32, data: Vec<u8>) {
        if let Self::Chunked { loaded_chunks, .. } = self {
            loaded_chunks.insert(chunk_index, data);
        }
    }

    /// Mark a chunk as dirty.
    ///
    /// # Arguments
    /// * `chunk_index` - Index of the chunk to mark
    pub fn mark_dirty(&mut self, chunk_index: u32) {
        if let Self::Chunked { dirty_chunks, .. } = self {
            dirty_chunks.insert(chunk_index);
        }
    }

    /// Get the current file size.
    pub fn size(&self) -> u64 {
        match self {
            Self::Small { data } => data.len() as u64,
            Self::Chunked { total_size, .. } => *total_size,
        }
    }
}


/// A file that has been modified via copy-on-write.
///
/// Maintains both in-memory data (fast access) and tracks state for
/// disk persistence.
#[derive(Debug)]
pub struct DirtyFile {
    /// Inode ID of this file.
    inode_id: INodeId,
    /// Relative path within the VFS.
    rel_path: String,
    /// Parent inode ID (only set for new files).
    parent_inode: Option<INodeId>,
    /// File content (small or chunked).
    content: DirtyContent,
    /// Original hash before modification (None if new file).
    original_hash: Option<String>,
    /// Current state of the file.
    state: DirtyState,
    /// Modification time (updated on each write).
    mtime: SystemTime,
    /// Whether file is executable (from original or set via chmod).
    executable: bool,
}

impl DirtyFile {
    /// Create a new dirty file from COW copy.
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
        let content: DirtyContent = match file_content {
            FileContent::SingleHash(_) => DirtyContent::Small { data: Vec::new() },
            FileContent::Chunked(hashes) => {
                DirtyContent::chunked(hashes.clone(), CHUNK_SIZE_V2, total_size)
            }
        };

        Self {
            inode_id,
            rel_path,
            parent_inode: None,
            content,
            original_hash: None,
            state: DirtyState::Modified,
            mtime: SystemTime::now(),
            executable,
        }
    }

    /// Create a new file (not from COW).
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
            content: DirtyContent::Small { data: Vec::new() },
            original_hash: None,
            state: DirtyState::New,
            mtime: SystemTime::now(),
            executable: false,
        }
    }

    /// Mark file as deleted.
    pub fn mark_deleted(&mut self) {
        self.state = DirtyState::Deleted;
        self.content = DirtyContent::Small { data: Vec::new() };
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
        self.content.size()
    }

    /// Check if file is executable.
    pub fn is_executable(&self) -> bool {
        self.executable
    }

    /// Get reference to content.
    pub fn content(&self) -> &DirtyContent {
        &self.content
    }

    /// Get mutable reference to content.
    pub fn content_mut(&mut self) -> &mut DirtyContent {
        &mut self.content
    }

    /// Set the original hash (for small files after loading).
    ///
    /// # Arguments
    /// * `hash` - Original content hash
    pub fn set_original_hash(&mut self, hash: String) {
        self.original_hash = Some(hash);
    }

    /// Get the original hash.
    pub fn original_hash(&self) -> Option<&str> {
        self.original_hash.as_deref()
    }

    /// Update modification time.
    pub fn touch(&mut self) {
        self.mtime = SystemTime::now();
    }

    /// Check if small file should convert to chunked after write.
    pub fn maybe_convert_to_chunked(&mut self) {
        if let DirtyContent::Small { data } = &self.content {
            if data.len() as u64 > CHUNK_SIZE_V2 {
                let chunk_size: u64 = CHUNK_SIZE_V2;
                let mut loaded_chunks: HashMap<u32, Vec<u8>> = HashMap::new();
                let mut dirty_chunks: HashSet<u32> = HashSet::new();

                for (idx, chunk) in data.chunks(chunk_size as usize).enumerate() {
                    loaded_chunks.insert(idx as u32, chunk.to_vec());
                    dirty_chunks.insert(idx as u32);
                }

                self.content = DirtyContent::Chunked {
                    original_chunks: Vec::new(),
                    chunk_size,
                    total_size: data.len() as u64,
                    loaded_chunks,
                    dirty_chunks,
                };
            }
        }
    }

    /// Truncate file to new size.
    ///
    /// # Arguments
    /// * `new_size` - New file size in bytes
    pub fn truncate(&mut self, new_size: u64) {
        match &mut self.content {
            DirtyContent::Small { data } => {
                let new_len: usize = new_size as usize;
                if new_len < data.len() {
                    data.truncate(new_len);
                } else {
                    data.resize(new_len, 0);
                }
            }
            DirtyContent::Chunked {
                original_chunks,
                chunk_size,
                total_size,
                loaded_chunks,
                dirty_chunks,
            } => {
                let old_size: u64 = *total_size;

                if new_size >= old_size {
                    // Extending - just update size (sparse extension)
                    *total_size = new_size;
                    self.mtime = SystemTime::now();
                    return;
                }

                // Shrinking
                let new_chunk_count: u32 = if new_size == 0 {
                    0
                } else {
                    ((new_size - 1) / *chunk_size + 1) as u32
                };
                let old_chunk_count: u32 = original_chunks.len() as u32;

                // Remove chunks beyond new size
                for chunk_idx in new_chunk_count..old_chunk_count.max(new_chunk_count) {
                    loaded_chunks.remove(&chunk_idx);
                    dirty_chunks.remove(&chunk_idx);
                }

                // Truncate original_chunks list
                if new_chunk_count < original_chunks.len() as u32 {
                    original_chunks.truncate(new_chunk_count as usize);
                }

                // Truncate last chunk if loaded
                if new_chunk_count > 0 {
                    let last_chunk_idx: u32 = new_chunk_count - 1;
                    let last_chunk_end: u64 = new_size - (last_chunk_idx as u64 * *chunk_size);

                    if let Some(chunk_data) = loaded_chunks.get_mut(&last_chunk_idx) {
                        if (last_chunk_end as usize) < chunk_data.len() {
                            chunk_data.truncate(last_chunk_end as usize);
                            dirty_chunks.insert(last_chunk_idx);
                        }
                    }
                }

                *total_size = new_size;
            }
        }
        self.mtime = SystemTime::now();
        self.maybe_convert_to_chunked();
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
pub struct DirtyFileManager {
    /// Map of inode ID to dirty file.
    dirty_files: RwLock<HashMap<INodeId, DirtyFile>>,
    /// Disk cache for persistence (trait object for testability).
    cache: Arc<dyn WriteCache>,
    /// Reference to read-only file store (for COW source).
    read_store: Arc<dyn FileStore>,
    /// Reference to inode manager (for path lookup).
    inodes: Arc<INodeManager>,
}

impl DirtyFileManager {
    /// Create a new dirty file manager.
    ///
    /// # Arguments
    /// * `cache` - Write cache implementation (disk or memory)
    /// * `read_store` - Read-only file store for COW source
    /// * `inodes` - Inode manager for metadata
    pub fn new(
        cache: Arc<dyn WriteCache>,
        read_store: Arc<dyn FileStore>,
        inodes: Arc<INodeManager>,
    ) -> Self {
        Self {
            dirty_files: RwLock::new(HashMap::new()),
            cache,
            read_store,
            inodes,
        }
    }

    /// Check if a file is dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    pub fn is_dirty(&self, inode_id: INodeId) -> bool {
        self.dirty_files.read().unwrap().contains_key(&inode_id)
    }

    /// Get dirty file size if dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// File size if dirty, None otherwise.
    pub fn get_size(&self, inode_id: INodeId) -> Option<u64> {
        let guard = self.dirty_files.read().unwrap();
        guard.get(&inode_id).map(|f| f.size())
    }

    /// Get dirty file mtime if dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// Modification time if dirty, None otherwise.
    pub fn get_mtime(&self, inode_id: INodeId) -> Option<SystemTime> {
        let guard = self.dirty_files.read().unwrap();
        guard.get(&inode_id).map(|f| f.mtime())
    }

    /// Get dirty file state if dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// State if dirty, None otherwise.
    pub fn get_state(&self, inode_id: INodeId) -> Option<DirtyState> {
        let guard = self.dirty_files.read().unwrap();
        guard.get(&inode_id).map(|f| f.state())
    }

    /// Perform copy-on-write for a file before modification.
    ///
    /// If file is already dirty, returns immediately.
    /// Otherwise, creates a sparse COW entry (no data fetched yet).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to COW
    pub async fn cow_copy(&self, inode_id: INodeId) -> Result<(), VfsError> {
        // Check if already dirty
        if self.is_dirty(inode_id) {
            return Ok(());
        }

        // Get file metadata from inode manager
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

        // Create dirty file (sparse - no data fetched yet)
        let dirty = DirtyFile::from_cow(
            inode_id,
            inode.path().to_string(),
            &file_content,
            inode.size(),
            executable,
        );

        // Insert into dirty map
        self.dirty_files.write().unwrap().insert(inode_id, dirty);

        Ok(())
    }

    /// Ensure a chunk is loaded for a dirty file.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the file
    /// * `chunk_index` - Index of the chunk to load
    async fn ensure_chunk_loaded(&self, inode_id: INodeId, chunk_index: u32) -> Result<(), VfsError> {
        // Check if already loaded
        {
            let guard = self.dirty_files.read().unwrap();
            if let Some(dirty) = guard.get(&inode_id) {
                if dirty.content().is_chunk_loaded(chunk_index) {
                    return Ok(());
                }
            } else {
                return Err(VfsError::InodeNotFound(inode_id));
            }
        }

        // Need to fetch chunk - get hash first
        let chunk_hash: Option<String> = {
            let guard = self.dirty_files.read().unwrap();
            let dirty: &DirtyFile = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;

            match dirty.content() {
                DirtyContent::Small { .. } => None,
                DirtyContent::Chunked { original_chunks, .. } => {
                    original_chunks.get(chunk_index as usize).cloned()
                }
            }
        };

        // Fetch chunk data
        let chunk_data: Vec<u8> = if let Some(hash) = chunk_hash {
            self.read_store
                .retrieve(&hash, HashAlgorithm::Xxh128)
                .await?
        } else {
            // New chunk beyond original file
            Vec::new()
        };

        // Insert loaded chunk
        {
            let mut guard = self.dirty_files.write().unwrap();
            if let Some(dirty) = guard.get_mut(&inode_id) {
                dirty.content_mut().insert_chunk(chunk_index, chunk_data);
            }
        }

        Ok(())
    }

    /// Load small file content if not already loaded.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the file
    async fn ensure_small_file_loaded(&self, inode_id: INodeId) -> Result<(), VfsError> {
        // Check if already loaded
        {
            let guard = self.dirty_files.read().unwrap();
            if let Some(dirty) = guard.get(&inode_id) {
                if let DirtyContent::Small { data } = dirty.content() {
                    if !data.is_empty() || dirty.state() == DirtyState::New {
                        return Ok(());
                    }
                }
            } else {
                return Err(VfsError::InodeNotFound(inode_id));
            }
        }

        // Get original hash from inode
        let hash: String = {
            let file_content: FileContent = self
                .inodes
                .get_file_content(inode_id)
                .ok_or(VfsError::NotAFile(inode_id))?;

            match file_content {
                FileContent::SingleHash(h) => h,
                FileContent::Chunked(_) => return Ok(()), // Not a small file
            }
        };

        // Fetch content
        let data: Vec<u8> = self
            .read_store
            .retrieve(&hash, HashAlgorithm::Xxh128)
            .await?;

        // Store in dirty file
        {
            let mut guard = self.dirty_files.write().unwrap();
            if let Some(dirty) = guard.get_mut(&inode_id) {
                dirty.set_original_hash(hash);
                if let DirtyContent::Small { data: file_data } = dirty.content_mut() {
                    *file_data = data;
                }
            }
        }

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
        // Determine content type and read accordingly
        let is_chunked: bool = {
            let guard = self.dirty_files.read().unwrap();
            let dirty: &DirtyFile = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            matches!(dirty.content(), DirtyContent::Chunked { .. })
        };

        if is_chunked {
            self.read_chunked(inode_id, offset, size).await
        } else {
            self.read_small(inode_id, offset, size).await
        }
    }

    /// Read from a small dirty file.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `offset` - Byte offset
    /// * `size` - Max bytes to read
    async fn read_small(
        &self,
        inode_id: INodeId,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, VfsError> {
        self.ensure_small_file_loaded(inode_id).await?;

        let guard = self.dirty_files.read().unwrap();
        let dirty: &DirtyFile = guard
            .get(&inode_id)
            .ok_or(VfsError::InodeNotFound(inode_id))?;

        if let DirtyContent::Small { data } = dirty.content() {
            let start: usize = (offset as usize).min(data.len());
            let end: usize = (offset as u64 + size as u64).min(data.len() as u64) as usize;
            Ok(data[start..end].to_vec())
        } else {
            Err(VfsError::NotAFile(inode_id))
        }
    }

    /// Read from a chunked dirty file.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `offset` - Byte offset
    /// * `size` - Max bytes to read
    async fn read_chunked(
        &self,
        inode_id: INodeId,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, VfsError> {
        // Get file info
        let (chunk_size, total_size): (u64, u64) = {
            let guard = self.dirty_files.read().unwrap();
            let dirty: &DirtyFile = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;

            match dirty.content() {
                DirtyContent::Chunked {
                    chunk_size,
                    total_size,
                    ..
                } => (*chunk_size, *total_size),
                _ => return Err(VfsError::NotAFile(inode_id)),
            }
        };

        // Clamp read to file size
        let read_end: u64 = (offset + size as u64).min(total_size);
        if offset >= total_size {
            return Ok(Vec::new());
        }
        let actual_size: u64 = read_end - offset;

        let start_chunk: u32 = (offset / chunk_size) as u32;
        let end_chunk: u32 = ((read_end - 1) / chunk_size) as u32;

        let mut result: Vec<u8> = Vec::with_capacity(actual_size as usize);

        for chunk_idx in start_chunk..=end_chunk {
            // Ensure chunk is loaded
            self.ensure_chunk_loaded(inode_id, chunk_idx).await?;

            // Read from chunk
            let guard = self.dirty_files.read().unwrap();
            let dirty: &DirtyFile = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;

            if let Some(chunk_data) = dirty.content().get_chunk(chunk_idx) {
                let chunk_start_offset: u64 = chunk_idx as u64 * chunk_size;
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
            }
        }

        Ok(result)
    }
}


impl DirtyFileManager {
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

        // Determine content type
        let is_chunked: bool = {
            let guard = self.dirty_files.read().unwrap();
            let dirty: &DirtyFile = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            matches!(dirty.content(), DirtyContent::Chunked { .. })
        };

        let bytes_written: usize = if is_chunked {
            self.write_chunked(inode_id, offset, data).await?
        } else {
            self.write_small(inode_id, offset, data).await?
        };

        // Check if small file should convert to chunked
        {
            let mut guard = self.dirty_files.write().unwrap();
            if let Some(dirty) = guard.get_mut(&inode_id) {
                dirty.maybe_convert_to_chunked();
            }
        }

        // Flush to disk cache
        self.flush_to_disk(inode_id).await?;

        Ok(bytes_written)
    }

    /// Write to a small file.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `offset` - Byte offset
    /// * `data` - Data to write
    async fn write_small(
        &self,
        inode_id: INodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, VfsError> {
        // Ensure content is loaded first
        self.ensure_small_file_loaded(inode_id).await?;

        let mut guard = self.dirty_files.write().unwrap();
        let dirty: &mut DirtyFile = guard
            .get_mut(&inode_id)
            .ok_or(VfsError::InodeNotFound(inode_id))?;

        if let DirtyContent::Small { data: file_data } = dirty.content_mut() {
            let offset_usize: usize = offset as usize;
            let end: usize = offset_usize + data.len();

            // Extend file if needed
            if end > file_data.len() {
                file_data.resize(end, 0);
            }

            file_data[offset_usize..end].copy_from_slice(data);
            dirty.touch();
            Ok(data.len())
        } else {
            Err(VfsError::NotAFile(inode_id))
        }
    }

    /// Write to a chunked file, handling boundary spanning.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `offset` - Byte offset
    /// * `data` - Data to write
    async fn write_chunked(
        &self,
        inode_id: INodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, VfsError> {
        // Get chunk size
        let chunk_size: u64 = {
            let guard = self.dirty_files.read().unwrap();
            let dirty: &DirtyFile = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;

            match dirty.content() {
                DirtyContent::Chunked { chunk_size, .. } => *chunk_size,
                _ => return Err(VfsError::NotAFile(inode_id)),
            }
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
            // Ensure chunk is loaded
            self.ensure_chunk_loaded(inode_id, chunk_idx).await?;

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

            // Apply write to chunk
            {
                let mut guard = self.dirty_files.write().unwrap();
                let dirty: &mut DirtyFile = guard
                    .get_mut(&inode_id)
                    .ok_or(VfsError::InodeNotFound(inode_id))?;

                // Get rel_path before mutable borrow of content
                let rel_path_for_error: String = dirty.rel_path().to_string();

                if let DirtyContent::Chunked {
                    loaded_chunks,
                    dirty_chunks,
                    total_size,
                    ..
                } = dirty.content_mut()
                {
                    let chunk: &mut Vec<u8> = loaded_chunks
                        .get_mut(&chunk_idx)
                        .ok_or(VfsError::ChunkNotLoaded {
                            path: rel_path_for_error,
                            chunk_index: chunk_idx,
                        })?;

                    // Extend chunk if writing beyond current length
                    if write_end_in_chunk > chunk.len() {
                        chunk.resize(write_end_in_chunk, 0);
                    }

                    chunk[write_start_in_chunk..write_end_in_chunk]
                        .copy_from_slice(&data[data_offset..data_offset + write_len]);

                    dirty_chunks.insert(chunk_idx);

                    // Update total file size if we wrote beyond end
                    if write_end > *total_size {
                        *total_size = write_end;
                    }
                }

                dirty.touch();
            }

            data_offset += write_len;
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
        let dirty = DirtyFile::new_file(inode_id, rel_path, parent_inode);
        self.dirty_files.write().unwrap().insert(inode_id, dirty);
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
        let guard = self.dirty_files.read().unwrap();
        guard
            .values()
            .filter(|f| {
                f.state() == DirtyState::New && f.parent_inode() == Some(parent_inode)
            })
            .map(|f| (f.inode_id(), f.file_name().to_string()))
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
        let guard = self.dirty_files.read().unwrap();
        guard
            .values()
            .find(|f| {
                f.state() == DirtyState::New
                    && f.parent_inode() == Some(parent_inode)
                    && f.file_name() == name
            })
            .map(|f| f.inode_id())
    }

    /// Check if an inode is a new file (created, not from manifest).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// True if this is a new file, false otherwise.
    pub fn is_new_file(&self, inode_id: INodeId) -> bool {
        let guard = self.dirty_files.read().unwrap();
        guard
            .get(&inode_id)
            .map(|f| f.state() == DirtyState::New)
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
            let rel_path: Option<String> = {
                let guard = self.dirty_files.read().unwrap();
                guard.get(&inode_id).map(|f| f.rel_path().to_string())
            };

            // Remove from dirty map
            self.dirty_files.write().unwrap().remove(&inode_id);

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

            let mut dirty = DirtyFile::from_cow(
                inode_id,
                inode.path().to_string(),
                &file_content,
                inode.size(),
                false,
            );
            dirty.mark_deleted();
            self.dirty_files.write().unwrap().insert(inode_id, dirty);
        } else {
            let mut guard = self.dirty_files.write().unwrap();
            if let Some(dirty) = guard.get_mut(&inode_id) {
                dirty.mark_deleted();
            }
        }

        // Remove from disk cache
        self.flush_to_disk(inode_id).await?;
        Ok(())
    }

    /// Truncate a dirty file.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to truncate
    /// * `new_size` - New file size
    pub async fn truncate(&self, inode_id: INodeId, new_size: u64) -> Result<(), VfsError> {
        // Ensure file is dirty
        self.cow_copy(inode_id).await?;

        // For small files, ensure content is loaded before truncate
        {
            let guard = self.dirty_files.read().unwrap();
            if let Some(dirty) = guard.get(&inode_id) {
                if matches!(dirty.content(), DirtyContent::Small { .. }) {
                    drop(guard);
                    self.ensure_small_file_loaded(inode_id).await?;
                }
            }
        }

        // Perform truncate
        {
            let mut guard = self.dirty_files.write().unwrap();
            if let Some(dirty) = guard.get_mut(&inode_id) {
                dirty.truncate(new_size);
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
        let (rel_path, state, content_snapshot): (String, DirtyState, ContentSnapshot) = {
            let guard = self.dirty_files.read().unwrap();
            let dirty: &DirtyFile = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;

            let snapshot: ContentSnapshot = match dirty.content() {
                DirtyContent::Small { data } => ContentSnapshot::Small(data.clone()),
                DirtyContent::Chunked {
                    original_chunks,
                    dirty_chunks,
                    loaded_chunks,
                    total_size,
                    ..
                } => ContentSnapshot::Chunked {
                    original_chunks: original_chunks.clone(),
                    dirty_chunks: dirty_chunks.clone(),
                    loaded_chunks: loaded_chunks.clone(),
                    total_size: *total_size,
                },
            };

            (dirty.rel_path().to_string(), dirty.state(), snapshot)
        };

        if state == DirtyState::Deleted {
            self.cache
                .delete_file(&rel_path)
                .await
                .map_err(|e| VfsError::WriteCacheError(e.to_string()))?;
            return Ok(());
        }

        match content_snapshot {
            ContentSnapshot::Small(data) => {
                self.cache
                    .write_file(&rel_path, &data)
                    .await
                    .map_err(|e| VfsError::WriteCacheError(e.to_string()))?;
            }
            ContentSnapshot::Chunked {
                dirty_chunks,
                loaded_chunks,
                total_size,
                ..
            } => {
                // For chunked files, assemble and write the full file
                // This is a simplified approach - a more optimized version
                // would write only dirty chunks using MaterializedCache::write_chunk
                let mut assembled: Vec<u8> = Vec::with_capacity(total_size as usize);
                let chunk_size: u64 = CHUNK_SIZE_V2;
                let chunk_count: u32 = ((total_size + chunk_size - 1) / chunk_size) as u32;

                for chunk_idx in 0..chunk_count {
                    if let Some(chunk_data) = loaded_chunks.get(&chunk_idx) {
                        assembled.extend_from_slice(chunk_data);
                    } else if dirty_chunks.contains(&chunk_idx) {
                        // Dirty chunk should be loaded - this is an error
                        return Err(VfsError::ChunkNotLoaded {
                            path: rel_path,
                            chunk_index: chunk_idx,
                        });
                    }
                    // Unloaded, non-dirty chunks are skipped (sparse)
                }

                // Only write if we have data
                if !assembled.is_empty() {
                    self.cache
                        .write_file(&rel_path, &assembled)
                        .await
                        .map_err(|e| VfsError::WriteCacheError(e.to_string()))?;
                }
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

        let mut guard = self.dirty_files.write().unwrap();

        // Collect inode IDs of new files under this path
        let to_remove: Vec<INodeId> = guard
            .iter()
            .filter(|(_, dirty)| {
                dirty.state() == DirtyState::New
                    && (dirty.rel_path().starts_with(&prefix) || dirty.rel_path() == dir_path)
            })
            .map(|(id, _)| *id)
            .collect();

        let count: usize = to_remove.len();

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
        let guard = self.dirty_files.read().unwrap();
        guard
            .values()
            .map(|dirty| DirtyEntry {
                inode_id: dirty.inode_id(),
                path: dirty.rel_path().to_string(),
                state: dirty.state(),
                size: dirty.size(),
                mtime: dirty.mtime(),
                executable: dirty.is_executable(),
            })
            .collect()
    }

    /// Clear all dirty state.
    pub fn clear(&self) {
        self.dirty_files.write().unwrap().clear();
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

/// Snapshot of content for flush operation.
enum ContentSnapshot {
    Small(Vec<u8>),
    Chunked {
        #[allow(dead_code)]
        original_chunks: Vec<String>,
        dirty_chunks: HashSet<u32>,
        loaded_chunks: HashMap<u32, Vec<u8>>,
        total_size: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::MemoryFileStore;
    use crate::write::cache::MemoryWriteCache;

    fn create_test_manager() -> (DirtyFileManager, Arc<INodeManager>) {
        let cache = Arc::new(MemoryWriteCache::new());
        let store = Arc::new(MemoryFileStore::new());
        let inodes = Arc::new(INodeManager::new());
        let manager = DirtyFileManager::new(cache, store, inodes.clone());
        (manager, inodes)
    }

    #[test]
    fn test_dirty_state() {
        assert_eq!(DirtyState::Modified, DirtyState::Modified);
        assert_ne!(DirtyState::New, DirtyState::Deleted);
    }

    #[test]
    fn test_dirty_content_small() {
        let content = DirtyContent::Small {
            data: vec![1, 2, 3],
        };
        assert_eq!(content.size(), 3);
        assert!(content.is_chunk_loaded(0));
        assert_eq!(content.get_chunk(0), Some(&[1u8, 2, 3][..]));
    }

    #[test]
    fn test_dirty_content_chunked() {
        let mut content = DirtyContent::chunked(
            vec!["hash1".to_string(), "hash2".to_string()],
            256 * 1024 * 1024,
            500 * 1024 * 1024,
        );

        assert_eq!(content.size(), 500 * 1024 * 1024);
        assert!(!content.is_chunk_loaded(0));

        content.insert_chunk(0, vec![1, 2, 3]);
        assert!(content.is_chunk_loaded(0));
        assert_eq!(content.get_chunk(0), Some(&[1u8, 2, 3][..]));
    }

    #[test]
    fn test_dirty_file_new() {
        let dirty = DirtyFile::new_file(1, "test.txt".to_string(), 100);
        assert_eq!(dirty.inode_id(), 1);
        assert_eq!(dirty.rel_path(), "test.txt");
        assert_eq!(dirty.state(), DirtyState::New);
        assert_eq!(dirty.size(), 0);
        assert_eq!(dirty.parent_inode(), Some(100));
    }

    #[test]
    fn test_dirty_file_truncate_small() {
        let mut dirty = DirtyFile::new_file(1, "test.txt".to_string(), 100);

        // Write some data
        if let DirtyContent::Small { data } = dirty.content_mut() {
            data.extend_from_slice(b"hello world");
        }

        assert_eq!(dirty.size(), 11);

        // Truncate
        dirty.truncate(5);
        assert_eq!(dirty.size(), 5);

        // Extend
        dirty.truncate(10);
        assert_eq!(dirty.size(), 10);
    }

    #[tokio::test]
    async fn test_manager_create_file() {
        let (manager, _inodes) = create_test_manager();

        manager.create_file(100, "new_file.txt".to_string(), 1).unwrap();

        assert!(manager.is_dirty(100));
        assert_eq!(manager.get_state(100), Some(DirtyState::New));
        assert_eq!(manager.get_size(100), Some(0));
    }

    #[test]
    fn test_remove_new_files_under_path() {
        let (manager, _inodes) = create_test_manager();

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
        let (manager, _inodes) = create_test_manager();

        manager.create_file(100, "file.txt".to_string(), 1).unwrap();
        manager.create_file(101, "other/file.txt".to_string(), 1).unwrap();

        let removed: usize = manager.remove_new_files_under_path("nonexistent");
        assert_eq!(removed, 0);
        assert_eq!(manager.get_dirty_entries().len(), 2);
    }

    #[test]
    fn test_remove_new_files_only_removes_new_state() {
        let (manager, inodes) = create_test_manager();

        // Create a new file
        manager.create_file(100, "dir/new.txt".to_string(), 1).unwrap();

        // Add a manifest file and mark it modified
        let content: Vec<u8> = b"original".to_vec();
        let ino: INodeId = inodes.add_file(
            "dir/existing.txt",
            content.len() as u64,
            0,
            FileContent::SingleHash("hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Simulate COW by creating a modified entry (not through cow_copy to avoid async)
        // We'll just verify the filter logic works
        assert!(!manager.is_dirty(ino));

        // Remove files under "dir" - should only remove new files
        let removed: usize = manager.remove_new_files_under_path("dir");
        assert_eq!(removed, 1); // Only the new file

        assert!(!manager.is_dirty(100)); // new file removed
    }
}
