//! Bidirectional path↔inode registry for ProjFS.
//!
//! Provides O(1) lookup in both directions using DashMap for lock-free
//! concurrent access. Paths are normalized (lowercase, forward slashes)
//! for case-insensitive matching on Windows.

use dashmap::DashMap;
use rusty_attachments_vfs::inode::INodeId;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::projection::folder::FolderData;
use crate::projection::types::FolderEntry;
use crate::projection::ManifestProjection;

/// Bidirectional path↔inode registry.
///
/// Provides O(1) lookup in both directions using DashMap for lock-free
/// concurrent access. Paths are normalized (lowercase, forward slashes)
/// for case-insensitive matching.
pub struct PathRegistry {
    /// Path → INodeId mapping.
    path_to_inode: DashMap<String, INodeId>,
    /// INodeId → Path mapping.
    inode_to_path: DashMap<INodeId, String>,
    /// Next inode ID to allocate.
    next_inode: AtomicU64,
}

impl PathRegistry {
    /// Create new empty registry.
    pub fn new() -> Self {
        Self {
            path_to_inode: DashMap::new(),
            inode_to_path: DashMap::new(),
            next_inode: AtomicU64::new(1), // 0 reserved for root
        }
    }

    /// Normalize path for case-insensitive lookup.
    ///
    /// # Arguments
    /// * `path` - Path to normalize
    ///
    /// # Returns
    /// Lowercase path with forward slashes.
    fn normalize(path: &str) -> String {
        path.to_lowercase().replace('\\', "/")
    }

    /// Get or create inode for path.
    ///
    /// # Arguments
    /// * `path` - Relative path
    ///
    /// # Returns
    /// INodeId for the path (existing or newly allocated).
    pub fn get_or_create(&self, path: &str) -> INodeId {
        let normalized: String = Self::normalize(path);

        // Fast path: already exists
        if let Some(id) = self.path_to_inode.get(&normalized) {
            return *id;
        }

        // Slow path: allocate new inode
        let id: INodeId = self.next_inode.fetch_add(1, Ordering::SeqCst);
        self.path_to_inode.insert(normalized.clone(), id);
        self.inode_to_path.insert(id, normalized);
        id
    }

    /// Get inode for path if exists.
    ///
    /// # Arguments
    /// * `path` - Relative path
    ///
    /// # Returns
    /// INodeId if path is registered.
    pub fn get(&self, path: &str) -> Option<INodeId> {
        let normalized: String = Self::normalize(path);
        self.path_to_inode.get(&normalized).map(|r| *r)
    }

    /// Get path for inode.
    ///
    /// # Arguments
    /// * `inode` - INode ID
    ///
    /// # Returns
    /// Path if inode is registered.
    pub fn get_path(&self, inode: INodeId) -> Option<String> {
        self.inode_to_path.get(&inode).map(|r| r.clone())
    }

    /// Remove path from registry.
    ///
    /// # Arguments
    /// * `path` - Path to remove
    ///
    /// # Returns
    /// INodeId that was removed, if any.
    pub fn remove(&self, path: &str) -> Option<INodeId> {
        let normalized: String = Self::normalize(path);
        if let Some((_, id)) = self.path_to_inode.remove(&normalized) {
            self.inode_to_path.remove(&id);
            Some(id)
        } else {
            None
        }
    }

    /// Rename path in registry.
    ///
    /// # Arguments
    /// * `old_path` - Original path
    /// * `new_path` - New path
    ///
    /// # Returns
    /// true if rename succeeded.
    pub fn rename(&self, old_path: &str, new_path: &str) -> bool {
        let old_normalized: String = Self::normalize(old_path);
        let new_normalized: String = Self::normalize(new_path);

        if let Some((_, id)) = self.path_to_inode.remove(&old_normalized) {
            self.path_to_inode.insert(new_normalized.clone(), id);
            self.inode_to_path.insert(id, new_normalized);
            true
        } else {
            false
        }
    }

    /// Pre-populate registry from manifest projection.
    ///
    /// # Arguments
    /// * `projection` - Manifest projection to scan
    pub fn populate_from_projection(&self, projection: &ManifestProjection) {
        let root_guard = projection.root();
        self.populate_folder("", &root_guard);
    }

    /// Recursively populate registry from folder.
    ///
    /// # Arguments
    /// * `parent_path` - Parent path prefix
    /// * `folder` - Folder to scan
    fn populate_folder(&self, parent_path: &str, folder: &FolderData) {
        for entry in folder.children().entries() {
            let entry_name: &str = entry.name();
            let path: String = if parent_path.is_empty() {
                entry_name.to_string()
            } else {
                format!("{}/{}", parent_path, entry_name)
            };

            self.get_or_create(&path);

            if let FolderEntry::Folder(subfolder) = entry {
                self.populate_folder(&path, subfolder);
            }
        }
    }

    /// Check if registry has any entries.
    pub fn is_empty(&self) -> bool {
        self.path_to_inode.is_empty()
    }

    /// Get count of registered paths.
    pub fn len(&self) -> usize {
        self.path_to_inode.len()
    }
}

impl Default for PathRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_or_create() {
        let registry = PathRegistry::new();

        let id1: INodeId = registry.get_or_create("path/to/file.txt");
        let id2: INodeId = registry.get_or_create("path/to/file.txt");

        assert_eq!(id1, id2);
    }

    #[test]
    fn test_case_insensitive() {
        let registry = PathRegistry::new();

        let id1: INodeId = registry.get_or_create("Path/To/File.txt");
        let id2: INodeId = registry.get_or_create("path/to/file.txt");
        let id3: INodeId = registry.get_or_create("PATH/TO/FILE.TXT");

        assert_eq!(id1, id2);
        assert_eq!(id2, id3);
    }

    #[test]
    fn test_backslash_normalization() {
        let registry = PathRegistry::new();

        let id1: INodeId = registry.get_or_create("path/to/file.txt");
        let id2: INodeId = registry.get_or_create("path\\to\\file.txt");

        assert_eq!(id1, id2);
    }

    #[test]
    fn test_get() {
        let registry = PathRegistry::new();

        assert!(registry.get("nonexistent").is_none());

        let id: INodeId = registry.get_or_create("test.txt");
        assert_eq!(registry.get("test.txt"), Some(id));
        assert_eq!(registry.get("TEST.TXT"), Some(id));
    }

    #[test]
    fn test_get_path() {
        let registry = PathRegistry::new();

        let id: INodeId = registry.get_or_create("Path/To/File.txt");
        let path: Option<String> = registry.get_path(id);

        // Path is normalized to lowercase
        assert_eq!(path, Some("path/to/file.txt".to_string()));
    }

    #[test]
    fn test_remove() {
        let registry = PathRegistry::new();

        let id: INodeId = registry.get_or_create("test.txt");
        assert!(registry.get("test.txt").is_some());

        let removed: Option<INodeId> = registry.remove("test.txt");
        assert_eq!(removed, Some(id));
        assert!(registry.get("test.txt").is_none());
        assert!(registry.get_path(id).is_none());
    }

    #[test]
    fn test_rename() {
        let registry = PathRegistry::new();

        let id: INodeId = registry.get_or_create("old.txt");
        assert!(registry.rename("old.txt", "new.txt"));

        assert!(registry.get("old.txt").is_none());
        assert_eq!(registry.get("new.txt"), Some(id));
        assert_eq!(registry.get_path(id), Some("new.txt".to_string()));
    }

    #[test]
    fn test_rename_nonexistent() {
        let registry = PathRegistry::new();

        assert!(!registry.rename("nonexistent.txt", "new.txt"));
    }
}
