//! Input path expansion and validation utilities.

use std::path::{Path, PathBuf};

use rusty_attachments_common::to_absolute;
use walkdir::WalkDir;

use crate::error::FileSystemError;
use crate::glob::GlobFilter;

/// Result of expanding input paths.
#[derive(Debug, Clone, Default)]
pub struct ExpandedInputPaths {
    /// All discovered file paths (absolute).
    pub files: Vec<PathBuf>,
    /// Directories that were expanded.
    pub expanded_directories: Vec<PathBuf>,
    /// Paths that don't exist (when allow_missing=true).
    pub missing: Vec<PathBuf>,
    /// Total size of all discovered files in bytes.
    pub total_size: u64,
}

/// Result of validating input paths.
#[derive(Debug, Clone, Default)]
pub struct ValidatedInputPaths {
    /// Valid file paths that exist.
    pub valid_files: Vec<PathBuf>,
    /// Paths that don't exist.
    pub missing: Vec<PathBuf>,
    /// Paths that are directories.
    pub directories: Vec<PathBuf>,
}

/// Expand a list of input paths to individual files.
///
/// Directories are recursively walked to discover all files within them.
/// Files are returned as-is after validation.
///
/// # Arguments
/// * `input_paths` - Mix of file and directory paths to expand
/// * `filter` - Optional glob filter to apply during directory walking
/// * `allow_missing` - If true, missing paths are collected instead of error
///
/// # Returns
/// `ExpandedInputPaths` containing all discovered files.
///
/// # Errors
/// - `FileSystemError::PathNotFound` if a path doesn't exist (when allow_missing=false)
/// - `FileSystemError::IoError` if directory walking fails
pub fn expand_input_paths(
    input_paths: &[PathBuf],
    filter: Option<&GlobFilter>,
    allow_missing: bool,
) -> Result<ExpandedInputPaths, FileSystemError> {
    let mut result: ExpandedInputPaths = ExpandedInputPaths::default();

    for path in input_paths {
        let abs_path: PathBuf = to_absolute(path).map_err(|e| FileSystemError::Path(e))?;

        if !abs_path.exists() {
            if allow_missing {
                result.missing.push(abs_path);
                continue;
            } else {
                return Err(FileSystemError::PathNotFound {
                    path: abs_path.display().to_string(),
                });
            }
        }

        if abs_path.is_dir() {
            // Expand directory recursively
            let (files, total_size): (Vec<PathBuf>, u64) =
                walk_directory_for_files(&abs_path, filter)?;
            result.total_size += total_size;
            result.files.extend(files);
            result.expanded_directories.push(abs_path);
        } else {
            // Single file
            let size: u64 = abs_path.metadata().map(|m| m.len()).unwrap_or(0);
            result.total_size += size;
            result.files.push(abs_path);
        }
    }

    Ok(result)
}

/// Validate a list of paths, categorizing them by type and existence.
///
/// This is a lightweight check that doesn't walk directories.
///
/// # Arguments
/// * `paths` - Paths to validate
///
/// # Returns
/// `ValidatedInputPaths` with paths categorized by type.
pub fn validate_input_paths(paths: &[PathBuf]) -> ValidatedInputPaths {
    let mut result: ValidatedInputPaths = ValidatedInputPaths::default();

    for path in paths {
        let abs_path: PathBuf = to_absolute(path).unwrap_or_else(|_| path.clone());

        if !abs_path.exists() {
            result.missing.push(abs_path);
        } else if abs_path.is_dir() {
            result.directories.push(abs_path);
        } else {
            result.valid_files.push(abs_path);
        }
    }

    result
}

/// Walk a directory and return all files (not directories).
///
/// # Arguments
/// * `dir` - Directory to walk
/// * `filter` - Optional glob filter to apply
///
/// # Returns
/// Tuple of (file paths, total size in bytes).
fn walk_directory_for_files(
    dir: &Path,
    filter: Option<&GlobFilter>,
) -> Result<(Vec<PathBuf>, u64), FileSystemError> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut total_size: u64 = 0;

    for entry in WalkDir::new(dir).follow_links(false).into_iter() {
        let entry: walkdir::DirEntry = entry.map_err(|e| FileSystemError::IoError {
            path: e
                .path()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            source: e.into(),
        })?;

        let path: &Path = entry.path();

        // Skip directories
        if path.is_dir() {
            continue;
        }

        // Apply glob filter if provided
        if let Some(f) = filter {
            let relative: String = path
                .strip_prefix(dir)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if !f.matches(&relative) {
                continue;
            }
        }

        let size: u64 = entry.metadata().map(|m| m.len()).unwrap_or(0);
        total_size += size;
        files.push(path.to_path_buf());
    }

    Ok((files, total_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_structure(dir: &Path) {
        // Create files
        let mut f1 = std::fs::File::create(dir.join("file1.txt")).unwrap();
        f1.write_all(b"hello").unwrap();

        let mut f2 = std::fs::File::create(dir.join("file2.py")).unwrap();
        f2.write_all(b"print('hi')").unwrap();

        // Create subdirectory with files
        std::fs::create_dir(dir.join("subdir")).unwrap();
        let mut f3 = std::fs::File::create(dir.join("subdir/nested.txt")).unwrap();
        f3.write_all(b"nested content").unwrap();

        let mut f4 = std::fs::File::create(dir.join("subdir/data.tmp")).unwrap();
        f4.write_all(b"temp").unwrap();
    }

    #[test]
    fn test_expand_single_file() {
        let dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = dir.path().join("test.txt");
        let mut file = std::fs::File::create(&file_path).unwrap();
        file.write_all(b"hello").unwrap();
        drop(file);

        let result: ExpandedInputPaths =
            expand_input_paths(&[file_path.clone()], None, false).unwrap();

        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0], file_path);
        assert_eq!(result.total_size, 5);
        assert!(result.expanded_directories.is_empty());
        assert!(result.missing.is_empty());
    }

    #[test]
    fn test_expand_directory() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_structure(dir.path());

        let result: ExpandedInputPaths =
            expand_input_paths(&[dir.path().to_path_buf()], None, false).unwrap();

        assert_eq!(result.files.len(), 4);
        assert_eq!(result.expanded_directories.len(), 1);
        assert!(result.missing.is_empty());
    }

    #[test]
    fn test_expand_with_filter() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_structure(dir.path());

        let filter: GlobFilter = GlobFilter::include(vec!["**/*.txt".to_string()]).unwrap();
        let result: ExpandedInputPaths =
            expand_input_paths(&[dir.path().to_path_buf()], Some(&filter), false).unwrap();

        assert_eq!(result.files.len(), 2); // file1.txt and subdir/nested.txt
        for file in &result.files {
            assert!(file.extension().map(|e| e == "txt").unwrap_or(false));
        }
    }

    #[test]
    fn test_expand_with_exclude_filter() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_structure(dir.path());

        let filter: GlobFilter = GlobFilter::exclude(vec!["**/*.tmp".to_string()]).unwrap();
        let result: ExpandedInputPaths =
            expand_input_paths(&[dir.path().to_path_buf()], Some(&filter), false).unwrap();

        assert_eq!(result.files.len(), 3); // All except .tmp
        for file in &result.files {
            assert!(file.extension().map(|e| e != "tmp").unwrap_or(true));
        }
    }

    #[test]
    fn test_expand_missing_path_error() {
        let result = expand_input_paths(&[PathBuf::from("/nonexistent/path")], None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_expand_missing_path_allowed() {
        let result: ExpandedInputPaths =
            expand_input_paths(&[PathBuf::from("/nonexistent/path")], None, true).unwrap();

        assert!(result.files.is_empty());
        assert_eq!(result.missing.len(), 1);
    }

    #[test]
    fn test_validate_input_paths() {
        let dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = dir.path().join("test.txt");
        std::fs::File::create(&file_path).unwrap();

        let subdir: PathBuf = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let paths: Vec<PathBuf> = vec![
            file_path.clone(),
            subdir.clone(),
            PathBuf::from("/nonexistent"),
        ];

        let result: ValidatedInputPaths = validate_input_paths(&paths);

        assert_eq!(result.valid_files.len(), 1);
        assert_eq!(result.valid_files[0], file_path);
        assert_eq!(result.directories.len(), 1);
        assert_eq!(result.directories[0], subdir);
        assert_eq!(result.missing.len(), 1);
    }

    #[test]
    fn test_expand_mixed_inputs() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_structure(dir.path());

        let single_file: PathBuf = dir.path().join("file1.txt");
        let subdir: PathBuf = dir.path().join("subdir");

        let result: ExpandedInputPaths =
            expand_input_paths(&[single_file.clone(), subdir.clone()], None, false).unwrap();

        // file1.txt + 2 files from subdir
        assert_eq!(result.files.len(), 3);
        assert_eq!(result.expanded_directories.len(), 1);
        assert_eq!(result.expanded_directories[0], subdir);
    }
}
