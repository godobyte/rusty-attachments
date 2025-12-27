//! INode manager for allocating and tracking inodes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusty_attachments_model::HashAlgorithm;

use super::dir::INodeDir;
use super::file::{FileContent, INodeFile};
use super::symlink::INodeSymlink;
use super::types::{INode, INodeId, INodeType, ROOT_INODE};

/// Manages inode allocation and lookup.
///
/// Provides thread-safe access to inodes by ID or path.
pub struct INodeManager {
    /// Next inode ID to allocate.
    next_id: AtomicU64,
    /// All inodes by ID.
    inodes: RwLock<HashMap<INodeId, Arc<dyn INode>>>,
    /// Path to inode ID index.
    path_index: RwLock<HashMap<String, INodeId>>,
}

impl INodeManager {
    /// Create a new inode manager with root directory.
    pub fn new() -> Self {
        let manager = Self {
            next_id: AtomicU64::new(ROOT_INODE + 1),
            inodes: RwLock::new(HashMap::new()),
            path_index: RwLock::new(HashMap::new()),
        };

        // Create root directory
        let root: Arc<INodeDir> =
            Arc::new(INodeDir::new(ROOT_INODE, ROOT_INODE, String::new(), String::new()));

        {
            let mut inodes: std::sync::RwLockWriteGuard<'_, HashMap<INodeId, Arc<dyn INode>>> =
                manager.inodes.write().unwrap();
            inodes.insert(ROOT_INODE, root);
        }
        {
            let mut path_index: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
                manager.path_index.write().unwrap();
            path_index.insert(String::new(), ROOT_INODE);
        }

        manager
    }

    /// Allocate a new inode ID.
    fn allocate_id(&self) -> INodeId {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Get an inode by ID.
    ///
    /// # Arguments
    /// * `id` - Inode ID to look up
    ///
    /// # Returns
    /// The inode if found.
    pub fn get(&self, id: INodeId) -> Option<Arc<dyn INode>> {
        let inodes: std::sync::RwLockReadGuard<'_, HashMap<INodeId, Arc<dyn INode>>> =
            self.inodes.read().unwrap();
        inodes.get(&id).cloned()
    }

    /// Get an inode by path.
    ///
    /// # Arguments
    /// * `path` - Path to look up (empty string for root)
    ///
    /// # Returns
    /// The inode if found.
    pub fn get_by_path(&self, path: &str) -> Option<Arc<dyn INode>> {
        let path_index: std::sync::RwLockReadGuard<'_, HashMap<String, INodeId>> =
            self.path_index.read().unwrap();
        let id: INodeId = *path_index.get(path)?;
        drop(path_index);
        self.get(id)
    }

    /// Get the root directory.
    pub fn root(&self) -> Arc<dyn INode> {
        self.get(ROOT_INODE).expect("Root inode must exist")
    }

    /// Get an inode as a directory using downcasting.
    ///
    /// # Arguments
    /// * `id` - Inode ID
    ///
    /// # Returns
    /// The directory inode if found and is a directory.
    pub fn get_as_dir(&self, id: INodeId) -> Option<Arc<dyn INode>> {
        let inode: Arc<dyn INode> = self.get(id)?;
        if inode.inode_type() == INodeType::Directory {
            Some(inode)
        } else {
            None
        }
    }

    /// Get an inode as a file using downcasting.
    ///
    /// # Arguments
    /// * `id` - Inode ID
    ///
    /// # Returns
    /// The file inode if found and is a file.
    pub fn get_as_file(&self, id: INodeId) -> Option<Arc<dyn INode>> {
        let inode: Arc<dyn INode> = self.get(id)?;
        if inode.inode_type() == INodeType::File {
            Some(inode)
        } else {
            None
        }
    }

    /// Get file content for an inode.
    ///
    /// # Arguments
    /// * `id` - Inode ID
    ///
    /// # Returns
    /// The file content if the inode is a file.
    pub fn get_file_content(&self, id: INodeId) -> Option<FileContent> {
        let inode: Arc<dyn INode> = self.get(id)?;
        if inode.inode_type() != INodeType::File {
            return None;
        }
        // Downcast to INodeFile
        let file: &INodeFile = inode.as_any().downcast_ref::<INodeFile>()?;
        Some(file.content().clone())
    }

    /// Get symlink target for an inode.
    ///
    /// # Arguments
    /// * `id` - Inode ID
    ///
    /// # Returns
    /// The symlink target if the inode is a symlink.
    pub fn get_symlink_target(&self, id: INodeId) -> Option<String> {
        let inode: Arc<dyn INode> = self.get(id)?;
        if inode.inode_type() != INodeType::Symlink {
            return None;
        }
        // Downcast to INodeSymlink
        let symlink: &INodeSymlink = inode.as_any().downcast_ref::<INodeSymlink>()?;
        Some(symlink.target().to_string())
    }

    /// Get children of a directory.
    ///
    /// # Arguments
    /// * `id` - Directory inode ID
    ///
    /// # Returns
    /// Vector of (name, inode_id) pairs if the inode is a directory.
    pub fn get_dir_children(&self, id: INodeId) -> Option<Vec<(String, INodeId)>> {
        let inode: Arc<dyn INode> = self.get(id)?;
        if inode.inode_type() != INodeType::Directory {
            return None;
        }
        // Downcast to INodeDir
        let dir: &INodeDir = inode.as_any().downcast_ref::<INodeDir>()?;
        Some(dir.children())
    }

    /// Add a directory, creating parent directories as needed.
    ///
    /// # Arguments
    /// * `path` - Directory path (e.g., "a/b/c")
    ///
    /// # Returns
    /// The inode ID of the directory.
    pub fn add_directory(&self, path: &str) -> INodeId {
        if path.is_empty() {
            return ROOT_INODE;
        }

        // Check if already exists
        {
            let path_index: std::sync::RwLockReadGuard<'_, HashMap<String, INodeId>> =
                self.path_index.read().unwrap();
            if let Some(&id) = path_index.get(path) {
                return id;
            }
        }

        // Create parent directories first
        let parts: Vec<&str> = path.split('/').collect();
        let mut current_path = String::new();
        let mut parent_id: INodeId = ROOT_INODE;

        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                current_path.push('/');
            }
            current_path.push_str(part);

            // Check if this path exists
            let existing_id: Option<INodeId> = {
                let path_index: std::sync::RwLockReadGuard<'_, HashMap<String, INodeId>> =
                    self.path_index.read().unwrap();
                path_index.get(&current_path).copied()
            };

            if let Some(id) = existing_id {
                parent_id = id;
            } else {
                // Create new directory
                let id: INodeId = self.allocate_id();
                let dir: Arc<INodeDir> = Arc::new(INodeDir::new(
                    id,
                    parent_id,
                    (*part).to_string(),
                    current_path.clone(),
                ));

                // Add to parent's children
                if let Some(parent) = self.get(parent_id) {
                    if let Some(parent_dir) = parent.as_any().downcast_ref::<INodeDir>() {
                        parent_dir.add_child((*part).to_string(), id);
                    }
                }

                // Insert into maps
                {
                    let mut inodes: std::sync::RwLockWriteGuard<
                        '_,
                        HashMap<INodeId, Arc<dyn INode>>,
                    > = self.inodes.write().unwrap();
                    inodes.insert(id, dir);
                }
                {
                    let mut path_index: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
                        self.path_index.write().unwrap();
                    path_index.insert(current_path.clone(), id);
                }

                parent_id = id;
            }
        }

        parent_id
    }

    /// Add a file inode.
    ///
    /// # Arguments
    /// * `path` - File path
    /// * `size` - File size in bytes
    /// * `mtime_micros` - Modification time in microseconds since epoch
    /// * `content` - File content (hash or chunk hashes)
    /// * `hash_algorithm` - Hash algorithm used
    /// * `executable` - Whether file is executable
    ///
    /// # Returns
    /// The inode ID of the file.
    #[allow(clippy::too_many_arguments)]
    pub fn add_file(
        &self,
        path: &str,
        size: u64,
        mtime_micros: i64,
        content: FileContent,
        hash_algorithm: HashAlgorithm,
        executable: bool,
    ) -> INodeId {
        let (parent_path, name) = split_path(path);

        // Ensure parent directory exists
        let parent_id: INodeId = self.add_directory(parent_path);

        // Create file inode
        let id: INodeId = self.allocate_id();
        let mtime: SystemTime = micros_to_system_time(mtime_micros);
        let file: Arc<INodeFile> = Arc::new(
            INodeFile::new(
                id,
                parent_id,
                name.to_string(),
                path.to_string(),
                size,
                mtime,
                content,
                hash_algorithm,
            )
            .with_executable(executable),
        );

        // Add to parent's children
        if let Some(parent) = self.get(parent_id) {
            if let Some(parent_dir) = parent.as_any().downcast_ref::<INodeDir>() {
                parent_dir.add_child(name.to_string(), id);
            }
        }

        // Insert into maps
        {
            let mut inodes: std::sync::RwLockWriteGuard<'_, HashMap<INodeId, Arc<dyn INode>>> =
                self.inodes.write().unwrap();
            inodes.insert(id, file);
        }
        {
            let mut path_index: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
                self.path_index.write().unwrap();
            path_index.insert(path.to_string(), id);
        }

        id
    }

    /// Add a symlink inode.
    ///
    /// # Arguments
    /// * `path` - Symlink path
    /// * `target` - Target path (relative)
    ///
    /// # Returns
    /// The inode ID of the symlink.
    pub fn add_symlink(&self, path: &str, target: &str) -> INodeId {
        let (parent_path, name) = split_path(path);

        // Ensure parent directory exists
        let parent_id: INodeId = self.add_directory(parent_path);

        // Create symlink inode
        let id: INodeId = self.allocate_id();
        let symlink: Arc<INodeSymlink> = Arc::new(INodeSymlink::new(
            id,
            parent_id,
            name.to_string(),
            path.to_string(),
            target.to_string(),
        ));

        // Add to parent's children
        if let Some(parent) = self.get(parent_id) {
            if let Some(parent_dir) = parent.as_any().downcast_ref::<INodeDir>() {
                parent_dir.add_child(name.to_string(), id);
            }
        }

        // Insert into maps
        {
            let mut inodes: std::sync::RwLockWriteGuard<'_, HashMap<INodeId, Arc<dyn INode>>> =
                self.inodes.write().unwrap();
            inodes.insert(id, symlink);
        }
        {
            let mut path_index: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
                self.path_index.write().unwrap();
            path_index.insert(path.to_string(), id);
        }

        id
    }

    /// Get the total number of inodes.
    pub fn inode_count(&self) -> usize {
        let inodes: std::sync::RwLockReadGuard<'_, HashMap<INodeId, Arc<dyn INode>>> =
            self.inodes.read().unwrap();
        inodes.len()
    }

    /// Remove a child from a parent directory.
    ///
    /// # Arguments
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Child name to remove
    ///
    /// # Returns
    /// Ok if removed, Err if parent not found or not a directory.
    pub fn remove_child(&self, parent_id: INodeId, name: &str) -> Result<(), crate::VfsError> {
        let parent: Arc<dyn INode> = self
            .get(parent_id)
            .ok_or(crate::VfsError::InodeNotFound(parent_id))?;

        let parent_dir: &INodeDir = parent
            .as_any()
            .downcast_ref::<INodeDir>()
            .ok_or(crate::VfsError::NotADirectory(parent_id))?;

        parent_dir.remove_child(name);
        Ok(())
    }

    /// Remove an inode from the manager.
    ///
    /// # Arguments
    /// * `id` - Inode ID to remove
    ///
    /// # Returns
    /// Ok if removed, Err if not found.
    pub fn remove_inode(&self, id: INodeId) -> Result<(), crate::VfsError> {
        // Get path before removing
        let path: String = {
            let inode: Arc<dyn INode> = self
                .get(id)
                .ok_or(crate::VfsError::InodeNotFound(id))?;
            inode.path().to_string()
        };

        // Remove from inodes map
        {
            let mut inodes: std::sync::RwLockWriteGuard<'_, HashMap<INodeId, Arc<dyn INode>>> =
                self.inodes.write().unwrap();
            inodes.remove(&id);
        }

        // Remove from path index
        {
            let mut path_index: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
                self.path_index.write().unwrap();
            path_index.remove(&path);
        }

        Ok(())
    }
}

impl Default for INodeManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Split a path into parent directory and name.
///
/// # Arguments
/// * `path` - Path to split
///
/// # Returns
/// (parent_path, name) tuple.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => ("", path),
    }
}

/// Convert microseconds since epoch to SystemTime.
///
/// # Arguments
/// * `micros` - Microseconds since Unix epoch
///
/// # Returns
/// SystemTime representing the timestamp.
fn micros_to_system_time(micros: i64) -> SystemTime {
    if micros >= 0 {
        UNIX_EPOCH + Duration::from_micros(micros as u64)
    } else {
        UNIX_EPOCH - Duration::from_micros((-micros) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_manager_has_root() {
        let manager: INodeManager = INodeManager::new();
        let root: Arc<dyn INode> = manager.root();
        assert_eq!(root.id(), ROOT_INODE);
        assert_eq!(root.inode_type(), INodeType::Directory);
        assert_eq!(root.path(), "");
    }

    #[test]
    fn test_add_directory() {
        let manager: INodeManager = INodeManager::new();
        let id: INodeId = manager.add_directory("a/b/c");

        assert!(id > ROOT_INODE);

        // Check all directories were created
        assert!(manager.get_by_path("a").is_some());
        assert!(manager.get_by_path("a/b").is_some());
        assert!(manager.get_by_path("a/b/c").is_some());

        // Check parent-child relationships
        let a: Arc<dyn INode> = manager.get_by_path("a").unwrap();
        assert_eq!(a.parent_id(), ROOT_INODE);
    }

    #[test]
    fn test_add_file() {
        let manager: INodeManager = INodeManager::new();
        let id: INodeId = manager.add_file(
            "dir/file.txt",
            1024,
            1234567890,
            FileContent::SingleHash("hash123".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        let file: Arc<dyn INode> = manager.get(id).unwrap();
        assert_eq!(file.name(), "file.txt");
        assert_eq!(file.path(), "dir/file.txt");
        assert_eq!(file.size(), 1024);
        assert_eq!(file.inode_type(), INodeType::File);

        // Check parent directory was created
        let dir: Arc<dyn INode> = manager.get_by_path("dir").unwrap();
        assert_eq!(dir.inode_type(), INodeType::Directory);
    }

    #[test]
    fn test_add_symlink() {
        let manager: INodeManager = INodeManager::new();
        let id: INodeId = manager.add_symlink("link.txt", "target.txt");

        let symlink: Arc<dyn INode> = manager.get(id).unwrap();
        assert_eq!(symlink.name(), "link.txt");
        assert_eq!(symlink.inode_type(), INodeType::Symlink);
    }

    #[test]
    fn test_split_path() {
        assert_eq!(split_path("a/b/c"), ("a/b", "c"));
        assert_eq!(split_path("file.txt"), ("", "file.txt"));
        assert_eq!(split_path("dir/file.txt"), ("dir", "file.txt"));
    }

    #[test]
    fn test_micros_to_system_time() {
        let time: SystemTime = micros_to_system_time(1_000_000);
        assert_eq!(time, UNIX_EPOCH + Duration::from_secs(1));
    }

    #[test]
    fn test_remove_child() {
        let manager: INodeManager = INodeManager::new();

        // Add a directory with a file
        manager.add_directory("parent");
        manager.add_file(
            "parent/file.txt",
            100,
            0,
            FileContent::SingleHash("hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Verify file exists
        let parent: Arc<dyn INode> = manager.get_by_path("parent").unwrap();
        let parent_dir: &INodeDir = parent.as_any().downcast_ref::<INodeDir>().unwrap();
        assert_eq!(parent_dir.child_count(), 1);

        // Remove the child
        manager.remove_child(parent.id(), "file.txt").unwrap();

        // Verify child is removed from parent
        assert_eq!(parent_dir.child_count(), 0);
    }

    #[test]
    fn test_remove_inode() {
        let manager: INodeManager = INodeManager::new();

        // Add a directory
        let dir_id: INodeId = manager.add_directory("test_dir");

        // Verify it exists
        assert!(manager.get(dir_id).is_some());
        assert!(manager.get_by_path("test_dir").is_some());

        // Remove the inode
        manager.remove_inode(dir_id).unwrap();

        // Verify it's gone from both maps
        assert!(manager.get(dir_id).is_none());
        assert!(manager.get_by_path("test_dir").is_none());
    }

    #[test]
    fn test_remove_child_not_a_directory() {
        let manager: INodeManager = INodeManager::new();

        let file_id: INodeId = manager.add_file(
            "file.txt",
            100,
            0,
            FileContent::SingleHash("hash".to_string()),
            HashAlgorithm::Xxh128,
            false,
        );

        // Try to remove child from a file - should fail
        let result = manager.remove_child(file_id, "child");
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_inode_not_found() {
        let manager: INodeManager = INodeManager::new();

        // Try to remove non-existent inode
        let result = manager.remove_inode(99999);
        assert!(result.is_err());
    }
}
