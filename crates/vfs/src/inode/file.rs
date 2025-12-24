//! File inode implementation.

use std::any::Any;
use std::time::SystemTime;

use rusty_attachments_model::HashAlgorithm;

use super::types::{INode, INodeId, INodeType};

/// Default file permissions (rw-r--r--).
pub const DEFAULT_FILE_PERMS: u16 = 0o644;

/// Executable file permissions (rwxr-xr-x).
pub const EXECUTABLE_FILE_PERMS: u16 = 0o755;

/// File content source - handles both V1 (single hash) and V2 (chunked) files.
#[derive(Debug, Clone)]
pub enum FileContent {
    /// Single hash for entire file (V1 and small V2 files).
    SingleHash(String),
    /// Chunk hashes for large V2 files (>256MB).
    Chunked(Vec<String>),
}

impl FileContent {
    /// Get the hash for a single-hash file.
    ///
    /// # Returns
    /// The hash if this is a single-hash file, None for chunked files.
    pub fn single_hash(&self) -> Option<&str> {
        match self {
            FileContent::SingleHash(h) => Some(h),
            FileContent::Chunked(_) => None,
        }
    }

    /// Get the chunk hashes for a chunked file.
    ///
    /// # Returns
    /// The chunk hashes if this is a chunked file, None for single-hash files.
    pub fn chunk_hashes(&self) -> Option<&[String]> {
        match self {
            FileContent::SingleHash(_) => None,
            FileContent::Chunked(h) => Some(h),
        }
    }

    /// Check if this is a chunked file.
    pub fn is_chunked(&self) -> bool {
        matches!(self, FileContent::Chunked(_))
    }

    /// Get the number of chunks.
    pub fn chunk_count(&self) -> usize {
        match self {
            FileContent::SingleHash(_) => 1,
            FileContent::Chunked(h) => h.len(),
        }
    }
}

/// File inode representing a regular file.
#[derive(Debug)]
pub struct INodeFile {
    /// Inode ID.
    id: INodeId,
    /// Parent directory inode ID.
    parent_id: INodeId,
    /// File name.
    name: String,
    /// Full path from root.
    path: String,
    /// File size in bytes.
    size: u64,
    /// Modification time.
    mtime: SystemTime,
    /// Content hash(es).
    content: FileContent,
    /// Hash algorithm used.
    hash_algorithm: HashAlgorithm,
    /// Whether file is executable (V2 runnable flag).
    executable: bool,
}

impl INodeFile {
    /// Create a new file inode.
    ///
    /// # Arguments
    /// * `id` - Inode ID
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - File name
    /// * `path` - Full path from root
    /// * `size` - File size in bytes
    /// * `mtime` - Modification time
    /// * `content` - Content hash(es)
    /// * `hash_algorithm` - Hash algorithm used
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: INodeId,
        parent_id: INodeId,
        name: String,
        path: String,
        size: u64,
        mtime: SystemTime,
        content: FileContent,
        hash_algorithm: HashAlgorithm,
    ) -> Self {
        Self {
            id,
            parent_id,
            name,
            path,
            size,
            mtime,
            content,
            hash_algorithm,
            executable: false,
        }
    }

    /// Set the executable flag.
    ///
    /// # Arguments
    /// * `executable` - Whether the file is executable
    pub fn with_executable(mut self, executable: bool) -> Self {
        self.executable = executable;
        self
    }

    /// Get the file content (hash or chunk hashes).
    pub fn content(&self) -> &FileContent {
        &self.content
    }

    /// Check if the file is executable.
    pub fn is_executable(&self) -> bool {
        self.executable
    }
}

impl INode for INodeFile {
    fn id(&self) -> INodeId {
        self.id
    }

    fn parent_id(&self) -> INodeId {
        self.parent_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn inode_type(&self) -> INodeType {
        INodeType::File
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn mtime(&self) -> SystemTime {
        self.mtime
    }

    fn permissions(&self) -> u16 {
        if self.executable {
            EXECUTABLE_FILE_PERMS
        } else {
            DEFAULT_FILE_PERMS
        }
    }

    fn hash_algorithm(&self) -> Option<HashAlgorithm> {
        Some(self.hash_algorithm)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn test_file_content_single_hash() {
        let content: FileContent = FileContent::SingleHash("abc123".to_string());
        assert_eq!(content.single_hash(), Some("abc123"));
        assert!(content.chunk_hashes().is_none());
        assert!(!content.is_chunked());
        assert_eq!(content.chunk_count(), 1);
    }

    #[test]
    fn test_file_content_chunked() {
        let hashes: Vec<String> = vec!["h1".to_string(), "h2".to_string(), "h3".to_string()];
        let content: FileContent = FileContent::Chunked(hashes.clone());
        assert!(content.single_hash().is_none());
        assert_eq!(content.chunk_hashes(), Some(hashes.as_slice()));
        assert!(content.is_chunked());
        assert_eq!(content.chunk_count(), 3);
    }

    #[test]
    fn test_inode_file_basic() {
        let file: INodeFile = INodeFile::new(
            2,
            1,
            "test.txt".to_string(),
            "test.txt".to_string(),
            1024,
            UNIX_EPOCH,
            FileContent::SingleHash("hash123".to_string()),
            HashAlgorithm::Xxh128,
        );

        assert_eq!(file.id(), 2);
        assert_eq!(file.parent_id(), 1);
        assert_eq!(file.name(), "test.txt");
        assert_eq!(file.path(), "test.txt");
        assert_eq!(file.size(), 1024);
        assert_eq!(file.inode_type(), INodeType::File);
        assert_eq!(file.permissions(), DEFAULT_FILE_PERMS);
        assert!(!file.is_executable());
    }

    #[test]
    fn test_inode_file_executable() {
        let file: INodeFile = INodeFile::new(
            2,
            1,
            "script.sh".to_string(),
            "script.sh".to_string(),
            512,
            UNIX_EPOCH,
            FileContent::SingleHash("hash123".to_string()),
            HashAlgorithm::Xxh128,
        )
        .with_executable(true);

        assert!(file.is_executable());
        assert_eq!(file.permissions(), EXECUTABLE_FILE_PERMS);
    }
}
