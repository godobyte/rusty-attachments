//! Symlink inode implementation.

use std::any::Any;
use std::time::SystemTime;

use super::types::{INode, INodeId, INodeType};

/// Symlink permissions (always 0o777 - target determines access).
pub const SYMLINK_PERMS: u16 = 0o777;

/// Symlink inode representing a symbolic link.
#[derive(Debug)]
pub struct INodeSymlink {
    /// Inode ID.
    id: INodeId,
    /// Parent directory inode ID.
    parent_id: INodeId,
    /// Symlink name.
    name: String,
    /// Full path from root.
    path: String,
    /// Target path (relative).
    target: String,
}

impl INodeSymlink {
    /// Create a new symlink inode.
    ///
    /// # Arguments
    /// * `id` - Inode ID
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Symlink name
    /// * `path` - Full path from root
    /// * `target` - Target path (relative)
    pub fn new(
        id: INodeId,
        parent_id: INodeId,
        name: String,
        path: String,
        target: String,
    ) -> Self {
        Self {
            id,
            parent_id,
            name,
            path,
            target,
        }
    }

    /// Get the symlink target path.
    pub fn target(&self) -> &str {
        &self.target
    }
}

impl INode for INodeSymlink {
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
        INodeType::Symlink
    }

    fn size(&self) -> u64 {
        self.target.len() as u64
    }

    fn mtime(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn permissions(&self) -> u16 {
        SYMLINK_PERMS
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inode_symlink_basic() {
        let symlink: INodeSymlink = INodeSymlink::new(
            2,
            1,
            "link.txt".to_string(),
            "link.txt".to_string(),
            "target.txt".to_string(),
        );

        assert_eq!(symlink.id(), 2);
        assert_eq!(symlink.parent_id(), 1);
        assert_eq!(symlink.name(), "link.txt");
        assert_eq!(symlink.path(), "link.txt");
        assert_eq!(symlink.target(), "target.txt");
        assert_eq!(symlink.inode_type(), INodeType::Symlink);
        assert_eq!(symlink.permissions(), SYMLINK_PERMS);
        assert_eq!(symlink.size(), 10); // "target.txt".len()
    }
}
