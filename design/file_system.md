# Rusty Attachments: File System Module Design

**Status: ✅ IMPLEMENTED** - See `crates/filesystem/`

## Overview

This document outlines the design for file system operations that scan directories and create manifests. The module provides two core operations:

1. **Snapshot** - Scan a directory and create a manifest capturing its current state
2. **Diff** - Compare a directory against an existing manifest to detect changes

Both operations support include/exclude glob patterns for filtering which files to process.

---

## Goals

1. **Performance** - Efficient parallel directory walking and hashing
2. **Flexibility** - Configurable glob patterns for include/exclude filtering
3. **Composability** - Operations can be chained (snapshot → diff → upload)
4. **Cross-platform** - Works on Windows, macOS, and Linux with proper path handling
5. **Security** - Prevent symlink attacks and path traversal vulnerabilities

---

## Dependencies

This module depends on the `common` crate for shared utilities:

```rust
use rusty_attachments_common::{
    // Constants
    CHUNK_SIZE_V2, DEFAULT_STAT_CACHE_CAPACITY,
    // Path utilities
    normalize_for_manifest, lexical_normalize, to_posix_path, 
    from_posix_path, is_within_root, to_long_path,
    // Hash utilities
    hash_file, hash_bytes, Xxh3Hasher,
    // Progress callback
    ProgressCallback,
    // Error types
    PathError,
};
```

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           Application Layer                                  │
│                    (CLI, Python bindings, WASM)                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                         File System Module                                   │
│              (Snapshot, Diff, GlobFilter, DirectoryWalker)                  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           Model Module                                       │
│                    (Manifest, ManifestFilePath, etc.)                       │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── model/                    # Existing - manifest models
│   ├── storage/                  # Existing - S3 operations
│   │
│   └── filesystem/               # NEW - File system operations
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── glob.rs           # Glob pattern matching
│           ├── walker.rs         # Directory walking
│           ├── snapshot.rs       # Snapshot operation
│           ├── diff.rs           # Diff operation
│           ├── hash.rs           # File hashing utilities
│           ├── symlink.rs        # Symlink security validation
│           ├── stat_cache.rs     # File stat caching
│           ├── path_utils.rs     # Path normalization utilities
│           └── error.rs          # Error types
```

---

## Core Data Structures

### Glob Filter Configuration

```rust
/// Configuration for filtering files during directory operations
#[derive(Debug, Clone, Default)]
pub struct GlobFilter {
    /// Patterns for files to include (empty = include all)
    /// Examples: ["*.blend", "textures/**/*.png", "scripts/*.py"]
    pub include: Vec<String>,
    
    /// Patterns for files to exclude
    /// Examples: ["*.tmp", "*.bak", "__pycache__/**", ".git/**"]
    pub exclude: Vec<String>,
}

impl GlobFilter {
    /// Create a new filter with no patterns (matches everything)
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Create a filter with include patterns only
    pub fn include(patterns: Vec<String>) -> Self {
        Self { include: patterns, exclude: vec![] }
    }
    
    /// Create a filter with exclude patterns only
    pub fn exclude(patterns: Vec<String>) -> Self {
        Self { include: vec![], exclude: patterns }
    }
    
    /// Create a filter with both include and exclude patterns
    pub fn with_patterns(include: Vec<String>, exclude: Vec<String>) -> Self {
        Self { include, exclude }
    }
    
    /// Check if a path matches the filter criteria
    /// Returns true if the path should be included
    pub fn matches(&self, path: &str) -> bool;
}
```

### Snapshot Options

```rust
/// Options for creating a directory snapshot
#[derive(Debug, Clone)]
pub struct SnapshotOptions {
    /// Root directory to snapshot.
    /// 
    /// When `input_files` is None, the scanner recursively walks this directory
    /// to discover all files (subject to `filter` patterns).
    /// 
    /// When `input_files` is Some, this root is used as the base for computing
    /// relative paths in the manifest.
    pub root: PathBuf,
    
    /// Explicit list of input files to include in the snapshot.
    /// 
    /// When Some, only these files are included (no directory walking).
    /// When None, the scanner walks `root` recursively to discover files.
    /// 
    /// This is useful when:
    /// - Input files are already known (e.g., from asset_references.yaml)
    /// - Files span multiple directories but share a common root
    /// - You want to avoid the overhead of directory walking
    /// 
    /// All paths must be absolute and within `root`.
    pub input_files: Option<Vec<PathBuf>>,
    
    /// Manifest version to create
    pub version: ManifestVersion,
    
    /// Glob filter for include/exclude patterns.
    /// Applied during directory walking (when `input_files` is None).
    pub filter: GlobFilter,
    
    /// Hash algorithm to use
    pub hash_algorithm: HashAlgorithm,
    
    /// Whether to follow symlinks (false = capture as symlinks)
    pub follow_symlinks: bool,
    
    /// Whether to include empty directories (v2025 only).
    /// Only applies when walking directories (`input_files` is None).
    pub include_empty_dirs: bool,
    
    /// Optional hash cache for incremental hashing
    pub hash_cache: Option<PathBuf>,
    
    /// Number of parallel hashing threads (0 = auto-detect)
    pub parallelism: usize,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            input_files: None, // Walk directory by default
            version: ManifestVersion::V2025_12_04_beta,
            filter: GlobFilter::default(),
            hash_algorithm: HashAlgorithm::Xxh128,
            follow_symlinks: false,
            include_empty_dirs: true,
            hash_cache: None,
            parallelism: 0, // Auto-detect
        }
    }
}
```

---

## Input Path Utilities

When processing job bundle inputs, paths may be either files or directories. These utilities help expand and validate input paths before creating manifests.

### Expand Input Paths

```rust
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

/// Expand a list of input paths to individual files.
/// 
/// Directories are recursively walked to discover all files within them.
/// Files are returned as-is after validation.
/// 
/// This is a pure expansion operation - no hashing or manifest creation.
/// Use the result with `SnapshotOptions::input_files` for manifest creation.
/// 
/// # Arguments
/// * `input_paths` - Mix of file and directory paths to expand
/// * `filter` - Optional glob filter to apply during directory walking
/// * `allow_missing` - If true, missing paths are collected in `missing` instead of error
/// 
/// # Returns
/// `ExpandedInputPaths` containing all discovered files
/// 
/// # Errors
/// - `FileSystemError::PathNotFound` if a path doesn't exist (when allow_missing=false)
/// - `FileSystemError::IoError` if directory walking fails
/// 
/// # Example
/// ```rust
/// use rusty_attachments_filesystem::{expand_input_paths, GlobFilter};
/// 
/// let inputs = vec![
///     PathBuf::from("/project/scene.blend"),      // File
///     PathBuf::from("/project/textures"),          // Directory
///     PathBuf::from("/project/scripts/render.py"), // File
/// ];
/// 
/// let filter = GlobFilter::exclude(vec!["**/*.tmp".into()]);
/// let expanded = expand_input_paths(&inputs, Some(&filter), false)?;
/// 
/// // expanded.files contains:
/// // - /project/scene.blend
/// // - /project/textures/wood.png
/// // - /project/textures/metal.png
/// // - /project/scripts/render.py
/// 
/// // Use with snapshot
/// let options = SnapshotOptions {
///     root: PathBuf::from("/project"),
///     input_files: Some(expanded.files),
///     ..Default::default()
/// };
/// ```
pub fn expand_input_paths(
    input_paths: &[PathBuf],
    filter: Option<&GlobFilter>,
    allow_missing: bool,
) -> Result<ExpandedInputPaths, FileSystemError> {
    let mut result = ExpandedInputPaths::default();
    
    for path in input_paths {
        let abs_path = to_absolute(path)?;
        
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
            let files = walk_directory_for_files(&abs_path, filter)?;
            result.total_size += files.iter().map(|f| f.size).sum::<u64>();
            result.files.extend(files.into_iter().map(|f| f.path));
            result.expanded_directories.push(abs_path);
        } else {
            // Single file
            let size = abs_path.metadata()
                .map(|m| m.len())
                .unwrap_or(0);
            result.total_size += size;
            result.files.push(abs_path);
        }
    }
    
    Ok(result)
}

/// Internal: Walk a directory and return all files (not directories).
fn walk_directory_for_files(
    dir: &Path,
    filter: Option<&GlobFilter>,
) -> Result<Vec<FileInfo>, FileSystemError> {
    let mut files = Vec::new();
    
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        
        // Skip directories
        if path.is_dir() {
            continue;
        }
        
        // Apply glob filter if provided
        if let Some(f) = filter {
            let relative = path.strip_prefix(dir)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if !f.matches(&relative) {
                continue;
            }
        }
        
        let metadata = path.metadata()
            .map_err(|e| FileSystemError::IoError {
                path: path.display().to_string(),
                source: e,
            })?;
        
        files.push(FileInfo {
            path: path.to_path_buf(),
            size: metadata.len(),
        });
    }
    
    Ok(files)
}

/// Simple file info for expansion results.
#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    size: u64,
}
```

### Validate Input Paths

```rust
/// Result of validating input paths.
#[derive(Debug, Clone, Default)]
pub struct ValidatedInputPaths {
    /// Valid file paths that exist.
    pub valid_files: Vec<PathBuf>,
    /// Paths that don't exist.
    pub missing: Vec<PathBuf>,
    /// Paths that are directories (should use expand_input_paths first).
    pub directories: Vec<PathBuf>,
}

/// Validate a list of paths, categorizing them by type and existence.
/// 
/// This is a lightweight check that doesn't walk directories.
/// Use before `expand_input_paths` to detect issues early, or use
/// `expand_input_paths` directly which handles both validation and expansion.
/// 
/// # Arguments
/// * `paths` - Paths to validate
/// 
/// # Returns
/// `ValidatedInputPaths` with paths categorized by type
/// 
/// # Example
/// ```rust
/// let validated = validate_input_paths(&paths);
/// 
/// if !validated.missing.is_empty() {
///     eprintln!("Warning: {} paths not found", validated.missing.len());
/// }
/// 
/// if !validated.directories.is_empty() {
///     // Need to expand directories first
///     let expanded = expand_input_paths(&validated.directories, None, false)?;
///     validated.valid_files.extend(expanded.files);
/// }
/// ```
pub fn validate_input_paths(paths: &[PathBuf]) -> ValidatedInputPaths {
    let mut result = ValidatedInputPaths::default();
    
    for path in paths {
        let abs_path = to_absolute(path).unwrap_or_else(|_| path.clone());
        
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
```

### Diff Options

```rust
/// Options for diffing a directory against a manifest
#[derive(Debug, Clone)]
pub struct DiffOptions {
    /// Root directory to compare
    pub root: PathBuf,
    
    /// Glob filter for include/exclude patterns
    /// Applied to BOTH the manifest entries and current directory
    pub filter: GlobFilter,
    
    /// Comparison mode
    pub mode: DiffMode,
    
    /// Optional hash cache for incremental hashing
    pub hash_cache: Option<PathBuf>,
    
    /// Number of parallel hashing threads (0 = auto-detect)
    pub parallelism: usize,
}

/// How to compare files for changes
#[derive(Debug, Clone, Copy, Default)]
pub enum DiffMode {
    /// Compare by mtime and size only (fast, may miss content-only changes)
    #[default]
    Fast,
    
    /// Compare by hash (slower, definitive)
    Hash,
}
```

### Diff Result

```rust
/// Result of comparing a directory against a manifest
#[derive(Debug, Clone)]
pub struct DiffResult {
    /// Files that are new (not in parent manifest)
    pub added: Vec<FileEntry>,
    
    /// Files that were modified (different mtime/size or hash)
    pub modified: Vec<FileEntry>,
    
    /// Files that were deleted (in parent but not on disk)
    pub deleted: Vec<String>,
    
    /// Directories that are new
    pub added_dirs: Vec<String>,
    
    /// Directories that were deleted
    pub deleted_dirs: Vec<String>,
    
    /// Files that are unchanged
    pub unchanged: Vec<String>,
    
    /// Statistics about the diff operation
    pub stats: DiffStats,
}

/// Statistics from a diff operation
#[derive(Debug, Clone, Default)]
pub struct DiffStats {
    /// Total files scanned on disk
    pub files_scanned: u64,
    
    /// Total directories scanned
    pub dirs_scanned: u64,
    
    /// Files that were hashed (in Hash mode or for modified detection)
    pub files_hashed: u64,
    
    /// Bytes hashed
    pub bytes_hashed: u64,
    
    /// Time spent walking directories
    pub walk_duration: Duration,
    
    /// Time spent hashing files
    pub hash_duration: Duration,
}

/// Entry representing a file found during scanning
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative path from root
    pub path: String,
    
    /// File size in bytes
    pub size: u64,
    
    /// Modification time (microseconds since epoch)
    pub mtime: i64,
    
    /// File hash (if computed)
    pub hash: Option<String>,
    
    /// Chunk hashes for large files (if computed, v2025 only)
    pub chunkhashes: Option<Vec<String>>,
    
    /// Whether file has execute permission
    pub runnable: bool,
    
    /// Symlink target (if this is a symlink)
    pub symlink_target: Option<String>,
}
```

### Progress Reporting

Progress reporting uses the generic `ProgressCallback<T>` trait from the `common` crate:

```rust
use rusty_attachments_common::ProgressCallback;

/// Progress updates during file system operations
#[derive(Debug, Clone)]
pub struct ScanProgress {
    /// Current operation phase
    pub phase: ScanPhase,
    
    /// Current file being processed
    pub current_path: Option<String>,
    
    /// Files processed so far
    pub files_processed: u64,
    
    /// Total files found (if known)
    pub total_files: Option<u64>,
    
    /// Bytes processed (for hashing phase)
    pub bytes_processed: u64,
    
    /// Total bytes to process (if known)
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub enum ScanPhase {
    /// Walking directory tree
    Walking,
    /// Filtering entries
    Filtering,
    /// Hashing file contents
    Hashing,
    /// Building manifest
    Building,
    /// Complete
    Complete,
}

/// Type alias for scan progress callbacks.
/// Uses the generic ProgressCallback<T> from common crate.
pub type ScanProgressCallback = dyn ProgressCallback<ScanProgress>;
```

---

## Core Traits

### Snapshot Trait

```rust
/// Interface for creating directory snapshots
pub trait Snapshot {
    /// Create a snapshot manifest from a directory
    ///
    /// # Arguments
    /// * `options` - Snapshot configuration
    /// * `progress` - Optional progress callback
    ///
    /// # Returns
    /// A manifest representing the directory state
    ///
    /// # Errors
    /// Returns error if directory cannot be read or hashing fails
    fn snapshot(
        &self,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<Manifest, FileSystemError>;
    
    /// Create a snapshot without computing hashes
    /// Useful for fast directory scanning before selective hashing
    fn snapshot_structure(
        &self,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<Manifest, FileSystemError>;
}
```

### Diff Trait

```rust
/// Interface for comparing directories against manifests
pub trait Diff {
    /// Compare a directory against an existing manifest
    ///
    /// # Arguments
    /// * `manifest` - The parent manifest to compare against
    /// * `options` - Diff configuration
    /// * `progress` - Optional progress callback
    ///
    /// # Returns
    /// DiffResult containing added, modified, deleted, and unchanged entries
    ///
    /// # Errors
    /// Returns error if directory cannot be read or comparison fails
    fn diff(
        &self,
        manifest: &Manifest,
        options: &DiffOptions,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<DiffResult, FileSystemError>;
    
    /// Create a diff manifest from the comparison result
    /// 
    /// # Arguments
    /// * `parent_manifest` - The original parent manifest
    /// * `parent_manifest_bytes` - Raw bytes of parent (for computing parentManifestHash)
    /// * `diff_result` - Result from diff() operation
    /// * `options` - Options used for the diff
    ///
    /// # Returns
    /// A diff manifest with parentManifestHash, changes, and deletion markers
    fn create_diff_manifest(
        &self,
        parent_manifest: &Manifest,
        parent_manifest_bytes: &[u8],
        diff_result: &DiffResult,
        options: &DiffOptions,
    ) -> Result<Manifest, FileSystemError>;
}
```

---

## Implementation

### FileSystemScanner

The main implementation struct that provides both Snapshot and Diff capabilities:

```rust
/// File system scanner for snapshot and diff operations
pub struct FileSystemScanner {
    /// Thread pool for parallel operations
    thread_pool: ThreadPool,
}

impl FileSystemScanner {
    /// Create a new scanner with default parallelism
    pub fn new() -> Self;
    
    /// Create a scanner with specific parallelism
    pub fn with_parallelism(num_threads: usize) -> Self;
}

impl Snapshot for FileSystemScanner {
    fn snapshot(
        &self,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<Manifest, FileSystemError> {
        // 1. Get file entries - either from explicit list or directory walk
        let entries = if let Some(input_files) = &options.input_files {
            // Use explicit file list - skip directory walking
            self.entries_from_file_list(input_files, &options.root)?
        } else {
            // Walk directory tree to discover files
            self.walk_directory(&options.root, &options.filter, progress)?
        };
        
        // 2. Separate files, symlinks, directories
        let (files, symlinks, dirs) = self.categorize_entries(entries, options)?;
        
        // 3. Hash files (parallel)
        let hashed_files = self.hash_files(files, options, progress)?;
        
        // 4. Build manifest
        self.build_manifest(hashed_files, symlinks, dirs, options)
    }
    
    fn snapshot_structure(
        &self,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<Manifest, FileSystemError> {
        // Same as snapshot but skip hashing step
        // Returns manifest with hash="" for all files
    }
}

impl Diff for FileSystemScanner {
    fn diff(
        &self,
        manifest: &Manifest,
        options: &DiffOptions,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<DiffResult, FileSystemError> {
        // 1. Walk current directory
        let current_entries = self.walk_directory(&options.root, &options.filter, progress)?;
        
        // 2. Filter manifest entries with same patterns
        let filtered_manifest = self.filter_manifest(manifest, &options.filter)?;
        
        // 3. Build lookup maps
        let manifest_files = self.build_manifest_lookup(&filtered_manifest);
        let current_files = self.build_current_lookup(&current_entries);
        
        // 4. Compute differences
        let added = self.find_added(&manifest_files, &current_files);
        let deleted = self.find_deleted(&manifest_files, &current_files);
        let potentially_modified = self.find_common(&manifest_files, &current_files);
        
        // 5. Determine modified vs unchanged
        let (modified, unchanged) = match options.mode {
            DiffMode::Fast => self.compare_by_metadata(potentially_modified, &manifest_files),
            DiffMode::Hash => self.compare_by_hash(potentially_modified, &manifest_files, options, progress)?,
        };
        
        // 6. Hash added and modified files if not already hashed
        let added = self.ensure_hashed(added, options, progress)?;
        let modified = self.ensure_hashed(modified, options, progress)?;
        
        Ok(DiffResult { added, modified, deleted, unchanged, .. })
    }
    
    fn create_diff_manifest(
        &self,
        parent_manifest: &Manifest,
        parent_manifest_bytes: &[u8],
        diff_result: &DiffResult,
        options: &DiffOptions,
    ) -> Result<Manifest, FileSystemError> {
        // 1. Compute parent manifest hash
        let parent_hash = hash_bytes(parent_manifest_bytes, HashAlgorithm::Xxh128);
        
        // 2. Build file entries from added + modified
        let mut file_entries = Vec::new();
        for entry in &diff_result.added {
            file_entries.push(self.file_entry_to_manifest_path(entry)?);
        }
        for entry in &diff_result.modified {
            file_entries.push(self.file_entry_to_manifest_path(entry)?);
        }
        
        // 3. Add deletion markers
        for path in &diff_result.deleted {
            file_entries.push(ManifestFilePath::deleted(path.clone()));
        }
        
        // 4. Build directory entries with deletion markers
        let dir_entries = self.build_dir_entries_with_deletions(
            &diff_result.added_dirs,
            &diff_result.deleted_dirs,
        );
        
        // 5. Create diff manifest
        Ok(Manifest::V2025_12_04_beta(AssetManifest {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2025_12_04_beta,
            dirs: dir_entries,
            paths: file_entries,
            total_size: self.compute_total_size(&file_entries),
            parent_manifest_hash: Some(parent_hash),
        }))
    }
}
```

---

## Glob Pattern Matching

### Supported Patterns

| Pattern | Description | Example |
|---------|-------------|---------|
| `*` | Match any characters except `/` | `*.txt` matches `file.txt` |
| `**` | Match any characters including `/` | `**/*.txt` matches `a/b/c.txt` |
| `?` | Match single character | `file?.txt` matches `file1.txt` |
| `[abc]` | Match character class | `file[123].txt` matches `file1.txt` |
| `[!abc]` | Negated character class | `file[!0-9].txt` matches `fileA.txt` |
| `{a,b}` | Alternation | `*.{png,jpg}` matches both extensions |

### Pattern Matching Rules

```rust
impl GlobFilter {
    /// Check if a path matches the filter criteria
    pub fn matches(&self, path: &str) -> bool {
        // If include patterns are specified, path must match at least one
        let included = if self.include.is_empty() {
            true
        } else {
            self.include.iter().any(|pattern| glob_match(pattern, path))
        };
        
        // Path must not match any exclude pattern
        let excluded = self.exclude.iter().any(|pattern| glob_match(pattern, path));
        
        included && !excluded
    }
}
```

### Example Filter Configurations

```rust
// Include only Blender files and textures
let filter = GlobFilter::with_patterns(
    vec!["**/*.blend".into(), "textures/**".into()],
    vec![],
);

// Exclude temporary and backup files
let filter = GlobFilter::exclude(vec![
    "**/*.tmp".into(),
    "**/*.bak".into(),
    "**/__pycache__/**".into(),
    "**/.git/**".into(),
]);

// Complex filter: include source files, exclude tests
let filter = GlobFilter::with_patterns(
    vec!["src/**/*.rs".into(), "src/**/*.py".into()],
    vec!["**/test_*.py".into(), "**/tests/**".into()],
);
```

---

## Usage Examples

### Basic Snapshot

```rust
use rusty_attachments_filesystem::{FileSystemScanner, SnapshotOptions, Snapshot};

let scanner = FileSystemScanner::new();

let options = SnapshotOptions {
    root: PathBuf::from("/project/assets"),
    version: ManifestVersion::V2025_12_04_beta,
    ..Default::default()
};

let manifest = scanner.snapshot(&options, None)?;
```

### Snapshot with Filtering

```rust
let options = SnapshotOptions {
    root: PathBuf::from("/project"),
    filter: GlobFilter::with_patterns(
        vec!["**/*.blend".into(), "**/*.png".into()],
        vec!["**/backup/**".into()],
    ),
    ..Default::default()
};

let manifest = scanner.snapshot(&options, Some(&progress_reporter))?;
```

### Fast Diff (by mtime/size)

```rust
use rusty_attachments_filesystem::{FileSystemScanner, DiffOptions, DiffMode, Diff};

let scanner = FileSystemScanner::new();

// Load parent manifest
let parent_bytes = std::fs::read("previous.manifest")?;
let parent_manifest = Manifest::decode(&parent_bytes)?;

let options = DiffOptions {
    root: PathBuf::from("/project/assets"),
    filter: GlobFilter::default(),
    mode: DiffMode::Fast,
    ..Default::default()
};

let diff_result = scanner.diff(&parent_manifest, &options, None)?;

println!("Added: {} files", diff_result.added.len());
println!("Modified: {} files", diff_result.modified.len());
println!("Deleted: {} files", diff_result.deleted.len());
```

### Hash-based Diff (definitive)

```rust
let options = DiffOptions {
    root: PathBuf::from("/project/assets"),
    mode: DiffMode::Hash,  // Compare by content hash
    ..Default::default()
};

let diff_result = scanner.diff(&parent_manifest, &options, None)?;
```

### Create Diff Manifest

```rust
// After computing diff, create a diff manifest
let diff_manifest = scanner.create_diff_manifest(
    &parent_manifest,
    &parent_bytes,
    &diff_result,
    &options,
)?;

// The diff manifest has:
// - parentManifestHash pointing to parent
// - Added/modified files with full content info
// - Deleted files with deleted=true markers
```

### With Progress Reporting

```rust
struct MyProgressReporter;

impl ProgressCallback for MyProgressReporter {
    fn on_progress(&self, progress: &ScanProgress) -> bool {
        match progress.phase {
            ScanPhase::Walking => println!("Scanning: {:?}", progress.current_path),
            ScanPhase::Hashing => {
                if let (Some(done), Some(total)) = (progress.files_processed, progress.total_files) {
                    println!("Hashing: {}/{} files", done, total);
                }
            }
            ScanPhase::Complete => println!("Done!"),
            _ => {}
        }
        true // Continue operation
    }
}

let manifest = scanner.snapshot(&options, Some(&MyProgressReporter))?;
```

---

## Error Types

File system errors extend the common `PathError` with filesystem-specific variants:

```rust
use rusty_attachments_common::PathError;

#[derive(Debug, thiserror::Error)]
pub enum FileSystemError {
    /// Path-related errors (from common crate)
    #[error(transparent)]
    Path(#[from] PathError),
    
    #[error("Directory not found: {path}")]
    DirectoryNotFound { path: String },
    
    #[error("Permission denied: {path}")]
    PermissionDenied { path: String },
    
    #[error("Invalid glob pattern: {pattern}: {reason}")]
    InvalidGlobPattern { pattern: String, reason: String },
    
    #[error("Symlink target escapes root: {symlink} -> {target}")]
    SymlinkEscapesRoot { symlink: String, target: String },
    
    #[error("Symlink has absolute target: {symlink} -> {target}")]
    SymlinkAbsoluteTarget { symlink: String, target: String },
    
    #[error("Symlink not allowed when opening file: {path}")]
    SymlinkNotAllowed { path: String },
    
    #[error("IO error at {path}: {source}")]
    IoError { path: String, source: std::io::Error },
    
    #[error("Hash error: {message}")]
    HashError { message: String },
    
    #[error("Operation cancelled")]
    Cancelled,
    
    #[error("Manifest error: {message}")]
    ManifestError { message: String },
    
    #[error("Group not found: {group}")]
    GroupNotFound { group: String },
    
    #[error("Too many file conflicts for: {path}")]
    TooManyConflicts { path: String },
}
```

---

## Implementation Notes

### Directory Walking Strategy

1. Use `walkdir` crate for efficient recursive directory traversal
2. Apply glob filters during walk to skip entire subtrees when possible
3. Collect symlinks separately for validation
4. Track empty directories for v2025 format

### Explicit File List Strategy

When `input_files` is provided in `SnapshotOptions`, directory walking is skipped:

```rust
impl FileSystemScanner {
    /// Create walk entries from an explicit list of files.
    /// 
    /// This is used when input files are already known (e.g., from asset_references.yaml)
    /// and directory walking would be redundant.
    /// 
    /// # Arguments
    /// * `input_files` - Absolute paths to files to include
    /// * `root` - Root directory for computing relative paths
    /// 
    /// # Returns
    /// Walk entries for each valid file
    /// 
    /// # Errors
    /// - `FileSystemError::PathOutsideRoot` if any file is not under root
    fn entries_from_file_list(
        &self,
        input_files: &[PathBuf],
        root: &Path,
    ) -> Result<Vec<WalkEntry>, FileSystemError> {
        let mut entries = Vec::with_capacity(input_files.len());
        
        for file_path in input_files {
            // Validate path is within root
            if !file_path.starts_with(root) {
                return Err(FileSystemError::PathOutsideRoot {
                    path: file_path.display().to_string(),
                    root: root.display().to_string(),
                });
            }
            
            // Skip non-existent files (they should have been filtered earlier)
            if !file_path.exists() {
                log::warn!("Skipping non-existent file: {}", file_path.display());
                continue;
            }
            
            entries.push(WalkEntry {
                path: file_path.clone(),
            });
        }
        
        Ok(entries)
    }
}
```

This approach is used by `group_asset_paths_validated()` which expands input directories to individual files before calling snapshot.

### Parallel Hashing Strategy

1. Collect all files to hash during walk phase
2. Sort by size (largest first) for better load balancing
3. Use thread pool with work-stealing for parallel hashing
4. Small files (<1MB): batch multiple files per task
5. Large files (>256MB): single file per task with chunked hashing

### Hash Cache Integration

```rust
/// Optional hash cache for incremental operations
pub trait HashCache: Send + Sync {
    /// Get cached hash for a file
    /// Returns None if not cached or cache is stale
    fn get(&self, path: &str, mtime: i64, size: u64) -> Option<String>;
    
    /// Store hash in cache
    fn put(&self, path: &str, mtime: i64, size: u64, hash: &str);
    
    /// Flush cache to persistent storage
    fn flush(&self) -> Result<(), std::io::Error>;
}
```

### Cross-Platform Considerations

- Path separators: Always use `/` in manifests, convert on Windows
- Symlinks: Use `std::os::unix::fs::symlink` on Unix, handle Windows differently
- Execute bit: Only meaningful on Unix, always `false` on Windows
- Long paths: Handle Windows MAX_PATH limitations

---

## Path Normalization

Path normalization utilities are provided by the `common` crate. See [common.md](common.md) for details.

All paths in manifests use POSIX-style forward slashes (`/`) regardless of the host OS. This ensures manifests are portable across platforms.

```rust
use rusty_attachments_common::{normalize_for_manifest, from_posix_path};

// Normalize for manifest storage
let manifest_path = normalize_for_manifest(
    Path::new("/project/assets/texture.png"),
    Path::new("/project"),
)?;
// Result: "assets/texture.png"

// Convert back for download
let local_path = from_posix_path("assets/texture.png", Path::new("/downloads"));
// Result: PathBuf("/downloads/assets/texture.png")
```

### Glob Pattern Matching

Glob patterns are always matched against normalized POSIX-style paths:

```rust
impl GlobFilter {
    /// Check if a path matches the filter criteria.
    /// The path should already be normalized to POSIX format.
    pub fn matches(&self, normalized_path: &str) -> bool {
        // Patterns use forward slashes regardless of host OS
        // e.g., "**/*.txt" works on both Windows and Unix
        
        let included = if self.include.is_empty() {
            true
        } else {
            self.include.iter().any(|pattern| glob_match(pattern, normalized_path))
        };
        
        let excluded = self.exclude.iter().any(|pattern| glob_match(pattern, normalized_path));
        
        included && !excluded
    }
}
```

---

## File Stat Caching

To avoid redundant filesystem calls during path grouping and size calculations, we use an LRU cache for file stat results.

### StatCache

```rust
use std::sync::Mutex;
use lru::LruCache;
use rusty_attachments_common::DEFAULT_STAT_CACHE_CAPACITY;

/// Cache for file stat results to avoid redundant filesystem calls.
/// Thread-safe via internal mutex.
pub struct StatCache {
    cache: Mutex<LruCache<PathBuf, Option<StatResult>>>,
}

/// Cached stat result
#[derive(Debug, Clone)]
pub struct StatResult {
    pub size: u64,
    pub mtime_us: i64,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub mode: u32,
}

impl StatCache {
    /// Create a new stat cache with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(capacity).unwrap()
            )),
        }
    }
    
    /// Create with default capacity from common constants.
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_STAT_CACHE_CAPACITY)
    }
    
    /// Get stat for a path, using cache if available.
    /// Returns None if the path doesn't exist or can't be accessed.
    pub fn stat(&self, path: &Path) -> Option<StatResult> {
        let mut cache = self.cache.lock().unwrap();
        
        if let Some(cached) = cache.get(path) {
            return cached.clone();
        }
        
        // Not in cache, perform actual stat
        let result = self.do_stat(path);
        cache.put(path.to_path_buf(), result.clone());
        result
    }
    
    /// Check if path exists using cached stat.
    pub fn exists(&self, path: &Path) -> bool {
        self.stat(path).is_some()
    }
    
    /// Check if path is a directory using cached stat.
    pub fn is_dir(&self, path: &Path) -> bool {
        self.stat(path).map(|s| s.is_dir).unwrap_or(false)
    }
    
    /// Check if path is a symlink using cached stat (uses lstat).
    pub fn is_symlink(&self, path: &Path) -> bool {
        self.stat(path).map(|s| s.is_symlink).unwrap_or(false)
    }
    
    /// Get file size using cached stat.
    pub fn size(&self, path: &Path) -> u64 {
        self.stat(path).map(|s| s.size).unwrap_or(0)
    }
    
    /// Clear the cache.
    pub fn clear(&self) {
        self.cache.lock().unwrap().clear();
    }
    
    fn do_stat(&self, path: &Path) -> Option<StatResult> {
        // Use lstat to not follow symlinks
        let metadata = match path.symlink_metadata() {
            Ok(m) => m,
            Err(_) => return None,
        };
        
        let is_symlink = metadata.file_type().is_symlink();
        
        // For symlinks, we need the target's metadata for size
        let (size, is_dir) = if is_symlink {
            match path.metadata() {
                Ok(target_meta) => (target_meta.len(), target_meta.is_dir()),
                Err(_) => (0, false), // Broken symlink
            }
        } else {
            (metadata.len(), metadata.is_dir())
        };
        
        let mtime_us = metadata.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode = 0;
        
        Some(StatResult {
            size,
            mtime_us,
            is_dir,
            is_symlink,
            mode,
        })
    }
}
```

### Usage in Scanner

```rust
impl FileSystemScanner {
    pub fn new() -> Self {
        Self {
            thread_pool: ThreadPool::new(),
            stat_cache: StatCache::default(),
        }
    }
    
    /// Calculate total size of files efficiently using stat cache.
    pub fn calculate_total_size(&self, paths: &[PathBuf]) -> u64 {
        paths.iter()
            .map(|p| self.stat_cache.size(p))
            .sum()
    }
}
```

---

## Symlink Security

Symlinks pose security risks if they can point outside the asset root or to sensitive system files. We implement strict validation during upload.

### Security Requirements

1. **Relative targets only**: Symlinks must have relative targets, not absolute paths
2. **Target within root**: The resolved target must remain within the asset root directory
3. **No symlink following during file reads**: When reading file content for hashing/upload, we must not follow symlinks to prevent TOCTOU attacks

### Symlink Validation

```rust
/// Validate a symlink for inclusion in a manifest.
/// 
/// # Security Checks
/// 1. Target must be relative (no absolute paths)
/// 2. Resolved target must be within the asset root
/// 3. Target path must not escape via `..` traversal
pub fn validate_symlink(
    symlink_path: &Path,
    root: &Path,
) -> Result<SymlinkInfo, FileSystemError> {
    // Read the symlink target without following it
    let target = std::fs::read_link(symlink_path)
        .map_err(|e| FileSystemError::IoError { 
            path: symlink_path.display().to_string(), 
            source: e 
        })?;
    
    // Check 1: Target must be relative
    if target.is_absolute() {
        return Err(FileSystemError::SymlinkAbsoluteTarget {
            symlink: symlink_path.display().to_string(),
            target: target.display().to_string(),
        });
    }
    
    // Check 2: Resolve target relative to symlink's parent directory
    let symlink_dir = symlink_path.parent()
        .ok_or_else(|| FileSystemError::InvalidPath { 
            path: symlink_path.display().to_string() 
        })?;
    
    // Lexically resolve the target (don't touch filesystem)
    let resolved = lexical_resolve(symlink_dir, &target);
    
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
/// Does NOT access the filesystem - pure path manipulation.
fn lexical_resolve(base: &Path, relative: &Path) -> PathBuf {
    let mut result = base.to_path_buf();
    for component in relative.components() {
        match component {
            Component::ParentDir => { result.pop(); }
            Component::CurDir => { /* skip */ }
            Component::Normal(name) => { result.push(name); }
            _ => { result.push(component); }
        }
    }
    result
}

/// Check if a path is within the root directory.
fn is_within_root(path: &Path, root: &Path) -> bool {
    // Normalize both paths lexically
    let norm_path = lexical_normalize(path);
    let norm_root = lexical_normalize(root);
    norm_path.starts_with(&norm_root)
}

#[derive(Debug, Clone)]
pub struct SymlinkInfo {
    /// Path to the symlink itself
    pub path: PathBuf,
    /// Original target as stored in symlink (relative, POSIX format)
    pub target: String,
    /// Fully resolved target path
    pub resolved_target: PathBuf,
}
```

### Secure File Opening

When reading files for hashing or upload, we must ensure we're not following symlinks to unexpected locations:

```rust
/// Securely open a file for reading, preventing symlink attacks.
/// 
/// # Security
/// - Uses O_NOFOLLOW on Unix to prevent symlink following
/// - On Windows, verifies the final path matches the expected path
pub fn open_file_secure(path: &Path, root: &Path) -> Result<File, FileSystemError> {
    // Verify path is within root before opening
    let abs_path = path.canonicalize()
        .map_err(|e| FileSystemError::IoError { 
            path: path.display().to_string(), 
            source: e 
        })?;
    
    if !abs_path.starts_with(root.canonicalize().unwrap_or_else(|_| root.to_path_buf())) {
        return Err(FileSystemError::PathOutsideRoot {
            path: path.display().to_string(),
            root: root.display().to_string(),
        });
    }
    
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| {
                if e.raw_os_error() == Some(libc::ELOOP) {
                    FileSystemError::SymlinkNotAllowed { 
                        path: path.display().to_string() 
                    }
                } else {
                    FileSystemError::IoError { 
                        path: path.display().to_string(), 
                        source: e 
                    }
                }
            })
    }
    
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|e| FileSystemError::IoError { 
                path: path.display().to_string(), 
                source: e 
            })?;
        
        // Verify the opened file's path matches expected
        verify_windows_file_path(&file, path)?;
        
        Ok(file)
    }
}

#[cfg(windows)]
fn verify_windows_file_path(file: &File, expected_path: &Path) -> Result<(), FileSystemError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, VOLUME_NAME_DOS
    };
    
    let handle = file.as_raw_handle();
    let mut buffer = [0u16; 4096];
    
    let len = unsafe {
        GetFinalPathNameByHandleW(
            handle as _,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            VOLUME_NAME_DOS,
        )
    };
    
    if len == 0 || len as usize > buffer.len() {
        return Err(FileSystemError::WindowsPathVerificationFailed {
            path: expected_path.display().to_string(),
        });
    }
    
    let final_path = String::from_utf16_lossy(&buffer[..len as usize]);
    
    // Strip \\?\ prefix if present
    let final_path = final_path
        .strip_prefix(r"\\?\")
        .or_else(|| final_path.strip_prefix(r"\\?\UNC\").map(|s| &s[..]))
        .unwrap_or(&final_path);
    
    let expected_str = expected_path.canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| expected_path.display().to_string());
    
    if final_path != expected_str {
        return Err(FileSystemError::PathMismatch {
            expected: expected_str,
            actual: final_path.to_string(),
        });
    }
    
    Ok(())
}
```

### Secure File Reader Guard

For upload operations, we need a pattern that gracefully handles symlinks by skipping them rather than failing:

```rust
/// A guard that ensures a file is opened securely and closed properly.
/// 
/// This is similar to Python's context manager pattern - it returns None
/// if the file cannot be opened securely (e.g., it's a symlink), allowing
/// the caller to skip the file gracefully.
pub struct SecureFileReader {
    file: Option<File>,
    path: PathBuf,
}

impl SecureFileReader {
    /// Attempt to open a file securely.
    /// 
    /// Returns a reader with `file = None` if:
    /// - The path is a symlink (on systems without O_NOFOLLOW support)
    /// - The file doesn't exist
    /// - Permission denied
    /// 
    /// The caller should check `is_valid()` before reading.
    pub fn open(path: &Path, root: &Path) -> Self {
        let file = open_file_secure_optional(path, root);
        Self {
            file,
            path: path.to_path_buf(),
        }
    }
    
    /// Check if the file was opened successfully.
    pub fn is_valid(&self) -> bool {
        self.file.is_some()
    }
    
    /// Get a reference to the file, if opened successfully.
    pub fn file(&self) -> Option<&File> {
        self.file.as_ref()
    }
    
    /// Take ownership of the file.
    pub fn into_file(self) -> Option<File> {
        self.file
    }
}

/// Open a file securely, returning None on failure instead of error.
/// 
/// This is useful for upload operations where we want to skip problematic
/// files (symlinks, missing files) rather than fail the entire operation.
fn open_file_secure_optional(path: &Path, root: &Path) -> Option<File> {
    // Verify path is within root
    let abs_path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Failed to canonicalize path {}: {}", path.display(), e);
            return None;
        }
    };
    
    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !abs_path.starts_with(&root_canonical) {
        log::warn!("Path {} is outside root {}", path.display(), root.display());
        return None;
    }
    
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(f) => Some(f),
            Err(e) => {
                if e.raw_os_error() == Some(libc::ELOOP) {
                    log::warn!("Skipping symlink: {}", path.display());
                } else {
                    log::warn!("Failed to open {}: {}", path.display(), e);
                }
                None
            }
        }
    }
    
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
        {
            Ok(f) => f,
            Err(e) => {
                log::warn!("Failed to open {}: {}", path.display(), e);
                return None;
            }
        };
        
        // Verify the opened file's path matches expected
        if verify_windows_file_path(&file, path).is_err() {
            log::warn!("Path mismatch for {}, skipping", path.display());
            return None;
        }
        
        Some(file)
    }
}
```

### Symlink Handling in Snapshot

```rust
impl FileSystemScanner {
    fn categorize_entries(
        &self,
        entries: Vec<WalkEntry>,
        options: &SnapshotOptions,
    ) -> Result<(Vec<FileInfo>, Vec<SymlinkInfo>, Vec<DirInfo>), FileSystemError> {
        let mut files = Vec::new();
        let mut symlinks = Vec::new();
        let mut dirs = Vec::new();
        
        for entry in entries {
            if self.stat_cache.is_symlink(&entry.path) {
                if options.follow_symlinks {
                    // Treat as regular file (will read through symlink)
                    // But still validate it doesn't escape root
                    let info = validate_symlink(&entry.path, &options.root)?;
                    files.push(FileInfo::from_symlink_target(&info));
                } else {
                    // Capture as symlink entry in manifest
                    let info = validate_symlink(&entry.path, &options.root)?;
                    symlinks.push(info);
                }
            } else if self.stat_cache.is_dir(&entry.path) {
                dirs.push(DirInfo { path: entry.path });
            } else {
                files.push(FileInfo { path: entry.path });
            }
        }
        
        Ok((files, symlinks, dirs))
    }
}
```

---

## Windows Long Path Handling

Windows long path utilities are provided by the `common` crate. See [common.md](common.md) for details.

```rust
use rusty_attachments_common::{to_long_path, exceeds_max_path, WINDOWS_MAX_PATH};

// Check if path needs long path handling
if exceeds_max_path(&path) {
    let long_path = to_long_path(&path);
    // Use long_path for file operations
}
```

### Usage in Download

When downloading files, apply long path conversion before file operations:

```rust
use rusty_attachments_common::to_long_path;

// In download logic
let local_path = to_long_path(&Path::new(&destination));
std::fs::create_dir_all(local_path.parent().unwrap())?;
// ... download to local_path
```

---

## File System Permissions

After downloading files, we may need to set ownership and permissions so that job worker processes can access them. This is particularly important in multi-user environments.

### Permission Settings

```rust
/// File system permission settings (platform-specific)
#[derive(Debug, Clone)]
pub enum FileSystemPermissionSettings {
    Posix(PosixPermissionSettings),
    Windows(WindowsPermissionSettings),
}

/// POSIX file system permission settings.
/// Permissions are OR'd with existing permissions.
#[derive(Debug, Clone)]
pub struct PosixPermissionSettings {
    /// Target user for ownership (chown)
    pub os_user: String,
    /// Target group for ownership (chgrp)
    pub os_group: String,
    /// Permission mode to add to directories (e.g., 0o750)
    pub dir_mode: u32,
    /// Permission mode to add to files (e.g., 0o640)
    pub file_mode: u32,
}

/// Windows file system permission settings.
#[derive(Debug, Clone)]
pub struct WindowsPermissionSettings {
    /// Target user for ACL
    pub os_user: String,
    /// Permission level for directories
    pub dir_mode: WindowsPermission,
    /// Permission level for files
    pub file_mode: WindowsPermission,
}

/// Windows permission levels
#[derive(Debug, Clone, Copy)]
pub enum WindowsPermission {
    Read,
    Write,
    Execute,
    ReadWrite,
    FullControl,
}
```

### Setting Permissions

```rust
/// Set file system permissions on downloaded files.
/// 
/// This function:
/// 1. Sets permissions on each file
/// 2. Collects unique parent directories
/// 3. Sets permissions on directories from root down
/// 
/// # Arguments
/// * `file_paths` - Absolute paths to downloaded files
/// * `local_root` - Root directory for the download
/// * `settings` - Permission settings to apply
pub fn set_file_permissions(
    file_paths: &[PathBuf],
    local_root: &Path,
    settings: &FileSystemPermissionSettings,
) -> Result<(), FileSystemError> {
    match settings {
        FileSystemPermissionSettings::Posix(posix) => {
            set_posix_permissions(file_paths, local_root, posix)
        }
        FileSystemPermissionSettings::Windows(windows) => {
            set_windows_permissions(file_paths, local_root, windows)
        }
    }
}

#[cfg(unix)]
fn set_posix_permissions(
    file_paths: &[PathBuf],
    local_root: &Path,
    settings: &PosixPermissionSettings,
) -> Result<(), FileSystemError> {
    use std::os::unix::fs::PermissionsExt;
    
    let mut dirs_to_update: HashSet<PathBuf> = HashSet::new();
    
    // Set permissions on files and collect directories
    for file_path in file_paths {
        // Validate path is within root
        if !file_path.starts_with(local_root) {
            return Err(FileSystemError::PathOutsideRoot {
                path: file_path.display().to_string(),
                root: local_root.display().to_string(),
            });
        }
        
        // Change group ownership
        change_group(file_path, &settings.os_group)?;
        
        // Add file permissions (OR with existing)
        let metadata = std::fs::metadata(file_path)?;
        let current_mode = metadata.permissions().mode();
        let new_mode = current_mode | settings.file_mode;
        std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(new_mode))?;
        
        // Collect parent directories
        if let Ok(relative) = file_path.strip_prefix(local_root) {
            for ancestor in relative.ancestors().skip(1) {
                if !ancestor.as_os_str().is_empty() {
                    dirs_to_update.insert(local_root.join(ancestor));
                }
            }
        }
    }
    
    // Set permissions on directories
    for dir_path in dirs_to_update {
        change_group(&dir_path, &settings.os_group)?;
        
        let metadata = std::fs::metadata(&dir_path)?;
        let current_mode = metadata.permissions().mode();
        let new_mode = current_mode | settings.dir_mode;
        std::fs::set_permissions(&dir_path, std::fs::Permissions::from_mode(new_mode))?;
    }
    
    Ok(())
}

#[cfg(unix)]
fn change_group(path: &Path, group: &str) -> Result<(), FileSystemError> {
    use std::ffi::CString;
    
    let path_cstr = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| FileSystemError::InvalidPath { 
            path: path.display().to_string() 
        })?;
    
    // Look up group ID
    let group_cstr = CString::new(group)
        .map_err(|_| FileSystemError::InvalidPath { 
            path: group.to_string() 
        })?;
    
    unsafe {
        let grp = libc::getgrnam(group_cstr.as_ptr());
        if grp.is_null() {
            return Err(FileSystemError::GroupNotFound { 
                group: group.to_string() 
            });
        }
        
        let gid = (*grp).gr_gid;
        if libc::chown(path_cstr.as_ptr(), u32::MAX, gid) != 0 {
            return Err(FileSystemError::IoError {
                path: path.display().to_string(),
                source: std::io::Error::last_os_error(),
            });
        }
    }
    
    Ok(())
}

#[cfg(windows)]
fn set_windows_permissions(
    file_paths: &[PathBuf],
    local_root: &Path,
    settings: &WindowsPermissionSettings,
) -> Result<(), FileSystemError> {
    // Windows ACL implementation using windows-sys
    // Sets DACL entries for the specified user
    todo!("Windows permission implementation")
}
```

### Error Types Addition

```rust
// Add to FileSystemError enum
#[error("Group not found: {group}")]
GroupNotFound { group: String },

#[error("Too many file conflicts for: {path}")]
TooManyConflicts { path: String },
```

---

## Dependencies

```toml
# crates/filesystem/Cargo.toml
[dependencies]
rusty-attachments-common = { path = "../common" }
rusty-attachments-model = { path = "../model" }
walkdir = "2.4"
glob = "0.3"
rayon = "1.8"
thiserror = "1.0"
lru = "0.12"
libc = "0.2"
log = "0.4"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.52", features = ["Win32_Storage_FileSystem"] }

[dev-dependencies]
tempfile = "3.8"
```

---

## Implementation Plan

### Phase 1: Core Infrastructure
- [ ] Create `filesystem` crate structure
- [ ] Implement `GlobFilter` with pattern matching
- [ ] Implement path normalization utilities (POSIX conversion)
- [ ] Implement `StatCache` for efficient filesystem access
- [ ] Implement basic directory walker
- [ ] Define error types

### Phase 2: Symlink Security
- [ ] Implement `validate_symlink()` with security checks
- [ ] Implement `open_file_secure()` with O_NOFOLLOW
- [ ] Implement Windows path verification via GetFinalPathNameByHandleW
- [ ] Add symlink categorization in scanner

### Phase 3: Snapshot Operation
- [ ] Implement `snapshot_structure()` (no hashing)
- [ ] Implement parallel file hashing
- [ ] Implement `snapshot()` with full hashing
- [ ] Add symlink handling and validation
- [ ] Add empty directory support (v2025)

### Phase 4: Diff Operation
- [ ] Implement `diff()` with Fast mode
- [ ] Implement `diff()` with Hash mode
- [ ] Implement `create_diff_manifest()`
- [ ] Add deletion marker generation

### Phase 5: Optimizations
- [ ] Hash cache integration
- [ ] Glob pattern optimization (skip subtrees)
- [ ] Memory-efficient large file handling
- [ ] Progress reporting

### Phase 6: Platform Support
- [ ] Windows long path handling (`\\?\` prefix)
- [ ] File system permissions (POSIX)
- [ ] File system permissions (Windows ACL)

### Phase 7: Bindings
- [ ] Python bindings via PyO3
- [ ] WASM bindings via wasm-bindgen

---

## Related Documents

- [common.md](common.md) - Shared path utilities, hash functions, progress callback
- [model-design.md](model-design.md) - Manifest data structures
- [storage-design.md](storage-design.md) - S3 upload/download operations
- [upload.md](upload.md) - Original upload prototype
