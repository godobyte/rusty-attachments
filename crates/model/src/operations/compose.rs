//! Manifest composition operation.
//!
//! Composes multiple manifests using trie-based merging where later
//! manifests override earlier ones. Deletion markers are applied correctly.

use std::collections::HashMap;

use crate::v2025_12::{ManifestDirectoryPath, ManifestFilePath};
use crate::{AbsSnapshot, AbsSnapshotDiff, Manifest, Snapshot, SnapshotDiff};

/// Compose multiple relative snapshots into a single snapshot.
///
/// Later manifests override earlier ones. This is the primary composition
/// operation for combining manifests.
///
/// # Arguments
/// * `manifests` - Snapshots to compose (order matters: later entries win)
///
/// # Returns
/// Composed snapshot, or None if input is empty.
pub fn compose_manifests(manifests: &[&Snapshot]) -> Option<Snapshot> {
    if manifests.is_empty() {
        return None;
    }
    if manifests.len() == 1 {
        return Some(manifests[0].clone());
    }

    let inner_manifests: Vec<&Manifest> = manifests.iter().map(|s| s.inner()).collect();
    let (dirs, files) = compose_entries(&inner_manifests);

    Some(Snapshot::new(dirs, files))
}

/// Compose multiple absolute snapshots into a single snapshot.
///
/// # Arguments
/// * `manifests` - Absolute snapshots to compose (order matters: later entries win)
///
/// # Returns
/// Composed absolute snapshot, or None if input is empty.
pub fn compose_abs_manifests(manifests: &[&AbsSnapshot]) -> Option<AbsSnapshot> {
    if manifests.is_empty() {
        return None;
    }
    if manifests.len() == 1 {
        return Some(manifests[0].clone());
    }

    let inner_manifests: Vec<&Manifest> = manifests.iter().map(|s| s.inner()).collect();
    let (dirs, files) = compose_entries(&inner_manifests);

    Some(AbsSnapshot::new(dirs, files))
}

/// Compose snapshots with diffs applied.
///
/// Takes a base snapshot and applies one or more diffs in order.
///
/// # Arguments
/// * `base` - Base snapshot
/// * `diffs` - Diffs to apply in order
///
/// # Returns
/// Resulting snapshot after applying all diffs.
pub fn apply_diffs(base: &Snapshot, diffs: &[&SnapshotDiff]) -> Snapshot {
    if diffs.is_empty() {
        return base.clone();
    }

    let mut trie: ManifestTrie = ManifestTrie::new();

    // Insert base snapshot
    insert_manifest_into_trie(&mut trie, base.inner());

    // Apply each diff
    for diff in diffs {
        apply_diff_to_trie(&mut trie, diff.inner());
    }

    // Extract results
    let (dirs, files) = trie.collect_entries();
    Snapshot::new(dirs, files)
}

/// Compose snapshots with diffs applied (absolute paths).
///
/// # Arguments
/// * `base` - Base absolute snapshot
/// * `diffs` - Absolute diffs to apply in order
///
/// # Returns
/// Resulting absolute snapshot after applying all diffs.
pub fn apply_abs_diffs(base: &AbsSnapshot, diffs: &[&AbsSnapshotDiff]) -> AbsSnapshot {
    if diffs.is_empty() {
        return base.clone();
    }

    let mut trie: ManifestTrie = ManifestTrie::new();

    // Insert base snapshot
    insert_manifest_into_trie(&mut trie, base.inner());

    // Apply each diff
    for diff in diffs {
        apply_diff_to_trie(&mut trie, diff.inner());
    }

    // Extract results
    let (dirs, files) = trie.collect_entries();
    AbsSnapshot::new(dirs, files)
}

/// Internal function to compose manifest entries.
fn compose_entries(manifests: &[&Manifest]) -> (Vec<ManifestDirectoryPath>, Vec<ManifestFilePath>) {
    let mut trie: ManifestTrie = ManifestTrie::new();

    for manifest in manifests {
        insert_manifest_into_trie(&mut trie, manifest);
    }

    trie.collect_entries()
}

/// Insert a manifest's entries into the trie.
fn insert_manifest_into_trie(trie: &mut ManifestTrie, manifest: &Manifest) {
    match manifest {
        Manifest::V2023_03_03(m) => {
            for path in &m.paths {
                let file: ManifestFilePath =
                    ManifestFilePath::file(&path.path, &path.hash, path.size, path.mtime);
                trie.insert_file(file);
            }
        }
        Manifest::V2025_12(m) => {
            for dir in &m.dirs {
                if dir.deleted {
                    trie.delete_dir(&dir.path);
                } else {
                    trie.insert_dir(dir.clone());
                }
            }
            for file in &m.files {
                if file.deleted {
                    trie.delete_file(&file.path);
                } else {
                    trie.insert_file(file.clone());
                }
            }
        }
    }
}

/// Apply a diff manifest to the trie.
fn apply_diff_to_trie(trie: &mut ManifestTrie, manifest: &Manifest) {
    if let Manifest::V2025_12(m) = manifest {
        // Apply directory changes
        for dir in &m.dirs {
            if dir.deleted {
                trie.delete_dir(&dir.path);
            } else {
                trie.insert_dir(dir.clone());
            }
        }

        // Apply file changes
        for file in &m.files {
            if file.deleted {
                trie.delete_file(&file.path);
            } else {
                trie.insert_file(file.clone());
            }
        }
    }
}

/// Trie node for efficient manifest composition.
#[derive(Debug, Default)]
struct TrieNode {
    /// Child nodes by path component.
    children: HashMap<String, TrieNode>,
    /// File entry at this node (if any).
    file_entry: Option<ManifestFilePath>,
    /// Directory entry at this node (if any).
    dir_entry: Option<ManifestDirectoryPath>,
}

/// Trie structure for manifest composition.
#[derive(Debug, Default)]
struct ManifestTrie {
    root: TrieNode,
}

impl ManifestTrie {
    /// Create a new empty trie.
    fn new() -> Self {
        Self::default()
    }

    /// Insert a file entry.
    fn insert_file(&mut self, file: ManifestFilePath) {
        let components: Vec<&str> = file.path.split('/').collect();
        let node: &mut TrieNode = self.navigate_to_node(&components);
        node.file_entry = Some(file);
    }

    /// Insert a directory entry.
    fn insert_dir(&mut self, dir: ManifestDirectoryPath) {
        let components: Vec<&str> = dir.path.split('/').collect();
        let node: &mut TrieNode = self.navigate_to_node(&components);
        node.dir_entry = Some(dir);
    }

    /// Delete a file entry.
    fn delete_file(&mut self, path: &str) {
        let components: Vec<&str> = path.split('/').collect();
        if let Some(node) = self.find_node_mut(&components) {
            node.file_entry = None;
        }
    }

    /// Delete a directory and all its contents.
    fn delete_dir(&mut self, path: &str) {
        let components: Vec<&str> = path.split('/').collect();
        if components.is_empty() {
            return;
        }

        // Navigate to parent and remove the child
        if components.len() == 1 {
            self.root.children.remove(components[0]);
            self.root.dir_entry = None;
        } else {
            let parent_components: &[&str] = &components[..components.len() - 1];
            if let Some(parent) = self.find_node_mut(parent_components) {
                let last: &str = components.last().unwrap();
                parent.children.remove(last);
            }
        }
    }

    /// Navigate to a node, creating intermediate nodes as needed.
    fn navigate_to_node(&mut self, components: &[&str]) -> &mut TrieNode {
        let mut current: &mut TrieNode = &mut self.root;
        for component in components {
            current = current
                .children
                .entry((*component).to_string())
                .or_default();
        }
        current
    }

    /// Find a node without creating it.
    fn find_node_mut(&mut self, components: &[&str]) -> Option<&mut TrieNode> {
        let mut current: &mut TrieNode = &mut self.root;
        for component in components {
            current = current.children.get_mut(*component)?;
        }
        Some(current)
    }

    /// Collect all entries from the trie.
    fn collect_entries(&self) -> (Vec<ManifestDirectoryPath>, Vec<ManifestFilePath>) {
        let mut dirs: Vec<ManifestDirectoryPath> = Vec::new();
        let mut files: Vec<ManifestFilePath> = Vec::new();

        Self::collect_from_node(&self.root, &mut dirs, &mut files);

        (dirs, files)
    }

    /// Recursively collect entries from a node.
    fn collect_from_node(
        node: &TrieNode,
        dirs: &mut Vec<ManifestDirectoryPath>,
        files: &mut Vec<ManifestFilePath>,
    ) {
        if let Some(ref dir) = node.dir_entry {
            dirs.push(dir.clone());
        }
        if let Some(ref file) = node.file_entry {
            files.push(file.clone());
        }

        for child in node.children.values() {
            Self::collect_from_node(child, dirs, files);
        }
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
    fn test_compose_empty() {
        let result: Option<Snapshot> = compose_manifests(&[]);
        assert!(result.is_none());
    }

    #[test]
    fn test_compose_single() {
        let s: Snapshot = make_snapshot(vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)]);
        let result: Option<Snapshot> = compose_manifests(&[&s]);

        assert!(result.is_some());
        if let Manifest::V2025_12(m) = result.unwrap().inner() {
            assert_eq!(m.files.len(), 1);
        }
    }

    #[test]
    fn test_compose_no_overlap() {
        let s1: Snapshot = make_snapshot(vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)]);
        let s2: Snapshot = make_snapshot(vec![ManifestFilePath::file("b.txt", "hash2", 200, 2000)]);

        let result: Snapshot = compose_manifests(&[&s1, &s2]).unwrap();

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 2);
        }
    }

    #[test]
    fn test_compose_later_wins() {
        let s1: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "old_hash", 100, 1000)]);
        let s2: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "new_hash", 200, 2000)]);

        let result: Snapshot = compose_manifests(&[&s1, &s2]).unwrap();

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].hash, Some("new_hash".to_string()));
            assert_eq!(m.files[0].size, Some(200));
        }
    }

    #[test]
    fn test_apply_diff_add() {
        let base: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)]);
        let diff: SnapshotDiff = SnapshotDiff::new(
            vec![],
            vec![ManifestFilePath::file("b.txt", "hash2", 200, 2000)],
            "parent_hash",
        );

        let result: Snapshot = apply_diffs(&base, &[&diff]);

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 2);
        }
    }

    #[test]
    fn test_apply_diff_delete() {
        let base: Snapshot = make_snapshot(vec![
            ManifestFilePath::file("a.txt", "hash1", 100, 1000),
            ManifestFilePath::file("b.txt", "hash2", 200, 2000),
        ]);
        let diff: SnapshotDiff = SnapshotDiff::new(
            vec![],
            vec![ManifestFilePath::deleted("b.txt")],
            "parent_hash",
        );

        let result: Snapshot = apply_diffs(&base, &[&diff]);

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].path, "a.txt");
        }
    }

    #[test]
    fn test_apply_diff_modify() {
        let base: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("a.txt", "old_hash", 100, 1000)]);
        let diff: SnapshotDiff = SnapshotDiff::new(
            vec![],
            vec![ManifestFilePath::file("a.txt", "new_hash", 150, 2000)],
            "parent_hash",
        );

        let result: Snapshot = apply_diffs(&base, &[&diff]);

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].hash, Some("new_hash".to_string()));
        }
    }
}
