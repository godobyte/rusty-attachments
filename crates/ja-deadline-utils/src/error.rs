//! Error types for ja-deadline-utils operations.

use rusty_attachments_filesystem::FileSystemError;
use rusty_attachments_profiles::PathGroupingError;
use rusty_attachments_storage::StorageError;
use thiserror::Error;

/// Errors that can occur during bundle submit.
#[derive(Debug, Error)]
pub enum BundleSubmitError {
    /// Input files that do not exist.
    #[error("Missing input files:\n{}", format_paths(.missing))]
    MissingInputFiles {
        /// List of missing file paths.
        missing: Vec<String>,
    },

    /// Directories specified as input files.
    #[error("Directories specified as input files:\n{}", format_paths(.directories))]
    DirectoriesAsFiles {
        /// List of directory paths incorrectly specified as files.
        directories: Vec<String>,
    },

    /// Path grouping error.
    #[error("Path grouping error: {0}")]
    PathGrouping(#[from] PathGroupingError),

    /// File system error.
    #[error("File system error: {0}")]
    FileSystem(#[from] FileSystemError),

    /// Storage error.
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),

    /// JSON serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Operation cancelled by user.
    #[error("Operation cancelled")]
    Cancelled,
}

/// Format a list of paths for error display.
///
/// # Arguments
/// * `paths` - List of path strings to format
///
/// # Returns
/// Formatted string with each path on a new line, indented.
fn format_paths(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| format!("\t{}", p))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_paths() {
        let paths: Vec<String> = vec!["/path/one".into(), "/path/two".into()];
        let formatted: String = format_paths(&paths);
        assert!(formatted.contains("\t/path/one"));
        assert!(formatted.contains("\t/path/two"));
    }

    #[test]
    fn test_missing_input_files_error() {
        let err: BundleSubmitError = BundleSubmitError::MissingInputFiles {
            missing: vec!["/missing/file.txt".into()],
        };
        let msg: String = err.to_string();
        assert!(msg.contains("Missing input files"));
        assert!(msg.contains("/missing/file.txt"));
    }

    #[test]
    fn test_directories_as_files_error() {
        let err: BundleSubmitError = BundleSubmitError::DirectoriesAsFiles {
            directories: vec!["/some/dir".into()],
        };
        let msg: String = err.to_string();
        assert!(msg.contains("Directories specified as input files"));
        assert!(msg.contains("/some/dir"));
    }
}
