//! Path normalization utilities for manifest operations.

use std::path::{Component, Path, PathBuf};

use crate::error::PathError;

/// Convert a path to absolute without resolving symlinks.
///
/// # Arguments
/// * `path` - Path to convert (relative or absolute)
///
/// # Returns
/// Absolute path, joining with current directory if relative.
///
/// # Errors
/// Returns error if current directory cannot be determined.
pub fn to_absolute(path: &Path) -> Result<PathBuf, PathError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd: PathBuf| cwd.join(path))
            .map_err(|e: std::io::Error| PathError::IoError {
                path: path.display().to_string(),
                message: e.to_string(),
            })
    }
}

/// Lexical path normalization without filesystem access.
///
/// Removes `.` components and resolves `..` components lexically.
/// Does not access the filesystem or resolve symlinks.
///
/// # Arguments
/// * `path` - Path to normalize
///
/// # Returns
/// Normalized path with `.` and `..` resolved lexically.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut components: Vec<Component> = Vec::new();

    for component in path.components() {
        match component {
            Component::CurDir => { /* skip . */ }
            Component::ParentDir => {
                // Pop if we can and it's not a ParentDir or RootDir
                if !components.is_empty()
                    && !matches!(
                        components.last(),
                        Some(Component::ParentDir) | Some(Component::RootDir)
                    )
                {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }

    components.iter().collect()
}

/// Normalize a path for storage in manifests.
///
/// This function:
/// 1. Converts to absolute path WITHOUT resolving symlinks
/// 2. Removes `.` and `..` components via lexical normalization
/// 3. Converts to POSIX format (forward slashes)
/// 4. Returns path relative to the asset root
///
/// # Arguments
/// * `path` - Path to normalize
/// * `root` - Asset root directory
///
/// # Returns
/// POSIX-style relative path suitable for manifest storage.
///
/// # Errors
/// Returns error if path is outside the root directory.
pub fn normalize_for_manifest(path: &Path, root: &Path) -> Result<String, PathError> {
    let abs_path: PathBuf = to_absolute(path)?;
    let normalized: PathBuf = lexical_normalize(&abs_path);

    let abs_root: PathBuf = to_absolute(root)?;
    let normalized_root: PathBuf = lexical_normalize(&abs_root);

    let relative: &Path =
        normalized
            .strip_prefix(&normalized_root)
            .map_err(|_| PathError::PathOutsideRoot {
                path: normalized.display().to_string(),
                root: normalized_root.display().to_string(),
            })?;

    Ok(to_posix_path(relative))
}

/// Convert a path to POSIX-style string (forward slashes).
///
/// Used for manifest paths which are always POSIX format.
///
/// # Arguments
/// * `path` - Path to convert
///
/// # Returns
/// String with forward slashes as separators.
pub fn to_posix_path(path: &Path) -> String {
    path.components()
        .map(|c: Component| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Convert a manifest path (POSIX format) to the host OS path format.
///
/// # Arguments
/// * `manifest_path` - POSIX-style path from manifest
/// * `destination_root` - Local destination root directory
///
/// # Returns
/// PathBuf with OS-native separators.
pub fn from_posix_path(manifest_path: &str, destination_root: &Path) -> PathBuf {
    let components: Vec<&str> = manifest_path.split('/').collect();
    let mut result: PathBuf = destination_root.to_path_buf();

    for component in components {
        if !component.is_empty() {
            result.push(component);
        }
    }

    result
}

/// Check if a path is within a root directory (security validation).
///
/// Uses lexical comparison, does not access filesystem.
///
/// # Arguments
/// * `path` - Path to check
/// * `root` - Root directory that should contain the path
///
/// # Returns
/// `true` if path is within root, `false` otherwise.
pub fn is_within_root(path: &Path, root: &Path) -> bool {
    let norm_path: PathBuf = lexical_normalize(path);
    let norm_root: PathBuf = lexical_normalize(root);
    norm_path.starts_with(&norm_root)
}

/// Convert a path to Windows long path format if needed.
///
/// On Windows, paths longer than MAX_PATH (260 chars) need the `\\?\` prefix.
/// No-op on non-Windows platforms.
///
/// # Arguments
/// * `path` - Path to convert
///
/// # Returns
/// Path with long path prefix if needed (Windows only).
#[cfg(windows)]
pub fn to_long_path(path: &Path) -> PathBuf {
    use crate::constants::WINDOWS_MAX_PATH;

    let path_str: std::borrow::Cow<str> = path.to_string_lossy();

    if path_str.starts_with(r"\\?\") {
        return path.to_path_buf();
    }

    if path_str.len() < WINDOWS_MAX_PATH {
        return path.to_path_buf();
    }

    let abs_path: PathBuf = to_absolute(path).unwrap_or_else(|_| path.to_path_buf());
    PathBuf::from(format!(r"\\?\{}", abs_path.display()))
}

/// Convert a path to Windows long path format if needed.
///
/// No-op on non-Windows platforms.
///
/// # Arguments
/// * `path` - Path to convert
///
/// # Returns
/// The same path unchanged.
#[cfg(not(windows))]
pub fn to_long_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexical_normalize_removes_dot() {
        let path: PathBuf = PathBuf::from("/a/./b/./c");
        let normalized: PathBuf = lexical_normalize(&path);
        assert_eq!(normalized, PathBuf::from("/a/b/c"));
    }

    #[test]
    fn test_lexical_normalize_resolves_dotdot() {
        let path: PathBuf = PathBuf::from("/a/b/../c");
        let normalized: PathBuf = lexical_normalize(&path);
        assert_eq!(normalized, PathBuf::from("/a/c"));
    }

    #[test]
    fn test_lexical_normalize_preserves_root_dotdot() {
        // Can't go above root, so extra .. are preserved
        let path: PathBuf = PathBuf::from("/a/../../../b");
        let normalized: PathBuf = lexical_normalize(&path);
        // After /a, we have ../../b - first .. pops /a, second and third can't go above /
        assert_eq!(normalized, PathBuf::from("/../../b"));
    }

    #[test]
    fn test_lexical_normalize_within_root() {
        let path: PathBuf = PathBuf::from("/a/b/c/../../d");
        let normalized: PathBuf = lexical_normalize(&path);
        assert_eq!(normalized, PathBuf::from("/a/d"));
    }

    #[test]
    fn test_to_posix_path() {
        let path: PathBuf = PathBuf::from("a/b/c");
        let posix: String = to_posix_path(&path);
        assert_eq!(posix, "a/b/c");
    }

    #[test]
    fn test_from_posix_path() {
        let result: PathBuf = from_posix_path("a/b/c", Path::new("/root"));
        assert_eq!(result, PathBuf::from("/root/a/b/c"));
    }

    #[test]
    fn test_from_posix_path_empty_components() {
        let result: PathBuf = from_posix_path("a//b", Path::new("/root"));
        assert_eq!(result, PathBuf::from("/root/a/b"));
    }

    #[test]
    fn test_is_within_root_true() {
        assert!(is_within_root(
            Path::new("/project/assets/file.txt"),
            Path::new("/project")
        ));
    }

    #[test]
    fn test_is_within_root_false() {
        assert!(!is_within_root(
            Path::new("/etc/passwd"),
            Path::new("/project")
        ));
    }

    #[test]
    fn test_is_within_root_with_dotdot() {
        assert!(!is_within_root(
            Path::new("/project/../etc/passwd"),
            Path::new("/project")
        ));
    }

    #[test]
    fn test_to_long_path_short() {
        let path: PathBuf = PathBuf::from("/short/path");
        let result: PathBuf = to_long_path(&path);
        assert_eq!(result, path);
    }
}
