//! v2025-12 manifest format (extended format).
//!
//! This format supports:
//! - Directory entries (including empty directories)
//! - Large file chunking (>256MB files use chunkhashes)
//! - Symlinks with relative targets
//! - POSIX execute bit (runnable)
//! - Diff manifests with deletion markers
//! - Directory index compression ($N/ references)
//! - Unhashed files (hash=None for files collected but not yet hashed)
//! - Four specification versions encoding path style and manifest type

use serde::{Deserialize, Serialize};

use crate::error::ValidationError;
use crate::hash::{HashAlgorithm, FILE_CHUNK_SIZE_BYTES};
use crate::version::{ManifestType, ManifestVersion, PathStyle, SpecVersion};

/// Directory entry in v2025-12 manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDirectoryPath {
    /// Directory path within the manifest (POSIX format).
    pub path: String,
    /// Whether this directory is deleted (diff manifests only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
}

impl ManifestDirectoryPath {
    /// Create a new directory entry.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            deleted: false,
        }
    }

    /// Create a deleted directory marker.
    pub fn deleted(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            deleted: true,
        }
    }
}

/// File entry in v2025-12 manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFilePath {
    /// File path within the manifest (POSIX format).
    pub path: String,
    /// File content hash (None for symlinks, chunked files, deleted entries, or unhashed files).
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
    pub deleted: bool,
}

impl ManifestFilePath {
    /// Create a new regular file entry with hash.
    pub fn file(path: impl Into<String>, hash: impl Into<String>, size: u64, mtime: i64) -> Self {
        Self {
            path: path.into(),
            hash: Some(hash.into()),
            size: Some(size),
            mtime: Some(mtime),
            runnable: false,
            chunkhashes: None,
            symlink_target: None,
            deleted: false,
        }
    }

    /// Create an unhashed file entry (collected but not yet hashed).
    pub fn unhashed(path: impl Into<String>, size: u64, mtime: i64) -> Self {
        Self {
            path: path.into(),
            hash: None,
            size: Some(size),
            mtime: Some(mtime),
            runnable: false,
            chunkhashes: None,
            symlink_target: None,
            deleted: false,
        }
    }

    /// Create a symlink entry.
    pub fn symlink(path: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            hash: None,
            size: None,
            mtime: None,
            runnable: false,
            chunkhashes: None,
            symlink_target: Some(target.into()),
            deleted: false,
        }
    }

    /// Create a chunked file entry.
    pub fn chunked(
        path: impl Into<String>,
        chunkhashes: Vec<String>,
        size: u64,
        mtime: i64,
    ) -> Self {
        Self {
            path: path.into(),
            hash: None,
            size: Some(size),
            mtime: Some(mtime),
            runnable: false,
            chunkhashes: Some(chunkhashes),
            symlink_target: None,
            deleted: false,
        }
    }

    /// Create a deleted file marker.
    pub fn deleted(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            hash: None,
            size: None,
            mtime: None,
            runnable: false,
            chunkhashes: None,
            symlink_target: None,
            deleted: true,
        }
    }

    /// Set the runnable flag.
    pub fn with_runnable(mut self, runnable: bool) -> Self {
        self.runnable = runnable;
        self
    }

    /// Set the hash value.
    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.hash = Some(hash.into());
        self
    }

    /// Check if this entry is a symlink.
    pub fn is_symlink(&self) -> bool {
        self.symlink_target.is_some()
    }

    /// Check if this entry is chunked.
    pub fn is_chunked(&self) -> bool {
        self.chunkhashes.is_some()
    }

    /// Check if this entry is unhashed (collected but not yet hashed).
    pub fn is_unhashed(&self) -> bool {
        !self.deleted
            && self.hash.is_none()
            && self.chunkhashes.is_none()
            && self.symlink_target.is_none()
    }

    /// Validate this entry according to format rules.
    ///
    /// # Arguments
    /// * `allow_unhashed` - If true, allows files without hash/chunkhashes/symlink_target.
    pub fn validate(&self, allow_unhashed: bool) -> Result<(), ValidationError> {
        if self.deleted {
            self.validate_deleted()
        } else {
            self.validate_non_deleted(allow_unhashed)
        }
    }

    fn validate_deleted(&self) -> Result<(), ValidationError> {
        if self.hash.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.path.clone(),
                field: "hash",
            });
        }
        if self.chunkhashes.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.path.clone(),
                field: "chunkhashes",
            });
        }
        if self.symlink_target.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.path.clone(),
                field: "symlink_target",
            });
        }
        if self.runnable {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.path.clone(),
                field: "runnable",
            });
        }
        if self.size.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.path.clone(),
                field: "size",
            });
        }
        if self.mtime.is_some() {
            return Err(ValidationError::DeletedEntryHasField {
                path: self.path.clone(),
                field: "mtime",
            });
        }
        Ok(())
    }

    fn validate_non_deleted(&self, allow_unhashed: bool) -> Result<(), ValidationError> {
        // Count content fields
        let content_fields: [bool; 3] = [
            self.hash.is_some(),
            self.chunkhashes.is_some(),
            self.symlink_target.is_some(),
        ];
        let content_count: usize = content_fields.iter().filter(|&&x| x).count();

        // Must have exactly one of: hash, chunkhashes, symlink_target
        // OR zero if allow_unhashed is true (for COLLECT without HASH)
        if content_count == 0 && !allow_unhashed {
            return Err(ValidationError::InvalidContentFields {
                path: self.path.clone(),
            });
        }
        if content_count > 1 {
            return Err(ValidationError::InvalidContentFields {
                path: self.path.clone(),
            });
        }

        // Non-symlink entries need size and mtime
        if self.symlink_target.is_none() {
            if self.size.is_none() {
                return Err(ValidationError::MissingSize {
                    path: self.path.clone(),
                });
            }
            if self.mtime.is_none() {
                return Err(ValidationError::MissingMtime {
                    path: self.path.clone(),
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
            path: self.path.clone(),
        })?;

        if size <= FILE_CHUNK_SIZE_BYTES {
            return Err(ValidationError::ChunkedFileTooSmall {
                path: self.path.clone(),
                chunk_size: FILE_CHUNK_SIZE_BYTES,
                actual_size: size,
            });
        }

        let expected_chunks: usize = size.div_ceil(FILE_CHUNK_SIZE_BYTES) as usize;
        if chunks.len() != expected_chunks {
            return Err(ValidationError::ChunkCountMismatch {
                path: self.path.clone(),
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
                path: self.path.clone(),
                target: target.to_string(),
            });
        }

        // Check Windows absolute paths
        if target.len() >= 2 && target.chars().nth(1) == Some(':') {
            return Err(ValidationError::AbsoluteSymlinkTarget {
                path: self.path.clone(),
                target: target.to_string(),
            });
        }

        // Check UNC paths
        if target.starts_with("\\\\") || target.starts_with("//") {
            return Err(ValidationError::AbsoluteSymlinkTarget {
                path: self.path.clone(),
                target: target.to_string(),
            });
        }

        // Check if target escapes root (symlink at root level with ..)
        if !self.path.contains('/') && target.starts_with("..") {
            return Err(ValidationError::EscapingSymlinkTarget {
                path: self.path.clone(),
                target: target.to_string(),
            });
        }

        Ok(())
    }
}

/// Asset manifest v2025-12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetManifest {
    /// Hashing algorithm used for file hashes.
    pub hash_alg: HashAlgorithm,
    /// Manifest format version.
    pub manifest_version: ManifestVersion,
    /// Specification version (encodes path style and manifest type).
    pub spec_version: SpecVersion,
    /// List of directory entries.
    pub dirs: Vec<ManifestDirectoryPath>,
    /// List of file entries.
    pub files: Vec<ManifestFilePath>,
    /// Total size of all files in bytes.
    pub total_size: u64,
    /// Hash of parent manifest (diff manifests only).
    pub parent_manifest_hash: Option<String>,
}

impl AssetManifest {
    /// Create a new v2025-12 snapshot manifest with relative paths.
    pub fn snapshot(dirs: Vec<ManifestDirectoryPath>, files: Vec<ManifestFilePath>) -> Self {
        Self::with_spec(dirs, files, SpecVersion::REL_SNAPSHOT, None)
    }

    /// Create a new v2025-12 snapshot manifest with absolute paths.
    pub fn abs_snapshot(dirs: Vec<ManifestDirectoryPath>, files: Vec<ManifestFilePath>) -> Self {
        Self::with_spec(dirs, files, SpecVersion::ABS_SNAPSHOT, None)
    }

    /// Create a new v2025-12 diff manifest with relative paths.
    pub fn diff(
        dirs: Vec<ManifestDirectoryPath>,
        files: Vec<ManifestFilePath>,
        parent_hash: impl Into<String>,
    ) -> Self {
        Self::with_spec(dirs, files, SpecVersion::REL_DIFF, Some(parent_hash.into()))
    }

    /// Create a new v2025-12 diff manifest with absolute paths.
    pub fn abs_diff(
        dirs: Vec<ManifestDirectoryPath>,
        files: Vec<ManifestFilePath>,
        parent_hash: impl Into<String>,
    ) -> Self {
        Self::with_spec(dirs, files, SpecVersion::ABS_DIFF, Some(parent_hash.into()))
    }

    /// Create a manifest with explicit spec version.
    pub fn with_spec(
        dirs: Vec<ManifestDirectoryPath>,
        files: Vec<ManifestFilePath>,
        spec_version: SpecVersion,
        parent_manifest_hash: Option<String>,
    ) -> Self {
        let total_size: u64 = files
            .iter()
            .filter(|f| !f.deleted && f.symlink_target.is_none())
            .filter_map(|f| f.size)
            .sum();

        Self {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2025_12,
            spec_version,
            dirs,
            files,
            total_size,
            parent_manifest_hash,
        }
    }

    /// Get the manifest type (snapshot or diff).
    pub fn manifest_type(&self) -> ManifestType {
        self.spec_version.manifest_type
    }

    /// Get the path style (absolute or relative).
    pub fn path_style(&self) -> PathStyle {
        self.spec_version.path_style
    }

    /// Check if this manifest has absolute paths.
    pub fn is_absolute(&self) -> bool {
        self.spec_version.is_absolute()
    }

    /// Check if this manifest has relative paths.
    pub fn is_relative(&self) -> bool {
        self.spec_version.is_relative()
    }

    /// Check if this is a snapshot manifest.
    pub fn is_snapshot(&self) -> bool {
        self.spec_version.is_snapshot()
    }

    /// Check if this is a diff manifest.
    pub fn is_diff(&self) -> bool {
        self.spec_version.is_diff()
    }

    /// Validate all entries in the manifest.
    ///
    /// # Arguments
    /// * `allow_unhashed` - If true, allows files without hash (for COLLECT without HASH).
    pub fn validate(&self, allow_unhashed: bool) -> Result<(), ValidationError> {
        // Check for deletions in snapshot manifests
        if self.is_snapshot()
            && (self.dirs.iter().any(|d| d.deleted) || self.files.iter().any(|f| f.deleted))
        {
            return Err(ValidationError::SnapshotWithDeletion);
        }

        // Diff manifests must have parent hash
        if self.is_diff() && self.parent_manifest_hash.is_none() {
            return Err(ValidationError::DiffMissingParentHash);
        }

        // Validate each file entry
        for file in &self.files {
            file.validate(allow_unhashed)?;
        }

        Ok(())
    }

    /// Encode to canonical JSON string with directory index compression.
    pub fn encode(&self) -> Result<String, crate::error::ManifestError> {
        crate::encode::encode_v2025_12(self)
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
        assert!(file.validate(false).is_ok());
    }

    #[test]
    fn test_file_with_runnable() {
        let file: ManifestFilePath =
            ManifestFilePath::file("script.sh", "abc123", 512, 1234567890).with_runnable(true);
        assert!(file.validate(false).is_ok());
        assert!(file.runnable);
    }

    #[test]
    fn test_symlink_entry_validation() {
        let symlink: ManifestFilePath = ManifestFilePath::symlink("link.txt", "target.txt");
        assert!(symlink.validate(false).is_ok());
    }

    #[test]
    fn test_unhashed_file_entry() {
        let file: ManifestFilePath = ManifestFilePath::unhashed("test.txt", 1024, 1234567890);
        assert!(file.is_unhashed());
        // Should fail without allow_unhashed
        assert!(file.validate(false).is_err());
        // Should pass with allow_unhashed
        assert!(file.validate(true).is_ok());
    }

    #[test]
    fn test_deleted_entry_validation() {
        let entry: ManifestFilePath = ManifestFilePath::deleted("old.txt");
        assert!(entry.validate(false).is_ok());
        assert!(entry.deleted);
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
        assert!(file.validate(false).is_ok());
        assert_eq!(file.chunkhashes, Some(chunkhashes));
        assert!(file.hash.is_none());
    }

    // ==================== Manifest type tests ====================

    #[test]
    fn test_snapshot_manifest() {
        let manifest: AssetManifest = AssetManifest::snapshot(vec![], vec![]);
        assert!(manifest.is_snapshot());
        assert!(manifest.is_relative());
        assert_eq!(manifest.spec_version, SpecVersion::REL_SNAPSHOT);
    }

    #[test]
    fn test_abs_snapshot_manifest() {
        let manifest: AssetManifest = AssetManifest::abs_snapshot(vec![], vec![]);
        assert!(manifest.is_snapshot());
        assert!(manifest.is_absolute());
        assert_eq!(manifest.spec_version, SpecVersion::ABS_SNAPSHOT);
    }

    #[test]
    fn test_diff_manifest() {
        let manifest: AssetManifest = AssetManifest::diff(vec![], vec![], "parent_hash");
        assert!(manifest.is_diff());
        assert!(manifest.is_relative());
        assert_eq!(
            manifest.parent_manifest_hash,
            Some("parent_hash".to_string())
        );
    }

    #[test]
    fn test_abs_diff_manifest() {
        let manifest: AssetManifest = AssetManifest::abs_diff(vec![], vec![], "parent_hash");
        assert!(manifest.is_diff());
        assert!(manifest.is_absolute());
    }

    // ==================== Validation errors ====================

    #[test]
    fn test_snapshot_manifest_with_deletion_fails() {
        let files: Vec<ManifestFilePath> = vec![ManifestFilePath::deleted("old.txt")];
        let manifest: AssetManifest = AssetManifest::snapshot(vec![], files);
        let result: Result<(), ValidationError> = manifest.validate(false);
        assert!(matches!(result, Err(ValidationError::SnapshotWithDeletion)));
    }

    #[test]
    fn test_diff_manifest_without_parent_hash_fails() {
        let manifest: AssetManifest = AssetManifest::with_spec(
            vec![],
            vec![],
            SpecVersion::REL_DIFF,
            None, // Missing parent hash
        );
        let result: Result<(), ValidationError> = manifest.validate(false);
        assert!(matches!(
            result,
            Err(ValidationError::DiffMissingParentHash)
        ));
    }

    #[test]
    fn test_diff_manifest_with_deletion_ok() {
        let files: Vec<ManifestFilePath> = vec![ManifestFilePath::deleted("old.txt")];
        let manifest: AssetManifest = AssetManifest::diff(vec![], files, "parent_hash");
        assert!(manifest.validate(false).is_ok());
    }

    // ==================== Symlink validation ====================

    #[test]
    fn test_absolute_symlink_fails() {
        let symlink: ManifestFilePath = ManifestFilePath::symlink("link.txt", "/absolute/path");
        let result: Result<(), ValidationError> = symlink.validate(false);
        assert!(matches!(
            result,
            Err(ValidationError::AbsoluteSymlinkTarget { .. })
        ));
    }

    #[test]
    fn test_escaping_symlink_fails() {
        let symlink: ManifestFilePath = ManifestFilePath::symlink("link.txt", "../outside.txt");
        let result: Result<(), ValidationError> = symlink.validate(false);
        assert!(matches!(
            result,
            Err(ValidationError::EscapingSymlinkTarget { .. })
        ));
    }

    #[test]
    fn test_dotdot_within_root_ok() {
        let symlink: ManifestFilePath =
            ManifestFilePath::symlink("subdir/link.txt", "../target.txt");
        assert!(symlink.validate(false).is_ok());
    }
}
