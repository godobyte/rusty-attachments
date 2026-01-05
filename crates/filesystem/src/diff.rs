//! Diff operations for comparing directories against manifests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusty_attachments_common::{hash_bytes, hash_file, normalize_for_manifest, ProgressCallback};
use rusty_attachments_model::Manifest;

use crate::error::FileSystemError;
use crate::glob::GlobFilter;
use crate::scanner::{ScanPhase, ScanProgress};
use crate::stat_cache::{StatCache, StatResult};

/// How to compare files for changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffMode {
    /// Compare by mtime and size only (fast, may miss content-only changes).
    #[default]
    Fast,

    /// Compare by hash (slower, definitive).
    Hash,
}

/// Options for diffing a directory against a manifest.
#[derive(Debug, Clone)]
pub struct DiffOptions {
    /// Root directory to compare.
    pub root: PathBuf,

    /// Glob filter for include/exclude patterns.
    /// Applied to BOTH the manifest entries and current directory.
    pub filter: GlobFilter,

    /// Comparison mode.
    pub mode: DiffMode,

    /// Number of parallel hashing threads (0 = auto-detect).
    pub parallelism: usize,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            filter: GlobFilter::default(),
            mode: DiffMode::Fast,
            parallelism: 0,
        }
    }
}

/// Entry representing a file found during scanning.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative path from root (POSIX format).
    pub path: String,

    /// File size in bytes.
    pub size: u64,

    /// Modification time (microseconds since epoch).
    pub mtime: i64,

    /// File hash (if computed).
    pub hash: Option<String>,

    /// Whether file has execute permission.
    pub runnable: bool,
}

/// Statistics from a diff operation.
#[derive(Debug, Clone, Default)]
pub struct DiffStats {
    /// Total files scanned on disk.
    pub files_scanned: u64,

    /// Total directories scanned.
    pub dirs_scanned: u64,

    /// Files that were hashed (in Hash mode or for modified detection).
    pub files_hashed: u64,

    /// Bytes hashed.
    pub bytes_hashed: u64,

    /// Time spent walking directories.
    pub walk_duration: Duration,

    /// Time spent hashing files.
    pub hash_duration: Duration,
}

/// Result of comparing a directory against a manifest.
#[derive(Debug, Clone, Default)]
pub struct DiffResult {
    /// Files that are new (not in parent manifest).
    pub added: Vec<FileEntry>,

    /// Files that were modified (different mtime/size or hash).
    pub modified: Vec<FileEntry>,

    /// Files that were deleted (in parent but not on disk).
    pub deleted: Vec<String>,

    /// Files that are unchanged.
    pub unchanged: Vec<String>,

    /// Directories that are new.
    pub added_dirs: Vec<String>,

    /// Directories that were deleted.
    pub deleted_dirs: Vec<String>,

    /// Statistics about the diff operation.
    pub stats: DiffStats,
}

/// Internal representation of a manifest file entry for comparison.
#[derive(Debug, Clone)]
struct ManifestFileInfo {
    /// File size in bytes.
    size: u64,
    /// Modification time in microseconds.
    mtime: i64,
    /// File hash.
    hash: String,
}

/// Diff engine for comparing directories against manifests.
pub struct DiffEngine {
    stat_cache: StatCache,
}

impl DiffEngine {
    /// Create a new diff engine.
    pub fn new() -> Self {
        Self {
            stat_cache: StatCache::with_default_capacity(),
        }
    }

    /// Compare a directory against an existing manifest.
    ///
    /// # Arguments
    /// * `manifest` - The parent manifest to compare against
    /// * `options` - Diff configuration
    /// * `progress` - Optional progress callback
    ///
    /// # Returns
    /// DiffResult containing added, modified, deleted, and unchanged entries.
    ///
    /// # Errors
    /// Returns error if directory cannot be read or comparison fails.
    pub fn diff(
        &self,
        manifest: &Manifest,
        options: &DiffOptions,
        progress: Option<&dyn ProgressCallback<ScanProgress>>,
    ) -> Result<DiffResult, FileSystemError> {
        let walk_start: Instant = Instant::now();

        // Report walking phase
        if let Some(cb) = progress {
            cb.on_progress(&ScanProgress {
                phase: ScanPhase::Walking,
                current_path: Some(options.root.display().to_string()),
                files_processed: 0,
                total_files: None,
                bytes_processed: 0,
                total_bytes: None,
            });
        }

        // 1. Walk current directory and collect files
        let (current_files, current_dirs): (HashMap<String, CurrentFileInfo>, Vec<String>) =
            self.walk_current_directory(&options.root, &options.filter)?;

        let walk_duration: Duration = walk_start.elapsed();

        // 2. Build manifest lookup
        let manifest_files: HashMap<String, ManifestFileInfo> =
            self.build_manifest_lookup(manifest, &options.filter);
        let manifest_dirs: Vec<String> = self.get_manifest_dirs(manifest, &options.filter);

        // 3. Categorize files
        let mut added: Vec<FileEntry> = Vec::new();
        let mut modified: Vec<FileEntry> = Vec::new();
        let mut unchanged: Vec<String> = Vec::new();
        let mut deleted: Vec<String> = Vec::new();

        let hash_start: Instant = Instant::now();
        let mut files_hashed: u64 = 0;
        let mut bytes_hashed: u64 = 0;

        // Find new and potentially modified files
        for (path, current) in &current_files {
            if let Some(manifest_entry) = manifest_files.get(path) {
                // File exists in both - check if modified
                let is_modified: bool = match options.mode {
                    DiffMode::Fast => {
                        self.is_modified_fast(current, manifest_entry)
                    }
                    DiffMode::Hash => {
                        let (modified, hashed_bytes): (bool, u64) =
                            self.is_modified_hash(current, manifest_entry, &options.root)?;
                        if hashed_bytes > 0 {
                            files_hashed += 1;
                            bytes_hashed += hashed_bytes;
                        }
                        modified
                    }
                };

                if is_modified {
                    modified.push(FileEntry {
                        path: path.clone(),
                        size: current.size,
                        mtime: current.mtime,
                        hash: None,
                        runnable: current.runnable,
                    });
                } else {
                    unchanged.push(path.clone());
                }
            } else {
                // New file
                added.push(FileEntry {
                    path: path.clone(),
                    size: current.size,
                    mtime: current.mtime,
                    hash: None,
                    runnable: current.runnable,
                });
            }
        }

        // Find deleted files
        for path in manifest_files.keys() {
            if !current_files.contains_key(path) {
                deleted.push(path.clone());
            }
        }

        // Find added and deleted directories
        let current_dir_set: std::collections::HashSet<&String> = current_dirs.iter().collect();
        let manifest_dir_set: std::collections::HashSet<&String> = manifest_dirs.iter().collect();

        let added_dirs: Vec<String> = current_dirs
            .iter()
            .filter(|d| !manifest_dir_set.contains(d))
            .cloned()
            .collect();

        let deleted_dirs: Vec<String> = manifest_dirs
            .iter()
            .filter(|d| !current_dir_set.contains(d))
            .cloned()
            .collect();

        let hash_duration: Duration = hash_start.elapsed();

        // Report complete
        if let Some(cb) = progress {
            cb.on_progress(&ScanProgress {
                phase: ScanPhase::Complete,
                current_path: None,
                files_processed: current_files.len() as u64,
                total_files: Some(current_files.len() as u64),
                bytes_processed: bytes_hashed,
                total_bytes: None,
            });
        }

        Ok(DiffResult {
            added,
            modified,
            deleted,
            unchanged,
            added_dirs,
            deleted_dirs,
            stats: DiffStats {
                files_scanned: current_files.len() as u64,
                dirs_scanned: current_dirs.len() as u64,
                files_hashed,
                bytes_hashed,
                walk_duration,
                hash_duration,
            },
        })
    }

    /// Create a diff manifest from the comparison result.
    ///
    /// # Arguments
    /// * `parent_manifest` - The original parent manifest
    /// * `parent_manifest_bytes` - Raw bytes of parent (for computing parentManifestHash)
    /// * `diff_result` - Result from diff() operation
    /// * `options` - Options used for the diff
    ///
    /// # Returns
    /// A diff manifest with parentManifestHash, changes, and deletion markers.
    ///
    /// # Errors
    /// Returns error if manifest creation fails.
    pub fn create_diff_manifest(
        &self,
        _parent_manifest: &Manifest,
        parent_manifest_bytes: &[u8],
        diff_result: &DiffResult,
        options: &DiffOptions,
    ) -> Result<Manifest, FileSystemError> {
        use rusty_attachments_model::v2025_12_04;

        // 1. Compute parent manifest hash
        let parent_hash: String = hash_bytes(parent_manifest_bytes);

        // 2. Build file entries from added + modified (need to hash them)
        let mut file_paths: Vec<v2025_12_04::ManifestFilePath> = Vec::new();

        for entry in &diff_result.added {
            let file_path: PathBuf = options.root.join(&entry.path);
            let hash: String = hash_file(&file_path).map_err(|e| FileSystemError::IoError {
                path: file_path.display().to_string(),
                source: e,
            })?;

            file_paths.push(
                v2025_12_04::ManifestFilePath::file(
                    entry.path.clone(),
                    hash,
                    entry.size,
                    entry.mtime,
                )
                .with_runnable(entry.runnable),
            );
        }

        for entry in &diff_result.modified {
            let file_path: PathBuf = options.root.join(&entry.path);
            let hash: String = hash_file(&file_path).map_err(|e| FileSystemError::IoError {
                path: file_path.display().to_string(),
                source: e,
            })?;

            file_paths.push(
                v2025_12_04::ManifestFilePath::file(
                    entry.path.clone(),
                    hash,
                    entry.size,
                    entry.mtime,
                )
                .with_runnable(entry.runnable),
            );
        }

        // 3. Add deletion markers
        for path in &diff_result.deleted {
            file_paths.push(v2025_12_04::ManifestFilePath::deleted(path.clone()));
        }

        // 4. Build directory entries with deletion markers
        let mut dir_paths: Vec<v2025_12_04::ManifestDirectoryPath> = Vec::new();

        for dir in &diff_result.added_dirs {
            dir_paths.push(v2025_12_04::ManifestDirectoryPath::new(dir.clone()));
        }

        for dir in &diff_result.deleted_dirs {
            dir_paths.push(v2025_12_04::ManifestDirectoryPath::deleted(dir.clone()));
        }

        // 5. Create diff manifest
        Ok(Manifest::V2025_12_04_beta(
            v2025_12_04::AssetManifest::diff(dir_paths, file_paths, parent_hash),
        ))
    }

    /// Walk the current directory and collect file information.
    fn walk_current_directory(
        &self,
        root: &Path,
        filter: &GlobFilter,
    ) -> Result<(HashMap<String, CurrentFileInfo>, Vec<String>), FileSystemError> {
        use walkdir::WalkDir;

        let mut files: HashMap<String, CurrentFileInfo> = HashMap::new();
        let mut dirs: Vec<String> = Vec::new();

        for entry in WalkDir::new(root).follow_links(false).into_iter() {
            let entry: walkdir::DirEntry = entry.map_err(|e| FileSystemError::IoError {
                path: e
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                source: e.into(),
            })?;

            let path: &Path = entry.path();

            // Skip the root directory itself
            if path == root {
                continue;
            }

            // Get relative path for filtering
            let relative_path: String =
                normalize_for_manifest(path, root).map_err(FileSystemError::Path)?;

            // Apply glob filter
            if !filter.is_empty() && !filter.matches(&relative_path) {
                continue;
            }

            let stat: StatResult = match self.stat_cache.stat(path) {
                Some(s) => s,
                None => continue,
            };

            if stat.is_dir && !stat.is_symlink {
                dirs.push(relative_path);
            } else if !stat.is_dir && !stat.is_symlink {
                files.insert(
                    relative_path,
                    CurrentFileInfo {
                        size: stat.size,
                        mtime: stat.mtime_us,
                        runnable: is_executable(stat.mode),
                        abs_path: path.to_path_buf(),
                    },
                );
            }
        }

        Ok((files, dirs))
    }

    /// Build a lookup map from manifest entries.
    fn build_manifest_lookup(
        &self,
        manifest: &Manifest,
        filter: &GlobFilter,
    ) -> HashMap<String, ManifestFileInfo> {
        let mut lookup: HashMap<String, ManifestFileInfo> = HashMap::new();

        match manifest {
            Manifest::V2023_03_03(m) => {
                for path in &m.paths {
                    if filter.is_empty() || filter.matches(&path.path) {
                        lookup.insert(
                            path.path.clone(),
                            ManifestFileInfo {
                                size: path.size,
                                mtime: path.mtime,
                                hash: path.hash.clone(),
                            },
                        );
                    }
                }
            }
            Manifest::V2025_12_04_beta(m) => {
                for file in &m.files {
                    // Skip deleted entries and symlinks
                    if file.delete || file.symlink_target.is_some() {
                        continue;
                    }
                    // Skip entries without size/mtime (deleted markers)
                    let size: u64 = match file.size {
                        Some(s) => s,
                        None => continue,
                    };
                    let mtime: i64 = match file.mtime {
                        Some(m) => m,
                        None => continue,
                    };
                    if filter.is_empty() || filter.matches(&file.name) {
                        lookup.insert(
                            file.name.clone(),
                            ManifestFileInfo {
                                size,
                                mtime,
                                hash: file.hash.clone().unwrap_or_default(),
                            },
                        );
                    }
                }
            }
        }

        lookup
    }

    /// Get directory paths from manifest.
    fn get_manifest_dirs(&self, manifest: &Manifest, filter: &GlobFilter) -> Vec<String> {
        match manifest {
            Manifest::V2023_03_03(_) => Vec::new(), // v2023 doesn't track directories
            Manifest::V2025_12_04_beta(m) => m
                .dirs
                .iter()
                .filter(|d| !d.delete)
                .filter(|d| filter.is_empty() || filter.matches(&d.name))
                .map(|d| d.name.clone())
                .collect(),
        }
    }

    /// Check if file is modified using fast comparison (mtime/size).
    fn is_modified_fast(&self, current: &CurrentFileInfo, manifest: &ManifestFileInfo) -> bool {
        // Check size first (most common change indicator)
        if current.size != manifest.size {
            return true;
        }

        // Check mtime - allow 1 microsecond tolerance for filesystem precision
        // (Python reference uses abs(...) > 1 for microsecond comparison)
        let mtime_diff: i64 = (current.mtime - manifest.mtime).abs();
        mtime_diff > 1
    }

    /// Check if file is modified using hash comparison.
    ///
    /// Returns (is_modified, bytes_hashed).
    fn is_modified_hash(
        &self,
        current: &CurrentFileInfo,
        manifest: &ManifestFileInfo,
        _root: &Path,
    ) -> Result<(bool, u64), FileSystemError> {
        // If size differs, definitely modified (no need to hash)
        if current.size != manifest.size {
            return Ok((true, 0));
        }

        // Hash the current file
        let current_hash: String =
            hash_file(&current.abs_path).map_err(|e| FileSystemError::IoError {
                path: current.abs_path.display().to_string(),
                source: e,
            })?;

        let is_modified: bool = current_hash != manifest.hash;
        Ok((is_modified, current.size))
    }

    /// Clear the stat cache.
    pub fn clear_cache(&self) {
        self.stat_cache.clear();
    }
}

impl Default for DiffEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal representation of a current file on disk.
#[derive(Debug, Clone)]
struct CurrentFileInfo {
    /// File size in bytes.
    size: u64,
    /// Modification time in microseconds.
    mtime: i64,
    /// Whether file has execute permission.
    runnable: bool,
    /// Absolute path to the file.
    abs_path: PathBuf,
}

/// Check if a file mode indicates executable permission.
#[cfg(unix)]
fn is_executable(mode: u32) -> bool {
    mode & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_mode: u32) -> bool {
    false
}


#[cfg(test)]
mod tests {
    use super::*;
    use rusty_attachments_model::v2023_03_03;
    use std::io::Write;
    use std::thread;
    use std::time::Duration as StdDuration;
    use tempfile::TempDir;

    /// Helper to create a test file with content.
    fn create_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path: PathBuf = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file: std::fs::File = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    /// Helper to create a manifest from a directory snapshot.
    fn snapshot_to_manifest(dir: &Path) -> (Manifest, Vec<u8>) {
        use crate::scanner::{FileSystemScanner, SnapshotOptions};

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: dir.to_path_buf(),
            version: ManifestVersion::V2023_03_03,
            ..Default::default()
        };

        let manifest: Manifest = scanner.snapshot(&options, None).unwrap();
        let bytes: Vec<u8> = manifest.encode().unwrap().into_bytes();
        (manifest, bytes)
    }

    #[test]
    fn test_diff_no_change() {
        // Given: snapshot folder with 1 test file
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        create_file(&root, "test_file", "testing123");

        let (manifest, _bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // When: diff with no changes
        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: no differences
        assert_eq!(result.added.len(), 0);
        assert_eq!(result.modified.len(), 0);
        assert_eq!(result.deleted.len(), 0);
        assert_eq!(result.unchanged.len(), 1);
    }

    #[test]
    fn test_diff_new_files() {
        // Given: snapshot folder with 1 test file
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        create_file(&root, "test_file", "testing123");

        let (manifest, _bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // When: add 2 new files
        create_file(&root, "new_file", "");
        std::fs::create_dir(root.join("new_dir")).unwrap();
        create_file(&root, "new_dir/new_file2", "");

        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: 2 new files found
        assert_eq!(result.deleted.len(), 0);
        assert_eq!(result.modified.len(), 0);
        assert_eq!(result.added.len(), 2);

        let added_paths: Vec<&str> = result.added.iter().map(|e| e.path.as_str()).collect();
        assert!(added_paths.contains(&"new_file"));
        assert!(added_paths.contains(&"new_dir/new_file2"));
    }

    #[test]
    fn test_diff_deleted_file() {
        // Given: snapshot folder with 1 test file
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        create_file(&root, "test_file", "testing123");

        let (manifest, _bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // When: delete the test file
        std::fs::remove_file(root.join("test_file")).unwrap();

        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: 1 deleted file found
        assert_eq!(result.modified.len(), 0);
        assert_eq!(result.added.len(), 0);
        assert_eq!(result.deleted.len(), 1);
        assert!(result.deleted.contains(&"test_file".to_string()));
    }

    #[test]
    fn test_diff_modified_file_size() {
        // Given: snapshot folder with 1 test file
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        create_file(&root, "test_file", "testing123");

        let (manifest, _bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // When: modify the file (different size)
        create_file(&root, "test_file", "something_different_and_longer");

        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: 1 modified file found
        assert_eq!(result.added.len(), 0);
        assert_eq!(result.deleted.len(), 0);
        assert_eq!(result.modified.len(), 1);
        assert_eq!(result.modified[0].path, "test_file");
    }

    #[test]
    fn test_diff_modified_file_mtime() {
        // Given: snapshot folder with 1 test file
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        let file_path: PathBuf = create_file(&root, "test_file", "testing123");

        let (manifest, _bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // When: touch the file to update mtime (same content, same size)
        // Sleep briefly to ensure mtime changes
        thread::sleep(StdDuration::from_millis(10));

        // Rewrite with same content to update mtime
        let mut file: std::fs::File = std::fs::File::create(&file_path).unwrap();
        file.write_all(b"testing123").unwrap();

        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: 1 modified file found (mtime changed)
        assert_eq!(result.added.len(), 0);
        assert_eq!(result.deleted.len(), 0);
        assert_eq!(result.modified.len(), 1);
        assert_eq!(result.modified[0].path, "test_file");
    }

    #[test]
    fn test_diff_hash_mode() {
        // Given: snapshot folder with 1 test file
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        create_file(&root, "test_file", "testing123");

        let (manifest, _bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // When: diff with hash mode (no changes)
        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Hash,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: no differences, but files were hashed
        assert_eq!(result.added.len(), 0);
        assert_eq!(result.modified.len(), 0);
        assert_eq!(result.deleted.len(), 0);
        assert_eq!(result.unchanged.len(), 1);
        assert!(result.stats.files_hashed > 0);
    }

    #[test]
    fn test_diff_with_filter() {
        // Given: snapshot folder with multiple files
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        create_file(&root, "file1.txt", "content1");
        create_file(&root, "file2.log", "content2");

        // Create manifest with all files
        let (manifest, _bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // When: add new files of both types
        create_file(&root, "new.txt", "new content");
        create_file(&root, "new.log", "new log");

        // Diff with filter for .txt only
        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            filter: GlobFilter::include(vec!["**/*.txt".to_string()]).unwrap(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: only .txt files are considered
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].path, "new.txt");
        assert_eq!(result.unchanged.len(), 1);
    }

    #[test]
    fn test_diff_v2025_manifest() {
        // Given: v2025 manifest with directories
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("subdir")).unwrap();
        create_file(&root, "subdir/file.txt", "content");

        // Create v2025 manifest
        use crate::scanner::{FileSystemScanner, SnapshotOptions};
        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: root.clone(),
            version: ManifestVersion::V2025_12_04_beta,
            ..Default::default()
        };
        let manifest: Manifest = scanner.snapshot(&options, None).unwrap();

        // When: add a new directory
        std::fs::create_dir(root.join("newdir")).unwrap();
        create_file(&root, "newdir/newfile.txt", "new content");

        let engine: DiffEngine = DiffEngine::new();
        let diff_options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &diff_options, None).unwrap();

        // Then: new directory and file detected
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].path, "newdir/newfile.txt");
        assert!(result.added_dirs.contains(&"newdir".to_string()));
    }

    #[test]
    fn test_create_diff_manifest() {
        // Given: snapshot and then modifications
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        create_file(&root, "original.txt", "original content");

        let (manifest, bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // Make changes
        create_file(&root, "new.txt", "new content");
        std::fs::remove_file(root.join("original.txt")).unwrap();

        // Compute diff
        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let diff_result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // When: create diff manifest
        let diff_manifest: Manifest = engine
            .create_diff_manifest(&manifest, &bytes, &diff_result, &options)
            .unwrap();

        // Then: diff manifest has parent hash and changes
        assert_eq!(diff_manifest.version(), ManifestVersion::V2025_12_04_beta);

        if let Manifest::V2025_12_04_beta(m) = diff_manifest {
            assert!(m.parent_manifest_hash.is_some());

            // Should have 1 added file and 1 deleted marker
            let added: Vec<_> = m
                .paths
                .iter()
                .filter(|p| !p.deleted)
                .collect();
            let deleted: Vec<_> = m
                .paths
                .iter()
                .filter(|p| p.deleted)
                .collect();

            assert_eq!(added.len(), 1);
            assert_eq!(added[0].path, "new.txt");
            assert_eq!(deleted.len(), 1);
            assert_eq!(deleted[0].path, "original.txt");
        } else {
            panic!("Expected V2025 manifest");
        }
    }

    #[test]
    fn test_diff_stats() {
        // Given: snapshot folder with files
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();
        create_file(&root, "file1.txt", "content1");
        create_file(&root, "file2.txt", "content2");

        let (manifest, _bytes): (Manifest, Vec<u8>) = snapshot_to_manifest(&root);

        // When: diff
        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: stats are populated
        assert_eq!(result.stats.files_scanned, 2);
        assert!(result.stats.walk_duration.as_nanos() > 0);
    }

    #[test]
    fn test_diff_empty_directory() {
        // Given: empty directory
        let dir: TempDir = TempDir::new().unwrap();
        let root: PathBuf = dir.path().join("snapshot");
        std::fs::create_dir(&root).unwrap();

        // Create empty manifest
        let manifest: Manifest = Manifest::V2023_03_03(v2023_03_03::AssetManifest::new(vec![]));

        // When: diff empty directory
        let engine: DiffEngine = DiffEngine::new();
        let options: DiffOptions = DiffOptions {
            root: root.clone(),
            mode: DiffMode::Fast,
            ..Default::default()
        };

        let result: DiffResult = engine.diff(&manifest, &options, None).unwrap();

        // Then: no differences
        assert_eq!(result.added.len(), 0);
        assert_eq!(result.modified.len(), 0);
        assert_eq!(result.deleted.len(), 0);
        assert_eq!(result.unchanged.len(), 0);
    }
}
