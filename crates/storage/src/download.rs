//! Download orchestration for CAS storage.
//!
//! This module provides high-level download operations that work with any
//! `StorageClient` implementation. It handles:
//!
//! - Manifest-based downloads (v1 and v2 formats)
//! - Parallel file downloads (CRT handles internal parallelism)
//! - Chunked file reassembly for large files
//! - Symlink creation
//! - Conflict resolution (skip, overwrite, create copy)
//! - Post-download verification and metadata setting
//! - Progress reporting and cancellation
//!
//! # Download Strategy
//!
//! Files are separated into small and large queues:
//! - Small files (<80MB): Downloaded in parallel batches
//! - Large files (>=80MB): Downloaded with CRT's internal multipart parallelism
//!
//! The CRT backend handles connection pooling and optimal parallelism internally.
//!
//! # Example
//!
//! ```ignore
//! use rusty_attachments_storage::{DownloadOrchestrator, S3Location, ConflictResolution};
//!
//! let orchestrator = DownloadOrchestrator::new(client, location);
//! let stats = orchestrator.download_manifest_contents(
//!     &manifest,
//!     "/local/destination/root",
//!     ConflictResolution::CreateCopy,
//!     Some(&progress_callback),
//! ).await?;
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use rusty_attachments_model::{HashAlgorithm, Manifest};

use crate::cas::{generate_chunks, ChunkInfo};
use crate::error::StorageError;
use crate::traits::{ProgressCallback, StorageClient};
use crate::types::{
    ConflictResolution, OperationType, S3Location, TransferProgress, TransferStatistics,
    CHUNK_SIZE_V2,
};

/// Default threshold for small vs large file classification (80MB).
pub const SMALL_FILE_THRESHOLD: u64 = 80 * 1024 * 1024;

/// Default concurrency for parallel downloads.
pub const DEFAULT_DOWNLOAD_CONCURRENCY: usize = 10;

/// Options for download operations.
///
/// Controls post-download verification, file metadata handling, and parallelism.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Verify downloaded file size matches manifest.
    pub verify_size: bool,
    /// Set file modification time from manifest.
    pub set_mtime: bool,
    /// Set executable bit on runnable files (Unix only).
    pub set_executable: bool,
    /// Maximum concurrent downloads for small files.
    pub max_concurrency: usize,
    /// Threshold for small vs large file separation.
    pub small_file_threshold: u64,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            verify_size: true,
            set_mtime: true,
            set_executable: true,
            max_concurrency: DEFAULT_DOWNLOAD_CONCURRENCY,
            small_file_threshold: SMALL_FILE_THRESHOLD,
        }
    }
}

impl DownloadOptions {
    /// Create options with all verification enabled (default).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create options with no post-download processing.
    pub fn no_verification() -> Self {
        Self {
            verify_size: false,
            set_mtime: false,
            set_executable: false,
            ..Default::default()
        }
    }

    /// Set maximum concurrency for parallel downloads.
    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency;
        self
    }

    /// Set threshold for small vs large file separation.
    pub fn with_small_file_threshold(mut self, threshold: u64) -> Self {
        self.small_file_threshold = threshold;
        self
    }
}

/// High-level download operations using any StorageClient implementation.
pub struct DownloadOrchestrator<'a, C: StorageClient> {
    /// The storage client for S3 operations.
    client: &'a C,
    /// S3 location configuration.
    location: S3Location,
    /// Download options.
    options: DownloadOptions,
}

impl<'a, C: StorageClient> DownloadOrchestrator<'a, C> {
    /// Create a new download orchestrator.
    ///
    /// # Arguments
    /// * `client` - Storage client for S3 operations
    /// * `location` - S3 location configuration
    pub fn new(client: &'a C, location: S3Location) -> Self {
        Self {
            client,
            location,
            options: DownloadOptions::default(),
        }
    }

    /// Set download options.
    ///
    /// # Arguments
    /// * `options` - Download options
    pub fn with_options(mut self, options: DownloadOptions) -> Self {
        self.options = options;
        self
    }

    /// Download all files referenced by a manifest to local filesystem.
    ///
    /// Downloads are performed in parallel:
    /// - Small files (<80MB by default) are downloaded concurrently
    /// - Large files use CRT's internal multipart parallelism
    ///
    /// Note: This downloads the *contents* of a manifest (the actual files).
    /// To download the manifest JSON itself, use `manifest_storage::download_manifest()`.
    ///
    /// # Arguments
    /// * `manifest` - The manifest containing files to download
    /// * `destination_root` - Local root path for downloaded files
    /// * `conflict_resolution` - How to handle existing files
    /// * `progress` - Optional progress callback
    ///
    /// # Returns
    /// Transfer statistics including files processed, transferred, and skipped.
    pub async fn download_manifest_contents(
        &self,
        manifest: &Manifest,
        destination_root: &str,
        conflict_resolution: ConflictResolution,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let hash_alg: HashAlgorithm = manifest.hash_alg();

        // Collect all downloadable entries
        let entries: Vec<DownloadEntry> = self.collect_download_entries(manifest);

        // Calculate totals for progress reporting
        let total_files: u64 = entries.len() as u64;
        let total_bytes: u64 = entries.iter().map(|e| e.size).sum();

        // Create directories first (v2 manifests)
        if let Manifest::V2025_12(m) = manifest {
            for dir in &m.dirs {
                if !dir.deleted {
                    let dir_path: String = format!("{}/{}", destination_root, dir.path);
                    std::fs::create_dir_all(&dir_path).map_err(|e| StorageError::IoError {
                        path: dir_path,
                        message: e.to_string(),
                    })?;
                }
            }
        }

        // Separate symlinks (handle synchronously) from files (download in parallel)
        let (symlinks, files): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|e| e.symlink_target.is_some());

        // Handle symlinks first (no I/O, just filesystem operations)
        let mut stats = TransferStatistics::default();
        for entry in &symlinks {
            let local_path: String = format!("{}/{}", destination_root, entry.relative_path);
            let path = Path::new(&local_path);
            if let Some(ref target) = entry.symlink_target {
                let result: TransferStatistics = self.create_symlink(path, target)?;
                stats.merge(result);
            }
        }

        // Separate small and large files
        let (small_files, large_files): (Vec<_>, Vec<_>) = files
            .into_iter()
            .partition(|e| e.size < self.options.small_file_threshold);

        // Download small files in parallel
        let ctx = DownloadContext {
            hash_alg,
            conflict_resolution,
            total_files,
            total_bytes,
        };

        let small_stats: TransferStatistics = self
            .download_files_parallel(small_files, destination_root, ctx, &stats, progress)
            .await?;
        stats.merge(small_stats);

        // Download large files (CRT handles internal parallelism for each)
        let large_stats: TransferStatistics = self
            .download_files_parallel(large_files, destination_root, ctx, &stats, progress)
            .await?;
        stats.merge(large_stats);

        Ok(stats)
    }

    /// Download files in parallel using buffer_unordered.
    ///
    /// # Arguments
    /// * `entries` - Files to download
    /// * `destination_root` - Local root path for downloaded files
    /// * `ctx` - Download context with hash algorithm, conflict resolution, and totals
    /// * `current_stats` - Current transfer statistics for progress tracking
    /// * `progress` - Optional progress callback
    async fn download_files_parallel(
        &self,
        entries: Vec<DownloadEntry>,
        destination_root: &str,
        ctx: DownloadContext,
        current_stats: &TransferStatistics,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        if entries.is_empty() {
            return Ok(TransferStatistics::default());
        }

        // Shared state for progress tracking
        let completed_count = Arc::new(AtomicU64::new(
            current_stats.files_processed + current_stats.files_skipped,
        ));
        let completed_bytes = Arc::new(AtomicU64::new(
            current_stats.bytes_transferred + current_stats.bytes_skipped,
        ));
        let cancelled = Arc::new(AtomicBool::new(false));

        let max_concurrency: usize = self.options.max_concurrency.max(1);

        // Create download futures
        let results: Vec<Result<TransferStatistics, StorageError>> = stream::iter(entries)
            .map(|entry| {
                let completed_count = Arc::clone(&completed_count);
                let completed_bytes = Arc::clone(&completed_bytes);
                let cancelled = Arc::clone(&cancelled);
                let local_path: String = format!("{}/{}", destination_root, entry.relative_path);

                async move {
                    // Check for cancellation
                    if cancelled.load(Ordering::Relaxed) {
                        return Err(StorageError::Cancelled);
                    }

                    // Report progress before download
                    if let Some(cb) = progress {
                        let progress_update = TransferProgress {
                            operation: OperationType::Downloading,
                            current_key: entry.relative_path.clone(),
                            current_bytes: 0,
                            current_total: entry.size,
                            overall_completed: completed_count.load(Ordering::Relaxed),
                            overall_total: ctx.total_files,
                            overall_bytes: completed_bytes.load(Ordering::Relaxed),
                            overall_total_bytes: ctx.total_bytes,
                        };
                        if !cb.on_progress(&progress_update) {
                            cancelled.store(true, Ordering::Relaxed);
                            return Err(StorageError::Cancelled);
                        }
                    }

                    // Download the file
                    let result: TransferStatistics = self
                        .download_entry(
                            &entry,
                            &local_path,
                            ctx.hash_alg,
                            ctx.conflict_resolution,
                            progress,
                        )
                        .await?;

                    // Update progress counters
                    completed_count.fetch_add(1, Ordering::Relaxed);
                    completed_bytes.fetch_add(
                        result.bytes_transferred + result.bytes_skipped,
                        Ordering::Relaxed,
                    );

                    Ok(result)
                }
            })
            .buffer_unordered(max_concurrency)
            .collect()
            .await;

        // Aggregate results
        let mut stats = TransferStatistics::default();
        for result in results {
            match result {
                Ok(s) => stats.merge(s),
                Err(StorageError::Cancelled) => return Err(StorageError::Cancelled),
                Err(e) => return Err(e),
            }
        }

        Ok(stats)
    }

    /// Collect downloadable entries from a manifest.
    fn collect_download_entries(&self, manifest: &Manifest) -> Vec<DownloadEntry> {
        let mut entries: Vec<DownloadEntry> = Vec::new();

        match manifest {
            Manifest::V2023_03_03(m) => {
                for path in &m.paths {
                    entries.push(DownloadEntry {
                        relative_path: path.path.clone(),
                        hash: Some(path.hash.clone()),
                        chunkhashes: None,
                        symlink_target: None,
                        size: path.size,
                        mtime: Some(path.mtime),
                        runnable: false,
                    });
                }
            }
            Manifest::V2025_12(m) => {
                for file in &m.files {
                    // Skip deleted entries
                    if file.deleted {
                        continue;
                    }

                    entries.push(DownloadEntry {
                        relative_path: file.path.clone(),
                        hash: file.hash.clone(),
                        chunkhashes: file.chunkhashes.clone(),
                        symlink_target: file.symlink_target.clone(),
                        size: file.size.unwrap_or(0),
                        mtime: file.mtime,
                        runnable: file.runnable,
                    });
                }
            }
        }

        entries
    }

    /// Download a single entry (file or chunked file).
    async fn download_entry(
        &self,
        entry: &DownloadEntry,
        local_path: &str,
        hash_alg: HashAlgorithm,
        conflict_resolution: ConflictResolution,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let path = Path::new(local_path);

        // Handle conflict resolution
        let final_path: PathBuf = if path.exists() {
            match conflict_resolution {
                ConflictResolution::Skip => {
                    return Ok(TransferStatistics::skipped(entry.size));
                }
                ConflictResolution::Overwrite => path.to_path_buf(),
                ConflictResolution::CreateCopy => generate_unique_copy_path(path)?,
            }
        } else {
            path.to_path_buf()
        };

        // Create parent directories
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::IoError {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }

        // Download based on entry type
        let stats: TransferStatistics = if let Some(ref hash) = entry.hash {
            self.download_single_file(&final_path, hash, entry.size, hash_alg, progress)
                .await?
        } else if let Some(ref chunkhashes) = entry.chunkhashes {
            self.download_chunked_file(&final_path, chunkhashes, entry.size, hash_alg, progress)
                .await?
        } else {
            return Err(StorageError::Other {
                message: format!("Entry {} has no hash or chunkhashes", entry.relative_path),
            });
        };

        // Apply post-download options
        self.apply_post_download_options(&final_path, entry)?;

        Ok(stats)
    }

    /// Download a single file from CAS.
    async fn download_single_file(
        &self,
        local_path: &Path,
        hash: &str,
        size: u64,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let s3_key: String = self.location.cas_key(hash, hash_alg);

        self.client
            .get_object_to_file(
                &self.location.bucket,
                &s3_key,
                &local_path.to_string_lossy(),
                progress,
            )
            .await?;

        Ok(TransferStatistics::uploaded(size)) // "uploaded" means transferred
    }

    /// Download a chunked file from CAS (reassemble from chunks).
    async fn download_chunked_file(
        &self,
        local_path: &Path,
        chunkhashes: &[String],
        total_size: u64,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let chunks: Vec<ChunkInfo> = generate_chunks(total_size, CHUNK_SIZE_V2);
        let mut bytes_transferred: u64 = 0;

        for (chunk, hash) in chunks.iter().zip(chunkhashes.iter()) {
            let s3_key: String = self.location.cas_key(hash, hash_alg);

            self.client
                .get_object_to_file_offset(
                    &self.location.bucket,
                    &s3_key,
                    &local_path.to_string_lossy(),
                    chunk.offset,
                    progress,
                )
                .await?;

            bytes_transferred += chunk.length;
        }

        Ok(TransferStatistics {
            files_processed: 1,
            files_transferred: 1,
            bytes_transferred,
            ..Default::default()
        })
    }

    /// Create a symlink.
    fn create_symlink(
        &self,
        link_path: &Path,
        target: &str,
    ) -> Result<TransferStatistics, StorageError> {
        // Create parent directories
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::IoError {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }

        // Remove existing symlink if present
        if link_path.is_symlink() {
            std::fs::remove_file(link_path).map_err(|e| StorageError::IoError {
                path: link_path.display().to_string(),
                message: e.to_string(),
            })?;
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link_path).map_err(|e| StorageError::IoError {
                path: link_path.display().to_string(),
                message: e.to_string(),
            })?;
        }

        #[cfg(windows)]
        {
            // On Windows, try file symlink first, then directory
            if std::os::windows::fs::symlink_file(target, link_path).is_err() {
                std::os::windows::fs::symlink_dir(target, link_path).map_err(|e| {
                    StorageError::IoError {
                        path: link_path.display().to_string(),
                        message: e.to_string(),
                    }
                })?;
            }
        }

        Ok(TransferStatistics {
            files_processed: 1,
            ..Default::default()
        })
    }

    /// Apply post-download options (verification, mtime, executable).
    fn apply_post_download_options(
        &self,
        path: &Path,
        entry: &DownloadEntry,
    ) -> Result<(), StorageError> {
        // Verify size
        if self.options.verify_size {
            verify_file_size(path, entry.size)?;
        }

        // Set mtime
        if self.options.set_mtime {
            if let Some(mtime_us) = entry.mtime {
                set_file_mtime(path, mtime_us)?;
            }
        }

        // Set executable
        if self.options.set_executable && entry.runnable {
            set_file_executable(path)?;
        }

        Ok(())
    }
}

/// Internal representation of a file to download.
#[derive(Debug, Clone)]
struct DownloadEntry {
    /// Relative path within the manifest.
    relative_path: String,
    /// File hash (for single-object downloads).
    hash: Option<String>,
    /// Chunk hashes (for chunked downloads).
    chunkhashes: Option<Vec<String>>,
    /// Symlink target (if this is a symlink).
    symlink_target: Option<String>,
    /// File size in bytes.
    size: u64,
    /// Modification time in microseconds since epoch.
    mtime: Option<i64>,
    /// Whether the file should be executable.
    runnable: bool,
}

/// Context for parallel download operations.
///
/// Groups related parameters to reduce function argument count.
#[derive(Debug, Clone, Copy)]
struct DownloadContext {
    /// Hash algorithm used in the manifest.
    hash_alg: HashAlgorithm,
    /// How to handle existing files.
    conflict_resolution: ConflictResolution,
    /// Total number of files to download.
    total_files: u64,
    /// Total bytes to download.
    total_bytes: u64,
}

// ============================================================================
// Post-Download Utilities
// ============================================================================

/// Verify downloaded file size matches expected size.
///
/// # Arguments
/// * `path` - Path to the downloaded file
/// * `expected_size` - Expected file size in bytes
///
/// # Errors
/// Returns `StorageError::SizeMismatch` if sizes don't match.
pub fn verify_file_size(path: &Path, expected_size: u64) -> Result<(), StorageError> {
    let actual_size: u64 = std::fs::metadata(path)
        .map_err(|e| StorageError::IoError {
            path: path.display().to_string(),
            message: e.to_string(),
        })?
        .len();

    if actual_size != expected_size {
        return Err(StorageError::SizeMismatch {
            key: path.display().to_string(),
            expected: expected_size,
            actual: actual_size,
        });
    }

    Ok(())
}

/// Set file modification time from manifest mtime.
///
/// # Arguments
/// * `path` - Path to the downloaded file
/// * `mtime_us` - Modification time in microseconds since Unix epoch
pub fn set_file_mtime(path: &Path, mtime_us: i64) -> Result<(), StorageError> {
    use std::time::{Duration, UNIX_EPOCH};

    // Convert microseconds to seconds and nanoseconds
    let secs: u64 = (mtime_us / 1_000_000) as u64;
    let nanos: u32 = ((mtime_us % 1_000_000) * 1000) as u32;

    let mtime = UNIX_EPOCH + Duration::new(secs, nanos);

    // Use filetime crate for cross-platform mtime setting
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(mtime)).map_err(|e| {
        StorageError::IoError {
            path: path.display().to_string(),
            message: format!("failed to set mtime: {}", e),
        }
    })
}

/// Set executable bit on a file (Unix only).
///
/// On Windows, this is a no-op.
#[cfg(unix)]
pub fn set_file_executable(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path).map_err(|e| StorageError::IoError {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    let mut perms = metadata.permissions();
    // Add execute bit for owner, group, and others (matching read bits)
    let mode: u32 = perms.mode();
    let new_mode: u32 = mode | ((mode & 0o444) >> 2);
    perms.set_mode(new_mode);

    std::fs::set_permissions(path, perms).map_err(|e| StorageError::IoError {
        path: path.display().to_string(),
        message: format!("failed to set executable: {}", e),
    })
}

#[cfg(not(unix))]
pub fn set_file_executable(_path: &Path) -> Result<(), StorageError> {
    // No-op on non-Unix platforms
    Ok(())
}

/// Generate a unique file path for conflict resolution.
///
/// Tries "file (1).ext", "file (2).ext", etc. until finding
/// a path that doesn't exist.
///
/// # Arguments
/// * `original` - Original file path
///
/// # Returns
/// A unique path that doesn't exist.
pub fn generate_unique_copy_path(original: &Path) -> Result<PathBuf, StorageError> {
    let stem: &str = original.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let extension: String = original
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e))
        .unwrap_or_default();
    let parent: &Path = original.parent().unwrap_or(Path::new(""));

    let mut counter: u32 = 1;
    loop {
        let new_name: String = format!("{} ({}){}", stem, counter, extension);
        let candidate: PathBuf = parent.join(&new_name);

        if !candidate.exists() {
            return Ok(candidate);
        }

        counter += 1;
        if counter > 10000 {
            return Err(StorageError::IoError {
                path: original.display().to_string(),
                message: "Too many file conflicts".to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_unique_copy_path() {
        let temp_dir: TempDir = TempDir::new().unwrap();
        let original: PathBuf = temp_dir.path().join("test.txt");

        // First call should return "test (1).txt"
        let result: PathBuf = generate_unique_copy_path(&original).unwrap();
        assert_eq!(
            result.file_name().unwrap().to_str().unwrap(),
            "test (1).txt"
        );

        // Create the file
        std::fs::write(&result, "content").unwrap();

        // Second call should return "test (2).txt"
        let result2: PathBuf = generate_unique_copy_path(&original).unwrap();
        assert_eq!(
            result2.file_name().unwrap().to_str().unwrap(),
            "test (2).txt"
        );
    }

    #[test]
    fn test_generate_unique_copy_path_no_extension() {
        let temp_dir: TempDir = TempDir::new().unwrap();
        let original: PathBuf = temp_dir.path().join("README");

        let result: PathBuf = generate_unique_copy_path(&original).unwrap();
        assert_eq!(result.file_name().unwrap().to_str().unwrap(), "README (1)");
    }

    #[test]
    fn test_verify_file_size() {
        let temp_dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();

        // Correct size should pass
        assert!(verify_file_size(&file_path, 5).is_ok());

        // Wrong size should fail
        let result = verify_file_size(&file_path, 10);
        assert!(matches!(result, Err(StorageError::SizeMismatch { .. })));
    }

    #[test]
    fn test_download_options_default() {
        let options = DownloadOptions::default();
        assert!(options.verify_size);
        assert!(options.set_mtime);
        assert!(options.set_executable);
        assert_eq!(options.max_concurrency, DEFAULT_DOWNLOAD_CONCURRENCY);
        assert_eq!(options.small_file_threshold, SMALL_FILE_THRESHOLD);
    }

    #[test]
    fn test_download_options_no_verification() {
        let options = DownloadOptions::no_verification();
        assert!(!options.verify_size);
        assert!(!options.set_mtime);
        assert!(!options.set_executable);
        // Concurrency settings should still be default
        assert_eq!(options.max_concurrency, DEFAULT_DOWNLOAD_CONCURRENCY);
    }

    #[test]
    fn test_download_options_with_concurrency() {
        let options = DownloadOptions::default().with_max_concurrency(20);
        assert_eq!(options.max_concurrency, 20);
    }

    #[test]
    fn test_download_options_with_threshold() {
        let options = DownloadOptions::default().with_small_file_threshold(100 * 1024 * 1024);
        assert_eq!(options.small_file_threshold, 100 * 1024 * 1024);
    }
}
