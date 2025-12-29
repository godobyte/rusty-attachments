# Rusty Attachments: VFS Write Support Design

**Status: ✅ CORE IMPLEMENTED | 🚧 DIFF EXPORT PENDING**

## Overview

This document extends the read-only VFS design with copy-on-write (COW) support, disk-based caching using materialized file paths, and on-demand diff manifest generation.

## Goals

1. **Copy-on-Write**: Modified files are copied to a writable layer before mutation
2. **Dual Storage**: Keep modified data in memory (fast access) AND on disk (persistence)
3. **Materialized Cache**: Store cached files using real relative paths (not CAS hashes)
4. **Diff Manifest Export**: Generate diff manifests on-demand via external API call

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Application Layer                                    │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  flush_diff_manifest() → DiffManifestExporter                       │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Layer 3: FUSE Interface                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  WritableVfs (fuser::Filesystem)                                    │    │
│  │    - write() → COW to dirty layer                                   │    │
│  │    - fsync() → flush to disk cache                                  │    │
│  │    - release() → finalize file state                                │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Layer 2: Write Layer                                 │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  DirtyFileManager                                                   │    │
│  │    - dirty_metadata: HashMap<INodeId, DirtyFileMetadata>            │    │
│  │    - pool: Arc<MemoryPool>  (unified content storage)               │    │
│  │    - cow_copy() → create sparse COW entry                           │    │
│  │    - write() → modify in pool + flush to disk                       │    │
│  │    - get_dirty_entries() → for diff manifest                        │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  MaterializedCache                                                  │    │
│  │    - cache_dir: PathBuf                                             │    │
│  │    - write_file(rel_path, data) → persist to disk                   │    │
│  │    - read_file(rel_path) → load from disk                           │    │
│  │    - delete_file(rel_path) → mark deleted                           │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Layer 1: Unified Memory Pool                         │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  MemoryPool (unified for read-only and dirty blocks)                │    │
│  │    - Hash-based blocks: read-only content from S3                   │    │
│  │    - Inode-based blocks: dirty content (needs flush before evict)   │    │
│  │  INodeManager (file metadata)                                       │    │
│  │  FileStore (S3 CAS retrieval)                                       │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Project Structure

```
crates/vfs/src/
├── lib.rs
├── error.rs              # VfsError enum with write variants
├── ...existing...
│
├── diskcache/            # Disk cache implementations
│   ├── mod.rs
│   ├── error.rs          # DiskCacheError
│   ├── traits.rs         # WriteCache trait
│   ├── materialized.rs   # MaterializedCache (dirty files)
│   ├── memory_cache.rs   # MemoryWriteCache (test impl)
│   └── read_cache.rs     # ReadCache (immutable CAS content)
│
├── write/                # Write support
│   ├── mod.rs
│   ├── dirty.rs          # DirtyFileMetadata, DirtyFileManager
│   ├── dirty_dir.rs      # DirtyDir, DirtyDirManager
│   ├── export.rs         # DiffManifestExporter trait
│   └── stats.rs          # WritableVfsStats
│
└── fuse_writable.rs      # WritableVfs (extends DeadlineVfs)
```

---

## Core Types

### WriteCache Trait (Abstraction for COW Disk Cache)

```rust
/// Trait for COW disk cache implementations.
///
/// Allows swapping cache backends for testing or alternative storage.
#[async_trait]
pub trait WriteCache: Send + Sync {
    /// Write file content to cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    /// * `data` - File content to write
    async fn write_file(&self, rel_path: &str, data: &[u8]) -> Result<(), WriteCacheError>;

    /// Read file content from cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    ///
    /// # Returns
    /// File content, or None if not in cache.
    async fn read_file(&self, rel_path: &str) -> Result<Option<Vec<u8>>, WriteCacheError>;

    /// Mark file as deleted (create tombstone).
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    async fn delete_file(&self, rel_path: &str) -> Result<(), WriteCacheError>;

    /// Check if file is marked as deleted.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    fn is_deleted(&self, rel_path: &str) -> bool;

    /// List all cached files (excluding deleted).
    ///
    /// # Returns
    /// Vector of relative paths.
    async fn list_files(&self) -> Result<Vec<String>, WriteCacheError>;

    /// Get cache directory path (for inspection/debugging).
    fn cache_dir(&self) -> &Path;
}

/// Errors from write cache operations.
#[derive(Debug, thiserror::Error)]
pub enum WriteCacheError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("File not found: {0}")]
    NotFound(String),

    #[error("Cache full")]
    CacheFull,
}

/// In-memory write cache for testing.
///
/// Stores all data in memory, no disk I/O.
pub struct MemoryWriteCache {
    files: RwLock<HashMap<String, Vec<u8>>>,
    deleted: RwLock<HashSet<String>>,
}

impl MemoryWriteCache {
    pub fn new() -> Self {
        Self {
            files: RwLock::new(HashMap::new()),
            deleted: RwLock::new(HashSet::new()),
        }
    }
}

#[async_trait]
impl WriteCache for MemoryWriteCache {
    async fn write_file(&self, rel_path: &str, data: &[u8]) -> Result<(), WriteCacheError> {
        self.deleted.write().unwrap().remove(rel_path);
        self.files.write().unwrap().insert(rel_path.to_string(), data.to_vec());
        Ok(())
    }

    async fn read_file(&self, rel_path: &str) -> Result<Option<Vec<u8>>, WriteCacheError> {
        Ok(self.files.read().unwrap().get(rel_path).cloned())
    }

    async fn delete_file(&self, rel_path: &str) -> Result<(), WriteCacheError> {
        self.files.write().unwrap().remove(rel_path);
        self.deleted.write().unwrap().insert(rel_path.to_string());
        Ok(())
    }

    fn is_deleted(&self, rel_path: &str) -> bool {
        self.deleted.read().unwrap().contains(rel_path)
    }

    async fn list_files(&self) -> Result<Vec<String>, WriteCacheError> {
        Ok(self.files.read().unwrap().keys().cloned().collect())
    }

    fn cache_dir(&self) -> &Path {
        Path::new("/dev/null") // No actual directory
    }
}
```

---

### DirtyFileMetadata

Metadata about a dirty file. Content is stored in the unified `MemoryPool`, not in this struct.

```rust
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
    ) -> Self;

    /// Create metadata for a new file (not from COW).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the file
    /// * `rel_path` - Relative path within VFS
    /// * `parent_inode` - Parent directory inode ID
    pub fn new_file(inode_id: INodeId, rel_path: String, parent_inode: INodeId) -> Self;

    // Getters
    pub fn inode_id(&self) -> INodeId;
    pub fn rel_path(&self) -> &str;
    pub fn parent_inode(&self) -> Option<INodeId>;
    pub fn file_name(&self) -> &str;
    pub fn state(&self) -> DirtyState;
    pub fn mtime(&self) -> SystemTime;
    pub fn size(&self) -> u64;
    pub fn is_executable(&self) -> bool;
    pub fn original_hashes(&self) -> &[String];
    pub fn chunk_size(&self) -> u64;
    pub fn chunk_count(&self) -> u32;
    pub fn is_chunk_dirty(&self, chunk_index: u32) -> bool;
    pub fn dirty_chunks(&self) -> &HashSet<u32>;

    // Mutators
    pub fn mark_chunk_dirty(&mut self, chunk_index: u32);
    pub fn mark_chunk_flushed(&mut self, chunk_index: u32);
    pub fn mark_all_flushed(&mut self);
    pub fn touch(&mut self);
    pub fn set_size(&mut self, size: u64);
    pub fn mark_deleted(&mut self);

    // Helpers
    pub fn is_small_file(&self) -> bool;
    pub fn original_hash(&self, chunk_index: u32) -> Option<&str>;
    pub fn chunk_index_for_offset(&self, offset: u64) -> u32;
    pub fn chunk_byte_range(&self, chunk_index: u32, file_offset: u64, size: u64) -> (usize, usize);
}
```

### DirtyFileManager

Manages all dirty files with COW semantics. Content is stored in the unified `MemoryPool`.

```rust
/// Manages dirty (modified) files with copy-on-write semantics.
///
/// Coordinates between in-memory dirty files and disk cache.
/// Content is stored in the unified memory pool for global memory management.
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
    read_cache: Option<Arc<ReadCache>>,
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
    ) -> Self;

    /// Set the read cache for immutable content.
    pub fn set_read_cache(&mut self, read_cache: Arc<ReadCache>);

    /// Get reference to the memory pool.
    pub fn pool(&self) -> &Arc<MemoryPool>;

    /// Check if a file is dirty.
    pub fn is_dirty(&self, inode_id: INodeId) -> bool;

    /// Get dirty file size if dirty.
    pub fn get_size(&self, inode_id: INodeId) -> Option<u64>;

    /// Get dirty file mtime if dirty.
    pub fn get_mtime(&self, inode_id: INodeId) -> Option<SystemTime>;

    /// Get dirty file state if dirty.
    pub fn get_state(&self, inode_id: INodeId) -> Option<DirtyState>;

    /// Perform copy-on-write for a file before modification.
    /// Creates a sparse COW entry (no data fetched yet).
    pub async fn cow_copy(&self, inode_id: INodeId) -> Result<(), VfsError>;

    /// Read from a dirty file.
    pub async fn read(&self, inode_id: INodeId, offset: u64, size: u32) -> Result<Vec<u8>, VfsError>;

    /// Write to a dirty file, performing COW if needed.
    pub async fn write(&self, inode_id: INodeId, offset: u64, data: &[u8]) -> Result<usize, VfsError>;

    /// Create a new file (not COW).
    pub fn create_file(&self, inode_id: INodeId, rel_path: String, parent_inode: INodeId) -> Result<(), VfsError>;

    /// Get new files in a directory.
    pub fn get_new_files_in_dir(&self, parent_inode: INodeId) -> Vec<(INodeId, String)>;

    /// Look up a new file by parent inode and name.
    pub fn lookup_new_file(&self, parent_inode: INodeId, name: &str) -> Option<INodeId>;

    /// Check if an inode is a new file.
    pub fn is_new_file(&self, inode_id: INodeId) -> bool;

    /// Mark a file as deleted.
    pub async fn delete_file(&self, inode_id: INodeId) -> Result<(), VfsError>;

    /// Truncate a file to a new size.
    pub async fn truncate(&self, inode_id: INodeId, new_size: u64) -> Result<(), VfsError>;

    /// Flush dirty file to disk cache.
    pub async fn flush_to_disk(&self, inode_id: INodeId) -> Result<(), VfsError>;

    /// Get all dirty entries for diff manifest generation.
    pub fn get_dirty_entries(&self) -> Vec<DirtyEntry>;

    /// Clear all dirty state.
    pub fn clear(&self);
}

        // Create metadata for COW (no data fetched yet - sparse COW)
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

    /// Write to a dirty file, performing COW if needed.
    ///
    /// Content is stored in the unified memory pool.
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

        // Flush to disk cache
        self.flush_to_disk(inode_id).await?;

        Ok(bytes_written)
    }

    /// Flush dirty file to disk cache.
    ///
    /// Assembles content from pool and writes to disk.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to flush
    pub async fn flush_to_disk(&self, inode_id: INodeId) -> Result<(), VfsError> {
        let (rel_path, state, size, chunk_count): (String, DirtyState, u64, u32) = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard
                .get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            (
                meta.rel_path().to_string(),
                meta.state(),
                meta.size(),
                meta.chunk_count(),
            )
        };

        if state == DirtyState::Deleted {
            self.cache.delete_file(&rel_path).await?;
            return Ok(());
        }

        // Assemble file content from pool
        let mut assembled: Vec<u8> = Vec::with_capacity(size as usize);
        for chunk_idx in 0..chunk_count {
            if let Some(handle) = self.pool.get_dirty(inode_id, chunk_idx) {
                assembled.extend_from_slice(handle.data());
            }
        }
        assembled.truncate(size as usize);

        self.cache.write_file(&rel_path, &assembled).await?;

        // Mark all chunks as flushed in pool
        for chunk_idx in 0..chunk_count {
            self.pool.mark_flushed(inode_id, chunk_idx);
        }

        Ok(())
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

    /// Mark a file as deleted.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to delete
    pub async fn delete_file(&self, inode_id: INodeId) -> Result<(), VfsError> {
        // Check if it's a new file - if so, just remove it entirely
        if self.is_new_file(inode_id) {
            self.pool.remove_inode_blocks(inode_id);
            self.dirty_metadata.write().unwrap().remove(&inode_id);
            return Ok(());
        }

        // If not dirty, create a deleted entry for manifest file
        if !self.is_dirty(inode_id) {
            let inode: Arc<dyn INode> = self.inodes.get(inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            let file_content: FileContent = self.inodes.get_file_content(inode_id)
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

    /// Get all dirty entries for diff manifest generation.
    ///
    /// # Returns
    /// Vector of dirty entry summaries.
    pub fn get_dirty_entries(&self) -> Vec<DirtyEntry> {
        let guard = self.dirty_metadata.read().unwrap();
        guard.values()
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
        let guard = self.dirty_metadata.read().unwrap();
        for inode_id in guard.keys() {
            self.pool.remove_inode_blocks(*inode_id);
        }
        drop(guard);
        self.dirty_metadata.write().unwrap().clear();
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
```

---

### MaterializedCache

Disk-based cache using real file paths with `.partN` suffix for sparse chunks.

```rust
/// Disk cache with hybrid storage strategy.
///
/// - Small files (single chunk): stored by relative path
/// - Large file chunks: stored as `{path}.part{N}` (only dirty chunks)
///
/// # Directory Structure
/// ```
/// cache_dir/
/// ├── .deleted/                    # Tombstones for deleted files
/// │   └── path/to/file             # Empty file marking deletion
/// ├── .meta/                       # Metadata for chunked files
/// │   └── path/to/large_video.mp4.json  # Chunk info
/// └── path/to/
///     ├── small_file.txt           # Small file - full content
///     ├── large_video.mp4.part4    # Only dirty chunk 4
///     └── large_video.mp4.part7    # Only dirty chunk 7
/// ```
pub struct MaterializedCache {
    /// Root directory for cache storage.
    cache_dir: PathBuf,
    /// Directory for deletion tombstones.
    deleted_dir: PathBuf,
    /// Directory for chunked file metadata.
    meta_dir: PathBuf,
}

/// Metadata for a chunked file in the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkedFileMeta {
    /// Total number of chunks.
    pub chunk_count: u32,
    /// Original chunk hashes (from manifest).
    pub original_hashes: Vec<String>,
    /// Which chunks are dirty (stored as .partN files).
    pub dirty_chunks: Vec<u32>,
    /// Total file size.
    pub total_size: u64,
}

impl MaterializedCache {
    /// Create a new materialized cache.
    pub fn new(cache_dir: PathBuf) -> std::io::Result<Self> {
        let deleted_dir: PathBuf = cache_dir.join(".deleted");
        let meta_dir: PathBuf = cache_dir.join(".meta");
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&deleted_dir)?;
        std::fs::create_dir_all(&meta_dir)?;

        Ok(Self {
            cache_dir,
            deleted_dir,
            meta_dir,
        })
    }

    /// Write a dirty chunk to cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    /// * `chunk_index` - Chunk index (0-based)
    /// * `data` - Chunk data
    pub fn write_chunk(
        &self,
        rel_path: &str,
        chunk_index: u32,
        data: &[u8],
    ) -> std::io::Result<()> {
        let chunk_path: PathBuf = self.chunk_path(rel_path, chunk_index);

        if let Some(parent) = chunk_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write atomically
        let temp_path: PathBuf = chunk_path.with_extension("tmp");
        std::fs::write(&temp_path, data)?;
        std::fs::rename(&temp_path, &chunk_path)?;

        Ok(())
    }

    /// Read a dirty chunk from cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    /// * `chunk_index` - Chunk index
    pub fn read_chunk(
        &self,
        rel_path: &str,
        chunk_index: u32,
    ) -> std::io::Result<Option<Vec<u8>>> {
        let chunk_path: PathBuf = self.chunk_path(rel_path, chunk_index);

        if chunk_path.exists() {
            Ok(Some(std::fs::read(&chunk_path)?))
        } else {
            Ok(None)
        }
    }

    /// Get path to a chunk file.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    /// * `chunk_index` - Chunk index
    fn chunk_path(&self, rel_path: &str, chunk_index: u32) -> PathBuf {
        self.cache_dir.join(format!("{}.part{}", rel_path, chunk_index))
    }

    /// Write metadata for a chunked file.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    /// * `meta` - Chunk metadata
    pub fn write_chunked_meta(
        &self,
        rel_path: &str,
        meta: &ChunkedFileMeta,
    ) -> std::io::Result<()> {
        let meta_path: PathBuf = self.meta_dir.join(format!("{}.json", rel_path));

        if let Some(parent) = meta_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json: String = serde_json::to_string_pretty(meta)?;
        std::fs::write(&meta_path, json)?;

        Ok(())
    }

    /// Read metadata for a chunked file.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    pub fn read_chunked_meta(&self, rel_path: &str) -> std::io::Result<Option<ChunkedFileMeta>> {
        let meta_path: PathBuf = self.meta_dir.join(format!("{}.json", rel_path));

        if meta_path.exists() {
            let json: String = std::fs::read_to_string(&meta_path)?;
            let meta: ChunkedFileMeta = serde_json::from_str(&json)?;
            Ok(Some(meta))
        } else {
            Ok(None)
        }
    }

    /// List all dirty chunks for a file.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    pub fn list_dirty_chunks(&self, rel_path: &str) -> std::io::Result<Vec<u32>> {
        let parent: PathBuf = self.cache_dir.join(
            Path::new(rel_path).parent().unwrap_or(Path::new(""))
        );
        let file_name: &str = Path::new(rel_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        let mut chunks: Vec<u32> = Vec::new();

        if parent.exists() {
            for entry in std::fs::read_dir(&parent)? {
                let entry = entry?;
                let name: String = entry.file_name().to_string_lossy().to_string();

                // Match pattern: {filename}.part{N}
                if let Some(rest) = name.strip_prefix(&format!("{}.", file_name)) {
                    if let Some(idx_str) = rest.strip_prefix("part") {
                        if let Ok(idx) = idx_str.parse::<u32>() {
                            chunks.push(idx);
                        }
                    }
                }
            }
        }

        chunks.sort();
        Ok(chunks)
    }
}
```

### Flush to Disk: Unified Pool Approach

Content is assembled from the unified memory pool and written to disk:

```rust
impl DirtyFileManager {
    /// Flush dirty file to disk cache.
    ///
    /// Assembles content from pool chunks and writes to materialized cache.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to flush
    async fn flush_to_disk(&self, inode_id: INodeId) -> Result<(), VfsError> {
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
            self.cache.delete_file(&rel_path).await?;
            return Ok(());
        }

        // Assemble file content from pool
        let mut assembled: Vec<u8> = Vec::with_capacity(size as usize);
        for chunk_idx in 0..chunk_count {
            if let Some(handle) = self.pool.get_dirty(inode_id, chunk_idx) {
                assembled.extend_from_slice(handle.data());
            } else if dirty_chunks.contains(&chunk_idx) {
                return Err(VfsError::ChunkNotLoaded {
                    path: rel_path,
                    chunk_index: chunk_idx,
                });
            }
        }
        assembled.truncate(size as usize);

        // Write to cache
        self.cache.write_file(&rel_path, &assembled).await?;

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
}
```

### Disk Layout Example

After modifying chunks 4 and 7 of a 2GB video:

```
cache_dir/
├── .meta/
│   └── renders/
│       └── final_video.mp4.json    # {"chunk_count":8,"dirty_chunks":[4,7],...}
└── renders/
    ├── final_video.mp4.part4       # 256MB - dirty chunk 4
    └── final_video.mp4.part7       # 256MB - dirty chunk 7
```

Total disk usage: 512MB (not 2GB)

#[async_trait]
impl WriteCache for MaterializedCache {
    async fn write_file(&self, rel_path: &str, data: &[u8]) -> Result<(), WriteCacheError> {
        let full_path: PathBuf = self.cache_dir.join(rel_path);

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Remove any deletion tombstone
        let tombstone: PathBuf = self.deleted_dir.join(rel_path);
        if tombstone.exists() {
            std::fs::remove_file(&tombstone)?;
        }

        // Write file atomically (write to temp, then rename)
        let temp_path: PathBuf = full_path.with_extension("tmp");
        std::fs::write(&temp_path, data)?;
        std::fs::rename(&temp_path, &full_path)?;

        Ok(())
    }

    async fn read_file(&self, rel_path: &str) -> Result<Option<Vec<u8>>, WriteCacheError> {
        let full_path: PathBuf = self.cache_dir.join(rel_path);

        if full_path.exists() {
            Ok(Some(std::fs::read(&full_path)?))
        } else {
            Ok(None)
        }
    }

    async fn delete_file(&self, rel_path: &str) -> Result<(), WriteCacheError> {
        // Remove actual file if exists
        let full_path: PathBuf = self.cache_dir.join(rel_path);
        if full_path.exists() {
            std::fs::remove_file(&full_path)?;
        }

        // Create tombstone
        let tombstone: PathBuf = self.deleted_dir.join(rel_path);
        if let Some(parent) = tombstone.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tombstone, b"")?;

        Ok(())
    }

    fn is_deleted(&self, rel_path: &str) -> bool {
        self.deleted_dir.join(rel_path).exists()
    }

    async fn list_files(&self) -> Result<Vec<String>, WriteCacheError> {
        let mut files: Vec<String> = Vec::new();
        self.walk_dir(&self.cache_dir, "", &mut files)?;
        Ok(files)
    }

    fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

impl MaterializedCache {
    /// Create a new materialized cache.
    ///
    /// # Arguments
    /// * `cache_dir` - Root directory for cache storage
    ///
    /// # Returns
    /// New cache instance. Creates directories if needed.
    pub fn new(cache_dir: PathBuf) -> std::io::Result<Self> {
        let deleted_dir: PathBuf = cache_dir.join(".deleted");
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&deleted_dir)?;

        Ok(Self {
            cache_dir,
            deleted_dir,
        })
    }

    /// Write file content to cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    /// * `data` - File content to write
    pub fn write_file(&self, rel_path: &str, data: &[u8]) -> std::io::Result<()> {
        let full_path: PathBuf = self.cache_dir.join(rel_path);

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Remove any deletion tombstone
        let tombstone: PathBuf = self.deleted_dir.join(rel_path);
        if tombstone.exists() {
            std::fs::remove_file(&tombstone)?;
        }

        // Write file atomically (write to temp, then rename)
        let temp_path: PathBuf = full_path.with_extension("tmp");
        std::fs::write(&temp_path, data)?;
        std::fs::rename(&temp_path, &full_path)?;

        Ok(())
    }

    /// Read file content from cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    ///
    /// # Returns
    /// File content, or None if not in cache.
    pub fn read_file(&self, rel_path: &str) -> std::io::Result<Option<Vec<u8>>> {
        let full_path: PathBuf = self.cache_dir.join(rel_path);

        if full_path.exists() {
            Ok(Some(std::fs::read(&full_path)?))
        } else {
            Ok(None)
        }
    }

    /// Mark file as deleted (create tombstone).
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    pub fn delete_file(&self, rel_path: &str) -> std::io::Result<()> {
        // Remove actual file if exists
        let full_path: PathBuf = self.cache_dir.join(rel_path);
        if full_path.exists() {
            std::fs::remove_file(&full_path)?;
        }

        // Create tombstone
        let tombstone: PathBuf = self.deleted_dir.join(rel_path);
        if let Some(parent) = tombstone.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tombstone, b"")?;

        Ok(())
    }

    /// Check if file is marked as deleted.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path within VFS
    pub fn is_deleted(&self, rel_path: &str) -> bool {
        self.deleted_dir.join(rel_path).exists()
    }

    /// Get cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// List all cached files (excluding deleted).
    ///
    /// # Returns
    /// Vector of relative paths.
    pub fn list_files(&self) -> std::io::Result<Vec<String>> {
        let mut files: Vec<String> = Vec::new();
        self.walk_dir(&self.cache_dir, "", &mut files)?;
        Ok(files)
    }

    /// Recursively walk directory collecting file paths.
    fn walk_dir(
        &self,
        dir: &Path,
        prefix: &str,
        files: &mut Vec<String>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name: String = entry.file_name().to_string_lossy().to_string();

            // Skip .deleted directory
            if name == ".deleted" && prefix.is_empty() {
                continue;
            }

            let rel_path: String = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", prefix, name)
            };

            if entry.file_type()?.is_dir() {
                self.walk_dir(&entry.path(), &rel_path, files)?;
            } else {
                files.push(rel_path);
            }
        }
        Ok(())
    }
}
```

---

## Diff Manifest Export

### DiffManifestExporter Trait

Trait for generating diff manifests from dirty state.

```rust
/// Trait for exporting dirty VFS state as a diff manifest.
///
/// Implemented by WritableVfs to allow external callers to
/// trigger diff manifest generation on demand.
#[async_trait]
pub trait DiffManifestExporter: Send + Sync {
    /// Generate a diff manifest from current dirty state.
    ///
    /// # Arguments
    /// * `parent_manifest` - The original manifest this VFS was mounted from
    /// * `parent_encoded` - Canonical JSON encoding of parent (for hash)
    ///
    /// # Returns
    /// Diff manifest containing all changes since mount.
    ///
    /// # Note
    /// This does NOT clear dirty state. Call `clear_dirty()` after
    /// successfully uploading the diff manifest.
    async fn export_diff_manifest(
        &self,
        parent_manifest: &Manifest,
        parent_encoded: &str,
    ) -> Result<Manifest, VfsError>;

    /// Clear all dirty state after successful export.
    ///
    /// Call this after the diff manifest has been successfully
    /// uploaded to S3 to reset the dirty tracking.
    fn clear_dirty(&self) -> Result<(), VfsError>;

    /// Get summary of dirty files without generating full manifest.
    ///
    /// # Returns
    /// Count of new, modified, and deleted files.
    fn dirty_summary(&self) -> DirtySummary;
}

/// Summary of dirty file counts.
#[derive(Debug, Clone, Default)]
pub struct DirtySummary {
    pub new_count: usize,
    pub modified_count: usize,
    pub deleted_count: usize,
}

impl DirtySummary {
    /// Total number of dirty files.
    pub fn total(&self) -> usize {
        self.new_count + self.modified_count + self.deleted_count
    }

    /// Returns true if there are any dirty files.
    pub fn has_changes(&self) -> bool {
        self.total() > 0
    }
}
```

### Export Implementation

The diff manifest export must handle chunked files correctly. A modified chunked file needs:
1. Full file assembly (dirty + unmodified chunks)
2. New hash of the complete content
3. The assembled file stored for CAS upload

```rust
impl DiffManifestExporter for WritableVfs {
    async fn export_diff_manifest(
        &self,
        parent_manifest: &Manifest,
        parent_encoded: &str,
    ) -> Result<Manifest, VfsError> {
        use rusty_attachments_common::{hash_data, hash_string};
        use rusty_attachments_model::{
            v2025_12_04::{AssetManifest, ManifestDirectoryPath, ManifestFilePath},
            HashAlgorithm, ManifestType, ManifestVersion,
        };

        // Compute parent manifest hash
        let parent_hash: String = hash_string(parent_encoded, HashAlgorithm::Xxh128);

        // Collect dirty entries
        let dirty_entries: Vec<DirtyEntry> = self.dirty_manager.get_dirty_entries();

        let mut files: Vec<ManifestFilePath> = Vec::new();
        let mut dirs: Vec<ManifestDirectoryPath> = Vec::new();
        let mut total_size: u64 = 0;

        // Track directories that need to be created
        let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();

        for entry in dirty_entries {
            match entry.state {
                DirtyState::New => {
                    // New file - must hash entire content
                    let file_entry: ManifestFilePath = self.hash_new_file(&entry).await?;
                    total_size += file_entry.size.unwrap_or(0);
                    files.push(file_entry);
                }
                DirtyState::Modified => {
                    // Modified file - reuse unmodified chunk hashes
                    let file_entry: ManifestFilePath = self.hash_modified_file(&entry).await?;
                    total_size += file_entry.size.unwrap_or(0);
                    files.push(file_entry);
                }
                DirtyState::Deleted => {
                    files.push(ManifestFilePath::deleted(&entry.path));
                    continue; // Skip directory tracking for deleted files
                }
            }

            // Track parent directories for new/modified files
            let mut current: &str = &entry.path;
            while let Some(idx) = current.rfind('/') {
                let dir: &str = &current[..idx];
                if seen_dirs.insert(dir.to_string()) {
                    if !self.original_dirs.contains(dir) {
                        dirs.push(ManifestDirectoryPath {
                            path: dir.to_string(),
                            deleted: false,
                        });
                    }
                }
                current = dir;
            }
        }

        // Check for deleted directories
        for dir in &self.original_dirs {
            if !seen_dirs.contains(dir.as_str()) {
                dirs.push(ManifestDirectoryPath::deleted(dir));
            }
        }

        Ok(Manifest::V2025_12_04_beta(AssetManifest {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2025_12_04_beta,
            manifest_type: ManifestType::Diff,
            dirs,
            paths: files,
            total_size,
            parent_manifest_hash: Some(parent_hash),
        }))
    }

impl WritableVfs {
    /// Hash a new file (not from original manifest).
    ///
    /// # Arguments
    /// * `entry` - Dirty entry for the new file
    async fn hash_new_file(&self, entry: &DirtyEntry) -> Result<ManifestFilePath, VfsError> {
        let data: Vec<u8> = self.dirty_manager.cache
            .read_file(&entry.path).await?
            .ok_or_else(|| VfsError::CacheReadFailed(entry.path.clone()))?;

        let mtime_us: i64 = to_mtime_micros(entry.mtime);

        // Check if file needs chunking
        if data.len() as u64 > CHUNK_SIZE_V2 {
            let chunkhashes: Vec<String> = compute_chunk_hashes(&data);
            Ok(ManifestFilePath {
                path: entry.path.clone(),
                hash: None, // Chunked files don't have single hash
                size: Some(entry.size),
                mtime: Some(mtime_us),
                runnable: entry.executable,
                chunkhashes: Some(chunkhashes),
                symlink_target: None,
                deleted: false,
            })
        } else {
            let hash: String = hash_data(&data, HashAlgorithm::Xxh128);
            Ok(ManifestFilePath {
                path: entry.path.clone(),
                hash: Some(hash),
                size: Some(entry.size),
                mtime: Some(mtime_us),
                runnable: entry.executable,
                chunkhashes: None,
                symlink_target: None,
                deleted: false,
            })
        }
    }

    /// Hash a modified file, reusing original chunk hashes where possible.
    ///
    /// For chunked files, only dirty chunks are re-hashed. Unmodified chunks
    /// retain their original hashes from the manifest - NO DOWNLOAD NEEDED.
    ///
    /// # Arguments
    /// * `entry` - Dirty entry for the modified file
    async fn hash_modified_file(&self, entry: &DirtyEntry) -> Result<ManifestFilePath, VfsError> {
        // Get metadata from dirty manager
        let (original_hashes, dirty_chunks, size): (Vec<String>, HashSet<u32>, u64) = {
            let guard = self.dirty_manager.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard.get(&entry.inode_id)
                .ok_or(VfsError::InodeNotFound(entry.inode_id))?;
            (
                meta.original_hashes().to_vec(),
                meta.dirty_chunks().clone(),
                meta.size(),
            )
        };

        let mtime_us: i64 = to_mtime_micros(entry.mtime);

        // Small file (single chunk) - hash from pool
        if original_hashes.len() <= 1 {
            if let Some(handle) = self.dirty_manager.pool().get_dirty(entry.inode_id, 0) {
                let hash: String = hash_data(handle.data(), HashAlgorithm::Xxh128);
                return Ok(ManifestFilePath {
                    path: entry.path.clone(),
                    hash: Some(hash),
                    size: Some(size),
                    mtime: Some(mtime_us),
                    runnable: entry.executable,
                    chunkhashes: None,
                    symlink_target: None,
                    deleted: false,
                });
            }
        }

        // Large file - only hash dirty chunks, reuse original hashes
        let mut new_chunkhashes: Vec<String> = Vec::with_capacity(original_hashes.len());

        for (chunk_idx, original_hash) in original_hashes.iter().enumerate() {
            let chunk_idx: u32 = chunk_idx as u32;

            if dirty_chunks.contains(&chunk_idx) {
                // Dirty chunk - must re-hash from pool
                let handle = self.dirty_manager.pool().get_dirty(entry.inode_id, chunk_idx)
                    .ok_or_else(|| VfsError::ChunkNotLoaded {
                        path: entry.path.clone(),
                        chunk_index: chunk_idx,
                    })?;
                let new_hash: String = hash_data(handle.data(), HashAlgorithm::Xxh128);
                new_chunkhashes.push(new_hash);
            } else {
                // Unmodified chunk - REUSE ORIGINAL HASH (no download!)
                new_chunkhashes.push(original_hash.clone());
            }
        }

        Ok(ManifestFilePath {
            path: entry.path.clone(),
            hash: None,
            size: Some(size),
            mtime: Some(mtime_us),
            runnable: entry.executable,
            chunkhashes: Some(new_chunkhashes),
            symlink_target: None,
            deleted: false,
        })
    }
}

/// Compute chunk hashes for data.
fn compute_chunk_hashes(data: &[u8]) -> Vec<String> {
    let chunk_size: usize = CHUNK_SIZE_V2 as usize;
    data.chunks(chunk_size)
        .map(|chunk| hash_data(chunk, HashAlgorithm::Xxh128))
        .collect()
}

/// Convert SystemTime to microseconds since epoch.
fn to_mtime_micros(time: SystemTime) -> i64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}
```

### Diff Manifest Export: Chunk Hash Optimization

For a 2GB file (8 chunks) where only chunk 4 was modified:

| Chunk | Original Hash | Action | New Hash |
|-------|---------------|--------|----------|
| 0 | `abc123...` | **Reuse** | `abc123...` |
| 1 | `def456...` | **Reuse** | `def456...` |
| 2 | `ghi789...` | **Reuse** | `ghi789...` |
| 3 | `jkl012...` | **Reuse** | `jkl012...` |
| 4 | `mno345...` | **Re-hash** | `xyz999...` |
| 5 | `pqr678...` | **Reuse** | `pqr678...` |
| 6 | `stu901...` | **Reuse** | `stu901...` |
| 7 | `vwx234...` | **Reuse** | `vwx234...` |

**Result**: Only 1 chunk hashed (already in memory), 7 chunks reuse original hashes. No S3 downloads needed for unmodified chunks during export.

    // ... other methods ...
}

impl WritableVfs {
    /// Assemble full file content from pool and compute hash.
    ///
    /// For chunked files, this fetches any unloaded chunks from S3,
    /// combines with dirty chunks from the pool, writes the assembled
    /// file to the materialized cache, and returns the full content with its hash.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the dirty file
    ///
    /// # Returns
    /// Tuple of (assembled_data, xxh128_hash)
    async fn assemble_and_hash_file(
        &self,
        inode_id: INodeId,
    ) -> Result<(Vec<u8>, String), VfsError> {
        // Get metadata
        let (rel_path, size, chunk_count, original_hashes): (String, u64, u32, Vec<String>) = {
            let guard = self.dirty_manager.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard.get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            (
                meta.rel_path().to_string(),
                meta.size(),
                meta.chunk_count(),
                meta.original_hashes().to_vec(),
            )
        };

        // Assemble content from pool
        let mut assembled: Vec<u8> = Vec::with_capacity(size as usize);

        for chunk_idx in 0..chunk_count {
            let chunk_data: Vec<u8> = if let Some(handle) = self.dirty_manager.pool().get_dirty(inode_id, chunk_idx) {
                // Use data from pool (dirty or loaded)
                handle.data().to_vec()
            } else if (chunk_idx as usize) < original_hashes.len() {
                // Fetch unmodified chunk from S3
                let hash: &str = &original_hashes[chunk_idx as usize];
                self.dirty_manager.read_store()
                    .retrieve(hash, HashAlgorithm::Xxh128)
                    .await?
            } else {
                // New chunk beyond original - should be in pool
                return Err(VfsError::ChunkNotLoaded {
                    path: rel_path,
                    chunk_index: chunk_idx,
                });
            };

            assembled.extend(chunk_data);
        }

        assembled.truncate(size as usize);

        // Compute hash of full content
        let hash: String = hash_data(&assembled, HashAlgorithm::Xxh128);

        // Write assembled file to materialized cache for later upload
        self.dirty_manager.cache()
            .write_file(&rel_path, &assembled)
            .await?;

        Ok((assembled, hash))
    }

    /// Compute chunk hashes for a large file.
    ///
    /// # Arguments
    /// * `data` - Full file content
    ///
    /// # Returns
    /// Vector of xxh128 hashes, one per 256MB chunk.
    async fn compute_chunk_hashes(&self, data: &[u8]) -> Result<Vec<String>, VfsError> {
        let chunk_size: usize = CHUNK_SIZE_V2 as usize;
        let mut hashes: Vec<String> = Vec::new();

        for chunk in data.chunks(chunk_size) {
            let hash: String = hash_data(chunk, HashAlgorithm::Xxh128);
            hashes.push(hash);
        }

        Ok(hashes)
    }
}
```

---

## Diff Manifest Semantics

### What Goes in a Diff Manifest?

A diff manifest contains entries for files that have **changed** relative to the parent:

| Change Type | Diff Entry | CAS Upload Required |
|-------------|------------|---------------------|
| New file | Full entry (hash, size, mtime) | Yes - upload new content |
| Modified file | Full entry with NEW hash | Yes - upload new content |
| Deleted file | `deleted: true` | No |
| Unchanged file | **Not included** | No |

### Modified Chunked File

When a chunked file is modified (even just one chunk), the diff manifest entry contains:

```json
{
  "path": "large_video.mp4",
  "hash": "abc123...",           // NEW hash of entire assembled file
  "size": 2147483648,            // 2GB
  "mtime": 1703001200000000,
  "chunkhashes": [               // NEW chunk hashes
    "chunk0_new_hash",           // May be same as original if unmodified
    "chunk1_new_hash",
    "chunk2_new_hash",
    "chunk3_modified_hash",      // This chunk was modified
    "chunk4_new_hash",
    "chunk5_new_hash",
    "chunk6_new_hash",
    "chunk7_new_hash"
  ]
}
```

### CAS Upload After Export

After `export_diff_manifest()`, the caller must upload the modified content to S3 CAS:

```rust
async fn upload_diff_changes(
    diff: &Manifest,
    cache: &MaterializedCache,
    storage: &dyn StorageClient,
    location: &S3Location,
) -> Result<(), Error> {
    for file in diff.files()? {
        if file.deleted {
            continue; // Nothing to upload for deletions
        }

        // Read assembled file from materialized cache
        let data: Vec<u8> = cache.read_file(&file.path).await?
            .ok_or(Error::CacheMiss(file.path.clone()))?;

        if let Some(ref chunkhashes) = file.chunkhashes {
            // Upload each chunk to CAS
            let chunk_size: usize = CHUNK_SIZE_V2 as usize;
            for (idx, chunk_hash) in chunkhashes.iter().enumerate() {
                let start: usize = idx * chunk_size;
                let end: usize = (start + chunk_size).min(data.len());
                let chunk: &[u8] = &data[start..end];

                let key: String = location.cas_key(chunk_hash, HashAlgorithm::Xxh128);
                storage.put_object(&location.bucket, &key, chunk).await?;
            }
        } else if let Some(ref hash) = file.hash {
            // Upload single file to CAS
            let key: String = location.cas_key(hash, HashAlgorithm::Xxh128);
            storage.put_object(&location.bucket, &key, &data).await?;
        }
    }

    Ok(())
}
```

### Chunk Deduplication

Even though we upload all chunks, CAS provides natural deduplication:

- Unmodified chunks have the **same hash** as the original
- S3 CAS `put_object` can skip if key already exists (or use conditional PUT)
- Only truly modified chunks result in new S3 objects

```rust
/// Upload chunk to CAS, skipping if already exists.
async fn upload_chunk_if_missing(
    storage: &dyn StorageClient,
    location: &S3Location,
    hash: &str,
    data: &[u8],
) -> Result<UploadResult, Error> {
    let key: String = location.cas_key(hash, HashAlgorithm::Xxh128);

    // Check if already exists (HEAD request)
    if storage.head_object(&location.bucket, &key).await.is_ok() {
        return Ok(UploadResult::Skipped);
    }

    storage.put_object(&location.bucket, &key, data).await?;
    Ok(UploadResult::Uploaded)
}
```

### Applying a Diff with Modified Chunked File

When a consumer applies this diff to the parent snapshot:

1. The diff entry **replaces** the parent's entry for that path
2. The new `chunkhashes` point to the new chunk content in CAS
3. Some chunk hashes may be identical to the original (unmodified chunks)
4. CAS deduplication means those chunks aren't stored twice

```
Parent manifest:
  large_video.mp4: chunkhashes = [A, B, C, D, E, F, G, H]

Diff manifest (chunk 3 modified):
  large_video.mp4: chunkhashes = [A, B, C, D', E, F, G, H]
                                         ↑
                                    Only D' is new in CAS

Applied result:
  large_video.mp4: chunkhashes = [A, B, C, D', E, F, G, H]
```

### DiffManifestExporter Remaining Methods

```rust
impl DiffManifestExporter for WritableVfs {
    // export_diff_manifest() defined above

    fn clear_dirty(&self) -> Result<(), VfsError> {
        self.dirty_manager.clear();
        Ok(())
    }

    fn dirty_summary(&self) -> DirtySummary {
        let entries: Vec<DirtyEntry> = self.dirty_manager.get_dirty_entries();
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
```

---

## WritableVfs (FUSE Implementation)

```rust
/// Writable VFS with copy-on-write support.
///
/// Extends the read-only DeadlineVfs with write operations.
/// Modified files are stored both in memory (fast access) and
/// on disk (persistence).
pub struct WritableVfs {
    /// Read-only base VFS.
    base: DeadlineVfs,
    /// Dirty file manager (COW layer).
    dirty_manager: Arc<DirtyFileManager>,
    /// Original directories from manifest (for diff tracking).
    original_dirs: HashSet<String>,
    /// Options for write behavior.
    write_options: WriteOptions,
}

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

impl WritableVfs {
    /// Create a new writable VFS.
    ///
    /// # Arguments
    /// * `manifest` - Manifest to mount
    /// * `store` - File store for reading original content
    /// * `options` - VFS options
    /// * `write_options` - Write-specific options
    /// * `pool` - Unified memory pool for content storage
    pub fn new(
        manifest: &Manifest,
        store: Arc<dyn FileStore>,
        options: VfsOptions,
        write_options: WriteOptions,
        pool: Arc<MemoryPool>,
    ) -> Result<Self, VfsError> {
        let base = DeadlineVfs::new(manifest, store.clone(), options)?;

        let cache = Arc::new(MaterializedCache::new(write_options.cache_dir.clone())?);
        let dirty_manager = Arc::new(DirtyFileManager::new(
            cache,
            store,
            base.inodes.clone(),
            pool,
        ));

        // Collect original directories for diff tracking
        let original_dirs: HashSet<String> = manifest.dirs()
            .map(|dirs| dirs.iter().map(|d| d.path.clone()).collect())
            .unwrap_or_default();

        Ok(Self {
            base,
            dirty_manager,
            original_dirs,
            write_options,
        })
    }

    /// Get the diff manifest exporter interface.
    pub fn exporter(&self) -> Arc<dyn DiffManifestExporter> {
        Arc::new(self.clone()) // Requires Clone impl
    }
}

impl fuser::Filesystem for WritableVfs {
    // Read operations delegate to base or dirty layer

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        // Check dirty layer first
        if self.dirty_manager.is_dirty(ino) {
            let rt = tokio::runtime::Handle::current();
            match rt.block_on(self.dirty_manager.read(ino, offset as u64, size)) {
                Ok(data) => reply.data(&data),
                Err(e) => {
                    tracing::error!("Read failed for inode {}: {}", ino, e);
                    reply.error(libc::EIO);
                }
            }
            return;
        }

        // Fall back to base VFS
        self.base.read(_req, ino, fh, offset, size, _flags, _lock, reply);
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
        // Perform COW and write
        let rt = tokio::runtime::Handle::current();
        match rt.block_on(self.dirty_manager.write(ino, offset as u64, data)) {
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
            let rt = tokio::runtime::Handle::current();

            // Truncate via dirty manager (handles COW internally)
            if let Err(e) = rt.block_on(self.dirty_manager.truncate(ino, new_size)) {
                tracing::error!("Truncate failed for inode {}: {}", ino, e);
                reply.error(libc::EIO);
                return;
            }
        }

        // Return updated attributes
        if let Some(inode) = self.base.inodes.get(ino) {
            let mut attr: FileAttr = inode.to_fuser_attr();

            // Override with dirty state if applicable
            if let Some(size) = self.dirty_manager.get_size(ino) {
                attr.size = size;
            }
            if let Some(mtime) = self.dirty_manager.get_mtime(ino) {
                attr.mtime = mtime;
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
        flags: i32,
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
        let parent_path: String = match self.base.inodes.get(parent) {
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

        // Allocate new inode
        let new_ino: INodeId = self.base.inodes.allocate_id();

        // Create dirty file entry (with parent inode for directory tracking)
        if let Err(e) = self.dirty_manager.create_file(new_ino, new_path.clone(), parent) {
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
        let fh: u64 = self.base.allocate_handle(new_ino);

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

        // Find inode by name in parent
        let ino: INodeId = match self.base.inodes.lookup_child(parent, name_str) {
            Some(id) => id,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Mark as deleted
        let rt = tokio::runtime::Handle::current();
        if let Err(e) = rt.block_on(self.dirty_manager.delete_file(ino)) {
            tracing::error!("Failed to delete file: {}", e);
            reply.error(libc::EIO);
            return;
        }

        reply.ok();
    }

    fn fsync(&mut self, _req: &Request, ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty) {
        // Flush dirty file to disk
        let rt = tokio::runtime::Handle::current();
        if let Err(e) = rt.block_on(self.dirty_manager.flush_to_disk(ino)) {
            tracing::error!("fsync failed: {}", e);
            reply.error(libc::EIO);
            return;
        }

        reply.ok();
    }

    // Delegate other read operations to base
    fn lookup(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        self.base.lookup(req, parent, name, reply);
    }

    fn getattr(&mut self, req: &Request, ino: u64, reply: ReplyAttr) {
        // Check dirty layer for updated attributes
        if self.dirty_manager.is_dirty(ino) {
            if let Some(inode) = self.base.inodes.get(ino) {
                let mut attr: FileAttr = inode.to_fuser_attr();
                if let Some(size) = self.dirty_manager.get_size(ino) {
                    attr.size = size;
                }
                if let Some(mtime) = self.dirty_manager.get_mtime(ino) {
                    attr.mtime = mtime;
                }
                reply.attr(&TTL, &attr);
                return;
            }
        }

        self.base.getattr(req, ino, reply);
    }

    fn readdir(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        reply: ReplyDirectory,
    ) {
        self.base.readdir(req, ino, fh, offset, reply);
    }

    fn open(&mut self, req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        self.base.open(req, ino, flags, reply);
    }

    fn release(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        flags: i32,
        lock_owner: Option<u64>,
        flush: bool,
        reply: ReplyEmpty,
    ) {
        self.base.release(req, ino, fh, flags, lock_owner, flush, reply);
    }
}
```

---

## Usage Example

```rust
use std::sync::Arc;
use rusty_attachments_model::Manifest;
use rusty_attachments_vfs::{
    WritableVfs, WriteOptions, VfsOptions, DiffManifestExporter,
    MemoryPool, MemoryPoolConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load manifest
    let json: &str = include_str!("manifest.json");
    let manifest = Manifest::decode(json)?;
    let manifest_encoded: String = manifest.encode()?;

    // Create storage client (existing infrastructure)
    let store: Arc<dyn FileStore> = create_storage_client().await?;

    // Create unified memory pool
    let pool = Arc::new(MemoryPool::new(MemoryPoolConfig::default()));

    // Configure write options
    let write_options = WriteOptions {
        cache_dir: PathBuf::from("/tmp/vfs-output"),
        sync_on_write: true,
        ..Default::default()
    };

    // Create writable VFS with unified pool
    let vfs = WritableVfs::new(
        &manifest,
        store,
        VfsOptions::default(),
        write_options,
        pool,
    )?;

    // Get exporter for later use
    let exporter: Arc<dyn DiffManifestExporter> = vfs.exporter();

    // Mount VFS (in background)
    let session = fuser::spawn_mount2(
        vfs,
        "/mnt/assets",
        &[MountOption::RW],
    )?;

    // ... application runs, files are modified via FUSE ...

    // On-demand: export diff manifest
    let diff: Manifest = exporter.export_diff_manifest(&manifest, &manifest_encoded).await?;
    println!("Diff manifest: {} changes", diff.file_count());

    // Upload diff manifest to S3 (using existing storage infrastructure)
    upload_manifest(&diff).await?;

    // Clear dirty state after successful upload
    exporter.clear_dirty()?;

    // Unmount
    session.join();

    Ok(())
}
```


---

## Large Files (>256MB) in COW Cache

V2 manifests support chunked files where files larger than 256MB are split into multiple chunks, each with its own hash. The unified memory pool handles these with sparse chunk tracking to avoid fetching the entire file on first write.

### Read Path vs Write Path (Unified Pool)

| Aspect | Read Path | Write Path |
|--------|-----------|------------|
| Storage | `ContentId::Hash` blocks in pool | `ContentId::Inode` blocks in pool |
| Eviction | Can evict freely (re-fetch from S3) | Must flush before eviction |
| Memory layout | Separate blocks per chunk | Same - unified pool |
| Disk layout | CAS by chunk hash | Materialized by path (assembled on flush) |
| Large file handling | Fetch only accessed chunks | Fetch only modified chunks |

### Sparse COW Design

Instead of fetching the entire file on first write, we track which chunks are dirty via `DirtyFileMetadata` and store content in the unified pool:

```rust
/// Metadata about a dirty file (content stored in memory pool).
///
/// This struct tracks file state and chunk information without storing
/// the actual content. Content is stored in the unified `MemoryPool`
/// using `ContentId::Inode(inode_id)` keys.
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
    /// Whether file is executable.
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
```

Content is stored in the unified `MemoryPool`:
- `pool.get_dirty(inode_id, chunk_index)` - Get chunk data
- `pool.insert_dirty(inode_id, chunk_index, data)` - Insert new chunk
- `pool.update_dirty(inode_id, chunk_index, data)` - Update existing chunk
- `pool.mark_flushed(inode_id, chunk_index)` - Mark as flushed (can evict)

### Sparse COW Flow

```
App opens 2GB file (8 × 256MB chunks)
│
├─ open() → No data fetched yet
│
├─ seek(768MB) + read(256MB)  [chunk 3]
│  │
│  └─ File not dirty → delegate to MemoryPool
│     └─ Fetch chunk 3 only from S3
│
├─ seek(1024MB) + write(100KB)  [chunk 4]
│  │
│  ├─ DirtyFileManager::write(ino, offset=1024MB, data)
│  │  │
│  │  ├─ is_dirty(ino)? → NO
│  │  │  │
│  │  │  └─ cow_copy(ino)
│  │  │     ├─ Get file metadata from INodeManager
│  │  │     ├─ Create DirtyFileMetadata {
│  │  │     │     original_hashes: [h0, h1, h2, h3, h4, h5, h6, h7],
│  │  │     │     dirty_chunks: {},
│  │  │     │  }
│  │  │     ├─ Invalidate read-only blocks for original hashes
│  │  │     └─ NO S3 FETCH - just metadata
│  │  │
│  │  ├─ Determine affected chunk: offset 1024MB → chunk_index = 4
│  │  │
│  │  ├─ Chunk 4 not in pool → ensure_chunk_in_pool(4)
│  │  │  └─ FileStore::retrieve(h4) → 256MB
│  │  │  └─ pool.insert_dirty(ino, 4, chunk_data)
│  │  │
│  │  ├─ Apply write to chunk 4 at local offset
│  │  │  └─ pool.update_dirty(ino, 4, modified_data)
│  │  │
│  │  └─ metadata.mark_chunk_dirty(4)
│  │
│  └─ Only chunk 4 fetched (256MB), not entire file (2GB)
│
└─ close() / fsync()
   │
   └─ flush_to_disk(ino)
      ├─ Assemble content from pool chunks
      ├─ Write to materialized cache
      └─ pool.mark_flushed(ino, chunk_idx) for each chunk
         ├─ Chunks 0-3: fetch from S3 (not in loaded_chunks)
         ├─ Chunk 4: use dirty data from loaded_chunks
         └─ Chunks 5-7: fetch from S3
```

### Optimized Flush: Incremental Assembly

For very large files, we can optimize the flush to avoid loading all chunks into memory at once:

```rust
impl DirtyFileManager {
    /// Flush a chunked dirty file to disk incrementally.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to flush
    async fn flush_chunked_to_disk(
        &self,
        inode_id: INodeId,
    ) -> Result<(), VfsError> {
        let (rel_path, chunk_count): (String, u32) = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard.get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;
            (meta.rel_path().to_string(), meta.chunk_count())
        };

        // Stream chunks to disk one at a time
        for chunk_idx in 0..chunk_count {
            if let Some(handle) = self.pool.get_dirty(inode_id, chunk_idx) {
                self.cache.write_chunk(&rel_path, chunk_idx, handle.data()).await?;
                self.pool.mark_flushed(inode_id, chunk_idx);
            }
        }

        Ok(())
    }
}

### Memory Efficiency Comparison

| Scenario | Old Design (Full COW) | New Design (Unified Pool) |
|----------|----------------------|---------------------------|
| Open 2GB file | 0 bytes | 0 bytes |
| Read chunk 3 | 0 bytes (via MemoryPool) | 0 bytes (via MemoryPool) |
| Write to chunk 4 | **2GB fetched** | **256MB fetched** |
| Flush to disk | 2GB in memory | Stream chunks (256MB peak) |
| Memory limit | Separate limits | **Unified limit** |

### Small vs Large Files

Small files (< 256MB, single chunk) and large files are handled uniformly through the pool:

```rust
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
}
```

---

## Operation Analysis & Edge Cases

### File Operations Matrix

| Operation | Small File | Chunked File (single chunk) | Chunked File (span boundary) |
|-----------|------------|----------------------------|------------------------------|
| **create** | ✅ Empty metadata | N/A (starts small) | N/A |
| **read clean** | ✅ MemoryPool | ✅ MemoryPool | ✅ MemoryPool |
| **read dirty** | ✅ From pool | ✅ From pool | ✅ From pool |
| **write** | ✅ Update pool | ✅ Load chunk, modify | ✅ Load both chunks |
| **truncate shrink** | ✅ Truncate pool data | ✅ Drop chunks | ✅ Adjust last chunk |
| **truncate extend** | ✅ Update size | ✅ Update size | ✅ Update size |
| **delete** | ✅ Remove from pool | ✅ Remove from pool | ✅ Remove from pool |

### Read from Dirty File

Reading from a dirty file uses the unified pool:

```rust
impl DirtyFileManager {
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
        self.read_multi_chunk(inode_id, offset, read_end, chunk_size).await
    }

    /// Ensure a chunk is loaded into the pool.
    ///
    /// Checks pool first, then fetches from S3 if needed.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID
    /// * `chunk_index` - Chunk index to load
    async fn ensure_chunk_in_pool(
        &self,
        inode_id: INodeId,
        chunk_index: u32,
    ) -> Result<(), VfsError> {
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
            self.read_store.retrieve(&hash, HashAlgorithm::Xxh128).await?
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
}
```

### Write to Dirty File

Write operations use the unified pool for content storage:

```rust
impl DirtyFileManager {
    /// Write to a dirty file, performing COW if needed.
    ///
    /// Content is stored in the unified memory pool.
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

        // Flush to disk cache
        self.flush_to_disk(inode_id).await?;

        Ok(bytes_written)
    }
}
```

### Truncate

Truncate operations update metadata and pool content:

```rust
impl DirtyFileManager {
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
                    let mut chunk_data: Vec<u8> = handle.data().to_vec();
                    drop(handle);

                    if (last_chunk_end as usize) < chunk_data.len() {
                        chunk_data.truncate(last_chunk_end as usize);
                        self.pool.update_dirty(inode_id, last_chunk_idx, chunk_data)
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
}
```

### Truncate Edge Cases

| Scenario | Before | After | Action |
|----------|--------|-------|--------|
| Shrink within chunk | 300MB (2 chunks) | 200MB | Keep chunk 0, truncate to 200MB |
| Shrink to zero | 1GB (4 chunks) | 0 | Remove all chunks from pool |
| Shrink exact boundary | 512MB (2 chunks) | 256MB | Keep chunk 0 only |
| Extend small | 100MB | 200MB | Update metadata size |
| Extend chunked | 512MB (2 chunks) | 1GB | Update metadata size (sparse) |

---

## Multi-threaded Access

### Concurrency Model

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         DirtyFileManager                                     │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  dirty_metadata: RwLock<HashMap<INodeId, DirtyFileMetadata>>        │    │
│  │  pool: Arc<MemoryPool>  (thread-safe with internal locking)         │    │
│  │                                                                      │    │
│  │  - Read lock on metadata: multiple concurrent reads                 │    │
│  │  - Write lock on metadata: COW, create, delete                      │    │
│  │  - Pool operations: internally synchronized                         │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Race Condition Fixes

#### 1. COW Race (Double Fetch)

```rust
impl DirtyFileManager {
    /// Perform COW with proper locking to prevent double fetch.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of file to COW
    async fn cow_copy(&self, inode_id: INodeId) -> Result<(), VfsError> {
        // Fast path: already dirty
        if self.is_dirty(inode_id) {
            return Ok(());
        }

        // Get file metadata from inode manager
        let inode: Arc<dyn INode> = self.inodes.get(inode_id)
            .ok_or(VfsError::InodeNotFound(inode_id))?;

        let file_content: FileContent = self.inodes.get_file_content(inode_id)
            .ok_or(VfsError::NotAFile(inode_id))?;

        // Create metadata (no data fetched yet - sparse COW)
        let metadata = DirtyFileMetadata::from_cow(
            inode_id,
            inode.path().to_string(),
            &file_content,
            inode.size(),
            false, // executable flag
        );

        // Invalidate any cached read-only blocks for original hashes
        for hash in metadata.original_hashes() {
            self.pool.invalidate_hash(hash);
        }

        self.dirty_metadata.write().unwrap().insert(inode_id, metadata);

        Ok(())
    }
}
```

#### 2. Read-Write Consistency

```rust
impl WritableVfs {
    fn read(&mut self, ino: u64, offset: i64, size: u32, reply: ReplyData) {
        let inode_id = INodeId(ino);

        // Check if file is dirty
        if self.dirty_manager.is_dirty(inode_id) {
            // File is dirty - read from dirty layer (content in pool)
            let rt = tokio::runtime::Handle::current();
            match rt.block_on(self.dirty_manager.read(inode_id, offset as u64, size)) {
                Ok(data) => reply.data(&data),
                Err(e) => {
                    tracing::error!("Read failed: {}", e);
                    reply.error(libc::EIO);
                }
            }
            return;
        }

        // File is clean - delegate to base VFS
        self.base.read(ino, offset, size, reply);
    }
}
```

#### 3. Flush Atomicity

```rust
impl DirtyFileManager {
    /// Flush with snapshot to avoid holding lock during I/O.
    async fn flush_to_disk(&self, inode_id: INodeId) -> Result<(), VfsError> {
        // Take snapshot of metadata under lock
        let (rel_path, state, size, chunk_count): (String, DirtyState, u64, u32) = {
            let guard = self.dirty_metadata.read().unwrap();
            let meta: &DirtyFileMetadata = guard.get(&inode_id)
                .ok_or(VfsError::InodeNotFound(inode_id))?;

            (
                meta.rel_path().to_string(),
                meta.state(),
                meta.size(),
                meta.chunk_count(),
            )
        };
        // Lock released here

        // Perform I/O without holding metadata lock
        if state == DirtyState::Deleted {
            self.cache.delete_file(&rel_path).await?;
            return Ok(());
        }

        // Assemble content from pool (pool has its own locking)
        let mut assembled: Vec<u8> = Vec::with_capacity(size as usize);
        for chunk_idx in 0..chunk_count {
            if let Some(handle) = self.pool.get_dirty(inode_id, chunk_idx) {
                assembled.extend_from_slice(handle.data());
            }
        }
        assembled.truncate(size as usize);

        self.cache.write_file(&rel_path, &assembled).await?;

        // Mark chunks as flushed in pool
        for chunk_idx in 0..chunk_count {
            self.pool.mark_flushed(inode_id, chunk_idx);
        }

        Ok(())
    }
}
```

### Thread Safety Summary

| Operation | Lock Type | Duration | Notes |
|-----------|-----------|----------|-------|
| `is_dirty()` | Read on `dirty_metadata` | Brief | Check only |
| `get_size()` / `get_mtime()` | Read on `dirty_metadata` | Brief | Metadata access |
| `read()` | Read on `dirty_metadata` + pool access | Brief | Pool has internal locking |
| `cow_copy()` | Write on `dirty_metadata` | Brief + async fetch | Uses pending map |
| `write()` | Write on `dirty_metadata` + pool write | Brief | Pool has internal locking |
| `flush_to_disk()` | Read on `dirty_metadata` | Brief | Snapshot then release |
| `delete_file()` | Write on `dirty_metadata` | Brief | Mark deleted |
| `get_dirty_entries()` | Read on `dirty_metadata` | Brief | Clone entries |

### Potential Deadlock Prevention

1. **Separate locks**: `dirty_metadata` lock for metadata, pool has internal locking for content
2. **No lock across await**: Release metadata lock before async operations
3. **Consistent ordering**: Always acquire `dirty_metadata` before `pending_cow`
4. **Timeout on pending**: COW waiters have timeout to prevent infinite wait

```rust
// Timeout for COW coordination
const COW_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

if let Some(waiter) = pending {
    match tokio::time::timeout(COW_TIMEOUT, waiter).await {
        Ok(_) => return Ok(()),
        Err(_) => return Err(VfsError::CowTimeout(inode_id)),
    }
}
```

---

## Concurrent Access

### Scenario: Two Users Access Same File

```
User A: open("/file.txt") → fh_a
User B: open("/file.txt") → fh_b

User A: write(fh_a, offset=0, "hello")
User B: read(fh_b, offset=0, size=5) → ???
```

### Current Design: Shared Dirty State

Both users see the same dirty file because dirty metadata is keyed by inode ID, not file handle:

```rust
// DirtyFileManager uses inode ID as key for metadata
dirty_metadata: RwLock<HashMap<INodeId, DirtyFileMetadata>>
// Content stored in unified pool with ContentId::Inode(inode_id)
```

**Behavior:**

1. User A opens file → no dirty entry yet
2. User B opens file → no dirty entry yet
3. User A writes → COW creates `DirtyFileMetadata`, content stored in pool
4. User B reads → checks dirty layer first → **sees User A's write**

```
Timeline:
─────────────────────────────────────────────────────────────────
User A: open ──────── write("hello") ────────────────────────────
User B: ──────── open ──────────────────── read() → "hello" ─────
                                              ↑
                                    Sees dirty data from User A
─────────────────────────────────────────────────────────────────
```

### Why This Design?

1. **POSIX semantics**: Multiple processes opening the same file should see each other's writes (after flush/sync).

2. **Simplicity**: One dirty copy per file, not per file handle.

3. **Memory efficiency**: Don't duplicate large files for each opener.

### Consistency Guarantees

| Operation | Visibility |
|-----------|------------|
| `write()` | Immediately visible to all readers of same inode |
| `fsync()` | Flushes to disk cache, visible after remount |
| `close()` | No special behavior (dirty state persists) |

### Race Condition Handling

Concurrent writes to the same file use `RwLock` on the dirty metadata map, with content stored in the pool:

```rust
pub async fn write(&self, inode_id: INodeId, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
    // COW if needed (takes write lock briefly on dirty_metadata)
    self.cow_copy(inode_id).await?;

    // Get metadata for chunk calculation
    let (chunk_size, chunk_count): (u64, u32) = {
        let guard = self.dirty_metadata.read().unwrap();
        let meta: &DirtyFileMetadata = guard.get(&inode_id)
            .ok_or(VfsError::InodeNotFound(inode_id))?;
        (meta.chunk_size(), meta.chunk_count())
    };

    // Write to pool (pool has internal locking for content)
    let bytes_written: usize = self.write_to_pool(inode_id, offset, data).await?;

    // Update metadata
    {
        let mut guard = self.dirty_metadata.write().unwrap();
        if let Some(meta) = guard.get_mut(&inode_id) {
            let chunk_idx: u32 = (offset / chunk_size) as u32;
            meta.mark_chunk_dirty(chunk_idx);
            meta.touch();
            let new_size: u64 = (offset + data.len() as u64).max(meta.size());
            meta.set_size(new_size);
        }
    }

    // Flush to disk
    self.flush_to_disk(inode_id).await?;

    Ok(bytes_written)
}
```

**Concurrent write behavior**: Last write wins at the byte level. If User A writes bytes 0-10 and User B writes bytes 5-15 concurrently, the final result depends on lock acquisition order.

### Alternative: Per-Handle Dirty State (Not Implemented)

For applications requiring isolated writes per file handle:

```rust
// Alternative design (not current)
struct PerHandleDirtyManager {
    dirty_metadata: RwLock<HashMap<FileHandle, DirtyFileMetadata>>,
    pool: Arc<MemoryPool>,
    // Each open() gets its own metadata entry and pool blocks
    // Requires merge on close or explicit sync
}
```

This would require:
- Copy on first write per handle
- Merge strategy on close (last-write-wins, conflict detection, etc.)
- Significantly more memory for concurrent access

The current shared design is simpler and matches typical FUSE filesystem behavior.

---

## Read/Write Flow Diagrams

### Write Flow (COW + Dual Storage)

```
Application: write("/mnt/vfs/file.txt", offset=0, data="hello")
│
├─ WritableVfs::write(ino, offset, data)
│  │
│  ├─ DirtyFileManager::write(ino, offset, data)
│  │  │
│  │  ├─ is_dirty(ino)? → NO
│  │  │  │
│  │  │  └─ cow_copy(ino)
│  │  │     ├─ INodeManager::get(ino) → INodeFile
│  │  │     ├─ Create DirtyFileMetadata (sparse - no data fetched)
│  │  │     └─ dirty_metadata.insert(ino, metadata)
│  │  │
│  │  ├─ pool.write_dirty(ino, chunk_idx, data) → bytes_written
│  │  │  └─ Store in unified memory pool
│  │  │
│  │  └─ flush_to_disk(ino)
│  │     └─ MaterializedCache::write_file(rel_path, data)
│  │        └─ Write to: cache_dir/path/to/file.txt
│  │
│  └─ reply.written(bytes_written)
```

### Read Flow (Dirty-First)

```
Application: read("/mnt/vfs/file.txt", offset=0, size=1024)
│
├─ WritableVfs::read(ino, offset, size)
│  │
│  ├─ DirtyFileManager::is_dirty(ino)?
│  │  │
│  │  ├─ YES (dirty) → DirtyFileManager::read(ino, offset, size)
│  │  │  └─ Assemble from pool chunks
│  │  │
│  │  └─ NO → delegate to base VFS
│  │     └─ DeadlineVfs::read(ino, offset, size)
│  │        └─ MemoryPool::acquire() → S3 fetch
│  │
│  └─ reply.data(bytes)
```

### Diff Manifest Export Flow

```
Application: exporter.export_diff_manifest(&parent, &parent_encoded)
│
├─ DirtyFileManager::get_dirty_entries()
│  └─ Collect all DirtyEntry from dirty_metadata map
│
├─ For each DirtyEntry:
│  │
│  ├─ DirtyState::New | Modified:
│  │  ├─ MaterializedCache::read_file(path) → data
│  │  ├─ hash_data(data, Xxh128) → new_hash
│  │  └─ ManifestFilePath { path, hash, size, mtime, ... }
│  │
│  └─ DirtyState::Deleted:
│     └─ ManifestFilePath::deleted(path)
│
├─ Build AssetManifest {
│     manifest_type: Diff,
│     parent_manifest_hash: hash(parent_encoded),
│     paths: [...],
│     dirs: [...],
│  }
│
└─ Return Manifest::V2025_12_04_beta(manifest)
```

---

## Error Types

```rust
// Add to error.rs

#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    // ... existing errors ...

    #[error("File is not writable: {0}")]
    NotWritable(INodeId),

    #[error("Not a file: {0}")]
    NotAFile(INodeId),

    #[error("Cache read failed for path: {0}")]
    CacheReadFailed(String),

    #[error("Cache write failed: {0}")]
    CacheWriteFailed(#[from] std::io::Error),

    #[error("COW copy failed for inode {inode}: {source}")]
    CowFailed {
        inode: INodeId,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
```

---

## Dependencies

```toml
# Add to crates/vfs/Cargo.toml

[dependencies]
# ... existing ...
rusty-attachments-model = { path = "../model" }  # For diff manifest types

[features]
default = []
fuse = ["dep:fuser", "dep:libc"]
write = ["fuse"]  # Write support requires FUSE
```

---

## Read Cache: S3 → Disk → Memory

The read path uses a two-tier cache to avoid repeated S3 fetches:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Read Request                                         │
│                              │                                               │
│                              ▼                                               │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  MemoryPool (Tier 1 - Hot)                                          │    │
│  │    - LRU eviction when full                                         │    │
│  │    - 8GB default, 256MB blocks                                      │    │
│  │    - Lock-free reads via Arc<Vec<u8>>                               │    │
│  │    - Last access time per block for time-based eviction             │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                              │ miss                                          │
│                              ▼                                               │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  DiskCache (Tier 2 - Warm)                                          │    │
│  │    - CAS layout: cache_dir/{hash}.{alg}                             │    │
│  │    - LRU eviction by atime                                          │    │
│  │    - Configurable max size (default 50GB)                           │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                              │ miss                                          │
│                              ▼                                               │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  S3 CAS (Tier 3 - Cold)                                             │    │
│  │    - Fetch via StorageClient                                        │    │
│  │    - Write-through to DiskCache on fetch                            │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

### MemoryPool with Time-Based Eviction

```rust
/// A block in the memory pool with access tracking.
pub struct PoolBlock {
    /// Block data (shared, lock-free reads).
    data: Arc<Vec<u8>>,
    /// Block key (hash + chunk_index).
    key: BlockKey,
    /// Reference count (active readers).
    ref_count: AtomicUsize,
    /// Last access time (updated on each acquire).
    last_access: AtomicU64, // Unix timestamp in seconds
}

impl PoolBlock {
    /// Update last access time to now.
    fn touch(&self) {
        let now: u64 = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.last_access.store(now, Ordering::Relaxed);
    }

    /// Get last access time as SystemTime.
    fn last_access_time(&self) -> SystemTime {
        let secs: u64 = self.last_access.load(Ordering::Relaxed);
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }
}

/// Trait for memory pool eviction strategies.
///
/// Allows custom eviction logic for testing or specialized use cases.
pub trait EvictionStrategy: Send + Sync {
    /// Select blocks to evict to free `needed_bytes`.
    ///
    /// # Arguments
    /// * `blocks` - Iterator of (BlockId, &PoolBlock) for all blocks
    /// * `needed_bytes` - Minimum bytes to free
    ///
    /// # Returns
    /// List of BlockIds to evict (in eviction order).
    fn select_for_eviction<'a>(
        &self,
        blocks: impl Iterator<Item = (BlockId, &'a PoolBlock)>,
        needed_bytes: u64,
    ) -> Vec<BlockId>;

    /// Scan and evict blocks based on time threshold.
    ///
    /// # Arguments
    /// * `blocks` - Iterator of (BlockId, &PoolBlock) for all blocks
    /// * `max_idle_secs` - Evict blocks not accessed within this duration
    ///
    /// # Returns
    /// List of BlockIds to evict.
    fn scan_idle_blocks<'a>(
        &self,
        blocks: impl Iterator<Item = (BlockId, &'a PoolBlock)>,
        max_idle_secs: u64,
    ) -> Vec<BlockId>;
}

/// LRU eviction with time-based idle scanning.
pub struct LruEvictionStrategy;

impl EvictionStrategy for LruEvictionStrategy {
    fn select_for_eviction<'a>(
        &self,
        blocks: impl Iterator<Item = (BlockId, &'a PoolBlock)>,
        needed_bytes: u64,
    ) -> Vec<BlockId> {
        // Collect blocks that can be evicted (ref_count == 0)
        let mut candidates: Vec<(BlockId, u64, u64)> = blocks
            .filter(|(_, block)| block.ref_count.load(Ordering::Relaxed) == 0)
            .map(|(id, block)| {
                let last_access: u64 = block.last_access.load(Ordering::Relaxed);
                let size: u64 = block.data.len() as u64;
                (id, last_access, size)
            })
            .collect();

        // Sort by last access (oldest first)
        candidates.sort_by_key(|(_, last_access, _)| *last_access);

        // Select enough blocks to free needed_bytes
        let mut to_evict: Vec<BlockId> = Vec::new();
        let mut freed: u64 = 0;

        for (id, _, size) in candidates {
            if freed >= needed_bytes {
                break;
            }
            to_evict.push(id);
            freed += size;
        }

        to_evict
    }

    fn scan_idle_blocks<'a>(
        &self,
        blocks: impl Iterator<Item = (BlockId, &'a PoolBlock)>,
        max_idle_secs: u64,
    ) -> Vec<BlockId> {
        let now: u64 = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let cutoff: u64 = now.saturating_sub(max_idle_secs);

        blocks
            .filter(|(_, block)| {
                block.ref_count.load(Ordering::Relaxed) == 0
                    && block.last_access.load(Ordering::Relaxed) < cutoff
            })
            .map(|(id, _)| id)
            .collect()
    }
}

impl MemoryPool {
    /// Scan and evict blocks not accessed within the idle threshold.
    ///
    /// # Arguments
    /// * `max_idle_secs` - Evict blocks not accessed within this duration
    ///
    /// # Returns
    /// Number of blocks evicted and bytes freed.
    pub fn scan_and_evict(&self, max_idle_secs: u64) -> EvictionResult {
        let mut inner = self.inner.lock().unwrap();

        let to_evict: Vec<BlockId> = self.eviction_strategy.scan_idle_blocks(
            inner.blocks.iter().map(|(id, block)| (*id, block.as_ref())),
            max_idle_secs,
        );

        let mut evicted_count: usize = 0;
        let mut freed_bytes: u64 = 0;

        for block_id in to_evict {
            if let Some(block) = inner.blocks.remove(&block_id) {
                // Double-check ref_count (may have changed)
                if block.ref_count.load(Ordering::Relaxed) == 0 {
                    freed_bytes += block.data.len() as u64;
                    evicted_count += 1;
                    inner.key_index.remove(&block.key);
                } else {
                    // Put it back, someone acquired it
                    inner.blocks.insert(block_id, block);
                }
            }
        }

        inner.current_size -= freed_bytes;

        EvictionResult {
            evicted_count,
            freed_bytes,
        }
    }
}

/// Result of an eviction scan.
#[derive(Debug, Clone)]
pub struct EvictionResult {
    pub evicted_count: usize,
    pub freed_bytes: u64,
}

/// Configuration for memory pool with eviction settings.
#[derive(Debug, Clone)]
pub struct MemoryPoolConfig {
    /// Maximum pool size in bytes.
    pub max_size: u64,
    /// Block size in bytes (default: 256MB).
    pub block_size: u64,
    /// Idle timeout for time-based eviction (from CLI).
    /// Blocks not accessed within this duration are eligible for eviction.
    pub idle_timeout_secs: Option<u64>,
}

impl Default for MemoryPoolConfig {
    fn default() -> Self {
        Self {
            max_size: 8 * 1024 * 1024 * 1024, // 8GB
            block_size: 256 * 1024 * 1024,    // 256MB
            idle_timeout_secs: None,           // No time-based eviction by default
        }
    }
}

impl MemoryPoolConfig {
    /// Set idle timeout for time-based eviction (builder pattern).
    ///
    /// # Arguments
    /// * `secs` - Seconds of idle time before eviction eligibility
    pub fn with_idle_timeout(mut self, secs: u64) -> Self {
        self.idle_timeout_secs = Some(secs);
        self
    }
}
```

### Background Eviction Sweeper

```rust
/// Background task that periodically scans and evicts idle blocks.
pub struct EvictionSweeper {
    pool: Arc<MemoryPool>,
    interval_secs: u64,
    idle_timeout_secs: u64,
    shutdown: Arc<AtomicBool>,
}

impl EvictionSweeper {
    /// Create a new eviction sweeper.
    ///
    /// # Arguments
    /// * `pool` - Memory pool to sweep
    /// * `interval_secs` - How often to run the sweep
    /// * `idle_timeout_secs` - Evict blocks idle longer than this
    pub fn new(pool: Arc<MemoryPool>, interval_secs: u64, idle_timeout_secs: u64) -> Self {
        Self {
            pool,
            interval_secs,
            idle_timeout_secs,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the background sweeper task.
    ///
    /// # Returns
    /// Handle to stop the sweeper.
    pub fn start(self) -> SweeperHandle {
        let shutdown = self.shutdown.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(self.interval_secs));

            loop {
                interval.tick().await;

                if self.shutdown.load(Ordering::Relaxed) {
                    break;
                }

                let result: EvictionResult = self.pool.scan_and_evict(self.idle_timeout_secs);

                if result.evicted_count > 0 {
                    tracing::info!(
                        "Eviction sweep: evicted {} blocks, freed {} bytes",
                        result.evicted_count,
                        result.freed_bytes
                    );
                }
            }
        });

        SweeperHandle { shutdown, handle }
    }
}

/// Handle to control the background sweeper.
pub struct SweeperHandle {
    shutdown: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

impl SweeperHandle {
    /// Stop the sweeper gracefully.
    pub async fn stop(self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.handle.await;
    }
}
```

---

### DiskCache (CAS Layout)

```rust
/// Disk-based cache using CAS (content-addressable) layout.
///
/// Files are stored by hash, enabling deduplication across
/// multiple VFS mounts and sessions.
///
/// # Directory Structure
/// ```
/// cache_dir/
/// ├── ab/                      # First 2 chars of hash (sharding)
/// │   └── ab1234...5678.xxh128 # Full hash + algorithm extension
/// └── cd/
///     └── cd9876...4321.xxh128
/// ```
pub struct DiskCache {
    /// Root directory for cache storage.
    cache_dir: PathBuf,
    /// Maximum cache size in bytes.
    max_size: u64,
    /// Current cache size (tracked in memory, persisted on shutdown).
    current_size: AtomicU64,
}

impl DiskCache {
    /// Create a new disk cache.
    ///
    /// # Arguments
    /// * `cache_dir` - Root directory for cache storage
    /// * `max_size` - Maximum cache size in bytes
    pub fn new(cache_dir: PathBuf, max_size: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(&cache_dir)?;

        // Scan existing cache to get current size
        let current_size: u64 = Self::scan_cache_size(&cache_dir)?;

        Ok(Self {
            cache_dir,
            max_size,
            current_size: AtomicU64::new(current_size),
        })
    }

    /// Get cached content by hash.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm (for file extension)
    ///
    /// # Returns
    /// Cached content, or None if not in cache.
    pub fn get(&self, hash: &str, algorithm: HashAlgorithm) -> std::io::Result<Option<Vec<u8>>> {
        let path: PathBuf = self.cache_path(hash, algorithm);

        if path.exists() {
            // Update atime for LRU tracking
            let now = filetime::FileTime::now();
            let _ = filetime::set_file_atime(&path, now);

            Ok(Some(std::fs::read(&path)?))
        } else {
            Ok(None)
        }
    }

    /// Store content in cache.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm
    /// * `data` - Content to cache
    pub fn put(&self, hash: &str, algorithm: HashAlgorithm, data: &[u8]) -> std::io::Result<()> {
        let path: PathBuf = self.cache_path(hash, algorithm);

        // Create shard directory
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Check if we need to evict
        let data_size: u64 = data.len() as u64;
        self.maybe_evict(data_size)?;

        // Write atomically
        let temp_path: PathBuf = path.with_extension("tmp");
        std::fs::write(&temp_path, data)?;
        std::fs::rename(&temp_path, &path)?;

        self.current_size.fetch_add(data_size, Ordering::Relaxed);

        Ok(())
    }

    /// Build cache file path for a hash.
    fn cache_path(&self, hash: &str, algorithm: HashAlgorithm) -> PathBuf {
        let shard: &str = &hash[..2.min(hash.len())];
        let ext: &str = match algorithm {
            HashAlgorithm::Xxh128 => "xxh128",
        };
        self.cache_dir.join(shard).join(format!("{}.{}", hash, ext))
    }

    /// Evict oldest entries if cache is over limit.
    fn maybe_evict(&self, needed: u64) -> std::io::Result<()> {
        let current: u64 = self.current_size.load(Ordering::Relaxed);
        if current + needed <= self.max_size {
            return Ok(());
        }

        // Collect files with atime
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        self.collect_cache_entries(&self.cache_dir, &mut entries)?;

        // Sort by atime (oldest first)
        entries.sort_by_key(|(_, _, atime)| *atime);

        // Evict until we have space
        let target: u64 = self.max_size.saturating_sub(needed);
        let mut freed: u64 = 0;

        for (path, size, _) in entries {
            if current - freed <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                freed += size;
            }
        }

        self.current_size.fetch_sub(freed, Ordering::Relaxed);
        Ok(())
    }

    /// Scan cache directory for total size.
    fn scan_cache_size(dir: &Path) -> std::io::Result<u64> {
        let mut total: u64 = 0;
        if dir.exists() {
            for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        Ok(total)
    }

    /// Collect cache entries with metadata.
    fn collect_cache_entries(
        &self,
        dir: &Path,
        entries: &mut Vec<(PathBuf, u64, SystemTime)>,
    ) -> std::io::Result<()> {
        for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                if let Ok(meta) = entry.metadata() {
                    let atime: SystemTime = meta.accessed().unwrap_or(SystemTime::UNIX_EPOCH);
                    entries.push((entry.path().to_path_buf(), meta.len(), atime));
                }
            }
        }
        Ok(())
    }
}
```

### CachedFileStore

Wraps `FileStore` with disk cache layer:

```rust
/// FileStore wrapper that adds disk caching.
///
/// On cache miss, fetches from inner store and writes through to disk.
pub struct CachedFileStore<S: FileStore> {
    /// Inner file store (S3).
    inner: S,
    /// Disk cache.
    cache: DiskCache,
}

impl<S: FileStore> CachedFileStore<S> {
    /// Create a new cached file store.
    ///
    /// # Arguments
    /// * `inner` - Inner file store (typically S3)
    /// * `cache_dir` - Directory for disk cache
    /// * `max_cache_size` - Maximum disk cache size in bytes
    pub fn new(inner: S, cache_dir: PathBuf, max_cache_size: u64) -> std::io::Result<Self> {
        Ok(Self {
            inner,
            cache: DiskCache::new(cache_dir, max_cache_size)?,
        })
    }
}

#[async_trait]
impl<S: FileStore + 'static> FileStore for CachedFileStore<S> {
    async fn retrieve(&self, hash: &str, algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError> {
        // Check disk cache first
        if let Some(data) = self.cache.get(hash, algorithm)? {
            tracing::debug!("Disk cache hit for {}", hash);
            return Ok(data);
        }

        // Fetch from S3
        tracing::debug!("Disk cache miss for {}, fetching from S3", hash);
        let data: Vec<u8> = self.inner.retrieve(hash, algorithm).await?;

        // Write through to disk cache
        if let Err(e) = self.cache.put(hash, algorithm, &data) {
            tracing::warn!("Failed to cache {} to disk: {}", hash, e);
        }

        Ok(data)
    }

    async fn retrieve_range(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, VfsError> {
        // For range requests, check if we have full content cached
        if let Some(data) = self.cache.get(hash, algorithm)? {
            let start: usize = offset as usize;
            let end: usize = (offset + size).min(data.len() as u64) as usize;
            return Ok(data[start..end].to_vec());
        }

        // Fetch full content and cache it
        let data: Vec<u8> = self.inner.retrieve(hash, algorithm).await?;

        if let Err(e) = self.cache.put(hash, algorithm, &data) {
            tracing::warn!("Failed to cache {} to disk: {}", hash, e);
        }

        let start: usize = offset as usize;
        let end: usize = (offset + size).min(data.len() as u64) as usize;
        Ok(data[start..end].to_vec())
    }
}
```

### Updated Read Flow

```
read(ino, offset, size)
│
├─ Check dirty layer → if dirty, return from memory
│
├─ MemoryPool::acquire(block_key, fetch_fn)
│  │
│  ├─ Check memory cache → HIT → return BlockHandle
│  │
│  └─ MISS → call fetch_fn:
│     │
│     ├─ CachedFileStore::retrieve(hash, alg)
│     │  │
│     │  ├─ DiskCache::get(hash, alg)
│     │  │  ├─ HIT → return cached data
│     │  │  └─ MISS → continue
│     │  │
│     │  ├─ S3::get_object(cas_key)
│     │  │
│     │  └─ DiskCache::put(hash, alg, data)  # Write-through
│     │
│     └─ Insert into MemoryPool
│
└─ Return data slice
```

### Cache Configuration

```rust
/// Configuration for read caching.
#[derive(Debug, Clone)]
pub struct ReadCacheConfig {
    /// Directory for disk cache (CAS layout).
    pub disk_cache_dir: PathBuf,
    /// Maximum disk cache size in bytes (default: 50GB).
    pub disk_cache_max_size: u64,
    /// Memory pool configuration.
    pub memory_pool: MemoryPoolConfig,
}

impl Default for ReadCacheConfig {
    fn default() -> Self {
        Self {
            disk_cache_dir: PathBuf::from("/tmp/vfs-read-cache"),
            disk_cache_max_size: Self::DEFAULT_DISK_CACHE_SIZE,
            memory_pool: MemoryPoolConfig::default(),
        }
    }
}

impl ReadCacheConfig {
    /// Default disk cache size: 50GB.
    pub const DEFAULT_DISK_CACHE_SIZE: u64 = 50 * 1024 * 1024 * 1024;

    /// Create config with custom disk cache size.
    ///
    /// # Arguments
    /// * `disk_cache_dir` - Directory for disk cache
    /// * `disk_cache_max_size` - Maximum size in bytes
    pub fn with_disk_cache(disk_cache_dir: PathBuf, disk_cache_max_size: u64) -> Self {
        Self {
            disk_cache_dir,
            disk_cache_max_size,
            ..Default::default()
        }
    }

    /// Set disk cache size (builder pattern).
    ///
    /// # Arguments
    /// * `size` - Maximum size in bytes
    pub fn disk_cache_size(mut self, size: u64) -> Self {
        self.disk_cache_max_size = size;
        self
    }

    /// Set disk cache directory (builder pattern).
    ///
    /// # Arguments
    /// * `dir` - Directory path
    pub fn disk_cache_dir(mut self, dir: PathBuf) -> Self {
        self.disk_cache_dir = dir;
        self
    }
}
```

### Usage

```rust
// Create S3 storage client
let s3_store: Arc<dyn FileStore> = Arc::new(StorageClientAdapter::new(client, location));

// Configure read cache (disk cache size is configurable)
let read_cache_config = ReadCacheConfig::default()
    .disk_cache_dir(PathBuf::from("/var/cache/vfs"))
    .disk_cache_size(100 * 1024 * 1024 * 1024); // 100GB

// Wrap with disk cache using config
let cached_store = CachedFileStore::new(
    s3_store,
    read_cache_config.disk_cache_dir.clone(),
    read_cache_config.disk_cache_max_size,
)?;

// Create VFS with cached store
let vfs = WritableVfs::new(
    &manifest,
    Arc::new(cached_store),
    VfsOptions::default(),
    WriteOptions::default(),
)?;
```

---

## Future Considerations

1. **Large File Chunking**: Files > 256MB should be chunked in the diff manifest
2. **Partial Flush**: Flush only modified ranges instead of entire file
3. **Cache Eviction**: LRU eviction for disk cache when space is limited
4. **Concurrent Writes**: Handle multiple writers to same file (currently last-write-wins)
5. **Atomic Rename**: Support `rename()` operation for atomic file updates
6. **Directory Operations**: `mkdir()`, `rmdir()` support
7. **Shared Disk Cache**: Allow multiple VFS instances to share the same disk cache
8. **Cache Warming**: Pre-populate disk cache from manifest on mount

---

## Writable VFS Statistics

The library provides a `WritableVfsStatsCollector` that extends the base `VfsStatsCollector` with dirty file tracking. This allows CLI tools and other consumers to query the current state of modified files.

### Data Structures

```rust
/// Information about a single dirty file.
#[derive(Debug, Clone)]
pub struct DirtyFileInfo {
    /// Relative path within VFS.
    pub path: String,
    /// Current file size in bytes.
    pub size: u64,
}

/// Statistics for a writable VFS.
#[derive(Debug, Clone, Default)]
pub struct WritableVfsStats {
    /// Base read-only stats (inode count, pool stats, cache hits, etc.).
    pub base: VfsStats,
    /// Summary counts of dirty files.
    pub dirty_summary: DirtySummary,
    /// List of modified files (state = Modified).
    pub modified_files: Vec<DirtyFileInfo>,
    /// List of newly created files (state = New).
    pub new_files: Vec<DirtyFileInfo>,
    /// List of deleted file paths (state = Deleted).
    pub deleted_files: Vec<String>,
}
```

### WritableVfsStatsCollector

```rust
/// Collects statistics from a writable VFS.
///
/// Wraps the base VfsStatsCollector and adds dirty file tracking.
/// Thread-safe and cloneable for use from background stats threads.
#[derive(Clone)]
pub struct WritableVfsStatsCollector {
    /// Base stats collector.
    base: VfsStatsCollector,
    /// Reference to dirty file manager.
    dirty_manager: Arc<DirtyFileManager>,
}

impl WritableVfsStatsCollector {
    /// Create a new writable stats collector.
    ///
    /// # Arguments
    /// * `base` - Base VFS stats collector
    /// * `dirty_manager` - Dirty file manager reference
    pub fn new(base: VfsStatsCollector, dirty_manager: Arc<DirtyFileManager>) -> Self {
        Self { base, dirty_manager }
    }

    /// Collect current statistics.
    ///
    /// # Returns
    /// Snapshot of writable VFS statistics including dirty file lists.
    pub fn collect(&self) -> WritableVfsStats {
        let base: VfsStats = self.base.collect();
        let entries: Vec<DirtyEntry> = self.dirty_manager.get_dirty_entries();

        let mut modified_files: Vec<DirtyFileInfo> = Vec::new();
        let mut new_files: Vec<DirtyFileInfo> = Vec::new();
        let mut deleted_files: Vec<String> = Vec::new();

        for entry in &entries {
            match entry.state {
                DirtyState::Modified => {
                    modified_files.push(DirtyFileInfo {
                        path: entry.path.clone(),
                        size: entry.size,
                    });
                }
                DirtyState::New => {
                    new_files.push(DirtyFileInfo {
                        path: entry.path.clone(),
                        size: entry.size,
                    });
                }
                DirtyState::Deleted => {
                    deleted_files.push(entry.path.clone());
                }
            }
        }

        let dirty_summary = DirtySummary {
            modified_count: modified_files.len(),
            new_count: new_files.len(),
            deleted_count: deleted_files.len(),
        };

        WritableVfsStats {
            base,
            dirty_summary,
            modified_files,
            new_files,
            deleted_files,
        }
    }
}
```

### WritableVfs Integration

```rust
impl WritableVfs {
    /// Get a stats collector for this writable VFS.
    ///
    /// # Returns
    /// Collector that can be cloned and used from another thread to query stats.
    pub fn stats_collector(&self) -> WritableVfsStatsCollector {
        let base: VfsStatsCollector = VfsStatsCollector::new(
            self.inodes.clone(),
            self.pool.clone(),
            self.start_time,
        );
        WritableVfsStatsCollector::new(base, self.dirty_manager.clone())
    }
}
```

### CLI Usage Example

The CLI is responsible for formatting and displaying the stats. The library only provides the data:

```rust
// In mount_vfs.rs example

/// Print writable VFS statistics dashboard.
///
/// # Arguments
/// * `stats` - Collected writable VFS stats
fn print_writable_stats(stats: &WritableVfsStats) {
    // Print base stats first (existing logic)
    print_base_stats(&stats.base);

    // Print dirty files section
    let summary: &DirtySummary = &stats.dirty_summary;
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ COW DIRTY FILES                                                   ║");
    println!("║   Modified: {:>3}    New: {:>3}    Deleted: {:>3}                    ║",
             summary.modified_count, summary.new_count, summary.deleted_count);

    // List modified + new files with sizes
    if !stats.modified_files.is_empty() || !stats.new_files.is_empty() {
        println!("║                                                                   ║");
        println!("║   Dirty files:                                                    ║");

        let all_dirty: Vec<(&DirtyFileInfo, &str)> = stats.modified_files.iter()
            .map(|f| (f, ""))
            .chain(stats.new_files.iter().map(|f| (f, "[NEW]")))
            .collect();

        for (i, (file, marker)) in all_dirty.iter().take(10).enumerate() {
            let path_display: String = truncate_path(&file.path, 42);
            println!("║   {:>2}. {:42} {:>8} {:5} ║",
                     i + 1, path_display, format_bytes(file.size), marker);
        }

        if all_dirty.len() > 10 {
            println!("║   ... and {} more                                               ║",
                     all_dirty.len() - 10);
        }
    }

    // List deleted files
    if !stats.deleted_files.is_empty() {
        println!("║                                                                   ║");
        println!("║   Deleted files:                                                  ║");

        for (i, path) in stats.deleted_files.iter().take(5).enumerate() {
            let path_display: String = truncate_path(path, 50);
            println!("║   {:>2}. {:50}        ║", i + 1, path_display);
        }

        if stats.deleted_files.len() > 5 {
            println!("║   ... and {} more                                               ║",
                     stats.deleted_files.len() - 5);
        }
    }

    println!("╚══════════════════════════════════════════════════════════════════╝");
}

/// Spawn background thread for writable stats display.
///
/// # Arguments
/// * `collector` - Stats collector (cloned into thread)
/// * `running` - Atomic flag to control thread lifetime
/// * `interval_secs` - Interval between stats updates
fn spawn_writable_stats_thread(
    collector: WritableVfsStatsCollector,
    running: Arc<AtomicBool>,
    interval_secs: u64,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while running.load(Ordering::SeqCst) {
            let stats: WritableVfsStats = collector.collect();
            print_writable_stats(&stats);
            thread::sleep(Duration::from_secs(interval_secs));
        }
    })
}
```

### Architecture Summary

| Layer | Component | Responsibility |
|-------|-----------|----------------|
| Library | `DirtyFileInfo` | Data struct for a single dirty file |
| Library | `WritableVfsStats` | Aggregated stats with file lists |
| Library | `WritableVfsStatsCollector` | Thread-safe collector, queries `DirtyFileManager` |
| Library | `WritableVfs::stats_collector()` | Factory method to create collector |
| CLI | `print_writable_stats()` | Format and display dashboard |
| CLI | `spawn_writable_stats_thread()` | Background refresh loop |

This separation keeps the library focused on data collection while the CLI handles presentation concerns.

---

## Implementation Notes

### New File Visibility (create/lookup/getattr)

New files created via `create()` are stored only in `DirtyFileManager`, not in `INodeManager` (which holds manifest files). This requires special handling in FUSE operations:

| Operation | Issue | Solution |
|-----------|-------|----------|
| `readdir()` | New files not listed | Call `dirty_manager.get_new_files_in_dir(parent)` and append to entries |
| `lookup()` | Returns ENOENT for new files | Check `dirty_manager.lookup_new_file(parent, name)` after manifest lookup |
| `getattr()` | Returns ENOENT for new file inodes | Check `dirty_manager.is_new_file(ino)` and build attrs from dirty state |
| `open()` | Returns ENOENT for new files | Check `dirty_manager.is_new_file(ino)` before manifest lookup |
| `setattr()` | Fails for new files | Check `dirty_manager.is_new_file(ino)` and build attrs from dirty state |

#### DirtyFileMetadata Parent Tracking

`DirtyFileMetadata` includes `parent_inode: Option<INodeId>` to track which directory contains new files:

```rust
pub struct DirtyFileMetadata {
    // ...existing fields...
    /// Parent inode ID (only set for new files).
    parent_inode: Option<INodeId>,
}

impl DirtyFileMetadata {
    /// Create a new file (not from COW).
    pub fn new_file(inode_id: INodeId, rel_path: String, parent_inode: INodeId) -> Self;
    
    /// Get parent inode ID (only set for new files).
    pub fn parent_inode(&self) -> Option<INodeId>;
    
    /// Get the file name (last component of path).
    pub fn file_name(&self) -> &str;
}
```

#### DirtyFileManager Lookup Methods

```rust
impl DirtyFileManager {
    /// Look up a new file by parent inode and name.
    pub fn lookup_new_file(&self, parent_inode: INodeId, name: &str) -> Option<INodeId>;
    
    /// Check if an inode is a new file (created, not from manifest).
    pub fn is_new_file(&self, inode_id: INodeId) -> bool;
    
    /// Get new files in a directory.
    pub fn get_new_files_in_dir(&self, parent_inode: INodeId) -> Vec<(INodeId, String)>;
}
```

### Deleting New Files (unlink)

When deleting files, behavior differs based on file origin:

| File Type | Behavior |
|-----------|----------|
| Manifest file (existing) | Mark as `DirtyState::Deleted` in dirty map |
| New file (created this session) | Remove entirely from dirty map |

The `unlink()` implementation must check both sources:

```rust
fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
    // ... name validation ...
    
    // Find inode - check manifest files first, then new files
    let ino: u64 = if let Some(inode) = self.inodes.get_by_path(&file_path) {
        inode.id()
    } else if let Some(new_ino) = self.dirty_manager.lookup_new_file(parent, name_str) {
        new_ino
    } else {
        reply.error(libc::ENOENT);
        return;
    };
    
    // delete_file() handles both cases appropriately
    self.dirty_manager.delete_file(ino).await?;
    reply.ok();
}
```

The `delete_file()` method handles both cases:

```rust
pub async fn delete_file(&self, inode_id: INodeId) -> Result<(), VfsError> {
    // New files: remove entirely (they never existed in manifest)
    if self.is_new_file(inode_id) {
        let rel_path: Option<String> = {
            let guard = self.dirty_metadata.read().unwrap();
            guard.get(&inode_id).map(|m| m.rel_path().to_string())
        };
        self.pool.remove_inode_blocks(inode_id);
        self.dirty_metadata.write().unwrap().remove(&inode_id);
        if let Some(path) = rel_path {
            let _ = self.cache.delete_file(&path).await;
        }
        return Ok(());
    }
    
    // Manifest files: mark as deleted (need to track for diff manifest)
    // ... existing logic ...
}
```

---

## Related Documents

- [vfs.md](vfs.md) - Read-only VFS design (base implementation)
- [model-design.md](model-design.md) - Manifest data structures
- [manifest-utils.md](manifest-utils.md) - Diff manifest creation utilities


---

## Implementation Status

### Completed ✅

| Component | Location | Notes |
|-----------|----------|-------|
| `DirtyState` enum | `src/write/dirty.rs` | Modified, New, Deleted states |
| `DirtyFileMetadata` struct | `src/write/dirty.rs` | Metadata for dirty files (content in pool) |
| `DirtyEntry` struct | `src/write/dirty.rs` | Summary for export |
| `DirtyFileManager` | `src/write/dirty.rs` | Full implementation with COW, read, write, truncate, delete |
| `WriteCache` trait | `src/write/cache.rs` | Abstraction for disk storage |
| `MaterializedCache` | `src/write/cache.rs` | Disk-based cache with tombstones |
| `MemoryWriteCache` | `src/write/cache.rs` | In-memory cache for testing |
| `ChunkedFileMeta` | `src/write/cache.rs` | Metadata for chunked files |
| `DiffManifestExporter` trait | `src/write/export.rs` | Interface for diff export |
| `DirtySummary` | `src/write/export.rs` | File and directory counts |
| `DirtyFileInfo` | `src/write/export.rs` | File info for stats |
| `WritableVfsStats` | `src/write/stats.rs` | Extended stats with dirty tracking |
| `WritableVfsStatsCollector` | `src/write/stats.rs` | Thread-safe stats collection |
| `WritableVfs` | `src/fuse_writable.rs` | FUSE implementation |
| `WriteOptions` | `src/fuse_writable.rs` | Configuration for write behavior |
| FUSE `write()` | `src/fuse_writable.rs` | Write with COW |
| FUSE `create()` | `src/fuse_writable.rs` | Create new files |
| FUSE `unlink()` | `src/fuse_writable.rs` | Delete files |
| FUSE `setattr()` | `src/fuse_writable.rs` | Truncate support |
| FUSE `fsync()` | `src/fuse_writable.rs` | Flush to disk |
| FUSE `mkdir()` | `src/fuse_writable.rs` | Create directories |
| FUSE `rmdir()` | `src/fuse_writable.rs` | Delete directories |

### Pending 🚧

| Component | Notes |
|-----------|-------|
| `export_diff_manifest()` | Returns "not yet implemented" error |
| Disk cache read tier | `CachedFileStore` not implemented |
| Hash verification | Post-download verification not implemented |
| Background eviction sweeper | `EvictionSweeper` not implemented |
| COW race prevention | `pending_cow` map not implemented |

### Design vs Implementation Differences

1. **Simplified flush**: The implementation assembles full files on flush rather than writing individual chunks. This is simpler but less efficient for very large files.

2. **No pending COW coordination**: The design includes a `pending_cow` map to prevent duplicate fetches during concurrent COW operations. The implementation uses simpler locking.

3. **No disk read cache**: The `CachedFileStore` wrapper for disk caching is not implemented. Reads go directly to S3.

4. **No background eviction**: The `EvictionSweeper` for time-based memory pool eviction is not implemented.

5. **DirtyDirManager integration**: The implementation includes additional helper methods (`is_new_dir`, `get_new_dirs_in_parent`, `lookup_new_dir`) not in the original design, needed for FUSE integration.
