//! v2025-12-04-beta manifest format (extended format).
//!
//! EXPERIMENTAL: This format is under development and subject to change.
//!
//! This format supports:
//! - Directory entries (including empty directories)
//! - Large file chunking (>256MB files use chunkhashes)
//! - Symlinks with relative targets
//! - POSIX execute bit (runnable)
//! - Diff manifests with deletion markers
//! - Directory index compression ($N/ references)

use serde::{Deserialize, Serialize};

use crate::error::{ManifestError, ValidationError};
use crate::hash::{HashAlgorithm, FILE_CHUNK_SIZE_BYTES};
use crate::version::{ManifestType, ManifestVersion};

/// Directory entry in v2025-12-04-beta manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDirectoryPath {
    /// Relative directory path within the manifest.
    pub name: String,
    /// Whether this directory is deleted (diff manifests only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete: bool,
}

impl ManifestDirectoryPath {
    /// Create a new directory entry.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            delete: false,
        }
    }

    /// Create a deleted directory marker.
    pub fn deleted(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            delete: true,
        }
    }
}

/// File entry in v2025-12-04-beta manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFilePath {
    /// Relative file path within the manifest.
    pub name: String,
    /// File content hash (None for symlinks, chunked files, or deleted entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// File size in bytes (None for deleted entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Modification time in microseconds since epoch (None for deleted entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    /// POSIX execute bit.
    #[serde(default, skip_serializing_if = "is_false")]
    pub runnable: bool,
    /// Chunk hashes for files >256MB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunkhashes: Option<Vec<String>>,
    /// Target path for symlinks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    /// Whether this file is deleted (diff manifests only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete: bool,
}

impl ManifestFilePath {
    /// Create a new regular file entry.
    pub fn file(
        name: impl Into<String>,
        hash: impl Into<String>,
        size: u64,
        mtime: i64,
    ) -> Self {
        Self {
            name: name.into(),
            hash: Some(hash.into()),
            size: Some(size),
            mtime: Some(mtime),
            runnable: false,
            chunkhashes: None,
            symlink_target: None,
            delete: false,
        }
    }

    /// Create a symlink entry.
    pub fn symlink(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            hash: None,
            size: None,
            mtime: None,
            runnable: false,
            chunkhashes: None,
            symlink_target: Some(target.into()),
            delete: false,
        }
    }

    /// Create a chunked file entry.
    pub fn chunked(
        name: impl Into<String>,
        chunkhashes: Vec<String>,
        size: u64,
        mtime: i64,
    ) -> Self {
        Self {
            name: name.into(),
            hash: None,
            size: Some(size),
            mtime: Some(mtime),
            runnable: false,
            chunkhashes: Some(chunkhashes),
            symlink_target: None,
            delete: false,
        }
    }

    /// Create a deleted file marker.
    pub fn deleted(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            hash: None,
            size: None,
            mtime: None,
            runnable: false,
            chunkhashes: None,
            symlink_target: None,
            delete: true,
        }
    }

    /// Set the runnable flag.
    pub fn with_runnable(mut self, runnable: bool) -> Self {
        self.runnable = runnable;
        self
    }

    /// Validate this entry according to format rules.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.delete {
            self.validate_deleted()
        } else {
            self.validate_non_deleted()
        }
    }

    fn validate_deleted(&self) -> Result<(), ValidationError> {
        if self.hash.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.name.clone(),
                field: "hash",
            });
        }
        if self.chunkhashes.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.name.clone(),
                field: "chunkhashes",
            });
        }
        if self.symlink_target.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.name.clone(),
                field: "symlink_target",
            });
        }
        if self.runnable {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.name.clone(),
                field: "runnable",
            });
        }
        if self.size.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.name.clone(),
                field: "size",
            });
        }
        if self.mtime.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.name.clone(),
                field: "mtime",
            });
        }
        Ok(())
    }

    fn validate_non_deleted(&self) -> Result<(), ValidationError> {
        // Must have exactly one of: hash, chunkhashes, symlink_target
        let content_fields: [bool; 3] = [
            self.hash.is_some(),
            self.chunkhashes.is_some(),
            self.symlink_target.is_some(),
        ];
        if content_fields.iter().filter(|&&x| x).count() != 1 {
            return Err(ValidationError::InvalidContentFields {
                path: self.name.clone(),
            });
        }

        // Non-symlink entries need size and mtime
        if self.symlink_target.is_none() {
            if self.size.is_none() {
                return Err(ValidationError::MissingSize {
                    path: self.name.clone(),
                });
            }
            if self.mtime.is_none() {
                return Err(ValidationError::MissingMtime {
                    path: self.name.clone(),
                });
            }
        }

        // Validate chunkhashes
        if let Some(ref chunks) = self.chunkhashes {
            self.validate_chunkhashes(chunks)?;
        }

        // Validate symlink target
        if let Some(ref target) = self.symlink_target {
            self.validate_symlink_target(target)?;
        }

        Ok(())
    }

    fn validate_chunkhashes(&self, chunks: &[String]) -> Result<(), ValidationError> {
        let size: u64 = self.size.ok_or_else(|| ValidationError::MissingSize {
            path: self.name.clone(),
        })?;

        if size <= FILE_CHUNK_SIZE_BYTES {
            return Err(ValidationError::ChunkedFileTooSmall {
                path: self.name.clone(),
                chunk_size: FILE_CHUNK_SIZE_BYTES,
                actual_size: size,
            });
        }

        let expected_chunks: usize = size.div_ceil(FILE_CHUNK_SIZE_BYTES) as usize;
        if chunks.len() != expected_chunks {
            return Err(ValidationError::ChunkCountMismatch {
                path: self.name.clone(),
                size,
                expected: expected_chunks,
                actual: chunks.len(),
            });
        }

        Ok(())
    }

    fn validate_symlink_target(&self, target: &str) -> Result<(), ValidationError> {
        // Must be relative (not absolute)
        if target.starts_with('/') {
            return Err(ValidationError::AbsoluteSymlinkTarget {
                path: self.name.clone(),
                target: target.to_string(),
            });
        }

        // Check Windows absolute paths
        if target.len() >= 2 && target.chars().nth(1) == Some(':') {
            return Err(ValidationError::AbsoluteSymlinkTarget {
                path: self.name.clone(),
                target: target.to_string(),
            });
        }

        // Check UNC paths
        if target.starts_with("\\\\") || target.starts_with("//") {
            return Err(ValidationError::AbsoluteSymlinkTarget {
                path: self.name.clone(),
                target: target.to_string(),
            });
        }

        // Check if target escapes root (symlink at root level with ..)
        if !self.name.contains('/') && target.starts_with("..") {
            return Err(ValidationError::EscapingSymlinkTarget {
                path: self.name.clone(),
                target: target.to_string(),
            });
        }

        Ok(())
    }
}

/// Asset manifest v2025-12-04-beta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetManifest {
    /// Hashing algorithm used for file hashes.
    pub hash_alg: HashAlgorithm,
    /// Manifest format version.
    pub manifest_version: ManifestVersion,
    /// List of directory entries.
    pub dirs: Vec<ManifestDirectoryPath>,
    /// List of file entries.
    pub files: Vec<ManifestFilePath>,
    /// Total size of all files in bytes.
    pub total_size: u64,
    /// Type of manifest (snapshot or diff). Inferred from parentManifestHash presence.
    #[serde(skip)]
    pub manifest_type: ManifestType,
    /// Hash of parent manifest (diff manifests only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_manifest_hash: Option<String>,
}

impl AssetManifest {
    /// Create a new v2025-12-04-beta snapshot manifest.
    pub fn snapshot(dirs: Vec<ManifestDirectoryPath>, files: Vec<ManifestFilePath>) -> Self {
        let total_size: u64 = files
            .iter()
            .filter(|f| f.symlink_target.is_none())
            .filter_map(|f| f.size)
            .sum();

        Self {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2025_12_04_beta,
            dirs,
            files,
            total_size,
            manifest_type: ManifestType::Snapshot,
            parent_manifest_hash: None,
        }
    }

    /// Create a new v2025-12-04-beta diff manifest.
    pub fn diff(
        dirs: Vec<ManifestDirectoryPath>,
        files: Vec<ManifestFilePath>,
        parent_hash: impl Into<String>,
    ) -> Self {
        let total_size: u64 = files
            .iter()
            .filter(|f| !f.delete && f.symlink_target.is_none())
            .filter_map(|f| f.size)
            .sum();

        Self {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2025_12_04_beta,
            dirs,
            files,
            total_size,
            manifest_type: ManifestType::Diff,
            parent_manifest_hash: Some(parent_hash.into()),
        }
    }

    /// Validate all entries in the manifest.
    pub fn validate(&self) -> Result<(), ValidationError> {
        // Check for deletions in snapshot manifests
        if self.manifest_type == ManifestType::Snapshot
            && (self.dirs.iter().any(|d| d.delete) || self.files.iter().any(|f| f.delete))
        {
            return Err(ValidationError::SnapshotWithDeletion);
        }

        // Validate each file entry
        for file in &self.files {
            file.validate()?;
        }

        Ok(())
    }

    /// Encode to canonical JSON string with directory index compression.
    pub fn encode(&self) -> Result<String, ManifestError> {
        crate::encode::encode_v2025_12_04(self)
    }

    /// Decode from JSON value.
    pub fn decode(data: serde_json::Value) -> Result<Self, ManifestError> {
        let mut manifest: Self = serde_json::from_value(data)?;
        // Infer manifest type from parentManifestHash presence
        manifest.manifest_type = if manifest.parent_manifest_hash.is_some() {
            ManifestType::Diff
        } else {
            ManifestType::Snapshot
        };
        Ok(manifest)
    }
}

/// Helper for serde skip_serializing_if.
fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Valid cases ====================

    #[test]
    fn test_file_entry_validation() {
        let file: ManifestFilePath = ManifestFilePath::file("test.txt", "abc123", 1024, 1234567890);
        assert!(file.validate().is_ok());
    }

    #[test]
    fn test_file_with_runnable() {
        let file: ManifestFilePath =
            ManifestFilePath::file("script.sh", "abc123", 512, 1234567890).with_runnable(true);
        assert!(file.validate().is_ok());
        assert!(file.runnable);
    }

    #[test]
    fn test_symlink_entry_validation() {
        let symlink: ManifestFilePath = ManifestFilePath::symlink("link.txt", "target.txt");
        assert!(symlink.validate().is_ok());
    }

    #[test]
    fn test_symlink_same_directory() {
        let symlink: ManifestFilePath = ManifestFilePath::symlink("link.txt", "target.txt");
        assert!(symlink.validate().is_ok());
        assert_eq!(symlink.symlink_target, Some("target.txt".to_string()));
    }

    #[test]
    fn test_deleted_entry_validation() {
        let entry: ManifestFilePath = ManifestFilePath::deleted("old.txt");
        assert!(entry.validate().is_ok());
        assert!(entry.delete);
        assert!(entry.hash.is_none());
        assert!(entry.size.is_none());
        assert!(entry.mtime.is_none());
    }

    #[test]
    fn test_chunked_file_valid() {
        // 300MB file = 2 chunks (256MB + 44MB)
        let size: u64 = 300 * 1024 * 1024;
        let chunkhashes: Vec<String> = vec!["hash1".to_string(), "hash2".to_string()];
        let file: ManifestFilePath =
            ManifestFilePath::chunked("large.bin", chunkhashes.clone(), size, 1234567890);
        assert!(file.validate().is_ok());
        assert_eq!(file.chunkhashes, Some(chunkhashes));
        assert!(file.hash.is_none());
    }

    #[test]
    fn test_chunked_file_exact_boundary() {
        // File exactly at chunk boundary + 1 byte needs 2 chunks
        let size: u64 = FILE_CHUNK_SIZE_BYTES + 1;
        let chunkhashes: Vec<String> = vec!["hash1".to_string(), "hash2".to_string()];
        let file: ManifestFilePath =
            ManifestFilePath::chunked("boundary.bin", chunkhashes, size, 1234567890);
        assert!(file.validate().is_ok());
    }

    // ==================== Deleted file validation errors ====================

    #[test]
    fn test_deleted_with_hash_fails() {
        let mut entry: ManifestFilePath = ManifestFilePath::deleted("old.txt");
        entry.hash = Some("abc".to_string());
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::DeletedEntryHasField { field: "hash", .. })
        ));
    }

    #[test]
    fn test_deleted_with_chunkhashes_fails() {
        let mut entry: ManifestFilePath = ManifestFilePath::deleted("old.txt");
        entry.chunkhashes = Some(vec!["hash1".to_string()]);
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::DeletedEntryHasField {
                field: "chunkhashes",
                ..
            })
        ));
    }

    #[test]
    fn test_deleted_with_symlink_target_fails() {
        let mut entry: ManifestFilePath = ManifestFilePath::deleted("old.txt");
        entry.symlink_target = Some("target.txt".to_string());
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::DeletedEntryHasField {
                field: "symlink_target",
                ..
            })
        ));
    }

    #[test]
    fn test_deleted_with_runnable_fails() {
        let mut entry: ManifestFilePath = ManifestFilePath::deleted("old.txt");
        entry.runnable = true;
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::DeletedEntryHasField {
                field: "runnable",
                ..
            })
        ));
    }

    #[test]
    fn test_deleted_with_size_fails() {
        let mut entry: ManifestFilePath = ManifestFilePath::deleted("old.txt");
        entry.size = Some(1024);
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::DeletedEntryHasField { field: "size", .. })
        ));
    }

    #[test]
    fn test_deleted_with_mtime_fails() {
        let mut entry: ManifestFilePath = ManifestFilePath::deleted("old.txt");
        entry.mtime = Some(1234567890);
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::DeletedEntryHasField { field: "mtime", .. })
        ));
    }

    // ==================== Non-deleted file validation errors ====================

    #[test]
    fn test_file_without_content_fails() {
        let entry: ManifestFilePath = ManifestFilePath {
            name: "test.txt".to_string(),
            hash: None,
            size: Some(100),
            mtime: Some(1234567890),
            runnable: false,
            chunkhashes: None,
            symlink_target: None,
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::InvalidContentFields { .. })
        ));
    }

    #[test]
    fn test_file_with_multiple_content_fields_fails() {
        let entry: ManifestFilePath = ManifestFilePath {
            name: "test.txt".to_string(),
            hash: Some("abc123".to_string()),
            size: Some(300 * 1024 * 1024),
            mtime: Some(1234567890),
            runnable: false,
            chunkhashes: Some(vec!["hash1".to_string(), "hash2".to_string()]),
            symlink_target: None,
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::InvalidContentFields { .. })
        ));
    }

    #[test]
    fn test_file_with_hash_and_symlink_fails() {
        let entry: ManifestFilePath = ManifestFilePath {
            name: "test.txt".to_string(),
            hash: Some("abc123".to_string()),
            size: Some(1024),
            mtime: Some(1234567890),
            runnable: false,
            chunkhashes: None,
            symlink_target: Some("target.txt".to_string()),
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::InvalidContentFields { .. })
        ));
    }

    #[test]
    fn test_regular_file_without_size_fails() {
        let entry: ManifestFilePath = ManifestFilePath {
            name: "test.txt".to_string(),
            hash: Some("abc123".to_string()),
            size: None,
            mtime: Some(1234567890),
            runnable: false,
            chunkhashes: None,
            symlink_target: None,
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(result, Err(ValidationError::MissingSize { .. })));
    }

    #[test]
    fn test_regular_file_without_mtime_fails() {
        let entry: ManifestFilePath = ManifestFilePath {
            name: "test.txt".to_string(),
            hash: Some("abc123".to_string()),
            size: Some(1024),
            mtime: None,
            runnable: false,
            chunkhashes: None,
            symlink_target: None,
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(result, Err(ValidationError::MissingMtime { .. })));
    }

    // ==================== Chunkhashes validation errors ====================

    #[test]
    fn test_chunked_file_without_size_fails() {
        let entry: ManifestFilePath = ManifestFilePath {
            name: "large.bin".to_string(),
            hash: None,
            size: None,
            mtime: Some(1234567890),
            runnable: false,
            chunkhashes: Some(vec!["hash1".to_string(), "hash2".to_string()]),
            symlink_target: None,
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(result, Err(ValidationError::MissingSize { .. })));
    }

    #[test]
    fn test_chunked_file_size_too_small_fails() {
        let entry: ManifestFilePath = ManifestFilePath {
            name: "small.bin".to_string(),
            hash: None,
            size: Some(1024), // Too small for chunking
            mtime: Some(1234567890),
            runnable: false,
            chunkhashes: Some(vec!["hash1".to_string()]),
            symlink_target: None,
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::ChunkedFileTooSmall { .. })
        ));
    }

    #[test]
    fn test_chunked_file_wrong_chunk_count_fails() {
        // 300MB = 2 chunks, but we provide 3
        let size: u64 = 300 * 1024 * 1024;
        let entry: ManifestFilePath = ManifestFilePath {
            name: "large.bin".to_string(),
            hash: None,
            size: Some(size),
            mtime: Some(1234567890),
            runnable: false,
            chunkhashes: Some(vec![
                "hash1".to_string(),
                "hash2".to_string(),
                "hash3".to_string(),
            ]),
            symlink_target: None,
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::ChunkCountMismatch {
                expected: 2,
                actual: 3,
                ..
            })
        ));
    }

    #[test]
    fn test_chunked_file_too_few_chunks_fails() {
        // 600MB = 3 chunks, but we provide 2
        let size: u64 = 600 * 1024 * 1024;
        let entry: ManifestFilePath = ManifestFilePath {
            name: "large.bin".to_string(),
            hash: None,
            size: Some(size),
            mtime: Some(1234567890),
            runnable: false,
            chunkhashes: Some(vec!["hash1".to_string(), "hash2".to_string()]),
            symlink_target: None,
            delete: false,
        };
        let result: Result<(), ValidationError> = entry.validate();
        assert!(matches!(
            result,
            Err(ValidationError::ChunkCountMismatch {
                expected: 3,
                actual: 2,
                ..
            })
        ));
    }

    // ==================== Symlink validation ====================

    #[test]
    fn test_absolute_symlink_fails() {
        let symlink: ManifestFilePath = ManifestFilePath::symlink("link.txt", "/absolute/path");
        let result: Result<(), ValidationError> = symlink.validate();
        assert!(matches!(
            result,
            Err(ValidationError::AbsoluteSymlinkTarget { .. })
        ));
    }

    #[test]
    fn test_windows_absolute_symlink_fails() {
        let symlink: ManifestFilePath = ManifestFilePath::symlink("link.txt", "C:\\absolute\\path");
        let result: Result<(), ValidationError> = symlink.validate();
        assert!(matches!(
            result,
            Err(ValidationError::AbsoluteSymlinkTarget { .. })
        ));
    }

    #[test]
    fn test_unc_path_symlink_fails() {
        let symlink: ManifestFilePath =
            ManifestFilePath::symlink("link.txt", "\\\\server\\share\\file");
        let result: Result<(), ValidationError> = symlink.validate();
        assert!(matches!(
            result,
            Err(ValidationError::AbsoluteSymlinkTarget { .. })
        ));
    }

    #[test]
    fn test_unc_path_forward_slash_symlink_fails() {
        let symlink: ManifestFilePath =
            ManifestFilePath::symlink("link.txt", "//server/share/file");
        let result: Result<(), ValidationError> = symlink.validate();
        assert!(matches!(
            result,
            Err(ValidationError::AbsoluteSymlinkTarget { .. })
        ));
    }

    #[test]
    fn test_escaping_symlink_fails() {
        let symlink: ManifestFilePath = ManifestFilePath::symlink("link.txt", "../outside.txt");
        let result: Result<(), ValidationError> = symlink.validate();
        assert!(matches!(
            result,
            Err(ValidationError::EscapingSymlinkTarget { .. })
        ));
    }

    #[test]
    fn test_dotdot_within_root_ok() {
        let symlink: ManifestFilePath =
            ManifestFilePath::symlink("subdir/link.txt", "../target.txt");
        assert!(symlink.validate().is_ok());
    }

    #[test]
    fn test_symlink_deep_nested_dotdot_ok() {
        let symlink: ManifestFilePath =
            ManifestFilePath::symlink("a/b/link.txt", "../../target.txt");
        assert!(symlink.validate().is_ok());
    }

    // ==================== Manifest validation ====================

    #[test]
    fn test_snapshot_manifest_with_deletion_fails() {
        let files: Vec<ManifestFilePath> = vec![ManifestFilePath::deleted("old.txt")];
        let manifest: AssetManifest = AssetManifest::snapshot(vec![], files);
        let result: Result<(), ValidationError> = manifest.validate();
        assert!(matches!(result, Err(ValidationError::SnapshotWithDeletion)));
    }

    #[test]
    fn test_diff_manifest_with_deletion_ok() {
        let files: Vec<ManifestFilePath> = vec![ManifestFilePath::deleted("old.txt")];
        let manifest: AssetManifest = AssetManifest::diff(vec![], files, "parent_hash");
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_snapshot_manifest_with_deleted_dir_fails() {
        let dirs: Vec<ManifestDirectoryPath> = vec![ManifestDirectoryPath::deleted("old_dir")];
        let manifest: AssetManifest = AssetManifest::snapshot(dirs, vec![]);
        let result: Result<(), ValidationError> = manifest.validate();
        assert!(matches!(result, Err(ValidationError::SnapshotWithDeletion)));
    }
}
