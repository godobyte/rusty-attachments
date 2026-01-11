//! Subtree extraction operation.
//!
//! Extracts a subtree from a manifest as a relative-path manifest.

use crate::v2025_12::{ManifestDirectoryPath, ManifestFilePath};
use crate::{AbsSnapshot, Manifest, Snapshot};

/// Extract a subtree from an absolute snapshot as a relative manifest.
///
/// # Arguments
/// * `manifest` - Source absolute snapshot
/// * `subtree` - Path prefix to extract (e.g., "/home/user/project")
///
/// # Returns
/// Relative-path snapshot containing only entries under the subtree,
/// with paths rebased to be relative to the subtree root.
pub fn subtree_manifest(manifest: &AbsSnapshot, subtree: &str) -> Snapshot {
    let normalized_subtree: String = normalize_subtree_path(subtree);
    let (dirs, files) = extract_subtree(manifest.inner(), &normalized_subtree);
    Snapshot::new(dirs, files)
}

/// Extract a subtree from a relative snapshot.
///
/// # Arguments
/// * `manifest` - Source relative snapshot
/// * `subtree` - Path prefix to extract (e.g., "src/components")
///
/// # Returns
/// Relative-path snapshot with paths rebased relative to subtree.
pub fn subtree_rel_manifest(manifest: &Snapshot, subtree: &str) -> Snapshot {
    let normalized_subtree: String = normalize_subtree_path(subtree);
    let (dirs, files) = extract_subtree(manifest.inner(), &normalized_subtree);
    Snapshot::new(dirs, files)
}

/// Normalize subtree path for consistent matching.
fn normalize_subtree_path(subtree: &str) -> String {
    let mut normalized: String = subtree.replace('\\', "/");

    // Remove trailing slash
    while normalized.ends_with('/') {
        normalized.pop();
    }

    normalized
}

/// Extract subtree entries from a manifest.
fn extract_subtree(
    manifest: &Manifest,
    subtree: &str,
) -> (Vec<ManifestDirectoryPath>, Vec<ManifestFilePath>) {
    match manifest {
        Manifest::V2023_03_03(_) => (vec![], vec![]),
        Manifest::V2025_12(m) => {
            let prefix: String = if subtree.is_empty() {
                String::new()
            } else {
                format!("{}/", subtree)
            };

            // Filter and rebase files
            let files: Vec<ManifestFilePath> = m
                .files
                .iter()
                .filter(|f| is_within_subtree(&f.path, subtree, &prefix))
                .map(|f| rebase_file(f, &prefix))
                .collect();

            // Filter and rebase directories
            let dirs: Vec<ManifestDirectoryPath> = m
                .dirs
                .iter()
                .filter(|d| is_within_subtree(&d.path, subtree, &prefix))
                .map(|d| rebase_dir(d, &prefix))
                .collect();

            (dirs, files)
        }
    }
}

/// Check if a path is within the subtree.
fn is_within_subtree(path: &str, subtree: &str, prefix: &str) -> bool {
    if subtree.is_empty() {
        return true;
    }

    // Path must start with prefix (subtree + "/")
    path.starts_with(prefix)
}

/// Rebase a file path by stripping the subtree prefix.
fn rebase_file(file: &ManifestFilePath, prefix: &str) -> ManifestFilePath {
    ManifestFilePath {
        path: strip_prefix(&file.path, prefix),
        hash: file.hash.clone(),
        size: file.size,
        mtime: file.mtime,
        runnable: file.runnable,
        chunkhashes: file.chunkhashes.clone(),
        symlink_target: file.symlink_target.clone(),
        deleted: file.deleted,
    }
}

/// Rebase a directory path by stripping the subtree prefix.
fn rebase_dir(dir: &ManifestDirectoryPath, prefix: &str) -> ManifestDirectoryPath {
    ManifestDirectoryPath {
        path: strip_prefix(&dir.path, prefix),
        deleted: dir.deleted,
    }
}

/// Strip prefix from path.
fn strip_prefix(path: &str, prefix: &str) -> String {
    path.strip_prefix(prefix).unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2025_12::{ManifestDirectoryPath, ManifestFilePath};

    fn make_abs_snapshot(
        dirs: Vec<ManifestDirectoryPath>,
        files: Vec<ManifestFilePath>,
    ) -> AbsSnapshot {
        AbsSnapshot::new(dirs, files)
    }

    #[test]
    fn test_subtree_basic() {
        let s: AbsSnapshot = make_abs_snapshot(
            vec![ManifestDirectoryPath::new("/home/user/project/src")],
            vec![
                ManifestFilePath::file("/home/user/project/src/main.rs", "hash1", 100, 1000),
                ManifestFilePath::file("/home/user/project/README.md", "hash2", 200, 2000),
            ],
        );

        let result: Snapshot = subtree_manifest(&s, "/home/user/project");

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 2);
            assert!(m.files.iter().any(|f| f.path == "src/main.rs"));
            assert!(m.files.iter().any(|f| f.path == "README.md"));
            assert_eq!(m.dirs.len(), 1);
            assert_eq!(m.dirs[0].path, "src");
        }
    }

    #[test]
    fn test_subtree_nested() {
        let s: AbsSnapshot = make_abs_snapshot(
            vec![],
            vec![
                ManifestFilePath::file("/project/src/lib.rs", "hash1", 100, 1000),
                ManifestFilePath::file("/project/src/utils/helper.rs", "hash2", 200, 2000),
                ManifestFilePath::file("/project/tests/test.rs", "hash3", 300, 3000),
            ],
        );

        let result: Snapshot = subtree_manifest(&s, "/project/src");

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 2);
            assert!(m.files.iter().any(|f| f.path == "lib.rs"));
            assert!(m.files.iter().any(|f| f.path == "utils/helper.rs"));
        }
    }

    #[test]
    fn test_subtree_empty_result() {
        let s: AbsSnapshot = make_abs_snapshot(
            vec![],
            vec![ManifestFilePath::file(
                "/other/file.txt",
                "hash1",
                100,
                1000,
            )],
        );

        let result: Snapshot = subtree_manifest(&s, "/project");

        if let Manifest::V2025_12(m) = result.inner() {
            assert!(m.files.is_empty());
        }
    }

    #[test]
    fn test_subtree_trailing_slash() {
        let s: AbsSnapshot = make_abs_snapshot(
            vec![],
            vec![ManifestFilePath::file(
                "/project/file.txt",
                "hash1",
                100,
                1000,
            )],
        );

        // Should work the same with or without trailing slash
        let result1: Snapshot = subtree_manifest(&s, "/project");
        let result2: Snapshot = subtree_manifest(&s, "/project/");

        if let (Manifest::V2025_12(m1), Manifest::V2025_12(m2)) = (result1.inner(), result2.inner())
        {
            assert_eq!(m1.files.len(), m2.files.len());
            assert_eq!(m1.files[0].path, m2.files[0].path);
        }
    }

    #[test]
    fn test_subtree_preserves_metadata() {
        let s: AbsSnapshot = make_abs_snapshot(
            vec![],
            vec![
                ManifestFilePath::file("/project/script.sh", "hash1", 100, 1000)
                    .with_runnable(true),
            ],
        );

        let result: Snapshot = subtree_manifest(&s, "/project");

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files[0].runnable, true);
            assert_eq!(m.files[0].hash, Some("hash1".to_string()));
        }
    }
}
