# File System Design Summary

**Full doc:** `design/file_system.md`  
**Status:** ✅ IMPLEMENTED in `crates/filesystem/`

## Purpose
Directory scanning, manifest creation, diff operations with glob filtering, stat caching, and symlink validation.

## Key Types

### Filtering
```rust
struct GlobFilter {
    include: Vec<String>,  // Empty = include all
    exclude: Vec<String>,
}
```
Patterns: `*`, `**`, `?`, `[abc]`, `[!abc]`, `{a,b}`

### Snapshot Options
```rust
struct SnapshotOptions {
    root: PathBuf,
    input_files: Option<Vec<PathBuf>>,  // None = walk directory
    version: ManifestVersion,
    filter: GlobFilter,
    hash_algorithm: HashAlgorithm,
    follow_symlinks: bool,
    include_empty_dirs: bool,
    hash_cache: Option<PathBuf>,
    parallelism: usize,
}
```

### Diff Options
```rust
struct DiffOptions {
    root: PathBuf,
    filter: GlobFilter,
    mode: DiffMode,  // Fast (mtime/size) or Hash
    hash_cache: Option<PathBuf>,
    parallelism: usize,
}
```

### Results
- `DiffResult`: added, modified, deleted, unchanged files/dirs, stats
- `ExpandedInputPaths`: files, expanded_directories, missing, total_size

## Key Functions

### Input Path Handling
- `expand_input_paths()`: Expand directories to files, apply filters
- `validate_input_paths()`: Categorize as valid_files, missing, directories

### Scanner Operations
```rust
impl FileSystemScanner {
    fn snapshot(&self, options: &SnapshotOptions, progress: Option<...>) -> Result<Manifest>;
    fn snapshot_structure(&self, options: &SnapshotOptions, progress: Option<...>) -> Result<Manifest>;
    fn diff(&self, manifest: &Manifest, options: &DiffOptions, progress: Option<...>) -> Result<DiffResult>;
    fn create_diff_manifest(&self, parent: &Manifest, parent_bytes: &[u8], diff: &DiffResult, options: &DiffOptions) -> Result<Manifest>;
}
```

## Stat Cache

LRU cache for file metadata to avoid redundant filesystem calls:

```rust
struct StatResult { size: u64, mtime_us: i64, is_dir: bool, is_symlink: bool, mode: u32 }

struct StatCache {
    fn new(capacity: usize) -> Self;
    fn with_default_capacity() -> Self;  // DEFAULT_STAT_CACHE_CAPACITY = 1024
    fn stat(&self, path: &Path) -> Option<StatResult>;
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn is_symlink(&self, path: &Path) -> bool;
    fn size(&self, path: &Path) -> u64;
    fn mtime_us(&self, path: &Path) -> i64;
    fn clear(&self);
}
```

Thread-safe via internal Mutex. Handles symlinks (reports target size, detects broken symlinks).

## Diff Engine

Separate from scanner, compares directory state against a manifest:

```rust
struct DiffEngine;
enum DiffMode { Fast, Hash }  // Fast = mtime/size only, Hash = full content hash

struct DiffResult {
    added: Vec<FileEntry>,
    modified: Vec<FileEntry>,
    deleted: Vec<FileEntry>,
    unchanged: Vec<FileEntry>,
    stats: DiffStats,
}
```

## Symlink Validation

```rust
enum SymlinkPolicy { Follow, Skip, Validate }

struct SymlinkInfo { path: PathBuf, target: PathBuf, is_relative: bool }

fn validate_symlink(path: &Path, root: &Path, policy: SymlinkPolicy) -> Result<Option<SymlinkInfo>, FileSystemError>;
```

## Security
- Symlink validation: target must be within root
- Path traversal prevention via `is_within_root()`
- Absolute symlink targets rejected in manifests

## When to Read Full Doc
- Implementing directory scanning
- Adding new filter patterns
- Understanding diff algorithms
- Symlink security validation
- Stat cache tuning
