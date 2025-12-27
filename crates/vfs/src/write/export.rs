//! Diff manifest export functionality.
//!
//! Provides traits and types for exporting dirty VFS state as diff manifests.

use async_trait::async_trait;

use rusty_attachments_model::Manifest;

use crate::VfsError;

/// Summary of dirty file and directory counts.
#[derive(Debug, Clone, Default)]
pub struct DirtySummary {
    /// Number of new files.
    pub new_count: usize,
    /// Number of modified files.
    pub modified_count: usize,
    /// Number of deleted files.
    pub deleted_count: usize,
    /// Number of new directories.
    pub new_dir_count: usize,
    /// Number of deleted directories.
    pub deleted_dir_count: usize,
}

/// Information about a single dirty file.
#[derive(Debug, Clone)]
pub struct DirtyFileInfo {
    /// Relative path within VFS.
    pub path: String,
    /// Current file size in bytes.
    pub size: u64,
}

impl DirtySummary {
    /// Total number of dirty files.
    pub fn total_files(&self) -> usize {
        self.new_count + self.modified_count + self.deleted_count
    }

    /// Total number of dirty directories.
    pub fn total_dirs(&self) -> usize {
        self.new_dir_count + self.deleted_dir_count
    }

    /// Total number of dirty entries (files + directories).
    pub fn total(&self) -> usize {
        self.total_files() + self.total_dirs()
    }

    /// Returns true if there are any dirty files or directories.
    pub fn has_changes(&self) -> bool {
        self.total() > 0
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirty_summary_default() {
        let summary = DirtySummary::default();
        assert_eq!(summary.total_files(), 0);
        assert_eq!(summary.total_dirs(), 0);
        assert_eq!(summary.total(), 0);
        assert!(!summary.has_changes());
    }

    #[test]
    fn test_dirty_summary_with_file_changes() {
        let summary = DirtySummary {
            new_count: 2,
            modified_count: 3,
            deleted_count: 1,
            new_dir_count: 0,
            deleted_dir_count: 0,
        };
        assert_eq!(summary.total_files(), 6);
        assert_eq!(summary.total_dirs(), 0);
        assert_eq!(summary.total(), 6);
        assert!(summary.has_changes());
    }

    #[test]
    fn test_dirty_summary_with_dir_changes() {
        let summary = DirtySummary {
            new_count: 0,
            modified_count: 0,
            deleted_count: 0,
            new_dir_count: 2,
            deleted_dir_count: 1,
        };
        assert_eq!(summary.total_files(), 0);
        assert_eq!(summary.total_dirs(), 3);
        assert_eq!(summary.total(), 3);
        assert!(summary.has_changes());
    }

    #[test]
    fn test_dirty_summary_with_mixed_changes() {
        let summary = DirtySummary {
            new_count: 1,
            modified_count: 2,
            deleted_count: 1,
            new_dir_count: 3,
            deleted_dir_count: 2,
        };
        assert_eq!(summary.total_files(), 4);
        assert_eq!(summary.total_dirs(), 5);
        assert_eq!(summary.total(), 9);
        assert!(summary.has_changes());
    }
}
