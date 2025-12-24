//! Core INode types and traits.

use std::any::Any;
use std::time::SystemTime;

use rusty_attachments_model::HashAlgorithm;

/// Unique identifier for an inode.
pub type INodeId = u64;

/// Root directory inode ID (always 1 per FUSE convention).
pub const ROOT_INODE: INodeId = 1;

/// Type of inode entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum INodeType {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
}

/// Common trait for all inode types.
pub trait INode: Send + Sync + std::fmt::Debug {
    /// Get the inode ID.
    fn id(&self) -> INodeId;

    /// Get the parent inode ID.
    fn parent_id(&self) -> INodeId;

    /// Get the entry name (filename or directory name).
    fn name(&self) -> &str;

    /// Get the full path from root.
    fn path(&self) -> &str;

    /// Get the inode type.
    fn inode_type(&self) -> INodeType;

    /// Get the size in bytes.
    fn size(&self) -> u64;

    /// Get the modification time.
    fn mtime(&self) -> SystemTime;

    /// Get the permissions (POSIX mode bits).
    fn permissions(&self) -> u16;

    /// Get the hash algorithm (for files).
    fn hash_algorithm(&self) -> Option<HashAlgorithm> {
        None
    }

    /// Downcast to Any for type-safe downcasting.
    fn as_any(&self) -> &dyn Any;
}
