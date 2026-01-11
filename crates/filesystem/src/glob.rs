//! Glob pattern matching for file filtering.
//!
//! This module provides glob-based filtering for file operations. It supports:
//! - Include/exclude patterns using standard glob syntax
//! - Recursive matching with `**`
//! - Brace expansion like `*.{png,jpg}`
//! - Character classes like `[abc]`
//!
//! ## Literal Path Matching
//!
//! When you need to match literal paths that contain glob metacharacters
//! (like `[`, `]`, `*`, `?`, `{`, `}`, `!`), use the `escape_glob()` function
//! to escape these characters:
//!
//! ```
//! use rusty_attachments_filesystem::glob::{escape_glob, GlobFilter};
//!
//! // Match a directory with brackets in its name
//! let pattern = format!("{}/**", escape_glob("output[v1]"));
//! let filter = GlobFilter::include(vec![pattern]).unwrap();
//!
//! assert!(filter.matches("output[v1]/file.txt"));
//! assert!(!filter.matches("outputv1/file.txt"));
//! ```
//!
//! This is particularly important when working with user-provided paths or
//! output directories that may contain special characters.

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::error::FileSystemError;

/// Escape special glob characters in a string to treat it as a literal path.
///
/// This function escapes the following glob metacharacters:
/// - `*` (matches any sequence of characters)
/// - `?` (matches any single character)
/// - `[` and `]` (character class delimiters)
/// - `{` and `}` (brace expansion delimiters)
/// - `!` (negation in character classes)
///
/// # Arguments
/// * `s` - String to escape
///
/// # Returns
/// A new string with all glob metacharacters escaped with backslashes.
///
/// # Example
/// ```
/// use rusty_attachments_filesystem::glob::escape_glob;
///
/// assert_eq!(escape_glob("file[1].txt"), r"file\[1\].txt");
/// assert_eq!(escape_glob("test*.log"), r"test\*.log");
/// ```
pub fn escape_glob(s: &str) -> String {
    let mut escaped: String = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '*' | '?' | '[' | ']' | '{' | '}' | '!' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Configuration for filtering files during directory operations.
#[derive(Debug, Clone, Default)]
pub struct GlobFilter {
    /// Patterns for files to include (empty = include all).
    include: Vec<String>,
    /// Patterns for files to exclude.
    exclude: Vec<String>,
    /// Compiled include patterns.
    include_set: Option<GlobSet>,
    /// Compiled exclude patterns.
    exclude_set: Option<GlobSet>,
}

impl GlobFilter {
    /// Create a new filter with no patterns (matches everything).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a filter with include patterns only.
    ///
    /// # Arguments
    /// * `patterns` - Glob patterns for files to include
    ///
    /// # Errors
    /// Returns error if any pattern is invalid.
    pub fn include(patterns: Vec<String>) -> Result<Self, FileSystemError> {
        let mut filter: GlobFilter = Self {
            include: patterns,
            exclude: vec![],
            include_set: None,
            exclude_set: None,
        };
        filter.compile()?;
        Ok(filter)
    }

    /// Create a filter with exclude patterns only.
    ///
    /// # Arguments
    /// * `patterns` - Glob patterns for files to exclude
    ///
    /// # Errors
    /// Returns error if any pattern is invalid.
    pub fn exclude(patterns: Vec<String>) -> Result<Self, FileSystemError> {
        let mut filter: GlobFilter = Self {
            include: vec![],
            exclude: patterns,
            include_set: None,
            exclude_set: None,
        };
        filter.compile()?;
        Ok(filter)
    }

    /// Create a filter with both include and exclude patterns.
    ///
    /// # Arguments
    /// * `include` - Glob patterns for files to include
    /// * `exclude` - Glob patterns for files to exclude
    ///
    /// # Errors
    /// Returns error if any pattern is invalid.
    pub fn with_patterns(
        include: Vec<String>,
        exclude: Vec<String>,
    ) -> Result<Self, FileSystemError> {
        let mut filter: GlobFilter = Self {
            include,
            exclude,
            include_set: None,
            exclude_set: None,
        };
        filter.compile()?;
        Ok(filter)
    }

    /// Check if a path matches the filter criteria.
    ///
    /// # Arguments
    /// * `path` - Normalized POSIX-style path to check
    ///
    /// # Returns
    /// `true` if the path should be included, `false` otherwise.
    pub fn matches(&self, path: &str) -> bool {
        // If include patterns are specified, path must match at least one
        let included: bool = match &self.include_set {
            Some(set) => set.is_match(path),
            None => true, // No include patterns = include all
        };

        // Path must not match any exclude pattern
        let excluded: bool = match &self.exclude_set {
            Some(set) => set.is_match(path),
            None => false,
        };

        included && !excluded
    }

    /// Check if the filter has any patterns.
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Get the include patterns.
    pub fn include_patterns(&self) -> &[String] {
        &self.include
    }

    /// Get the exclude patterns.
    pub fn exclude_patterns(&self) -> &[String] {
        &self.exclude
    }

    /// Compile the glob patterns into GlobSets.
    fn compile(&mut self) -> Result<(), FileSystemError> {
        if !self.include.is_empty() {
            let mut builder: GlobSetBuilder = GlobSetBuilder::new();
            for pattern in &self.include {
                let glob: Glob =
                    Glob::new(pattern).map_err(|e| FileSystemError::InvalidGlobPattern {
                        pattern: pattern.clone(),
                        reason: e.to_string(),
                    })?;
                builder.add(glob);
            }
            self.include_set =
                Some(
                    builder
                        .build()
                        .map_err(|e| FileSystemError::InvalidGlobPattern {
                            pattern: self.include.join(", "),
                            reason: e.to_string(),
                        })?,
                );
        }

        if !self.exclude.is_empty() {
            let mut builder: GlobSetBuilder = GlobSetBuilder::new();
            for pattern in &self.exclude {
                let glob: Glob =
                    Glob::new(pattern).map_err(|e| FileSystemError::InvalidGlobPattern {
                        pattern: pattern.clone(),
                        reason: e.to_string(),
                    })?;
                builder.add(glob);
            }
            self.exclude_set =
                Some(
                    builder
                        .build()
                        .map_err(|e| FileSystemError::InvalidGlobPattern {
                            pattern: self.exclude.join(", "),
                            reason: e.to_string(),
                        })?,
                );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_filter_matches_all() {
        let filter: GlobFilter = GlobFilter::new();
        assert!(filter.matches("any/path/file.txt"));
        assert!(filter.matches("file.rs"));
        assert!(filter.matches("deep/nested/path/file.py"));
    }

    #[test]
    fn test_include_only() {
        let filter: GlobFilter = GlobFilter::include(vec!["*.txt".to_string()]).unwrap();
        assert!(filter.matches("file.txt"));
        assert!(!filter.matches("file.rs"));
        // Note: *.txt matches any path ending in .txt in globset
        assert!(filter.matches("path/file.txt"));
    }

    #[test]
    fn test_include_recursive() {
        let filter: GlobFilter = GlobFilter::include(vec!["**/*.txt".to_string()]).unwrap();
        assert!(filter.matches("file.txt"));
        assert!(filter.matches("path/file.txt"));
        assert!(filter.matches("deep/nested/file.txt"));
        assert!(!filter.matches("file.rs"));
    }

    #[test]
    fn test_exclude_only() {
        let filter: GlobFilter = GlobFilter::exclude(vec!["*.tmp".to_string()]).unwrap();
        assert!(filter.matches("file.txt"));
        assert!(!filter.matches("file.tmp"));
    }

    #[test]
    fn test_exclude_recursive() {
        let filter: GlobFilter =
            GlobFilter::exclude(vec!["**/__pycache__/**".to_string()]).unwrap();
        assert!(filter.matches("src/main.py"));
        assert!(!filter.matches("src/__pycache__/main.pyc"));
        assert!(!filter.matches("__pycache__/file.pyc"));
    }

    #[test]
    fn test_include_and_exclude() {
        let filter: GlobFilter = GlobFilter::with_patterns(
            vec!["**/*.py".to_string()],
            vec!["**/test_*.py".to_string()],
        )
        .unwrap();
        assert!(filter.matches("src/main.py"));
        assert!(filter.matches("lib/utils.py"));
        assert!(!filter.matches("tests/test_main.py"));
        assert!(!filter.matches("src/test_utils.py"));
        assert!(!filter.matches("file.txt")); // Not .py
    }

    #[test]
    fn test_multiple_include_patterns() {
        let filter: GlobFilter =
            GlobFilter::include(vec!["**/*.py".to_string(), "**/*.rs".to_string()]).unwrap();
        assert!(filter.matches("src/main.py"));
        assert!(filter.matches("src/lib.rs"));
        assert!(!filter.matches("src/file.txt"));
    }

    #[test]
    fn test_invalid_pattern() {
        let result: Result<GlobFilter, FileSystemError> =
            GlobFilter::include(vec!["[invalid".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_brace_expansion() {
        let filter: GlobFilter =
            GlobFilter::include(vec!["**/*.{png,jpg,jpeg}".to_string()]).unwrap();
        assert!(filter.matches("textures/wood.png"));
        assert!(filter.matches("images/photo.jpg"));
        assert!(filter.matches("assets/image.jpeg"));
        assert!(!filter.matches("file.gif"));
    }

    // ==================== Additional tests from Python library ====================

    #[test]
    fn test_include_subdir_pattern() {
        let filter: GlobFilter = GlobFilter::include(vec!["nested/**".to_string()]).unwrap();
        assert!(filter.matches("nested/file.txt"));
        assert!(filter.matches("nested/deep/file.txt"));
        assert!(!filter.matches("other/file.txt"));
        assert!(!filter.matches("file.txt"));
    }

    #[test]
    fn test_exclude_subdir_pattern() {
        let filter: GlobFilter = GlobFilter::exclude(vec!["nested/**".to_string()]).unwrap();
        assert!(filter.matches("file.txt"));
        assert!(filter.matches("other/file.txt"));
        assert!(!filter.matches("nested/file.txt"));
        assert!(!filter.matches("nested/deep/file.txt"));
    }

    #[test]
    fn test_include_specific_filename_pattern() {
        let filter: GlobFilter = GlobFilter::include(vec![
            "*include.txt".to_string(),
            "*/*include.txt".to_string(),
        ])
        .unwrap();
        assert!(filter.matches("include.txt"));
        assert!(filter.matches("nested/nested_include.txt"));
        assert!(!filter.matches("exclude.txt"));
        assert!(!filter.matches("nested/nested_exclude.txt"));
    }

    #[test]
    fn test_exclude_specific_filename_pattern() {
        let filter: GlobFilter = GlobFilter::exclude(vec![
            "*exclude.txt".to_string(),
            "*/*exclude.txt".to_string(),
        ])
        .unwrap();
        assert!(filter.matches("include.txt"));
        assert!(filter.matches("nested/nested_include.txt"));
        assert!(!filter.matches("exclude.txt"));
        assert!(!filter.matches("nested/nested_exclude.txt"));
    }

    #[test]
    fn test_nonexistent_pattern_matches_nothing() {
        let filter: GlobFilter = GlobFilter::include(vec!["nonexistent/**".to_string()]).unwrap();
        assert!(!filter.matches("file.txt"));
        assert!(!filter.matches("other/file.txt"));
        assert!(!filter.matches("nested/file.txt"));
    }

    #[test]
    fn test_exclude_nonexistent_pattern_matches_all() {
        let filter: GlobFilter = GlobFilter::exclude(vec!["nonexistent/**".to_string()]).unwrap();
        assert!(filter.matches("file.txt"));
        assert!(filter.matches("other/file.txt"));
        assert!(filter.matches("nested/file.txt"));
    }

    #[test]
    fn test_is_empty() {
        let empty_filter: GlobFilter = GlobFilter::new();
        assert!(empty_filter.is_empty());

        let include_filter: GlobFilter = GlobFilter::include(vec!["*.txt".to_string()]).unwrap();
        assert!(!include_filter.is_empty());

        let exclude_filter: GlobFilter = GlobFilter::exclude(vec!["*.tmp".to_string()]).unwrap();
        assert!(!exclude_filter.is_empty());
    }

    #[test]
    fn test_pattern_accessors() {
        let filter: GlobFilter =
            GlobFilter::with_patterns(vec!["*.py".to_string()], vec!["*.tmp".to_string()]).unwrap();
        assert_eq!(filter.include_patterns(), &["*.py".to_string()]);
        assert_eq!(filter.exclude_patterns(), &["*.tmp".to_string()]);
    }

    #[test]
    fn test_hidden_files() {
        let filter: GlobFilter = GlobFilter::exclude(vec!["**/.*".to_string()]).unwrap();
        assert!(filter.matches("file.txt"));
        assert!(!filter.matches(".hidden"));
        assert!(!filter.matches("dir/.hidden"));
        assert!(!filter.matches(".git/config"));
    }

    #[test]
    fn test_node_modules_exclusion() {
        let filter: GlobFilter =
            GlobFilter::exclude(vec!["**/node_modules/**".to_string()]).unwrap();
        assert!(filter.matches("src/index.js"));
        assert!(!filter.matches("node_modules/lodash/index.js"));
        assert!(!filter.matches("packages/app/node_modules/react/index.js"));
    }

    // ==================== Glob escaping tests ====================

    #[test]
    fn test_escape_glob_brackets() {
        assert_eq!(escape_glob("file[1].txt"), r"file\[1\].txt");
        assert_eq!(escape_glob("[test]"), r"\[test\]");
    }

    #[test]
    fn test_escape_glob_asterisk() {
        assert_eq!(escape_glob("test*.log"), r"test\*.log");
        assert_eq!(escape_glob("**/*.txt"), r"\*\*/\*.txt");
    }

    #[test]
    fn test_escape_glob_question_mark() {
        assert_eq!(escape_glob("file?.txt"), r"file\?.txt");
    }

    #[test]
    fn test_escape_glob_braces() {
        assert_eq!(escape_glob("file{1,2}.txt"), r"file\{1,2\}.txt");
    }

    #[test]
    fn test_escape_glob_exclamation() {
        assert_eq!(escape_glob("!important.txt"), r"\!important.txt");
    }

    #[test]
    fn test_escape_glob_multiple_special_chars() {
        assert_eq!(escape_glob("test[*?{!}].txt"), r"test\[\*\?\{\!\}\].txt");
    }

    #[test]
    fn test_escape_glob_no_special_chars() {
        assert_eq!(escape_glob("normal_file.txt"), "normal_file.txt");
    }

    #[test]
    fn test_escaped_pattern_matches_literal() {
        // Create a pattern with escaped brackets to match literal brackets
        let literal_path: String = escape_glob("file[1].txt");
        let filter: GlobFilter = GlobFilter::include(vec![literal_path]).unwrap();

        // Should match the literal path with brackets
        assert!(filter.matches("file[1].txt"));

        // Should NOT match file1.txt (which would match unescaped pattern)
        assert!(!filter.matches("file1.txt"));
    }

    #[test]
    fn test_escaped_pattern_with_wildcard() {
        // Escape the literal part, then add a wildcard
        let pattern: String = format!("{}/**", escape_glob("output[v1]"));
        let filter: GlobFilter = GlobFilter::include(vec![pattern]).unwrap();

        // Should match files under the literal directory
        assert!(filter.matches("output[v1]/file.txt"));
        assert!(filter.matches("output[v1]/subdir/file.txt"));

        // Should NOT match similar but different paths
        assert!(!filter.matches("outputv1/file.txt"));
        assert!(!filter.matches("output[v2]/file.txt"));
    }

    #[test]
    fn test_unescaped_brackets_as_character_class() {
        // Without escaping, [1] is a character class matching '1'
        let filter: GlobFilter = GlobFilter::include(vec!["file[1].txt".to_string()]).unwrap();

        // Should match file1.txt (character class)
        assert!(filter.matches("file1.txt"));

        // Should NOT match the literal "file[1].txt"
        assert!(!filter.matches("file[1].txt"));
    }

    #[test]
    fn test_real_world_scenario_output_directories() {
        // Simulating the Python code pattern:
        // include=[glob.escape(subdir) + "/**" for subdir in output_relative_directories]
        let output_dirs: Vec<&str> = vec!["renders[final]", "cache*temp", "logs?debug"];

        let patterns: Vec<String> = output_dirs
            .iter()
            .map(|dir| format!("{}/**", escape_glob(dir)))
            .collect();

        let filter: GlobFilter = GlobFilter::include(patterns).unwrap();

        // Should match files in the literal directories
        assert!(filter.matches("renders[final]/image.png"));
        assert!(filter.matches("cache*temp/data.bin"));
        assert!(filter.matches("logs?debug/output.log"));

        // Should NOT match similar but different paths
        assert!(!filter.matches("rendersfinal/image.png"));
        assert!(!filter.matches("cache_temp/data.bin"));
        assert!(!filter.matches("logs_debug/output.log"));
    }
}
