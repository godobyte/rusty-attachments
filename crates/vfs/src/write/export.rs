//! Diff manifest export functionality.
//!
//! Provides traits and types for exporting dirty VFS state as diff manifests.

use async_trait::async_trait;

use rusty_attachments_model::Manifest;

use crate::VfsError;

/// Summary of dirty file counts.
#[derive(Debug, Clone, Default)]
pub struct DirtySummary {
    /// Number of new files.
    pub new_count: usize,
    /// Number of modified files.
    pub modified_count: usize,
    /// Number of deleted files.
    pub deleted_count: usize,
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
    pub fn total(&self) -> usize {
        self.new_count + self.modified_count + self.deleted_count
    }

    /// Returns true if there are any dirty files.
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
        assert_eq!(summary.total(), 0);
        assert!(!summary.has_changes());
    }

    #[test]
    fn test_dirty_summary_with_changes() {
        let summary = DirtySummary {
            new_count: 2,
            modified_count: 3,
            deleted_count: 1,
        };
        assert_eq!(summary.total(), 6);
        assert!(summary.has_changes());
    }
}
