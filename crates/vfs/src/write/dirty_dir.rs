//! Dirty directory tracking for writable VFS.
//!
//! This module provides data structures for tracking created and deleted
//! directories in the writable VFS layer.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use crate::inode::{INode, INodeId, INodeManager, INodeType};
use crate::VfsError;

/// State of a dirty directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyDirState {
    /// Directory is newly created (not in original manifest).
    New,
    /// Directory has been deleted.
    Deleted,
}

/// A directory that has been created or deleted.
///
/// Unlike files, directories don't have content to track.
/// We only need to track their existence state for diff manifests.
#[derive(Debug)]
pub struct DirtyDir {
    /// Inode ID of this directory.
    inode_id: INodeId,
    /// Relative path within the VFS.
    rel_path: String,
    /// Current state of the directory.
    state: DirtyDirState,
    /// Modification time.
    mtime: SystemTime,
}

impl DirtyDir {
    /// Create a new dirty directory entry.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the directory
    /// * `rel_path` - Relative path within VFS
    /// * `state` - Initial state (New or Deleted)
    pub fn new(inode_id: INodeId, rel_path: String, state: DirtyDirState) -> Self {
        Self {
            inode_id,
            rel_path,
            state,
            mtime: SystemTime::now(),
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

    /// Get current state.
    pub fn state(&self) -> DirtyDirState {
        self.state
    }

    /// Get modification time.
    pub fn mtime(&self) -> SystemTime {
        self.mtime
    }

    /// Mark directory as deleted.
    pub fn mark_deleted(&mut self) {
        self.state = DirtyDirState::Deleted;
        self.mtime = SystemTime::now();
    }
}

/// Summary of a dirty directory for export.
#[derive(Debug, Clone)]
pub struct DirtyDirEntry {
    /// Relative path.
    pub path: String,
    /// Current state.
    pub state: DirtyDirState,
}

/// Manages dirty (created/deleted) directories.
///
/// Tracks directory changes for diff manifest generation.
///
/// # Lock Safety
///
/// This struct uses `RwLock` for internal synchronization. Lock operations use
/// `unwrap()` because lock poisoning indicates unrecoverable internal state
/// corruption. See `DirtyFileManager` documentation for details.
pub struct DirtyDirManager {
    /// Map of inode ID to dirty directory.
    dirty_dirs: RwLock<HashMap<INodeId, DirtyDir>>,
    /// Reference to inode manager.
    inodes: Arc<INodeManager>,
    /// Set of original directory paths from manifest.
    original_dirs: RwLock<HashSet<String>>,
}

impl DirtyDirManager {
    /// Create a new dirty directory manager.
    ///
    /// # Arguments
    /// * `inodes` - Inode manager for metadata
    /// * `original_dirs` - Set of directory paths from original manifest
    pub fn new(inodes: Arc<INodeManager>, original_dirs: HashSet<String>) -> Self {
        Self {
            dirty_dirs: RwLock::new(HashMap::new()),
            inodes,
            original_dirs: RwLock::new(original_dirs),
        }
    }

    /// Check if a directory is dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    pub fn is_dirty(&self, inode_id: INodeId) -> bool {
        self.dirty_dirs.read().unwrap().contains_key(&inode_id)
    }

    /// Get dirty directory state if dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// State if dirty, None otherwise.
    pub fn get_state(&self, inode_id: INodeId) -> Option<DirtyDirState> {
        let guard = self.dirty_dirs.read().unwrap();
        guard.get(&inode_id).map(|d| d.state())
    }

    /// Check if an inode is a new directory (created, not from manifest).
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// True if this is a new directory, false otherwise.
    pub fn is_new_dir(&self, inode_id: INodeId) -> bool {
        let guard = self.dirty_dirs.read().unwrap();
        guard
            .get(&inode_id)
            .map(|d| d.state() == DirtyDirState::New)
            .unwrap_or(false)
    }

    /// Get the path of a new directory by inode ID.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to look up
    ///
    /// # Returns
    /// Path if found and is a new directory, None otherwise.
    pub fn get_new_dir_path(&self, inode_id: INodeId) -> Option<String> {
        let guard = self.dirty_dirs.read().unwrap();
        guard
            .get(&inode_id)
            .filter(|d| d.state() == DirtyDirState::New)
            .map(|d| d.rel_path().to_string())
    }

    /// Create a new directory.
    ///
    /// # Arguments
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Name of the new directory
    ///
    /// # Returns
    /// Inode ID of the new directory.
    pub fn create_dir(&self, parent_id: INodeId, name: &str) -> Result<INodeId, VfsError> {
        // Get parent inode
        let parent_inode: Arc<dyn INode> = self
            .inodes
            .get(parent_id)
            .ok_or(VfsError::InodeNotFound(parent_id))?;

        if parent_inode.inode_type() != INodeType::Directory {
            return Err(VfsError::NotADirectory(parent_id));
        }

        // Build new directory path
        let parent_path: &str = parent_inode.path();
        let new_path: String = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        };

        // Check if directory already exists
        if self.inodes.get_by_path(&new_path).is_some() {
            return Err(VfsError::AlreadyExists(new_path));
        }

        // Create directory in inode manager
        let new_id: INodeId = self.inodes.add_directory(&new_path);

        // Check if this was an original directory that was deleted and is being recreated
        let is_original: bool = self.original_dirs.read().unwrap().contains(&new_path);

        // Check if there's an existing deleted entry for this path
        let existing_deleted_id: Option<INodeId> = {
            let guard = self.dirty_dirs.read().unwrap();
            guard
                .iter()
                .find(|(_, d)| d.rel_path() == new_path && d.state() == DirtyDirState::Deleted)
                .map(|(id, _)| *id)
        };

        if let Some(deleted_id) = existing_deleted_id {
            // Remove the deleted entry - we're recreating it
            self.dirty_dirs.write().unwrap().remove(&deleted_id);
        } else if !is_original {
            // Track as new directory
            let dirty = DirtyDir::new(new_id, new_path, DirtyDirState::New);
            self.dirty_dirs.write().unwrap().insert(new_id, dirty);
        }

        Ok(new_id)
    }

    /// Delete an empty directory.
    ///
    /// # Arguments
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Name of the directory to delete
    pub fn delete_dir(&self, parent_id: INodeId, name: &str) -> Result<(), VfsError> {
        // Find the directory
        let children: Vec<(String, INodeId)> = self
            .inodes
            .get_dir_children(parent_id)
            .ok_or(VfsError::NotADirectory(parent_id))?;

        let dir_id: INodeId = children
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
            .ok_or(VfsError::NotFound(name.to_string()))?;

        let dir_inode: Arc<dyn INode> = self
            .inodes
            .get(dir_id)
            .ok_or(VfsError::InodeNotFound(dir_id))?;

        // Verify it's a directory
        if dir_inode.inode_type() != INodeType::Directory {
            return Err(VfsError::NotADirectory(dir_id));
        }

        // Check if directory is empty
        let dir_children: Vec<(String, INodeId)> = self
            .inodes
            .get_dir_children(dir_id)
            .ok_or(VfsError::NotADirectory(dir_id))?;

        if !dir_children.is_empty() {
            return Err(VfsError::DirectoryNotEmpty(dir_inode.path().to_string()));
        }

        let dir_path: String = dir_inode.path().to_string();

        // Check if this is an original directory or a newly created one
        let is_original: bool = self.original_dirs.read().unwrap().contains(&dir_path);
        let was_new: bool = self
            .dirty_dirs
            .read()
            .unwrap()
            .get(&dir_id)
            .map(|d| d.state() == DirtyDirState::New)
            .unwrap_or(false);

        if was_new {
            // Newly created directory being deleted - just remove from tracking
            self.dirty_dirs.write().unwrap().remove(&dir_id);
        } else if is_original {
            // Original directory being deleted - track as deleted
            let dirty = DirtyDir::new(dir_id, dir_path.clone(), DirtyDirState::Deleted);
            self.dirty_dirs.write().unwrap().insert(dir_id, dirty);
        }
        // else: not original and not new - shouldn't happen, but safe to ignore

        // Remove from inode manager
        self.inodes.remove_child(parent_id, name)?;
        self.inodes.remove_inode(dir_id)?;

        Ok(())
    }

    /// Get all dirty directory entries for diff manifest generation.
    ///
    /// # Returns
    /// Vector of dirty directory entries.
    pub fn get_dirty_dir_entries(&self) -> Vec<DirtyDirEntry> {
        let guard = self.dirty_dirs.read().unwrap();
        guard
            .values()
            .map(|dirty| DirtyDirEntry {
                path: dirty.rel_path().to_string(),
                state: dirty.state(),
            })
            .collect()
    }

    /// Clear all dirty state after successful export.
    pub fn clear(&self) {
        self.dirty_dirs.write().unwrap().clear();
    }

    /// Get new directories in a parent directory.
    ///
    /// # Arguments
    /// * `parent_id` - Parent directory inode ID
    ///
    /// # Returns
    /// Vector of (inode_id, dir_name) for new directories in the parent.
    pub fn get_new_dirs_in_parent(&self, parent_id: INodeId) -> Vec<(INodeId, String)> {
        let guard = self.dirty_dirs.read().unwrap();
        guard
            .values()
            .filter(|d| d.state() == DirtyDirState::New)
            .filter_map(|d| {
                // Check if this directory's parent matches
                if let Some(inode) = self.inodes.get(d.inode_id()) {
                    if inode.parent_id() == parent_id {
                        return Some((d.inode_id(), inode.name().to_string()));
                    }
                }
                None
            })
            .collect()
    }

    /// Look up a new directory by parent inode and name.
    ///
    /// # Arguments
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Directory name to look up
    ///
    /// # Returns
    /// Inode ID if found, None otherwise.
    pub fn lookup_new_dir(&self, parent_id: INodeId, name: &str) -> Option<INodeId> {
        let guard = self.dirty_dirs.read().unwrap();
        guard
            .values()
            .find(|d| {
                if d.state() != DirtyDirState::New {
                    return false;
                }
                if let Some(inode) = self.inodes.get(d.inode_id()) {
                    return inode.parent_id() == parent_id && inode.name() == name;
                }
                false
            })
            .map(|d| d.inode_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inode::ROOT_INODE;

    fn create_test_manager() -> (DirtyDirManager, Arc<INodeManager>) {
        let inodes = Arc::new(INodeManager::new());
        let original_dirs: HashSet<String> = HashSet::new();
        let manager = DirtyDirManager::new(inodes.clone(), original_dirs);
        (manager, inodes)
    }

    fn create_test_manager_with_originals(
        original_paths: Vec<&str>,
    ) -> (DirtyDirManager, Arc<INodeManager>) {
        let inodes = Arc::new(INodeManager::new());
        // Add original directories to inode manager
        for path in &original_paths {
            inodes.add_directory(path);
        }
        let original_dirs: HashSet<String> = original_paths.iter().map(|s| s.to_string()).collect();
        let manager = DirtyDirManager::new(inodes.clone(), original_dirs);
        (manager, inodes)
    }

    #[test]
    fn test_dirty_dir_state() {
        assert_eq!(DirtyDirState::New, DirtyDirState::New);
        assert_ne!(DirtyDirState::New, DirtyDirState::Deleted);
    }

    #[test]
    fn test_dirty_dir_basic() {
        let dirty = DirtyDir::new(1, "test_dir".to_string(), DirtyDirState::New);
        assert_eq!(dirty.inode_id(), 1);
        assert_eq!(dirty.rel_path(), "test_dir");
        assert_eq!(dirty.state(), DirtyDirState::New);
    }

    #[test]
    fn test_dirty_dir_mark_deleted() {
        let mut dirty = DirtyDir::new(1, "test_dir".to_string(), DirtyDirState::New);
        dirty.mark_deleted();
        assert_eq!(dirty.state(), DirtyDirState::Deleted);
    }

    #[test]
    fn test_create_new_directory() {
        let (manager, _inodes) = create_test_manager();

        let id: INodeId = manager.create_dir(ROOT_INODE, "new_dir").unwrap();

        assert!(manager.is_dirty(id));
        assert_eq!(manager.get_state(id), Some(DirtyDirState::New));
        assert!(manager.is_new_dir(id));
    }

    #[test]
    fn test_create_nested_directory() {
        let (manager, inodes) = create_test_manager();

        // Create parent directory first
        let parent_id: INodeId = manager.create_dir(ROOT_INODE, "parent").unwrap();
        assert!(manager.is_dirty(parent_id));

        // Create child directory
        let child_id: INodeId = manager.create_dir(parent_id, "child").unwrap();
        assert!(manager.is_dirty(child_id));

        // Verify paths
        let parent_inode: Arc<dyn INode> = inodes.get(parent_id).unwrap();
        assert_eq!(parent_inode.path(), "parent");

        let child_inode: Arc<dyn INode> = inodes.get(child_id).unwrap();
        assert_eq!(child_inode.path(), "parent/child");
    }

    #[test]
    fn test_create_directory_already_exists() {
        let (manager, _inodes) = create_test_manager();

        manager.create_dir(ROOT_INODE, "existing").unwrap();
        let result = manager.create_dir(ROOT_INODE, "existing");

        assert!(matches!(result, Err(VfsError::AlreadyExists(_))));
    }

    #[test]
    fn test_delete_empty_new_directory() {
        let (manager, _inodes) = create_test_manager();

        // Create and then delete a new directory
        let id: INodeId = manager.create_dir(ROOT_INODE, "new_dir").unwrap();
        assert!(manager.is_dirty(id));

        manager.delete_dir(ROOT_INODE, "new_dir").unwrap();

        // Should no longer be tracked (was new, now deleted = no change)
        assert!(!manager.is_dirty(id));
        assert_eq!(manager.get_dirty_dir_entries().len(), 0);
    }

    #[test]
    fn test_delete_original_directory() {
        let (manager, _inodes) = create_test_manager_with_originals(vec!["existing"]);

        // Delete original directory
        manager.delete_dir(ROOT_INODE, "existing").unwrap();

        // Should be tracked as deleted
        let entries: Vec<DirtyDirEntry> = manager.get_dirty_dir_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, DirtyDirState::Deleted);
        assert_eq!(entries[0].path, "existing");
    }

    #[test]
    fn test_delete_non_empty_directory_fails() {
        let (manager, inodes) = create_test_manager();

        // Create directory with a subdirectory
        let dir_id: INodeId = manager.create_dir(ROOT_INODE, "dir").unwrap();
        manager.create_dir(dir_id, "subdir").unwrap();

        // Try to delete - should fail
        let result = manager.delete_dir(ROOT_INODE, "dir");
        assert!(matches!(result, Err(VfsError::DirectoryNotEmpty(_))));
    }

    #[test]
    fn test_recreate_deleted_original_directory() {
        let (manager, _inodes) = create_test_manager_with_originals(vec!["orig_dir"]);

        // Delete original directory
        manager.delete_dir(ROOT_INODE, "orig_dir").unwrap();
        assert_eq!(manager.get_dirty_dir_entries().len(), 1);

        // Recreate it
        manager.create_dir(ROOT_INODE, "orig_dir").unwrap();

        // Should no longer be dirty (back to original state)
        assert_eq!(manager.get_dirty_dir_entries().len(), 0);
    }

    #[test]
    fn test_delete_nonexistent_directory() {
        let (manager, _inodes) = create_test_manager();

        let result = manager.delete_dir(ROOT_INODE, "nonexistent");
        assert!(matches!(result, Err(VfsError::NotFound(_))));
    }

    #[test]
    fn test_get_dirty_dir_entries() {
        let (manager, _inodes) = create_test_manager_with_originals(vec!["orig1", "orig2"]);

        // Create a new directory
        manager.create_dir(ROOT_INODE, "new_dir").unwrap();

        // Delete an original directory
        manager.delete_dir(ROOT_INODE, "orig1").unwrap();

        let entries: Vec<DirtyDirEntry> = manager.get_dirty_dir_entries();
        assert_eq!(entries.len(), 2);

        let new_entry: Option<&DirtyDirEntry> = entries.iter().find(|e| e.path == "new_dir");
        assert!(new_entry.is_some());
        assert_eq!(new_entry.unwrap().state, DirtyDirState::New);

        let deleted_entry: Option<&DirtyDirEntry> = entries.iter().find(|e| e.path == "orig1");
        assert!(deleted_entry.is_some());
        assert_eq!(deleted_entry.unwrap().state, DirtyDirState::Deleted);
    }

    #[test]
    fn test_clear() {
        let (manager, _inodes) = create_test_manager();

        manager.create_dir(ROOT_INODE, "dir1").unwrap();
        manager.create_dir(ROOT_INODE, "dir2").unwrap();

        assert_eq!(manager.get_dirty_dir_entries().len(), 2);

        manager.clear();

        assert_eq!(manager.get_dirty_dir_entries().len(), 0);
    }

    #[test]
    fn test_get_new_dirs_in_parent() {
        let (manager, _inodes) = create_test_manager();

        // Create directories in root
        manager.create_dir(ROOT_INODE, "dir1").unwrap();
        let parent_id: INodeId = manager.create_dir(ROOT_INODE, "parent").unwrap();

        // Create directory in parent
        manager.create_dir(parent_id, "child").unwrap();

        // Get new dirs in root
        let root_dirs: Vec<(INodeId, String)> = manager.get_new_dirs_in_parent(ROOT_INODE);
        assert_eq!(root_dirs.len(), 2);

        // Get new dirs in parent
        let parent_dirs: Vec<(INodeId, String)> = manager.get_new_dirs_in_parent(parent_id);
        assert_eq!(parent_dirs.len(), 1);
        assert_eq!(parent_dirs[0].1, "child");
    }

    #[test]
    fn test_lookup_new_dir() {
        let (manager, _inodes) = create_test_manager();

        let id: INodeId = manager.create_dir(ROOT_INODE, "new_dir").unwrap();

        assert_eq!(manager.lookup_new_dir(ROOT_INODE, "new_dir"), Some(id));
        assert_eq!(manager.lookup_new_dir(ROOT_INODE, "nonexistent"), None);
    }

    #[test]
    fn test_create_new_dir_in_existing_manifest_dir() {
        // Test creating a new directory inside an existing manifest directory
        let (manager, inodes) = create_test_manager_with_originals(vec!["existing_parent"]);

        // Get the existing parent directory's inode ID
        let parent_inode: Arc<dyn INode> = inodes.get_by_path("existing_parent").unwrap();
        let parent_id: INodeId = parent_inode.id();

        // Create a new subdirectory inside the existing manifest directory
        let new_id: INodeId = manager.create_dir(parent_id, "new_child").unwrap();

        // Should be tracked as new
        assert!(manager.is_dirty(new_id));
        assert_eq!(manager.get_state(new_id), Some(DirtyDirState::New));

        // Verify the path is correct
        let new_inode: Arc<dyn INode> = inodes.get(new_id).unwrap();
        assert_eq!(new_inode.path(), "existing_parent/new_child");

        // The parent should NOT be dirty (it's from manifest)
        assert!(!manager.is_dirty(parent_id));
    }

    #[test]
    fn test_delete_manifest_dir_with_new_subdir() {
        // Test that deleting a manifest directory fails if it has a new subdirectory
        let (manager, inodes) = create_test_manager_with_originals(vec!["parent"]);

        let parent_inode: Arc<dyn INode> = inodes.get_by_path("parent").unwrap();
        let parent_id: INodeId = parent_inode.id();

        // Create a new subdirectory
        manager.create_dir(parent_id, "new_child").unwrap();

        // Try to delete parent - should fail because it's not empty
        let result = manager.delete_dir(ROOT_INODE, "parent");
        assert!(matches!(result, Err(VfsError::DirectoryNotEmpty(_))));
    }

    #[test]
    fn test_delete_new_subdir_then_manifest_dir() {
        // Test deleting a new subdirectory, then the manifest parent
        let (manager, inodes) = create_test_manager_with_originals(vec!["parent"]);

        let parent_inode: Arc<dyn INode> = inodes.get_by_path("parent").unwrap();
        let parent_id: INodeId = parent_inode.id();

        // Create a new subdirectory
        manager.create_dir(parent_id, "new_child").unwrap();

        // Delete the new subdirectory first
        manager.delete_dir(parent_id, "new_child").unwrap();

        // Now delete the parent - should succeed
        manager.delete_dir(ROOT_INODE, "parent").unwrap();

        // Parent should be tracked as deleted
        let entries: Vec<DirtyDirEntry> = manager.get_dirty_dir_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "parent");
        assert_eq!(entries[0].state, DirtyDirState::Deleted);
    }

    #[test]
    fn test_create_deeply_nested_new_dirs() {
        // Test creating multiple levels of new directories
        let (manager, inodes) = create_test_manager();

        // Create a/b/c hierarchy
        let a_id: INodeId = manager.create_dir(ROOT_INODE, "a").unwrap();
        let b_id: INodeId = manager.create_dir(a_id, "b").unwrap();
        let c_id: INodeId = manager.create_dir(b_id, "c").unwrap();

        // All should be tracked as new
        assert!(manager.is_new_dir(a_id));
        assert!(manager.is_new_dir(b_id));
        assert!(manager.is_new_dir(c_id));

        // Verify paths
        assert_eq!(inodes.get(a_id).unwrap().path(), "a");
        assert_eq!(inodes.get(b_id).unwrap().path(), "a/b");
        assert_eq!(inodes.get(c_id).unwrap().path(), "a/b/c");

        // Should have 3 dirty entries
        assert_eq!(manager.get_dirty_dir_entries().len(), 3);
    }

    #[test]
    fn test_delete_nested_dirs_bottom_up() {
        // Test deleting nested directories from bottom to top
        let (manager, _inodes) = create_test_manager();

        // Create a/b/c hierarchy
        let a_id: INodeId = manager.create_dir(ROOT_INODE, "a").unwrap();
        let b_id: INodeId = manager.create_dir(a_id, "b").unwrap();
        let _c_id: INodeId = manager.create_dir(b_id, "c").unwrap();

        assert_eq!(manager.get_dirty_dir_entries().len(), 3);

        // Delete from bottom up
        manager.delete_dir(b_id, "c").unwrap();
        assert_eq!(manager.get_dirty_dir_entries().len(), 2);

        manager.delete_dir(a_id, "b").unwrap();
        assert_eq!(manager.get_dirty_dir_entries().len(), 1);

        manager.delete_dir(ROOT_INODE, "a").unwrap();
        assert_eq!(manager.get_dirty_dir_entries().len(), 0);
    }

    #[test]
    fn test_create_dir_in_nonexistent_parent() {
        let (manager, _inodes) = create_test_manager();

        // Try to create directory in non-existent parent
        let result = manager.create_dir(99999, "child");
        assert!(matches!(result, Err(VfsError::InodeNotFound(99999))));
    }

    #[test]
    fn test_delete_dir_from_nonexistent_parent() {
        let (manager, _inodes) = create_test_manager();

        // Try to delete from non-existent parent
        let result = manager.delete_dir(99999, "child");
        assert!(matches!(result, Err(VfsError::NotADirectory(99999))));
    }
}
