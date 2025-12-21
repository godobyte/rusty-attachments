//! File system scanner for snapshot operations.

use std::path::{Path, PathBuf};

use rusty_attachments_common::{hash_file, normalize_for_manifest, ProgressCallback, CHUNK_SIZE_V2};
use rusty_attachments_model::{
    v2023_03_03, v2025_12_04, HashAlgorithm, Manifest, ManifestVersion,
};
use walkdir::WalkDir;

use crate::error::FileSystemError;
use crate::glob::GlobFilter;
use crate::stat_cache::{StatCache, StatResult};
use crate::symlink::{validate_symlink, SymlinkInfo};

/// Options for creating a directory snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotOptions {
    /// Root directory to snapshot.
    pub root: PathBuf,
    /// Explicit list of input files (skips directory walking if provided).
    pub input_files: Option<Vec<PathBuf>>,
    /// Manifest version to create.
    pub version: ManifestVersion,
    /// Glob filter for include/exclude patterns.
    pub filter: GlobFilter,
    /// Hash algorithm to use.
    pub hash_algorithm: HashAlgorithm,
    /// Whether to follow symlinks (false = capture as symlinks).
    pub follow_symlinks: bool,
    /// Whether to include empty directories (v2025 only).
    pub include_empty_dirs: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            input_files: None,
            version: ManifestVersion::V2025_12_04_beta,
            filter: GlobFilter::default(),
            hash_algorithm: HashAlgorithm::Xxh128,
            follow_symlinks: false,
            include_empty_dirs: true,
        }
    }
}

/// Progress updates during file system operations.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    /// Current operation phase.
    pub phase: ScanPhase,
    /// Current file being processed.
    pub current_path: Option<String>,
    /// Files processed so far.
    pub files_processed: u64,
    /// Total files found (if known).
    pub total_files: Option<u64>,
    /// Bytes processed (for hashing phase).
    pub bytes_processed: u64,
    /// Total bytes to process (if known).
    pub total_bytes: Option<u64>,
}

/// Phase of the scan operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanPhase {
    /// Walking directory tree.
    Walking,
    /// Hashing file contents.
    Hashing,
    /// Building manifest.
    Building,
    /// Complete.
    Complete,
}

/// Internal representation of a discovered file.
#[derive(Debug, Clone)]
struct FileEntry {
    /// Absolute path to the file.
    path: PathBuf,
    /// Relative path for manifest (POSIX format).
    relative_path: String,
    /// File size in bytes.
    size: u64,
    /// Modification time in microseconds.
    mtime_us: i64,
    /// Whether file has execute permission.
    runnable: bool,
}

/// Internal representation of a discovered directory.
#[derive(Debug, Clone)]
struct DirEntry {
    /// Relative path for manifest (POSIX format).
    relative_path: String,
}

/// File system scanner for snapshot and diff operations.
pub struct FileSystemScanner {
    stat_cache: StatCache,
}

impl FileSystemScanner {
    /// Create a new scanner.
    pub fn new() -> Self {
        Self {
            stat_cache: StatCache::with_default_capacity(),
        }
    }

    /// Create a snapshot manifest from a directory.
    ///
    /// # Arguments
    /// * `options` - Snapshot configuration
    /// * `progress` - Optional progress callback
    ///
    /// # Returns
    /// A manifest representing the directory state.
    ///
    /// # Errors
    /// Returns error if directory cannot be read or hashing fails.
    pub fn snapshot(
        &self,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback<ScanProgress>>,
    ) -> Result<Manifest, FileSystemError> {
        // 1. Get file entries
        let (files, symlinks, dirs): (Vec<FileEntry>, Vec<SymlinkInfo>, Vec<DirEntry>) =
            if let Some(input_files) = &options.input_files {
                self.entries_from_file_list(input_files, &options.root, options)?
            } else {
                self.walk_directory(&options.root, options, progress)?
            };

        // Report hashing phase
        let total_files: u64 = files.len() as u64;
        let total_bytes: u64 = files.iter().map(|f: &FileEntry| f.size).sum();

        if let Some(cb) = progress {
            cb.on_progress(&ScanProgress {
                phase: ScanPhase::Hashing,
                current_path: None,
                files_processed: 0,
                total_files: Some(total_files),
                bytes_processed: 0,
                total_bytes: Some(total_bytes),
            });
        }

        // 2. Hash files and build manifest
        let manifest: Manifest =
            self.build_manifest(files, symlinks, dirs, options, progress)?;

        // Report complete
        if let Some(cb) = progress {
            cb.on_progress(&ScanProgress {
                phase: ScanPhase::Complete,
                current_path: None,
                files_processed: total_files,
                total_files: Some(total_files),
                bytes_processed: total_bytes,
                total_bytes: Some(total_bytes),
            });
        }

        Ok(manifest)
    }

    /// Create a snapshot without computing hashes.
    ///
    /// Useful for fast directory scanning before selective hashing.
    ///
    /// # Arguments
    /// * `options` - Snapshot configuration
    /// * `progress` - Optional progress callback
    ///
    /// # Returns
    /// A manifest with empty hashes for all files.
    pub fn snapshot_structure(
        &self,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback<ScanProgress>>,
    ) -> Result<Manifest, FileSystemError> {
        let (files, symlinks, dirs): (Vec<FileEntry>, Vec<SymlinkInfo>, Vec<DirEntry>) =
            if let Some(input_files) = &options.input_files {
                self.entries_from_file_list(input_files, &options.root, options)?
            } else {
                self.walk_directory(&options.root, options, progress)?
            };

        self.build_manifest_without_hashing(files, symlinks, dirs, options)
    }

    /// Walk directory and collect entries.
    fn walk_directory(
        &self,
        root: &Path,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback<ScanProgress>>,
    ) -> Result<(Vec<FileEntry>, Vec<SymlinkInfo>, Vec<DirEntry>), FileSystemError> {
        let mut files: Vec<FileEntry> = Vec::new();
        let mut symlinks: Vec<SymlinkInfo> = Vec::new();
        let mut dirs: Vec<DirEntry> = Vec::new();

        if let Some(cb) = progress {
            cb.on_progress(&ScanProgress {
                phase: ScanPhase::Walking,
                current_path: Some(root.display().to_string()),
                files_processed: 0,
                total_files: None,
                bytes_processed: 0,
                total_bytes: None,
            });
        }

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
            let relative_path: String = normalize_for_manifest(path, root)
                .map_err(|e| FileSystemError::Path(e))?;

            // Apply glob filter
            if !options.filter.is_empty() && !options.filter.matches(&relative_path) {
                continue;
            }

            let stat: StatResult = match self.stat_cache.stat(path) {
                Some(s) => s,
                None => continue, // Skip inaccessible entries
            };

            if stat.is_symlink && !options.follow_symlinks {
                // Validate and capture symlink
                match validate_symlink(path, root) {
                    Ok(info) => symlinks.push(info),
                    Err(e) => {
                        log::warn!("Skipping invalid symlink {}: {}", path.display(), e);
                    }
                }
            } else if stat.is_dir {
                if options.include_empty_dirs {
                    dirs.push(DirEntry { relative_path });
                }
            } else {
                files.push(FileEntry {
                    path: path.to_path_buf(),
                    relative_path,
                    size: stat.size,
                    mtime_us: stat.mtime_us,
                    runnable: is_executable(stat.mode),
                });
            }
        }

        Ok((files, symlinks, dirs))
    }

    /// Create entries from explicit file list.
    fn entries_from_file_list(
        &self,
        input_files: &[PathBuf],
        root: &Path,
        options: &SnapshotOptions,
    ) -> Result<(Vec<FileEntry>, Vec<SymlinkInfo>, Vec<DirEntry>), FileSystemError> {
        let mut files: Vec<FileEntry> = Vec::new();
        let mut symlinks: Vec<SymlinkInfo> = Vec::new();

        for file_path in input_files {
            // Validate path is within root
            if !file_path.starts_with(root) {
                return Err(FileSystemError::PathOutsideRoot {
                    path: file_path.display().to_string(),
                    root: root.display().to_string(),
                });
            }

            // Skip non-existent files
            let stat: StatResult = match self.stat_cache.stat(file_path) {
                Some(s) => s,
                None => {
                    log::warn!("Skipping non-existent file: {}", file_path.display());
                    continue;
                }
            };

            let relative_path: String = normalize_for_manifest(file_path, root)
                .map_err(|e| FileSystemError::Path(e))?;

            if stat.is_symlink && !options.follow_symlinks {
                match validate_symlink(file_path, root) {
                    Ok(info) => symlinks.push(info),
                    Err(e) => {
                        log::warn!("Skipping invalid symlink {}: {}", file_path.display(), e);
                    }
                }
            } else if !stat.is_dir {
                files.push(FileEntry {
                    path: file_path.clone(),
                    relative_path,
                    size: stat.size,
                    mtime_us: stat.mtime_us,
                    runnable: is_executable(stat.mode),
                });
            }
        }

        // No directories when using explicit file list
        Ok((files, symlinks, Vec::new()))
    }

    /// Build manifest with hashing.
    fn build_manifest(
        &self,
        files: Vec<FileEntry>,
        symlinks: Vec<SymlinkInfo>,
        dirs: Vec<DirEntry>,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback<ScanProgress>>,
    ) -> Result<Manifest, FileSystemError> {
        match options.version {
            ManifestVersion::V2023_03_03 => {
                self.build_v2023_manifest(files, options, progress)
            }
            ManifestVersion::V2025_12_04_beta => {
                self.build_v2025_manifest(files, symlinks, dirs, options, progress)
            }
        }
    }

    /// Build v2023-03-03 manifest.
    fn build_v2023_manifest(
        &self,
        files: Vec<FileEntry>,
        _options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback<ScanProgress>>,
    ) -> Result<Manifest, FileSystemError> {
        let total_files: u64 = files.len() as u64;
        let mut paths: Vec<v2023_03_03::ManifestPath> = Vec::with_capacity(files.len());
        let mut bytes_processed: u64 = 0;

        for (idx, file) in files.into_iter().enumerate() {
            // Report progress
            if let Some(cb) = progress {
                let should_continue: bool = cb.on_progress(&ScanProgress {
                    phase: ScanPhase::Hashing,
                    current_path: Some(file.relative_path.clone()),
                    files_processed: idx as u64,
                    total_files: Some(total_files),
                    bytes_processed,
                    total_bytes: None,
                });
                if !should_continue {
                    return Err(FileSystemError::Cancelled);
                }
            }

            // Hash the file
            let hash: String = hash_file(&file.path).map_err(|e| FileSystemError::IoError {
                path: file.path.display().to_string(),
                source: e,
            })?;

            paths.push(v2023_03_03::ManifestPath {
                path: file.relative_path,
                hash,
                size: file.size,
                mtime: file.mtime_us,
            });

            bytes_processed += file.size;
        }

        Ok(Manifest::V2023_03_03(v2023_03_03::AssetManifest::new(
            paths,
        )))
    }

    /// Build v2025-12-04-beta manifest.
    fn build_v2025_manifest(
        &self,
        files: Vec<FileEntry>,
        symlinks: Vec<SymlinkInfo>,
        dirs: Vec<DirEntry>,
        options: &SnapshotOptions,
        progress: Option<&dyn ProgressCallback<ScanProgress>>,
    ) -> Result<Manifest, FileSystemError> {
        let total_files: u64 = files.len() as u64;
        let mut file_paths: Vec<v2025_12_04::ManifestFilePath> =
            Vec::with_capacity(files.len() + symlinks.len());
        let mut bytes_processed: u64 = 0;

        // Process regular files
        for (idx, file) in files.into_iter().enumerate() {
            if let Some(cb) = progress {
                let should_continue: bool = cb.on_progress(&ScanProgress {
                    phase: ScanPhase::Hashing,
                    current_path: Some(file.relative_path.clone()),
                    files_processed: idx as u64,
                    total_files: Some(total_files),
                    bytes_processed,
                    total_bytes: None,
                });
                if !should_continue {
                    return Err(FileSystemError::Cancelled);
                }
            }

            // Hash the file
            let hash: String = hash_file(&file.path).map_err(|e| FileSystemError::IoError {
                path: file.path.display().to_string(),
                source: e,
            })?;

            // Check if file needs chunking
            if file.size > CHUNK_SIZE_V2 {
                let chunkhashes: Vec<String> = self.compute_chunk_hashes(&file.path, file.size)?;
                file_paths.push(
                    v2025_12_04::ManifestFilePath::chunked(
                        file.relative_path,
                        chunkhashes,
                        file.size,
                        file.mtime_us,
                    )
                    .with_runnable(file.runnable),
                );
            } else {
                file_paths.push(
                    v2025_12_04::ManifestFilePath::file(
                        file.relative_path,
                        hash,
                        file.size,
                        file.mtime_us,
                    )
                    .with_runnable(file.runnable),
                );
            }

            bytes_processed += file.size;
        }

        // Add symlinks
        for symlink in symlinks {
            let relative_path: String = normalize_for_manifest(&symlink.path, &options.root)
                .map_err(|e| FileSystemError::Path(e))?;
            file_paths.push(v2025_12_04::ManifestFilePath::symlink(
                relative_path,
                symlink.target,
            ));
        }

        // Build directory entries
        let dir_paths: Vec<v2025_12_04::ManifestDirectoryPath> = dirs
            .into_iter()
            .map(|d: DirEntry| v2025_12_04::ManifestDirectoryPath::new(d.relative_path))
            .collect();

        Ok(Manifest::V2025_12_04_beta(
            v2025_12_04::AssetManifest::snapshot(dir_paths, file_paths),
        ))
    }

    /// Build manifest without hashing (structure only).
    fn build_manifest_without_hashing(
        &self,
        files: Vec<FileEntry>,
        symlinks: Vec<SymlinkInfo>,
        dirs: Vec<DirEntry>,
        options: &SnapshotOptions,
    ) -> Result<Manifest, FileSystemError> {
        let root: &Path = &options.root;
        match options.version {
            ManifestVersion::V2023_03_03 => {
                let paths: Vec<v2023_03_03::ManifestPath> = files
                    .into_iter()
                    .map(|f: FileEntry| v2023_03_03::ManifestPath {
                        path: f.relative_path,
                        hash: String::new(), // Empty hash
                        size: f.size,
                        mtime: f.mtime_us,
                    })
                    .collect();
                Ok(Manifest::V2023_03_03(v2023_03_03::AssetManifest::new(
                    paths,
                )))
            }
            ManifestVersion::V2025_12_04_beta => {
                let mut file_paths: Vec<v2025_12_04::ManifestFilePath> =
                    Vec::with_capacity(files.len() + symlinks.len());

                for file in files {
                    file_paths.push(
                        v2025_12_04::ManifestFilePath::file(
                            file.relative_path,
                            "", // Empty hash
                            file.size,
                            file.mtime_us,
                        )
                        .with_runnable(file.runnable),
                    );
                }

                for symlink in symlinks {
                    let relative_path: String =
                        normalize_for_manifest(&symlink.path, root)
                            .map_err(|e| FileSystemError::Path(e))?;
                    file_paths.push(v2025_12_04::ManifestFilePath::symlink(
                        relative_path,
                        symlink.target,
                    ));
                }

                let dir_paths: Vec<v2025_12_04::ManifestDirectoryPath> = dirs
                    .into_iter()
                    .map(|d: DirEntry| v2025_12_04::ManifestDirectoryPath::new(d.relative_path))
                    .collect();

                Ok(Manifest::V2025_12_04_beta(
                    v2025_12_04::AssetManifest::snapshot(dir_paths, file_paths),
                ))
            }
        }
    }

    /// Compute chunk hashes for a large file.
    fn compute_chunk_hashes(
        &self,
        path: &Path,
        size: u64,
    ) -> Result<Vec<String>, FileSystemError> {
        use rusty_attachments_common::Xxh3Hasher;
        use std::io::Read;

        let mut file: std::fs::File =
            std::fs::File::open(path).map_err(|e| FileSystemError::IoError {
                path: path.display().to_string(),
                source: e,
            })?;

        let chunk_count: usize = ((size + CHUNK_SIZE_V2 - 1) / CHUNK_SIZE_V2) as usize;
        let mut hashes: Vec<String> = Vec::with_capacity(chunk_count);
        let mut buffer: Vec<u8> = vec![0u8; 64 * 1024]; // 64KB read buffer
        let mut remaining: u64 = size;

        for _ in 0..chunk_count {
            let chunk_size: u64 = std::cmp::min(CHUNK_SIZE_V2, remaining);
            let mut hasher: Xxh3Hasher = Xxh3Hasher::new();
            let mut chunk_remaining: u64 = chunk_size;

            while chunk_remaining > 0 {
                let to_read: usize = std::cmp::min(buffer.len() as u64, chunk_remaining) as usize;
                let bytes_read: usize = file
                    .read(&mut buffer[..to_read])
                    .map_err(|e| FileSystemError::IoError {
                        path: path.display().to_string(),
                        source: e,
                    })?;

                if bytes_read == 0 {
                    break;
                }

                hasher.update(&buffer[..bytes_read]);
                chunk_remaining -= bytes_read as u64;
            }

            hashes.push(hasher.finish_hex());
            remaining -= chunk_size;
        }

        Ok(hashes)
    }

    /// Clear the stat cache.
    pub fn clear_cache(&self) {
        self.stat_cache.clear();
    }
}

impl Default for FileSystemScanner {
    fn default() -> Self {
        Self::new()
    }
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
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_files(dir: &Path) {
        let mut f1 = std::fs::File::create(dir.join("file1.txt")).unwrap();
        f1.write_all(b"hello world").unwrap();

        let mut f2 = std::fs::File::create(dir.join("file2.txt")).unwrap();
        f2.write_all(b"goodbye").unwrap();

        std::fs::create_dir(dir.join("subdir")).unwrap();
        let mut f3 = std::fs::File::create(dir.join("subdir/nested.txt")).unwrap();
        f3.write_all(b"nested content").unwrap();
    }

    #[test]
    fn test_snapshot_v2023() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_files(dir.path());

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: dir.path().to_path_buf(),
            version: ManifestVersion::V2023_03_03,
            ..Default::default()
        };

        let manifest: Manifest = scanner.snapshot(&options, None).unwrap();

        assert_eq!(manifest.version(), ManifestVersion::V2023_03_03);
        assert_eq!(manifest.file_count(), 3);
    }

    #[test]
    fn test_snapshot_v2025() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_files(dir.path());

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: dir.path().to_path_buf(),
            version: ManifestVersion::V2025_12_04_beta,
            ..Default::default()
        };

        let manifest: Manifest = scanner.snapshot(&options, None).unwrap();

        assert_eq!(manifest.version(), ManifestVersion::V2025_12_04_beta);
        assert_eq!(manifest.file_count(), 3);
    }

    #[test]
    fn test_snapshot_with_filter() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_files(dir.path());

        // Also create a .tmp file
        std::fs::File::create(dir.path().join("temp.tmp")).unwrap();

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: dir.path().to_path_buf(),
            filter: GlobFilter::exclude(vec!["**/*.tmp".to_string()]).unwrap(),
            ..Default::default()
        };

        let manifest: Manifest = scanner.snapshot(&options, None).unwrap();

        assert_eq!(manifest.file_count(), 3); // Excludes .tmp
    }

    #[test]
    fn test_snapshot_structure() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_files(dir.path());

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: dir.path().to_path_buf(),
            version: ManifestVersion::V2023_03_03,
            ..Default::default()
        };

        let manifest: Manifest = scanner.snapshot_structure(&options, None).unwrap();

        assert_eq!(manifest.file_count(), 3);
        // Hashes should be empty
        if let Manifest::V2023_03_03(m) = manifest {
            for path in &m.paths {
                assert!(path.hash.is_empty());
            }
        }
    }

    #[test]
    fn test_snapshot_with_input_files() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_files(dir.path());

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: dir.path().to_path_buf(),
            input_files: Some(vec![
                dir.path().join("file1.txt"),
                dir.path().join("subdir/nested.txt"),
            ]),
            ..Default::default()
        };

        let manifest: Manifest = scanner.snapshot(&options, None).unwrap();

        assert_eq!(manifest.file_count(), 2);
    }

    #[test]
    fn test_snapshot_cancellation() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_files(dir.path());

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };

        // Progress callback that cancels after first file
        let callback = rusty_attachments_common::progress_fn(|p: &ScanProgress| {
            p.phase != ScanPhase::Hashing || p.files_processed == 0
        });

        let result = scanner.snapshot(&options, Some(&callback));
        assert!(matches!(result, Err(FileSystemError::Cancelled)));
    }

    #[cfg(unix)]
    #[test]
    fn test_snapshot_with_symlink() {
        let dir: TempDir = TempDir::new().unwrap();
        create_test_files(dir.path());

        // Create a symlink
        let link_path: PathBuf = dir.path().join("link.txt");
        std::os::unix::fs::symlink("file1.txt", &link_path).unwrap();

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let options: SnapshotOptions = SnapshotOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            ..Default::default()
        };

        let manifest: Manifest = scanner.snapshot(&options, None).unwrap();

        // Should have 3 files + 1 symlink
        assert_eq!(manifest.file_count(), 4);
    }
}
