//! Disk-based cache for dirty (modified) files.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{DiskCacheError, WriteCache};

/// Metadata for a chunked file in the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkedFileMeta {
    /// Total number of chunks.
    pub chunk_count: u32,
    /// Original chunk hashes (from manifest).
    pub original_hashes: Vec<String>,
    /// Which chunks are dirty (stored as .partN files).
    pub dirty_chunks: Vec<u32>,
    /// Total file size.
    pub total_size: u64,
}

/// Disk cache for dirty (modified) files with hybrid storage strategy.
///
/// - Small files (single chunk): stored by relative path
/// - Large file chunks: stored as `{path}.part{N}` (only dirty chunks)
///
/// # Directory Structure
/// ```text
/// cache_dir/
/// ├── .deleted/                    # Tombstones for deleted files
/// │   └── path/to/file             # Empty file marking deletion
/// ├── .meta/                       # Metadata for chunked files
/// │   └── path/to/large_video.mp4.json  # Chunk info
/// └── path/to/
///     ├── small_file.txt           # Small file - full content
///     ├── large_video.mp4.part4    # Only dirty chunk 4
///     └── large_video.mp4.part7    # Only dirty chunk 7
/// ```
pub struct MaterializedCache {
    /// Root directory for cache storage.
    cache_dir: PathBuf,
    /// Directory for deletion tombstones.
    deleted_dir: PathBuf,
    /// Directory for chunked file metadata.
    meta_dir: PathBuf,
}

impl MaterializedCache {
    /// Create a new materialized cache.
    ///
    /// # Arguments
    /// * `cache_dir` - Root directory for cache storage
    ///
    /// # Returns
    /// New cache instance. Creates directories if needed.
    pub fn new(cache_dir: PathBuf) -> std::io::Result<Self> {
        let deleted_dir: PathBuf = cache_dir.join(".deleted");
        let meta_dir: PathBuf = cache_dir.join(".meta");
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&deleted_dir)?;
        std::fs::create_dir_all(&meta_dir)?;

        Ok(Self {
            cache_dir,
            deleted_dir,
            meta_dir,
        })
    }

    /// Write a dirty chunk to cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    /// * `chunk_index` - Chunk index (0-based)
    /// * `data` - Chunk data
    pub fn write_chunk(
        &self,
        rel_path: &str,
        chunk_index: u32,
        data: &[u8],
    ) -> std::io::Result<()> {
        let chunk_path: PathBuf = self.chunk_path(rel_path, chunk_index);

        if let Some(parent) = chunk_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write atomically
        let temp_path: PathBuf = chunk_path.with_extension("tmp");
        std::fs::write(&temp_path, data)?;
        std::fs::rename(&temp_path, &chunk_path)?;

        Ok(())
    }

    /// Read a dirty chunk from cache.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    /// * `chunk_index` - Chunk index
    ///
    /// # Returns
    /// Chunk data if found, None otherwise.
    pub fn read_chunk(&self, rel_path: &str, chunk_index: u32) -> std::io::Result<Option<Vec<u8>>> {
        let chunk_path: PathBuf = self.chunk_path(rel_path, chunk_index);

        if chunk_path.exists() {
            Ok(Some(std::fs::read(&chunk_path)?))
        } else {
            Ok(None)
        }
    }

    /// Get path to a chunk file.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    /// * `chunk_index` - Chunk index
    fn chunk_path(&self, rel_path: &str, chunk_index: u32) -> PathBuf {
        self.cache_dir
            .join(format!("{}.part{}", rel_path, chunk_index))
    }

    /// Write metadata for a chunked file.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    /// * `meta` - Chunk metadata
    pub fn write_chunked_meta(
        &self,
        rel_path: &str,
        meta: &ChunkedFileMeta,
    ) -> Result<(), DiskCacheError> {
        let meta_path: PathBuf = self.meta_dir.join(format!("{}.json", rel_path));

        if let Some(parent) = meta_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json: String = serde_json::to_string_pretty(meta)?;
        std::fs::write(&meta_path, json)?;

        Ok(())
    }

    /// Read metadata for a chunked file.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    ///
    /// # Returns
    /// Chunk metadata if found, None otherwise.
    pub fn read_chunked_meta(
        &self,
        rel_path: &str,
    ) -> Result<Option<ChunkedFileMeta>, DiskCacheError> {
        let meta_path: PathBuf = self.meta_dir.join(format!("{}.json", rel_path));

        if meta_path.exists() {
            let json: String = std::fs::read_to_string(&meta_path)?;
            let meta: ChunkedFileMeta = serde_json::from_str(&json)?;
            Ok(Some(meta))
        } else {
            Ok(None)
        }
    }

    /// List all dirty chunks for a file.
    ///
    /// # Arguments
    /// * `rel_path` - Relative path of the file
    ///
    /// # Returns
    /// Sorted vector of chunk indices.
    pub fn list_dirty_chunks(&self, rel_path: &str) -> std::io::Result<Vec<u32>> {
        let parent: PathBuf = self
            .cache_dir
            .join(Path::new(rel_path).parent().unwrap_or(Path::new("")));
        let file_name: &str = Path::new(rel_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        let mut chunks: Vec<u32> = Vec::new();

        if parent.exists() {
            for entry in std::fs::read_dir(&parent)? {
                let entry = entry?;
                let name: String = entry.file_name().to_string_lossy().to_string();

                // Match pattern: {filename}.part{N}
                if let Some(rest) = name.strip_prefix(&format!("{}.", file_name)) {
                    if let Some(idx_str) = rest.strip_prefix("part") {
                        if let Ok(idx) = idx_str.parse::<u32>() {
                            chunks.push(idx);
                        }
                    }
                }
            }
        }

        chunks.sort();
        Ok(chunks)
    }

    /// Recursively walk directory collecting file paths.
    ///
    /// # Arguments
    /// * `dir` - Directory to walk
    /// * `prefix` - Path prefix for relative paths
    /// * `files` - Output vector to collect paths
    #[allow(clippy::only_used_in_recursion)]
    fn walk_dir(&self, dir: &Path, prefix: &str, files: &mut Vec<String>) -> std::io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name: String = entry.file_name().to_string_lossy().to_string();

            // Skip special directories
            if (name == ".deleted" || name == ".meta") && prefix.is_empty() {
                continue;
            }

            let rel_path: String = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", prefix, name)
            };

            if entry.file_type()?.is_dir() {
                self.walk_dir(&entry.path(), &rel_path, files)?;
            } else if !name.contains(".part") && !name.ends_with(".tmp") {
                // Skip chunk files and temp files
                files.push(rel_path);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl WriteCache for MaterializedCache {
    async fn write_file(&self, rel_path: &str, data: &[u8]) -> Result<(), DiskCacheError> {
        let full_path: PathBuf = self.cache_dir.join(rel_path);

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Remove any deletion tombstone
        let tombstone: PathBuf = self.deleted_dir.join(rel_path);
        if tombstone.exists() {
            std::fs::remove_file(&tombstone)?;
        }

        // Write file atomically (write to temp, then rename)
        let temp_path: PathBuf = full_path.with_extension("tmp");
        std::fs::write(&temp_path, data)?;
        std::fs::rename(&temp_path, &full_path)?;

        Ok(())
    }

    async fn read_file(&self, rel_path: &str) -> Result<Option<Vec<u8>>, DiskCacheError> {
        let full_path: PathBuf = self.cache_dir.join(rel_path);

        if full_path.exists() {
            Ok(Some(std::fs::read(&full_path)?))
        } else {
            Ok(None)
        }
    }

    async fn delete_file(&self, rel_path: &str) -> Result<(), DiskCacheError> {
        // Remove actual file if exists
        let full_path: PathBuf = self.cache_dir.join(rel_path);
        if full_path.exists() {
            std::fs::remove_file(&full_path)?;
        }

        // Create tombstone
        let tombstone: PathBuf = self.deleted_dir.join(rel_path);
        if let Some(parent) = tombstone.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tombstone, b"")?;

        Ok(())
    }

    fn is_deleted(&self, rel_path: &str) -> bool {
        self.deleted_dir.join(rel_path).exists()
    }

    async fn list_files(&self) -> Result<Vec<String>, DiskCacheError> {
        let mut files: Vec<String> = Vec::new();
        self.walk_dir(&self.cache_dir, "", &mut files)?;
        Ok(files)
    }

    fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}
