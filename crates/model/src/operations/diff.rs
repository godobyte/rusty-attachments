//! Manifest-to-manifest diff operation.
//!
//! Computes the difference between two snapshots, producing a diff manifest
//! with added, modified, and deleted entries.

use std::collections::{HashMap, HashSet};

use crate::v2025_12::{ManifestDirectoryPath, ManifestFilePath};
use crate::{AbsSnapshot, AbsSnapshotDiff, Manifest, Snapshot, SnapshotDiff};

/// Options for computing diff manifests.
#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    /// If true, ignore hash differences and compare only by mtime/size.
    pub ignore_hashes: bool,
    /// If true, preserve runnable flag from parent for unchanged files.
    pub preserve_runnable: bool,
}

/// Compute the difference between two snapshots.
///
/// # Arguments
/// * `parent` - The parent/base snapshot
/// * `current` - The current snapshot to compare against parent
/// * `parent_manifest_hash` - Hash of the parent manifest (for diff metadata)
/// * `options` - Diff options
///
/// # Returns
/// A diff manifest containing only the changes (added, modified, deleted).
pub fn compute_diff_manifest(
    parent: &Snapshot,
    current: &Snapshot,
    parent_manifest_hash: impl Into<String>,
    options: &DiffOptions,
) -> SnapshotDiff {
    let (dirs, files) = compute_diff_entries(parent.inner(), current.inner(), options);

    SnapshotDiff::new(dirs, files, parent_manifest_hash)
}

/// Compute the difference between two absolute snapshots.
///
/// # Arguments
/// * `parent` - The parent/base absolute snapshot
/// * `current` - The current absolute snapshot to compare against parent
/// * `parent_manifest_hash` - Hash of the parent manifest (for diff metadata)
/// * `options` - Diff options
///
/// # Returns
/// An absolute diff manifest containing only the changes.
pub fn compute_abs_diff_manifest(
    parent: &AbsSnapshot,
    current: &AbsSnapshot,
    parent_manifest_hash: impl Into<String>,
    options: &DiffOptions,
) -> AbsSnapshotDiff {
    let (dirs, files) = compute_diff_entries(parent.inner(), current.inner(), options);

    AbsSnapshotDiff::new(dirs, files, parent_manifest_hash)
}

/// Internal function to compute diff entries between two manifests.
fn compute_diff_entries(
    parent: &Manifest,
    current: &Manifest,
    options: &DiffOptions,
) -> (Vec<ManifestDirectoryPath>, Vec<ManifestFilePath>) {
    // Build lookup maps for parent
    let parent_files: HashMap<&str, &ManifestFilePath> = get_file_map(parent);
    let parent_dirs: HashSet<&str> = get_dir_set(parent);

    // Build lookup maps for current
    let current_files: HashMap<&str, &ManifestFilePath> = get_file_map(current);
    let current_dirs: HashSet<&str> = get_dir_set(current);

    let mut result_files: Vec<ManifestFilePath> = Vec::new();
    let mut result_dirs: Vec<ManifestDirectoryPath> = Vec::new();

    // Find new and modified files
    for (path, current_entry) in &current_files {
        if let Some(parent_entry) = parent_files.get(path) {
            // File exists in both - check if modified
            if entries_differ(parent_entry, current_entry, options) {
                result_files.push((*current_entry).clone());
            }
        } else {
            // New file
            result_files.push((*current_entry).clone());
        }
    }

    // Find deleted files
    for path in parent_files.keys() {
        if !current_files.contains_key(path) {
            result_files.push(ManifestFilePath::deleted(*path));
        }
    }

    // Find new directories
    for dir in &current_dirs {
        if !parent_dirs.contains(dir) {
            result_dirs.push(ManifestDirectoryPath::new(*dir));
        }
    }

    // Find deleted directories (only if all contents are also deleted)
    for dir in &parent_dirs {
        if !current_dirs.contains(dir) {
            // Check if any file under this dir still exists
            let has_remaining_files: bool = current_files
                .keys()
                .any(|p| p.starts_with(dir) && p.len() > dir.len());

            if !has_remaining_files {
                result_dirs.push(ManifestDirectoryPath::deleted(*dir));
            }
        }
    }

    (result_dirs, result_files)
}

/// Check if two file entries differ.
fn entries_differ(
    parent: &ManifestFilePath,
    current: &ManifestFilePath,
    options: &DiffOptions,
) -> bool {
    // Deleted entries are always different
    if parent.deleted || current.deleted {
        return parent.deleted != current.deleted;
    }

    // Symlink changes
    if parent.symlink_target != current.symlink_target {
        return true;
    }

    // Size changes
    if parent.size != current.size {
        return true;
    }

    // Mtime changes (with 1 microsecond tolerance)
    if let (Some(p_mtime), Some(c_mtime)) = (parent.mtime, current.mtime) {
        if (p_mtime - c_mtime).abs() > 1 {
            return true;
        }
    }

    // Hash comparison (unless ignored)
    if !options.ignore_hashes {
        if parent.hash != current.hash {
            return true;
        }
        if parent.chunkhashes != current.chunkhashes {
            return true;
        }
    }

    // Runnable flag (unless preserve_runnable is set)
    if !options.preserve_runnable && parent.runnable != current.runnable {
        return true;
    }

    false
}

/// Extract file map from manifest.
fn get_file_map(manifest: &Manifest) -> HashMap<&str, &ManifestFilePath> {
    match manifest {
        Manifest::V2023_03_03(_) => HashMap::new(),
        Manifest::V2025_12(m) => m
            .files
            .iter()
            .filter(|f| !f.deleted)
            .map(|f| (f.path.as_str(), f))
            .collect(),
    }
}

/// Extract directory set from manifest.
fn get_dir_set(manifest: &Manifest) -> HashSet<&str> {
    match manifest {
        Manifest::V2023_03_03(_) => HashSet::new(),
        Manifest::V2025_12(m) => m
            .dirs
            .iter()
            .filter(|d| !d.deleted)
            .map(|d| d.path.as_str())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2025_12::ManifestFilePath;

    fn make_snapshot(files: Vec<ManifestFilePath>) -> Snapshot {
        Snapshot::new(vec![], files)
    }

    #[test]
    fn test_diff_no_changes() {
        let files: Vec<ManifestFilePath> =
            vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)];
        let parent: Snapshot = make_snapshot(files.clone());
        let current: Snapshot = make_snapshot(files);

        let diff: SnapshotDiff =
            compute_diff_manifest(&parent, &current, "parent_hash", &DiffOptions::default());

        if let Manifest::V2025_12(m) = diff.inner() {
            assert!(m.files.is_empty());
            assert!(m.dirs.is_empty());
        }
    }

    #[test]
    fn test_diff_new_file() {
        let parent: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)]);
        let current: Snapshot = make_snapshot(vec![
            ManifestFilePath::file("a.txt", "hash1", 100, 1000),
            ManifestFilePath::file("b.txt", "hash2", 200, 2000),
        ]);

        let diff: SnapshotDiff =
            compute_diff_manifest(&parent, &current, "parent_hash", &DiffOptions::default());

        if let Manifest::V2025_12(m) = diff.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].path, "b.txt");
            assert!(!m.files[0].deleted);
        }
    }

    #[test]
    fn test_diff_deleted_file() {
        let parent: Snapshot = make_snapshot(vec![
            ManifestFilePath::file("a.txt", "hash1", 100, 1000),
            ManifestFilePath::file("b.txt", "hash2", 200, 2000),
        ]);
        let current: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)]);

        let diff: SnapshotDiff =
            compute_diff_manifest(&parent, &current, "parent_hash", &DiffOptions::default());

        if let Manifest::V2025_12(m) = diff.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].path, "b.txt");
            assert!(m.files[0].deleted);
        }
    }

    #[test]
    fn test_diff_modified_file() {
        let parent: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)]);
        let current: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "hash2", 150, 2000)]);

        let diff: SnapshotDiff =
            compute_diff_manifest(&parent, &current, "parent_hash", &DiffOptions::default());

        if let Manifest::V2025_12(m) = diff.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].path, "a.txt");
            assert_eq!(m.files[0].hash, Some("hash2".to_string()));
            assert!(!m.files[0].deleted);
        }
    }

    #[test]
    fn test_diff_ignore_hashes() {
        let parent: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)]);
        // Same size/mtime, different hash
        let current: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "hash2", 100, 1000)]);

        let options: DiffOptions = DiffOptions {
            ignore_hashes: true,
            ..Default::default()
        };
        let diff: SnapshotDiff = compute_diff_manifest(&parent, &current, "parent_hash", &options);

        if let Manifest::V2025_12(m) = diff.inner() {
            // Should be empty since we ignore hash differences
            assert!(m.files.is_empty());
        }
    }

    #[test]
    fn test_diff_parent_hash() {
        let parent: Snapshot = make_snapshot(vec![]);
        let current: Snapshot = make_snapshot(vec![]);

        let diff: SnapshotDiff =
            compute_diff_manifest(&parent, &current, "my_parent_hash", &DiffOptions::default());

        assert_eq!(diff.parent_hash(), "my_parent_hash");
    }
}
