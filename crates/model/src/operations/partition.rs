//! Manifest partitioning operation.
//!
//! Splits a manifest into multiple (root, RelManifest) pairs.

use std::collections::HashSet;

use crate::v2025_12::{ManifestDirectoryPath, ManifestFilePath};
use crate::{AbsSnapshot, Manifest, Snapshot};

/// Partition an absolute snapshot into multiple (root, Snapshot) pairs.
///
/// # Arguments
/// * `manifest` - Source absolute snapshot
/// * `roots` - Optional explicit roots. If None, auto-detects roots.
///
/// # Returns
/// List of (root_path, relative_snapshot) tuples.
pub fn partition_manifest(
    manifest: &AbsSnapshot,
    roots: Option<&[String]>,
) -> Vec<(String, Snapshot)> {
    let detected_roots: Vec<String> = match roots {
        Some(r) => r.to_vec(),
        None => detect_roots(manifest.inner()),
    };

    detected_roots
        .into_iter()
        .map(|root| {
            let snapshot: Snapshot = extract_partition(manifest.inner(), &root);
            (root, snapshot)
        })
        .filter(|(_, s)| {
            // Only include non-empty partitions
            if let Manifest::V2025_12(m) = s.inner() {
                !m.files.is_empty() || !m.dirs.is_empty()
            } else {
                false
            }
        })
        .collect()
}

/// Auto-detect roots from manifest paths.
///
/// On POSIX: finds the longest common prefix of all paths.
/// On Windows: groups by drive letter.
fn detect_roots(manifest: &Manifest) -> Vec<String> {
    match manifest {
        Manifest::V2023_03_03(_) => vec![],
        Manifest::V2025_12(m) => {
            let paths: Vec<&str> = m.files.iter().map(|f| f.path.as_str()).collect();

            if paths.is_empty() {
                return vec![];
            }

            // Check if paths look like Windows paths (have drive letters)
            let is_windows: bool = paths
                .iter()
                .any(|p| p.len() >= 2 && p.chars().nth(1) == Some(':'));

            if is_windows {
                detect_windows_roots(&paths)
            } else {
                detect_posix_roots(&paths)
            }
        }
    }
}

/// Detect roots for POSIX paths (longest common prefix).
fn detect_posix_roots(paths: &[&str]) -> Vec<String> {
    if paths.is_empty() {
        return vec![];
    }

    // Find longest common prefix
    let first: &str = paths[0];
    let mut prefix_len: usize = first.len();

    for path in paths.iter().skip(1) {
        prefix_len = common_prefix_len(first, path, prefix_len);
        if prefix_len == 0 {
            break;
        }
    }

    // Trim to last directory separator
    let prefix: &str = &first[..prefix_len];
    let root: String = if let Some(last_slash) = prefix.rfind('/') {
        prefix[..last_slash].to_string()
    } else {
        String::new()
    };

    if root.is_empty() {
        // No common root found, use "/" as root
        vec!["/".to_string()]
    } else {
        vec![root]
    }
}

/// Find common prefix length between two strings.
fn common_prefix_len(a: &str, b: &str, max_len: usize) -> usize {
    let a_chars: Vec<char> = a.chars().take(max_len).collect();
    let b_chars: Vec<char> = b.chars().collect();

    let mut len: usize = 0;
    for (ac, bc) in a_chars.iter().zip(b_chars.iter()) {
        if ac != bc {
            break;
        }
        len += ac.len_utf8();
    }
    len
}

/// Detect roots for Windows paths (group by drive letter).
fn detect_windows_roots(paths: &[&str]) -> Vec<String> {
    let mut drives: HashSet<String> = HashSet::new();

    for path in paths {
        if path.len() >= 2 && path.chars().nth(1) == Some(':') {
            let drive: String = path[..2].to_uppercase();
            drives.insert(drive);
        }
    }

    drives.into_iter().collect()
}

/// Extract a partition from the manifest.
fn extract_partition(manifest: &Manifest, root: &str) -> Snapshot {
    match manifest {
        Manifest::V2023_03_03(_) => Snapshot::new(vec![], vec![]),
        Manifest::V2025_12(m) => {
            let prefix: String = if root.is_empty() || root == "/" {
                String::new()
            } else if root.ends_with('/') {
                root.to_string()
            } else {
                format!("{}/", root)
            };

            // Filter and rebase files
            let files: Vec<ManifestFilePath> = m
                .files
                .iter()
                .filter(|f| {
                    if prefix.is_empty() {
                        true
                    } else {
                        f.path.starts_with(&prefix)
                    }
                })
                .map(|f| ManifestFilePath {
                    path: strip_root(&f.path, &prefix),
                    hash: f.hash.clone(),
                    size: f.size,
                    mtime: f.mtime,
                    runnable: f.runnable,
                    chunkhashes: f.chunkhashes.clone(),
                    symlink_target: f.symlink_target.clone(),
                    deleted: f.deleted,
                })
                .collect();

            // Filter and rebase directories
            let dirs: Vec<ManifestDirectoryPath> = m
                .dirs
                .iter()
                .filter(|d| {
                    if prefix.is_empty() {
                        true
                    } else {
                        d.path.starts_with(&prefix)
                    }
                })
                .map(|d| ManifestDirectoryPath {
                    path: strip_root(&d.path, &prefix),
                    deleted: d.deleted,
                })
                .collect();

            Snapshot::new(dirs, files)
        }
    }
}

/// Strip root prefix from path.
fn strip_root(path: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        // For root "/", strip leading slash
        path.strip_prefix('/').unwrap_or(path).to_string()
    } else {
        path.strip_prefix(prefix).unwrap_or(path).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2025_12::ManifestFilePath;

    fn make_abs_snapshot(files: Vec<ManifestFilePath>) -> AbsSnapshot {
        AbsSnapshot::new(vec![], files)
    }

    #[test]
    fn test_partition_single_root() {
        let s: AbsSnapshot = make_abs_snapshot(vec![
            ManifestFilePath::file("/home/user/project/a.txt", "hash1", 100, 1000),
            ManifestFilePath::file("/home/user/project/b.txt", "hash2", 200, 2000),
        ]);

        let partitions: Vec<(String, Snapshot)> = partition_manifest(&s, None);

        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0].0, "/home/user/project");

        if let Manifest::V2025_12(m) = partitions[0].1.inner() {
            assert_eq!(m.files.len(), 2);
            assert!(m.files.iter().any(|f| f.path == "a.txt"));
            assert!(m.files.iter().any(|f| f.path == "b.txt"));
        }
    }

    #[test]
    fn test_partition_explicit_roots() {
        let s: AbsSnapshot = make_abs_snapshot(vec![
            ManifestFilePath::file("/project1/a.txt", "hash1", 100, 1000),
            ManifestFilePath::file("/project2/b.txt", "hash2", 200, 2000),
        ]);

        let roots: Vec<String> = vec!["/project1".to_string(), "/project2".to_string()];
        let partitions: Vec<(String, Snapshot)> = partition_manifest(&s, Some(&roots));

        assert_eq!(partitions.len(), 2);

        let p1: &(String, Snapshot) = partitions.iter().find(|(r, _)| r == "/project1").unwrap();
        if let Manifest::V2025_12(m) = p1.1.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].path, "a.txt");
        }

        let p2: &(String, Snapshot) = partitions.iter().find(|(r, _)| r == "/project2").unwrap();
        if let Manifest::V2025_12(m) = p2.1.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].path, "b.txt");
        }
    }

    #[test]
    fn test_partition_empty_manifest() {
        let s: AbsSnapshot = make_abs_snapshot(vec![]);
        let partitions: Vec<(String, Snapshot)> = partition_manifest(&s, None);
        assert!(partitions.is_empty());
    }

    #[test]
    fn test_detect_posix_roots() {
        let paths: Vec<&str> = vec![
            "/home/user/project/src/main.rs",
            "/home/user/project/src/lib.rs",
            "/home/user/project/Cargo.toml",
        ];

        let roots: Vec<String> = detect_posix_roots(&paths);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], "/home/user/project");
    }

    #[test]
    fn test_detect_windows_roots() {
        let paths: Vec<&str> = vec!["C:/Users/user/project/file.txt", "D:/Data/other.txt"];

        let roots: Vec<String> = detect_windows_roots(&paths);
        assert_eq!(roots.len(), 2);
        assert!(roots.contains(&"C:".to_string()));
        assert!(roots.contains(&"D:".to_string()));
    }
}
