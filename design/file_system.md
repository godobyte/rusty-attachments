# Rusty Attachments: File System Module Design

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
    /// Root directory to snapshot
    pub root: PathBuf,
    
    /// Manifest version to create
    pub version: ManifestVersion,
    
    /// Glob filter for include/exclude patterns
    pub filter: GlobFilter,
    
    /// Hash algorithm to use
    pub hash_algorithm: HashAlgorithm,
    
    /// Whether to follow symlinks (false = capture as symlinks)
    pub follow_symlinks: bool,
    
    /// Whether to include empty directories (v2025 only)
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

```rust
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

/// Callback for progress reporting
pub trait ProgressCallback: Send + Sync {
    /// Called with progress updates
    /// Returns false to cancel the operation
    fn on_progress(&self, progress: &ScanProgress) -> bool;
}
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
        // 1. Walk directory tree
        let entries = self.walk_directory(&options.root, &options.filter, progress)?;
        
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

```rust
#[derive(Debug, thiserror::Error)]
pub enum FileSystemError {
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
    
    #[error("IO error at {path}: {source}")]
    IoError { path: String, source: std::io::Error },
    
    #[error("Hash error: {message}")]
    HashError { message: String },
    
    #[error("Operation cancelled")]
    Cancelled,
    
    #[error("Manifest error: {message}")]
    ManifestError { message: String },
}
```

---

## Implementation Notes

### Directory Walking Strategy

1. Use `walkdir` crate for efficient recursive directory traversal
2. Apply glob filters during walk to skip entire subtrees when possible
3. Collect symlinks separately for validation
4. Track empty directories for v2025 format

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

## Dependencies

```toml
# crates/filesystem/Cargo.toml
[dependencies]
rusty-attachments-model = { path = "../model" }
walkdir = "2.4"
glob = "0.3"
rayon = "1.8"
thiserror = "1.0"
xxhash-rust = { version = "0.8", features = ["xxh3"] }

[dev-dependencies]
tempfile = "3.8"
```

---

## Implementation Plan

### Phase 1: Core Infrastructure
- [ ] Create `filesystem` crate structure
- [ ] Implement `GlobFilter` with pattern matching
- [ ] Implement basic directory walker
- [ ] Define error types

### Phase 2: Snapshot Operation
- [ ] Implement `snapshot_structure()` (no hashing)
- [ ] Implement parallel file hashing
- [ ] Implement `snapshot()` with full hashing
- [ ] Add symlink handling and validation
- [ ] Add empty directory support (v2025)

### Phase 3: Diff Operation
- [ ] Implement `diff()` with Fast mode
- [ ] Implement `diff()` with Hash mode
- [ ] Implement `create_diff_manifest()`
- [ ] Add deletion marker generation

### Phase 4: Optimizations
- [ ] Hash cache integration
- [ ] Glob pattern optimization (skip subtrees)
- [ ] Memory-efficient large file handling
- [ ] Progress reporting

### Phase 5: Bindings
- [ ] Python bindings via PyO3
- [ ] WASM bindings via wasm-bindgen

---

## Related Documents

- [model-design.md](model-design.md) - Manifest data structures
- [storage-design.md](storage-design.md) - S3 upload/download operations
- [upload.md](upload.md) - Original upload prototype
