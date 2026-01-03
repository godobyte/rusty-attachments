//! Database tracking modified paths for diff manifest generation.
//!
//! Tracks created, modified, deleted, and renamed files/directories
//! to generate a diff manifest at job completion.

use parking_lot::RwLock;
use std::collections::HashSet;

/// Database tracking modified paths for diff manifest generation.
///
/// Tracks created, modified, deleted, and renamed files/directories
/// to generate a diff manifest at job completion.
pub struct ModifiedPathsDatabase {
    /// Files created during this session.
    created_files: RwLock<HashSet<String>>,
    /// Files modified during this session.
    modified_files: RwLock<HashSet<String>>,
    /// Files deleted during this session.
    deleted_files: RwLock<HashSet<String>>,
    /// Directories created during this session.
    created_dirs: RwLock<HashSet<String>>,
    /// Directories deleted during this session.
    deleted_dirs: RwLock<HashSet<String>>,
}

impl ModifiedPathsDatabase {
    /// Create new empty database.
    pub fn new() -> Self {
        Self {
            created_files: RwLock::new(HashSet::new()),
            modified_files: RwLock::new(HashSet::new()),
            deleted_files: RwLock::new(HashSet::new()),
            created_dirs: RwLock::new(HashSet::new()),
            deleted_dirs: RwLock::new(HashSet::new()),
        }
    }

    /// Record file creation.
    ///
    /// # Arguments
    /// * `path` - Path of created file
    pub fn file_created(&self, path: &str) {
        let mut created = self.created_files.write();
        let mut deleted = self.deleted_files.write();

        deleted.remove(path);
        created.insert(path.to_string());
    }

    /// Record file modification.
    ///
    /// # Arguments
    /// * `path` - Path of modified file
    pub fn file_modified(&self, path: &str) {
        let created = self.created_files.read();

        // Don't track as modified if created this session
        if !created.contains(path) {
            self.modified_files.write().insert(path.to_string());
        }
    }

    /// Record file deletion.
    ///
    /// # Arguments
    /// * `path` - Path of deleted file
    pub fn file_deleted(&self, path: &str) {
        let mut created = self.created_files.write();
        let mut modified = self.modified_files.write();
        let mut deleted = self.deleted_files.write();

        // If created this session, just remove (no net change)
        if created.remove(path) {
            return;
        }

        modified.remove(path);
        deleted.insert(path.to_string());
    }

    /// Record file rename.
    ///
    /// # Arguments
    /// * `old_path` - Original path
    /// * `new_path` - New path
    pub fn file_renamed(&self, old_path: &str, new_path: &str) {
        self.file_deleted(old_path);
        self.file_created(new_path);
    }

    /// Record directory creation.
    ///
    /// # Arguments
    /// * `path` - Path of created directory
    pub fn dir_created(&self, path: &str) {
        let mut created = self.created_dirs.write();
        let mut deleted = self.deleted_dirs.write();

        deleted.remove(path);
        created.insert(path.to_string());
    }

    /// Record directory deletion.
    ///
    /// # Arguments
    /// * `path` - Path of deleted directory
    pub fn dir_deleted(&self, path: &str) {
        let mut created = self.created_dirs.write();
        let mut deleted = self.deleted_dirs.write();

        if created.remove(path) {
            return;
        }
        deleted.insert(path.to_string());
    }

    /// Get summary of all modifications.
    ///
    /// # Returns
    /// Summary containing all tracked modifications.
    pub fn get_summary(&self) -> ModificationSummary {
        ModificationSummary {
            created_files: self.created_files.read().iter().cloned().collect(),
            modified_files: self.modified_files.read().iter().cloned().collect(),
            deleted_files: self.deleted_files.read().iter().cloned().collect(),
            created_dirs: self.created_dirs.read().iter().cloned().collect(),
            deleted_dirs: self.deleted_dirs.read().iter().cloned().collect(),
        }
    }

    /// Check if any modifications recorded.
    ///
    /// # Returns
    /// True if any modifications have been recorded.
    pub fn has_modifications(&self) -> bool {
        !self.created_files.read().is_empty()
            || !self.modified_files.read().is_empty()
            || !self.deleted_files.read().is_empty()
            || !self.created_dirs.read().is_empty()
            || !self.deleted_dirs.read().is_empty()
    }

    /// Clear all tracked modifications.
    pub fn clear(&self) {
        self.created_files.write().clear();
        self.modified_files.write().clear();
        self.deleted_files.write().clear();
        self.created_dirs.write().clear();
        self.deleted_dirs.write().clear();
    }
}

impl Default for ModifiedPathsDatabase {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of all modifications.
#[derive(Debug, Clone, Default)]
pub struct ModificationSummary {
    /// Files created during this session.
    pub created_files: Vec<String>,
    /// Files modified during this session.
    pub modified_files: Vec<String>,
    /// Files deleted during this session.
    pub deleted_files: Vec<String>,
    /// Directories created during this session.
    pub created_dirs: Vec<String>,
    /// Directories deleted during this session.
    pub deleted_dirs: Vec<String>,
}

impl ModificationSummary {
    /// Check if summary is empty (no modifications).
    pub fn is_empty(&self) -> bool {
        self.created_files.is_empty()
            && self.modified_files.is_empty()
            && self.deleted_files.is_empty()
            && self.created_dirs.is_empty()
            && self.deleted_dirs.is_empty()
    }

    /// Get total count of all modifications.
    pub fn total_count(&self) -> usize {
        self.created_files.len()
            + self.modified_files.len()
            + self.deleted_files.len()
            + self.created_dirs.len()
            + self.deleted_dirs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_created() {
        let db = ModifiedPathsDatabase::new();

        db.file_created("new_file.txt");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.created_files.contains(&"new_file.txt".to_string()));
        assert!(summary.modified_files.is_empty());
        assert!(summary.deleted_files.is_empty());
    }

    #[test]
    fn test_file_modified() {
        let db = ModifiedPathsDatabase::new();

        db.file_modified("existing.txt");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.created_files.is_empty());
        assert!(summary.modified_files.contains(&"existing.txt".to_string()));
    }

    #[test]
    fn test_file_modified_after_created_not_tracked() {
        let db = ModifiedPathsDatabase::new();

        db.file_created("new_file.txt");
        db.file_modified("new_file.txt");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.created_files.contains(&"new_file.txt".to_string()));
        // Should NOT be in modified since it was created this session
        assert!(!summary.modified_files.contains(&"new_file.txt".to_string()));
    }

    #[test]
    fn test_file_deleted() {
        let db = ModifiedPathsDatabase::new();

        db.file_deleted("existing.txt");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.deleted_files.contains(&"existing.txt".to_string()));
    }

    #[test]
    fn test_file_created_then_deleted_no_net_change() {
        let db = ModifiedPathsDatabase::new();

        db.file_created("new_file.txt");
        db.file_deleted("new_file.txt");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.created_files.is_empty());
        assert!(summary.deleted_files.is_empty());
    }

    #[test]
    fn test_file_renamed() {
        let db = ModifiedPathsDatabase::new();

        db.file_renamed("old.txt", "new.txt");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.deleted_files.contains(&"old.txt".to_string()));
        assert!(summary.created_files.contains(&"new.txt".to_string()));
    }

    #[test]
    fn test_dir_created() {
        let db = ModifiedPathsDatabase::new();

        db.dir_created("new_dir");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.created_dirs.contains(&"new_dir".to_string()));
    }

    #[test]
    fn test_dir_deleted() {
        let db = ModifiedPathsDatabase::new();

        db.dir_deleted("existing_dir");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.deleted_dirs.contains(&"existing_dir".to_string()));
    }

    #[test]
    fn test_dir_created_then_deleted_no_net_change() {
        let db = ModifiedPathsDatabase::new();

        db.dir_created("new_dir");
        db.dir_deleted("new_dir");

        let summary: ModificationSummary = db.get_summary();
        assert!(summary.created_dirs.is_empty());
        assert!(summary.deleted_dirs.is_empty());
    }

    #[test]
    fn test_has_modifications() {
        let db = ModifiedPathsDatabase::new();

        assert!(!db.has_modifications());

        db.file_created("test.txt");
        assert!(db.has_modifications());
    }

    #[test]
    fn test_clear() {
        let db = ModifiedPathsDatabase::new();

        db.file_created("file.txt");
        db.dir_created("dir");
        assert!(db.has_modifications());

        db.clear();
        assert!(!db.has_modifications());
    }

    #[test]
    fn test_summary_total_count() {
        let db = ModifiedPathsDatabase::new();

        db.file_created("new.txt");
        db.file_modified("mod.txt");
        db.file_deleted("del.txt");
        db.dir_created("new_dir");
        db.dir_deleted("del_dir");

        let summary: ModificationSummary = db.get_summary();
        assert_eq!(summary.total_count(), 5);
    }

    #[test]
    fn test_recreate_deleted_file() {
        let db = ModifiedPathsDatabase::new();

        // Delete existing file
        db.file_deleted("file.txt");
        assert!(db.get_summary().deleted_files.contains(&"file.txt".to_string()));

        // Recreate it
        db.file_created("file.txt");

        let summary: ModificationSummary = db.get_summary();
        // Should be in created, not deleted
        assert!(summary.created_files.contains(&"file.txt".to_string()));
        assert!(!summary.deleted_files.contains(&"file.txt".to_string()));
    }
}
