//! Manifest filtering operation.
//!
//! Filters manifest entries using predicates or glob patterns.

use crate::v2025_12::{ManifestDirectoryPath, ManifestFilePath};
use crate::{AbsSnapshot, Manifest, Snapshot};

/// Filter a relative snapshot using a predicate.
///
/// # Arguments
/// * `manifest` - Snapshot to filter
/// * `predicate` - Function returning true for entries to keep
///
/// # Returns
/// Filtered snapshot containing only matching entries.
pub fn filter_manifest<F>(manifest: &Snapshot, predicate: F) -> Snapshot
where
    F: Fn(&ManifestFilePath) -> bool,
{
    let (dirs, files) = filter_entries(manifest.inner(), &predicate);
    Snapshot::new(dirs, files)
}

/// Filter an absolute snapshot using a predicate.
///
/// # Arguments
/// * `manifest` - Absolute snapshot to filter
/// * `predicate` - Function returning true for entries to keep
///
/// # Returns
/// Filtered absolute snapshot containing only matching entries.
pub fn filter_abs_manifest<F>(manifest: &AbsSnapshot, predicate: F) -> AbsSnapshot
where
    F: Fn(&ManifestFilePath) -> bool,
{
    let (dirs, files) = filter_entries(manifest.inner(), &predicate);
    AbsSnapshot::new(dirs, files)
}

/// Internal function to filter manifest entries.
fn filter_entries<F>(
    manifest: &Manifest,
    predicate: &F,
) -> (Vec<ManifestDirectoryPath>, Vec<ManifestFilePath>)
where
    F: Fn(&ManifestFilePath) -> bool,
{
    match manifest {
        Manifest::V2023_03_03(_) => (vec![], vec![]),
        Manifest::V2025_12(m) => {
            // Filter files
            let files: Vec<ManifestFilePath> =
                m.files.iter().filter(|f| predicate(f)).cloned().collect();

            // Keep directories that have at least one matching file under them
            let dirs: Vec<ManifestDirectoryPath> = m
                .dirs
                .iter()
                .filter(|d| {
                    files
                        .iter()
                        .any(|f| f.path.starts_with(&d.path) && f.path.len() > d.path.len())
                })
                .cloned()
                .collect();

            (dirs, files)
        }
    }
}

/// Include/exclude filter based on glob patterns.
#[derive(Debug, Clone, Default)]
pub struct IncludeExcludeFilter {
    include_patterns: Vec<glob::Pattern>,
    exclude_patterns: Vec<glob::Pattern>,
}

impl IncludeExcludeFilter {
    /// Create a new filter with include and exclude patterns.
    ///
    /// # Arguments
    /// * `include` - Glob patterns for files to include (empty = include all)
    /// * `exclude` - Glob patterns for files to exclude
    ///
    /// # Errors
    /// Returns error if any pattern is invalid.
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self, glob::PatternError> {
        let include_patterns: Vec<glob::Pattern> = include
            .iter()
            .map(|p| glob::Pattern::new(p))
            .collect::<Result<Vec<_>, _>>()?;

        let exclude_patterns: Vec<glob::Pattern> = exclude
            .iter()
            .map(|p| glob::Pattern::new(p))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            include_patterns,
            exclude_patterns,
        })
    }

    /// Create a filter with only include patterns.
    pub fn include_only(patterns: &[String]) -> Result<Self, glob::PatternError> {
        Self::new(patterns, &[])
    }

    /// Create a filter with only exclude patterns.
    pub fn exclude_only(patterns: &[String]) -> Result<Self, glob::PatternError> {
        Self::new(&[], patterns)
    }

    /// Check if a path matches the filter.
    ///
    /// # Arguments
    /// * `path` - Path to check
    ///
    /// # Returns
    /// True if the path should be included.
    pub fn matches(&self, path: &str) -> bool {
        // Check exclude patterns first
        for pattern in &self.exclude_patterns {
            if pattern.matches(path) {
                return false;
            }
        }

        // If no include patterns, include everything not excluded
        if self.include_patterns.is_empty() {
            return true;
        }

        // Check include patterns
        for pattern in &self.include_patterns {
            if pattern.matches(path) {
                return true;
            }
        }

        false
    }

    /// Create a predicate function for use with filter_manifest.
    pub fn as_predicate(&self) -> impl Fn(&ManifestFilePath) -> bool + '_ {
        move |file: &ManifestFilePath| self.matches(&file.path)
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
    fn test_filter_all_pass() {
        let s: Snapshot = make_snapshot(vec![
            ManifestFilePath::file("a.txt", "hash1", 100, 1000),
            ManifestFilePath::file("b.txt", "hash2", 200, 2000),
        ]);

        let result: Snapshot = filter_manifest(&s, |_| true);

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 2);
        }
    }

    #[test]
    fn test_filter_none_pass() {
        let s: Snapshot = make_snapshot(vec![
            ManifestFilePath::file("a.txt", "hash1", 100, 1000),
            ManifestFilePath::file("b.txt", "hash2", 200, 2000),
        ]);

        let result: Snapshot = filter_manifest(&s, |_| false);

        if let Manifest::V2025_12(m) = result.inner() {
            assert!(m.files.is_empty());
        }
    }

    #[test]
    fn test_filter_by_size() {
        let s: Snapshot = make_snapshot(vec![
            ManifestFilePath::file("small.txt", "hash1", 100, 1000),
            ManifestFilePath::file("large.txt", "hash2", 1000, 2000),
        ]);

        let result: Snapshot = filter_manifest(&s, |f| f.size.unwrap_or(0) > 500);

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 1);
            assert_eq!(m.files[0].path, "large.txt");
        }
    }

    #[test]
    fn test_include_exclude_filter_include_only() {
        let filter: IncludeExcludeFilter =
            IncludeExcludeFilter::include_only(&["*.txt".to_string()]).unwrap();

        assert!(filter.matches("file.txt"));
        assert!(!filter.matches("file.log"));
    }

    #[test]
    fn test_include_exclude_filter_exclude_only() {
        let filter: IncludeExcludeFilter =
            IncludeExcludeFilter::exclude_only(&["*.log".to_string()]).unwrap();

        assert!(filter.matches("file.txt"));
        assert!(!filter.matches("file.log"));
    }

    #[test]
    fn test_include_exclude_filter_both() {
        let filter: IncludeExcludeFilter =
            IncludeExcludeFilter::new(&["*.txt".to_string()], &["secret*".to_string()]).unwrap();

        assert!(filter.matches("file.txt"));
        assert!(!filter.matches("secret.txt")); // Excluded takes precedence
        assert!(!filter.matches("file.log")); // Not in include
    }

    #[test]
    fn test_filter_with_glob_filter() {
        let s: Snapshot = make_snapshot(vec![
            ManifestFilePath::file("src/main.rs", "hash1", 100, 1000),
            ManifestFilePath::file("src/lib.rs", "hash2", 200, 2000),
            ManifestFilePath::file("target/debug/app", "hash3", 300, 3000),
        ]);

        let glob_filter: IncludeExcludeFilter =
            IncludeExcludeFilter::exclude_only(&["target/*".to_string()]).unwrap();
        let result: Snapshot = filter_manifest(&s, glob_filter.as_predicate());

        if let Manifest::V2025_12(m) = result.inner() {
            assert_eq!(m.files.len(), 2);
            assert!(m.files.iter().all(|f| !f.path.starts_with("target/")));
        }
    }
}
