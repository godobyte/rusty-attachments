//! Data types for projection layer.

use std::sync::Arc;
use std::time::SystemTime;

/// Projected file/folder info for enumeration and placeholders.
#[derive(Clone, Debug)]
pub struct ProjectedFileInfo {
    /// File or folder name (Arc to avoid cloning).
    pub name: Arc<str>,
    /// Size in bytes (0 for folders).
    pub size: u64,
    /// Whether this is a folder.
    pub is_folder: bool,
    /// Content hash for files (None for folders).
    pub content_hash: Option<Arc<str>>,
    /// Symlink target (None for files/folders).
    pub symlink_target: Option<Arc<str>>,
    /// Modification time.
    pub mtime: SystemTime,
    /// Whether file is executable.
    pub executable: bool,
}

impl ProjectedFileInfo {
    /// Create info for a file.
    ///
    /// # Arguments
    /// * `name` - File name
    /// * `size` - File size in bytes
    /// * `content_hash` - Content hash
    /// * `mtime` - Modification time
    /// * `executable` - Whether file is executable
    pub fn file(
        name: String,
        size: u64,
        content_hash: String,
        mtime: SystemTime,
        executable: bool,
    ) -> Self {
        Self {
            name: Arc::from(name),
            size,
            is_folder: false,
            content_hash: Some(Arc::from(content_hash)),
            symlink_target: None,
            mtime,
            executable,
        }
    }

    /// Create info for a folder.
    ///
    /// # Arguments
    /// * `name` - Folder name
    pub fn folder(name: String) -> Self {
        Self {
            name: Arc::from(name),
            size: 0,
            is_folder: true,
            content_hash: None,
            symlink_target: None,
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        }
    }

    /// Create info for a symlink.
    ///
    /// # Arguments
    /// * `name` - Symlink name
    /// * `target` - Symlink target path
    pub fn symlink(name: String, target: String) -> Self {
        Self {
            name: Arc::from(name),
            size: 0,
            is_folder: false,
            content_hash: None,
            symlink_target: Some(Arc::from(target)),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        }
    }
}

/// Content hash - either single hash or chunked.
#[derive(Clone, Debug)]
pub enum ContentHash {
    /// Single hash for entire file (V1 and small V2 files).
    Single(String),
    /// Chunk hashes for large files (V2 only, >256MB).
    Chunked(Vec<String>),
}

impl ContentHash {
    /// Get the primary hash (first chunk or single hash).
    pub fn primary_hash(&self) -> &str {
        match self {
            ContentHash::Single(h) => h,
            ContentHash::Chunked(hashes) => hashes.first().map(|s| s.as_str()).unwrap_or(""),
        }
    }

    /// Check if this is chunked.
    #[allow(dead_code)]
    pub fn is_chunked(&self) -> bool {
        matches!(self, ContentHash::Chunked(_))
    }

    /// Get chunk count.
    #[allow(dead_code)]
    pub fn chunk_count(&self) -> usize {
        match self {
            ContentHash::Single(_) => 1,
            ContentHash::Chunked(hashes) => hashes.len(),
        }
    }
}

/// Data about a file in the projection.
#[derive(Clone, Debug)]
pub struct FileData {
    /// File name (not full path).
    pub name: String,
    /// File size in bytes.
    pub size: u64,
    /// Content hash for CAS lookup.
    pub content_hash: ContentHash,
    /// Modification time.
    pub mtime: SystemTime,
    /// Whether file is executable.
    pub executable: bool,
}

/// Data about a symlink (V2 manifest only).
#[derive(Clone, Debug)]
pub struct SymlinkData {
    /// Symlink name.
    pub name: String,
    /// Target path.
    pub target: String,
}

/// Entry in a folder (file, subfolder, or symlink).
#[derive(Clone, Debug)]
pub enum FolderEntry {
    /// File entry.
    File(FileData),
    /// Subfolder entry.
    Folder(Box<crate::projection::folder::FolderData>),
    /// Symlink entry.
    Symlink(SymlinkData),
}

impl FolderEntry {
    /// Get the entry name.
    pub fn name(&self) -> &str {
        match self {
            FolderEntry::File(f) => &f.name,
            FolderEntry::Folder(f) => f.name(),
            FolderEntry::Symlink(s) => &s.name,
        }
    }

    /// Check if this is a folder.
    pub fn is_folder(&self) -> bool {
        matches!(self, FolderEntry::Folder(_))
    }
}

// Note: FolderData is defined in crate::projection::folder to avoid circular dependency
