//! Symlink security validation.

use std::path::{Component, Path, PathBuf};

use rusty_attachments_common::{is_within_root, to_posix_path};

use crate::error::FileSystemError;

/// Information about a validated symlink.
#[derive(Debug, Clone)]
pub struct SymlinkInfo {
    /// Path to the symlink itself.
    pub path: PathBuf,
    /// Original target as stored in symlink (relative, POSIX format).
    pub target: String,
    /// Fully resolved target path.
    pub resolved_target: PathBuf,
}

/// Validate a symlink for inclusion in a manifest.
///
/// # Security Checks
/// 1. Target must be relative (no absolute paths)
/// 2. Resolved target must be within the asset root
/// 3. Target path must not escape via `..` traversal
///
/// # Arguments
/// * `symlink_path` - Path to the symlink
/// * `root` - Asset root directory
///
/// # Returns
/// `SymlinkInfo` with validated symlink details.
///
/// # Errors
/// - `SymlinkAbsoluteTarget` if target is absolute
/// - `SymlinkEscapesRoot` if resolved target is outside root
pub fn validate_symlink(symlink_path: &Path, root: &Path) -> Result<SymlinkInfo, FileSystemError> {
    // Read the symlink target without following it
    let target: PathBuf =
        std::fs::read_link(symlink_path).map_err(|e| FileSystemError::IoError {
            path: symlink_path.display().to_string(),
            source: e,
        })?;

    // Check 1: Target must be relative
    if target.is_absolute() {
        return Err(FileSystemError::SymlinkAbsoluteTarget {
            symlink: symlink_path.display().to_string(),
            target: target.display().to_string(),
        });
    }

    // Check for Windows absolute paths (e.g., C:\...)
    let target_str: String = target.to_string_lossy().to_string();
    if target_str.len() >= 2 && target_str.chars().nth(1) == Some(':') {
        return Err(FileSystemError::SymlinkAbsoluteTarget {
            symlink: symlink_path.display().to_string(),
            target: target_str,
        });
    }

    // Check for UNC paths
    if target_str.starts_with("\\\\") || target_str.starts_with("//") {
        return Err(FileSystemError::SymlinkAbsoluteTarget {
            symlink: symlink_path.display().to_string(),
            target: target_str,
        });
    }

    // Check 2: Resolve target relative to symlink's parent directory
    let symlink_dir: &Path =
        symlink_path
            .parent()
            .ok_or_else(|| FileSystemError::InvalidPath {
                path: symlink_path.display().to_string(),
            })?;

    // Lexically resolve the target (don't touch filesystem)
    let resolved: PathBuf = lexical_resolve(symlink_dir, &target);

    // Check 3: Resolved path must be within root
    if !is_within_root(&resolved, root) {
        return Err(FileSystemError::SymlinkEscapesRoot {
            symlink: symlink_path.display().to_string(),
            target: target.display().to_string(),
        });
    }

    Ok(SymlinkInfo {
        path: symlink_path.to_path_buf(),
        target: to_posix_path(&target),
        resolved_target: resolved,
    })
}

/// Lexically resolve a relative path from a base directory.
///
/// Does NOT access the filesystem - pure path manipulation.
///
/// # Arguments
/// * `base` - Base directory
/// * `relative` - Relative path to resolve
///
/// # Returns
/// Resolved absolute path.
fn lexical_resolve(base: &Path, relative: &Path) -> PathBuf {
    let mut result: PathBuf = base.to_path_buf();

    for component in relative.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => { /* skip */ }
            Component::Normal(name) => {
                result.push(name);
            }
            _ => {
                result.push(component);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    fn create_symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_symlink_relative_within_root() {
        let dir: TempDir = TempDir::new().unwrap();
        let target_path: PathBuf = dir.path().join("target.txt");
        let link_path: PathBuf = dir.path().join("link.txt");

        std::fs::File::create(&target_path).unwrap();
        create_symlink(Path::new("target.txt"), &link_path);

        let info: SymlinkInfo = validate_symlink(&link_path, dir.path()).unwrap();

        assert_eq!(info.path, link_path);
        assert_eq!(info.target, "target.txt");
        assert_eq!(info.resolved_target, target_path);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_symlink_nested_relative() {
        let dir: TempDir = TempDir::new().unwrap();
        let subdir: PathBuf = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let target_path: PathBuf = dir.path().join("target.txt");
        let link_path: PathBuf = subdir.join("link.txt");

        std::fs::File::create(&target_path).unwrap();
        create_symlink(Path::new("../target.txt"), &link_path);

        let info: SymlinkInfo = validate_symlink(&link_path, dir.path()).unwrap();

        assert_eq!(info.target, "../target.txt");
        assert_eq!(info.resolved_target, target_path);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_symlink_escapes_root() {
        let dir: TempDir = TempDir::new().unwrap();
        let link_path: PathBuf = dir.path().join("link.txt");

        // Create symlink pointing outside root
        create_symlink(Path::new("../../outside.txt"), &link_path);

        let result = validate_symlink(&link_path, dir.path());
        assert!(matches!(result, Err(FileSystemError::SymlinkEscapesRoot { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_symlink_absolute_target() {
        let dir: TempDir = TempDir::new().unwrap();
        let link_path: PathBuf = dir.path().join("link.txt");

        // Create symlink with absolute target
        create_symlink(Path::new("/etc/passwd"), &link_path);

        let result = validate_symlink(&link_path, dir.path());
        assert!(matches!(
            result,
            Err(FileSystemError::SymlinkAbsoluteTarget { .. })
        ));
    }

    #[test]
    fn test_lexical_resolve_simple() {
        let base: PathBuf = PathBuf::from("/project/assets");
        let relative: PathBuf = PathBuf::from("textures/wood.png");
        let resolved: PathBuf = lexical_resolve(&base, &relative);
        assert_eq!(resolved, PathBuf::from("/project/assets/textures/wood.png"));
    }

    #[test]
    fn test_lexical_resolve_with_dotdot() {
        let base: PathBuf = PathBuf::from("/project/assets/models");
        let relative: PathBuf = PathBuf::from("../textures/wood.png");
        let resolved: PathBuf = lexical_resolve(&base, &relative);
        assert_eq!(resolved, PathBuf::from("/project/assets/textures/wood.png"));
    }

    #[test]
    fn test_lexical_resolve_with_dot() {
        let base: PathBuf = PathBuf::from("/project/assets");
        let relative: PathBuf = PathBuf::from("./textures/wood.png");
        let resolved: PathBuf = lexical_resolve(&base, &relative);
        assert_eq!(resolved, PathBuf::from("/project/assets/textures/wood.png"));
    }
}
