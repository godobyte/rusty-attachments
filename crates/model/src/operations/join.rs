//! Join operation (inverse of subtree).
//!
//! Prepends a prefix to all paths in a manifest.

use crate::v2025_12::{ManifestDirectoryPath, ManifestFilePath};
use crate::{AbsSnapshot, Manifest, Snapshot};

/// Prepend a prefix to all paths in a relative manifest.
///
/// This is the inverse of the subtree operation. If the prefix is absolute,
/// returns an AbsSnapshot; otherwise returns a Snapshot.
///
/// # Arguments
/// * `manifest` - Relative-path manifest
/// * `prefix` - Path prefix to prepend
///
/// # Returns
/// Manifest with prefixed paths.
pub fn join_manifest(manifest: &Snapshot, prefix: &str) -> JoinResult {
    let normalized_prefix: String = normalize_prefix(prefix);
    let is_absolute: bool = is_absolute_path(&normalized_prefix);

    let (dirs, files) = prepend_prefix(manifest.inner(), &normalized_prefix);

    if is_absolute {
        JoinResult::Absolute(AbsSnapshot::new(dirs, files))
    } else {
        JoinResult::Relative(Snapshot::new(dirs, files))
    }
}

/// Join with an absolute prefix, returning an AbsSnapshot.
///
/// # Arguments
/// * `manifest` - Relative-path manifest
/// * `prefix` - Absolute path prefix to prepend
///
/// # Returns
/// Absolute snapshot with prefixed paths.
pub fn join_to_absolute(manifest: &Snapshot, prefix: &str) -> AbsSnapshot {
    let normalized_prefix: String = normalize_prefix(prefix);
    let (dirs, files) = prepend_prefix(manifest.inner(), &normalized_prefix);
    AbsSnapshot::new(dirs, files)
}

/// Join with a relative prefix, returning a Snapshot.
///
/// # Arguments
/// * `manifest` - Relative-path manifest
/// * `prefix` - Relative path prefix to prepend
///
/// # Returns
/// Relative snapshot with prefixed paths.
pub fn join_to_relative(manifest: &Snapshot, prefix: &str) -> Snapshot {
    let normalized_prefix: String = normalize_prefix(prefix);
    let (dirs, files) = prepend_prefix(manifest.inner(), &normalized_prefix);
    Snapshot::new(dirs, files)
}

/// Result of join operation - can be absolute or relative.
#[derive(Debug, Clone)]
pub enum JoinResult {
    Absolute(AbsSnapshot),
    Relative(Snapshot),
}

impl JoinResult {
    /// Check if result is absolute.
    pub fn is_absolute(&self) -> bool {
        matches!(self, JoinResult::Absolute(_))
    }

    /// Check if result is relative.
    pub fn is_relative(&self) -> bool {
        matches!(self, JoinResult::Relative(_))
    }

    /// Get as absolute snapshot (panics if relative).
    pub fn unwrap_absolute(self) -> AbsSnapshot {
        match self {
            JoinResult::Absolute(s) => s,
            JoinResult::Relative(_) => panic!("Expected absolute snapshot"),
        }
    }

    /// Get as relative snapshot (panics if absolute).
    pub fn unwrap_relative(self) -> Snapshot {
        match self {
            JoinResult::Relative(s) => s,
            JoinResult::Absolute(_) => panic!("Expected relative snapshot"),
        }
    }
}

/// Normalize prefix for consistent joining.
fn normalize_prefix(prefix: &str) -> String {
    let mut normalized: String = prefix.replace('\\', "/");

    // Remove trailing slash
    while normalized.ends_with('/') {
        normalized.pop();
    }

    normalized
}

/// Check if a path is absolute.
fn is_absolute_path(path: &str) -> bool {
    // POSIX absolute
    if path.starts_with('/') {
        return true;
    }

    // Windows absolute (drive letter)
    if path.len() >= 2 && path.chars().nth(1) == Some(':') {
        return true;
    }

    // UNC path
    if path.starts_with("//") {
        return true;
    }

    false
}

/// Prepend prefix to all paths in manifest.
fn prepend_prefix(
    manifest: &Manifest,
    prefix: &str,
) -> (Vec<ManifestDirectoryPath>, Vec<ManifestFilePath>) {
    match manifest {
        Manifest::V2023_03_03(_) => (vec![], vec![]),
        Manifest::V2025_12(m) => {
            let files: Vec<ManifestFilePath> = m
                .files
                .iter()
                .map(|f| ManifestFilePath {
                    path: join_paths(prefix, &f.path),
                    hash: f.hash.clone(),
                    size: f.size,
                    mtime: f.mtime,
                    runnable: f.runnable,
                    chunkhashes: f.chunkhashes.clone(),
                    symlink_target: f.symlink_target.clone(),
                    deleted: f.deleted,
                })
                .collect();

            let dirs: Vec<ManifestDirectoryPath> = m
                .dirs
                .iter()
                .map(|d| ManifestDirectoryPath {
                    path: join_paths(prefix, &d.path),
                    deleted: d.deleted,
                })
                .collect();

            (dirs, files)
        }
    }
}

/// Join two path components.
fn join_paths(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_string()
    } else if path.is_empty() {
        prefix.to_string()
    } else {
        format!("{}/{}", prefix, path)
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
    fn test_join_absolute_prefix() {
        let s: Snapshot = make_snapshot(vec![
            ManifestFilePath::file("src/main.rs", "hash1", 100, 1000),
            ManifestFilePath::file("Cargo.toml", "hash2", 200, 2000),
        ]);

        let result: JoinResult = join_manifest(&s, "/home/user/project");

        assert!(result.is_absolute());
        let abs: AbsSnapshot = result.unwrap_absolute();

        if let Manifest::V2025_12(m) = abs.inner() {
            assert_eq!(m.files.len(), 2);
            assert!(m
                .files
                .iter()
                .any(|f| f.path == "/home/user/project/src/main.rs"));
            assert!(m
                .files
                .iter()
                .any(|f| f.path == "/home/user/project/Cargo.toml"));
        }
    }

    #[test]
    fn test_join_relative_prefix() {
        let s: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("main.rs", "hash1", 100, 1000)]);

        let result: JoinResult = join_manifest(&s, "src");

        assert!(result.is_relative());
        let rel: Snapshot = result.unwrap_relative();

        if let Manifest::V2025_12(m) = rel.inner() {
            assert_eq!(m.files[0].path, "src/main.rs");
        }
    }

    #[test]
    fn test_join_empty_prefix() {
        let s: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("file.txt", "hash1", 100, 1000)]);

        let result: JoinResult = join_manifest(&s, "");

        assert!(result.is_relative());
        let rel: Snapshot = result.unwrap_relative();

        if let Manifest::V2025_12(m) = rel.inner() {
            assert_eq!(m.files[0].path, "file.txt");
        }
    }

    #[test]
    fn test_join_to_absolute() {
        let s: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("file.txt", "hash1", 100, 1000)]);

        let abs: AbsSnapshot = join_to_absolute(&s, "/project");

        if let Manifest::V2025_12(m) = abs.inner() {
            assert_eq!(m.files[0].path, "/project/file.txt");
        }
    }

    #[test]
    fn test_join_preserves_metadata() {
        let s: Snapshot = make_snapshot(vec![ManifestFilePath::file(
            "script.sh",
            "hash1",
            100,
            1000,
        )
        .with_runnable(true)]);

        let result: AbsSnapshot = join_to_absolute(&s, "/bin");

        if let Manifest::V2025_12(m) = result.inner() {
            assert!(m.files[0].runnable);
            assert_eq!(m.files[0].hash, Some("hash1".to_string()));
        }
    }

    #[test]
    fn test_join_trailing_slash_normalized() {
        let s: Snapshot =
            make_snapshot(vec![ManifestFilePath::file("file.txt", "hash1", 100, 1000)]);

        let result1: AbsSnapshot = join_to_absolute(&s, "/project");
        let result2: AbsSnapshot = join_to_absolute(&s, "/project/");

        if let (Manifest::V2025_12(m1), Manifest::V2025_12(m2)) = (result1.inner(), result2.inner())
        {
            assert_eq!(m1.files[0].path, m2.files[0].path);
        }
    }

    #[test]
    fn test_subtree_join_roundtrip() {
        use crate::operations::subtree::subtree_manifest;

        let original: AbsSnapshot = AbsSnapshot::new(
            vec![],
            vec![
                ManifestFilePath::file("/project/src/main.rs", "hash1", 100, 1000),
                ManifestFilePath::file("/project/Cargo.toml", "hash2", 200, 2000),
            ],
        );

        // Extract subtree
        let subtree: Snapshot = subtree_manifest(&original, "/project");

        // Join back
        let rejoined: AbsSnapshot = join_to_absolute(&subtree, "/project");

        if let (Manifest::V2025_12(orig), Manifest::V2025_12(rej)) =
            (original.inner(), rejoined.inner())
        {
            assert_eq!(orig.files.len(), rej.files.len());
            for orig_file in &orig.files {
                assert!(rej.files.iter().any(|f| f.path == orig_file.path));
            }
        }
    }
}
