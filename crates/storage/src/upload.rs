//! Upload orchestration for CAS storage.
//!
//! This module provides high-level upload operations that work with any
//! `StorageClient` implementation. It handles:
//!
//! - Manifest-based uploads (v1 and v2 formats)
//! - Parallel file uploads (CRT handles internal parallelism)
//! - Chunked file uploads for large files
//! - Existence checking to skip already-uploaded files
//! - Progress reporting and cancellation
//! - Small/large file queue separation for optimal throughput
//!
//! # Upload Strategy
//!
//! Files are separated into small and large queues:
//! - Small files (<80MB): Uploaded in parallel batches
//! - Large files (>=80MB): Uploaded with CRT's internal multipart parallelism
//!
//! The CRT backend handles connection pooling and optimal parallelism internally.
//!
//! # Example
//!
//! ```ignore
//! use rusty_attachments_storage::{UploadOrchestrator, S3Location};
//!
//! let orchestrator = UploadOrchestrator::new(client, location);
//! let stats = orchestrator.upload_manifest_contents(
//!     &manifest,
//!     "/local/source/root",
//!     Some(&progress_callback),
//! ).await?;
//! ```

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use rusty_attachments_model::{HashAlgorithm, Manifest};

use crate::cas::{generate_chunks, ChunkInfo};
use crate::error::StorageError;
use crate::s3_check_cache::S3CheckCache;
use crate::traits::{ProgressCallback, StorageClient};
use crate::types::{
    OperationType, S3Location, TransferProgress, TransferStatistics, CHUNK_SIZE_V2,
};

/// Default threshold for small vs large file classification (80MB).
///
/// Small files benefit from parallel object uploads.
/// Large files benefit from serial processing with parallel multipart.
pub const SMALL_FILE_THRESHOLD: u64 = 80 * 1024 * 1024;

/// Default concurrency for parallel uploads.
pub const DEFAULT_UPLOAD_CONCURRENCY: usize = 10;

/// Options for upload operations.
#[derive(Debug, Clone)]
pub struct UploadOptions {
    /// Maximum concurrent uploads for small files.
    pub max_concurrency: usize,
    /// Threshold for small vs large file separation.
    pub small_file_threshold: u64,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            max_concurrency: DEFAULT_UPLOAD_CONCURRENCY,
            small_file_threshold: SMALL_FILE_THRESHOLD,
        }
    }
}

impl UploadOptions {
    /// Create options with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum concurrency for parallel uploads.
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

/// High-level upload operations using any StorageClient implementation.
pub struct UploadOrchestrator<'a, C: StorageClient> {
    /// The storage client for S3 operations.
    client: &'a C,
    /// S3 location configuration.
    location: S3Location,
    /// Optional S3 check cache for skipping existence checks.
    s3_check_cache: Option<S3CheckCache>,
    /// Upload options.
    options: UploadOptions,
}

impl<'a, C: StorageClient> UploadOrchestrator<'a, C> {
    /// Create a new upload orchestrator.
    ///
    /// # Arguments
    /// * `client` - Storage client for S3 operations
    /// * `location` - S3 location configuration
    pub fn new(client: &'a C, location: S3Location) -> Self {
        Self {
            client,
            location,
            s3_check_cache: None,
            options: UploadOptions::default(),
        }
    }

    /// Set the S3 check cache for skipping existence checks.
    ///
    /// # Arguments
    /// * `cache` - S3 check cache instance
    pub fn with_s3_check_cache(mut self, cache: S3CheckCache) -> Self {
        self.s3_check_cache = Some(cache);
        self
    }

    /// Set upload options.
    ///
    /// # Arguments
    /// * `options` - Upload options
    pub fn with_options(mut self, options: UploadOptions) -> Self {
        self.options = options;
        self
    }

    /// Upload all files referenced by a manifest to CAS.
    ///
    /// Uploads are performed in parallel:
    /// - Small files (<80MB by default) are uploaded concurrently
    /// - Large files use CRT's internal multipart parallelism
    ///
    /// Note: This uploads the *contents* of a manifest (the actual files).
    /// To upload the manifest JSON itself, use `manifest_storage::upload_input_manifest()`.
    ///
    /// # Arguments
    /// * `manifest` - The manifest containing files to upload
    /// * `source_root` - Local root path where files are located
    /// * `progress` - Optional progress callback
    ///
    /// # Returns
    /// Transfer statistics including files processed, transferred, and skipped.
    pub async fn upload_manifest_contents(
        &self,
        manifest: &Manifest,
        source_root: &str,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let hash_alg: HashAlgorithm = manifest.hash_alg();

        // Collect all uploadable entries with their metadata
        let entries: Vec<UploadEntry> = self.collect_upload_entries(manifest, source_root);

        // Calculate totals for progress reporting
        let total_files: u64 = entries.len() as u64;
        let total_bytes: u64 = entries.iter().map(|e| e.size).sum();

        // Separate into small and large file queues
        let (small_files, large_files): (Vec<UploadEntry>, Vec<UploadEntry>) = entries
            .into_iter()
            .partition(|e| e.size < self.options.small_file_threshold);

        // Upload small files in parallel
        let small_stats: TransferStatistics = self
            .upload_files_parallel(
                small_files,
                hash_alg,
                total_files,
                total_bytes,
                &TransferStatistics::default(),
                progress,
            )
            .await?;

        // Upload large files in parallel (CRT handles internal multipart parallelism)
        let large_stats: TransferStatistics = self
            .upload_files_parallel(
                large_files,
                hash_alg,
                total_files,
                total_bytes,
                &small_stats,
                progress,
            )
            .await?;

        let mut stats = TransferStatistics::default();
        stats.merge(small_stats);
        stats.merge(large_stats);

        Ok(stats)
    }

    /// Upload files in parallel using buffer_unordered.
    async fn upload_files_parallel(
        &self,
        entries: Vec<UploadEntry>,
        hash_alg: HashAlgorithm,
        total_files: u64,
        total_bytes: u64,
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

        // Create upload futures
        let results: Vec<Result<TransferStatistics, StorageError>> = stream::iter(entries)
            .map(|entry| {
                let completed_count = Arc::clone(&completed_count);
                let completed_bytes = Arc::clone(&completed_bytes);
                let cancelled = Arc::clone(&cancelled);

                async move {
                    // Check for cancellation
                    if cancelled.load(Ordering::Relaxed) {
                        return Err(StorageError::Cancelled);
                    }

                    // Report progress before upload
                    if let Some(cb) = progress {
                        let progress_update = TransferProgress {
                            operation: OperationType::Uploading,
                            current_key: entry.local_path.clone(),
                            current_bytes: 0,
                            current_total: entry.size,
                            overall_completed: completed_count.load(Ordering::Relaxed),
                            overall_total: total_files,
                            overall_bytes: completed_bytes.load(Ordering::Relaxed),
                            overall_total_bytes: total_bytes,
                        };
                        if !cb.on_progress(&progress_update) {
                            cancelled.store(true, Ordering::Relaxed);
                            return Err(StorageError::Cancelled);
                        }
                    }

                    // Upload the file
                    let result: TransferStatistics =
                        self.upload_entry(&entry, hash_alg, progress).await?;

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

    /// Check if a CAS object already exists with correct size.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm
    /// * `expected_size` - Expected file size
    ///
    /// # Returns
    /// True if object exists with matching size.
    pub async fn cas_object_exists(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        expected_size: u64,
    ) -> Result<bool, StorageError> {
        let s3_key: String = self.location.cas_key(hash, algorithm);

        // Check cache first
        if let Some(ref cache) = self.s3_check_cache {
            if cache.exists(&self.location.bucket, &s3_key).await {
                return Ok(true);
            }
        }

        // Check S3
        let existing_size: Option<u64> = self
            .client
            .head_object(&self.location.bucket, &s3_key)
            .await?;

        match existing_size {
            Some(size) if size == expected_size => {
                // Update cache
                if let Some(ref cache) = self.s3_check_cache {
                    cache.mark_uploaded(&self.location.bucket, &s3_key).await;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Collect uploadable entries from a manifest.
    fn collect_upload_entries(&self, manifest: &Manifest, source_root: &str) -> Vec<UploadEntry> {
        let mut entries: Vec<UploadEntry> = Vec::new();

        match manifest {
            Manifest::V2023_03_03(m) => {
                for path in &m.paths {
                    entries.push(UploadEntry {
                        local_path: format!("{}/{}", source_root, path.path),
                        hash: Some(path.hash.clone()),
                        chunkhashes: None,
                        size: path.size,
                    });
                }
            }
            Manifest::V2025_12(m) => {
                for file in &m.files {
                    // Skip deleted entries and symlinks
                    if file.deleted || file.symlink_target.is_some() {
                        continue;
                    }

                    entries.push(UploadEntry {
                        local_path: format!("{}/{}", source_root, file.path),
                        hash: file.hash.clone(),
                        chunkhashes: file.chunkhashes.clone(),
                        size: file.size.unwrap_or(0),
                    });
                }
            }
        }

        entries
    }

    /// Upload a single entry (file or chunked file).
    async fn upload_entry(
        &self,
        entry: &UploadEntry,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        if let Some(ref hash) = entry.hash {
            // Single file upload
            self.upload_single_file(&entry.local_path, hash, entry.size, hash_alg, progress)
                .await
        } else if let Some(ref chunkhashes) = entry.chunkhashes {
            // Chunked file upload
            self.upload_chunked_file(
                &entry.local_path,
                chunkhashes,
                entry.size,
                hash_alg,
                progress,
            )
            .await
        } else {
            Err(StorageError::Other {
                message: format!("Entry {} has no hash or chunkhashes", entry.local_path),
            })
        }
    }

    /// Upload a single file to CAS.
    async fn upload_single_file(
        &self,
        local_path: &str,
        hash: &str,
        size: u64,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        // Check if already exists
        if self.cas_object_exists(hash, hash_alg, size).await? {
            return Ok(TransferStatistics::skipped(size));
        }

        // Verify file exists
        if !Path::new(local_path).exists() {
            return Err(StorageError::IoError {
                path: local_path.to_string(),
                message: "File not found".to_string(),
            });
        }

        // Upload
        let s3_key: String = self.location.cas_key(hash, hash_alg);
        self.client
            .put_object_from_file(
                &self.location.bucket,
                &s3_key,
                local_path,
                None,
                None,
                progress,
            )
            .await?;

        // Update cache
        if let Some(ref cache) = self.s3_check_cache {
            cache.mark_uploaded(&self.location.bucket, &s3_key).await;
        }

        Ok(TransferStatistics::uploaded(size))
    }

    /// Upload a chunked file to CAS (each chunk is a separate CAS object).
    async fn upload_chunked_file(
        &self,
        local_path: &str,
        chunkhashes: &[String],
        total_size: u64,
        hash_alg: HashAlgorithm,
        progress: Option<&dyn ProgressCallback>,
    ) -> Result<TransferStatistics, StorageError> {
        let chunks: Vec<ChunkInfo> = generate_chunks(total_size, CHUNK_SIZE_V2);
        let mut stats = TransferStatistics::default();

        // Verify file exists
        if !Path::new(local_path).exists() {
            return Err(StorageError::IoError {
                path: local_path.to_string(),
                message: "File not found".to_string(),
            });
        }

        for (chunk, hash) in chunks.iter().zip(chunkhashes.iter()) {
            // Check if chunk already exists
            if self.cas_object_exists(hash, hash_alg, chunk.length).await? {
                stats.files_skipped += 1;
                stats.bytes_skipped += chunk.length;
                continue;
            }

            // Upload chunk
            let s3_key: String = self.location.cas_key(hash, hash_alg);
            self.client
                .put_object_from_file_range(
                    &self.location.bucket,
                    &s3_key,
                    local_path,
                    chunk.offset,
                    chunk.length,
                    progress,
                )
                .await?;

            // Update cache
            if let Some(ref cache) = self.s3_check_cache {
                cache.mark_uploaded(&self.location.bucket, &s3_key).await;
            }

            stats.files_transferred += 1;
            stats.bytes_transferred += chunk.length;
        }

        stats.files_processed = 1; // One logical file
        Ok(stats)
    }
}

/// Internal representation of a file to upload.
#[derive(Debug, Clone)]
struct UploadEntry {
    /// Local file path.
    local_path: String,
    /// File hash (for single-object uploads).
    hash: Option<String>,
    /// Chunk hashes (for chunked uploads).
    chunkhashes: Option<Vec<String>>,
    /// File size in bytes.
    size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_separate_files_by_size() {
        let entries: Vec<UploadEntry> = vec![
            UploadEntry {
                local_path: "small1.txt".to_string(),
                hash: Some("hash1".to_string()),
                chunkhashes: None,
                size: 1000,
            },
            UploadEntry {
                local_path: "large1.bin".to_string(),
                hash: Some("hash2".to_string()),
                chunkhashes: None,
                size: 100_000_000, // 100MB
            },
            UploadEntry {
                local_path: "small2.txt".to_string(),
                hash: Some("hash3".to_string()),
                chunkhashes: None,
                size: 5000,
            },
        ];

        let threshold: u64 = SMALL_FILE_THRESHOLD;
        let (small, large): (Vec<&UploadEntry>, Vec<&UploadEntry>) =
            entries.iter().partition(|e| e.size < threshold);

        assert_eq!(small.len(), 2);
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].local_path, "large1.bin");
    }

    #[test]
    fn test_upload_options_default() {
        let options = UploadOptions::default();
        assert_eq!(options.max_concurrency, DEFAULT_UPLOAD_CONCURRENCY);
        assert_eq!(options.small_file_threshold, SMALL_FILE_THRESHOLD);
    }

    #[test]
    fn test_upload_options_with_concurrency() {
        let options = UploadOptions::default().with_max_concurrency(20);
        assert_eq!(options.max_concurrency, 20);
    }

    #[test]
    fn test_upload_options_with_threshold() {
        let options = UploadOptions::default().with_small_file_threshold(100 * 1024 * 1024);
        assert_eq!(options.small_file_threshold, 100 * 1024 * 1024);
    }
}
