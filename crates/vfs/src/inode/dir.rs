//! Directory inode implementation.

use std::any::Any;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::SystemTime;

use super::types::{INode, INodeId, INodeType};

/// Default directory permissions (rwxr-xr-x).
pub const DEFAULT_DIR_PERMS: u16 = 0o755;

/// Directory inode representing a directory.
#[derive(Debug)]
pub struct INodeDir {
    /// Inode ID.
    id: INodeId,
    /// Parent directory inode ID.
    parent_id: INodeId,
    /// Directory name.
    name: String,
    /// Full path from root.
    path: String,
    /// Child entries: name → inode ID.
    children: RwLock<HashMap<String, INodeId>>,
}

impl INodeDir {
    /// Create a new directory inode.
    ///
    /// # Arguments
    /// * `id` - Inode ID
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Directory name
    /// * `path` - Full path from root
    pub fn new(id: INodeId, parent_id: INodeId, name: String, path: String) -> Self {
        Self {
            id,
            parent_id,
            name,
            path,
            children: RwLock::new(HashMap::new()),
        }
    }

    /// Add a child entry to this directory.
    ///
    /// # Arguments
    /// * `name` - Child entry name
    /// * `id` - Child inode ID
    pub fn add_child(&self, name: String, id: INodeId) {
        let mut children: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
            self.children.write().unwrap();
        children.insert(name, id);
    }

    /// Get a child inode ID by name.
    ///
    /// # Arguments
    /// * `name` - Child entry name
    ///
    /// # Returns
    /// The child inode ID if found.
    pub fn get_child(&self, name: &str) -> Option<INodeId> {
        let children: std::sync::RwLockReadGuard<'_, HashMap<String, INodeId>> =
            self.children.read().unwrap();
        children.get(name).copied()
    }

    /// Get all children as (name, inode_id) pairs.
    ///
    /// # Returns
    /// Vector of (name, inode_id) tuples.
    pub fn children(&self) -> Vec<(String, INodeId)> {
        let children: std::sync::RwLockReadGuard<'_, HashMap<String, INodeId>> =
            self.children.read().unwrap();
        children.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }

    /// Get the number of children.
    pub fn child_count(&self) -> usize {
        let children: std::sync::RwLockReadGuard<'_, HashMap<String, INodeId>> =
            self.children.read().unwrap();
        children.len()
    }

    /// Remove a child entry from this directory.
    ///
    /// # Arguments
    /// * `name` - Child entry name to remove
    ///
    /// # Returns
    /// The removed child inode ID, or None if not found.
    pub fn remove_child(&self, name: &str) -> Option<INodeId> {
        let mut children: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
            self.children.write().unwrap();
        children.remove(name)
    }
}

impl INode for INodeDir {
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
        INodeType::Directory
    }

    fn size(&self) -> u64 {
        0
    }

    fn mtime(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn permissions(&self) -> u16 {
        DEFAULT_DIR_PERMS
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inode_dir_basic() {
        let dir: INodeDir = INodeDir::new(1, 1, "".to_string(), "".to_string());

        assert_eq!(dir.id(), 1);
        assert_eq!(dir.parent_id(), 1);
        assert_eq!(dir.name(), "");
        assert_eq!(dir.path(), "");
        assert_eq!(dir.inode_type(), INodeType::Directory);
        assert_eq!(dir.permissions(), DEFAULT_DIR_PERMS);
        assert_eq!(dir.child_count(), 0);
    }

    #[test]
    fn test_inode_dir_children() {
        let dir: INodeDir = INodeDir::new(1, 1, "root".to_string(), "".to_string());

        dir.add_child("file.txt".to_string(), 2);
        dir.add_child("subdir".to_string(), 3);

        assert_eq!(dir.child_count(), 2);
        assert_eq!(dir.get_child("file.txt"), Some(2));
        assert_eq!(dir.get_child("subdir"), Some(3));
        assert_eq!(dir.get_child("nonexistent"), None);

        let children: Vec<(String, INodeId)> = dir.children();
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn test_inode_dir_remove_child() {
        let dir: INodeDir = INodeDir::new(1, 1, "root".to_string(), "".to_string());

        dir.add_child("file1.txt".to_string(), 2);
        dir.add_child("file2.txt".to_string(), 3);

        assert_eq!(dir.child_count(), 2);

        // Remove existing child
        let removed: Option<INodeId> = dir.remove_child("file1.txt");
        assert_eq!(removed, Some(2));
        assert_eq!(dir.child_count(), 1);
        assert_eq!(dir.get_child("file1.txt"), None);
        assert_eq!(dir.get_child("file2.txt"), Some(3));

        // Remove non-existent child
        let not_found: Option<INodeId> = dir.remove_child("nonexistent");
        assert_eq!(not_found, None);
        assert_eq!(dir.child_count(), 1);
    }
}
