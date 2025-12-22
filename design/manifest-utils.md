# Rusty Attachments: Manifest Utilities Design

## Overview

This document outlines the design for manifest diff and merge utilities in Rust, based on analysis of the Python `deadline-cloud` implementation. These utilities are essential for:

1. **Diff**: Comparing manifests to detect new, modified, and deleted files
2. **Merge**: Combining multiple manifests (e.g., input + step outputs) into a single manifest
3. **Apply**: Applying a diff manifest to a base snapshot

## Module Structure

```
crates/model/src/
├── lib.rs           # Add: re-exports for diff/merge modules
├── diff.rs          # NEW: Manifest diff operations
├── merge.rs         # NEW: Manifest merge operations
├── ...existing...
```

---

## diff.rs - Complete Implementation

```rust
//! Manifest diff operations.
//!
//! This module provides functions for comparing manifests and creating diff manifests.
//! Based on Python implementation in `deadline-cloud/src/deadline/job_attachments/_diff.py`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusty_attachments_common::{hash_string, VersionNotCompatibleError};
use crate::error::{ManifestError, ValidationError};
use crate::hash::HashAlgorithm;
use crate::version::ManifestType;
use crate::v2025_12_04::{AssetManifest, ManifestDirectoryPath, ManifestFilePath};
use crate::Manifest;

/// Status of a file when comparing manifests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileStatus {
    /// File exists in both manifests with same content hash.
    Unchanged,
    /// File exists only in the "current" manifest.
    New,
    /// File exists in both but content hash differs.
    Modified,
    /// File exists only in the "reference" manifest.
    Deleted,
}

/// Result of comparing a single path between manifests.
#[derive(Debug, Clone)]
pub struct DiffEntry<'a> {
    pub status: FileStatus,
    pub path: &'a str,
    /// Entry from reference manifest (None if NEW).
    pub reference: Option<&'a ManifestFilePath>,
    /// Entry from current manifest (None if DELETED).
    pub current: Option<&'a ManifestFilePath>,
}

/// Summary of differences between two manifests.
#[derive(Debug, Clone, Default)]
pub struct ManifestDiff {
    pub new: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub unchanged: Vec<String>,
}

impl ManifestDiff {
    /// Returns true if there are any changes.
    pub fn has_changes(&self) -> bool {
        !self.new.is_empty() || !self.modified.is_empty() || !self.deleted.is_empty()
    }

    /// Total number of changed files (new + modified + deleted).
    pub fn total_changes(&self) -> usize {
        self.new.len() + self.modified.len() + self.deleted.len()
    }
}

/// Compare two manifests by hash, returning status for each path.
///
/// # Arguments
/// * `reference` - The base/parent manifest
/// * `current` - The manifest with potential changes
///
/// # Returns
/// Vector of DiffEntry for all paths in either manifest.
///
/// # Errors
/// Returns `VersionNotCompatibleError` if either manifest is v2023-03-03.
/// Use `compare_manifests_v2023` for v2023 manifests.
///
/// # Example
/// ```
/// use rusty_attachments_model::{Manifest, diff::compare_manifests};
///
/// let base = Manifest::decode(base_json)?;
/// let current = Manifest::decode(current_json)?;
/// let entries = compare_manifests(&base, &current)?;
///
/// for entry in entries {
///     println!("{}: {:?}", entry.path, entry.status);
/// }
/// ```
pub fn compare_manifests<'a>(
    reference: &'a Manifest,
    current: &'a Manifest,
) -> Result<Vec<DiffEntry<'a>>, VersionNotCompatibleError> {
    let ref_files = get_file_map(reference)?;
    let cur_files = get_file_map(current)?;

    let mut results = Vec::new();

    // Check current paths against reference
    for (path, cur_entry) in &cur_files {
        match ref_files.get(path) {
            None => {
                results.push(DiffEntry {
                    status: FileStatus::New,
                    path,
                    reference: None,
                    current: Some(cur_entry),
                });
            }
            Some(ref_entry) => {
                let is_modified = !content_hashes_equal(ref_entry, cur_entry);
                results.push(DiffEntry {
                    status: if is_modified {
                        FileStatus::Modified
                    } else {
                        FileStatus::Unchanged
                    },
                    path,
                    reference: Some(ref_entry),
                    current: Some(cur_entry),
                });
            }
        }
    }

    // Find deleted files (in reference but not in current)
    for (path, ref_entry) in &ref_files {
        if !cur_files.contains_key(path) {
            results.push(DiffEntry {
                status: FileStatus::Deleted,
                path,
                reference: Some(ref_entry),
                current: None,
            });
        }
    }

    Ok(results)
}

/// Convert diff entries to a ManifestDiff summary.
pub fn to_manifest_diff(entries: &[DiffEntry<'_>]) -> ManifestDiff {
    let mut diff = ManifestDiff::default();

    for entry in entries {
        let path = entry.path.to_string();
        match entry.status {
            FileStatus::New => diff.new.push(path),
            FileStatus::Modified => diff.modified.push(path),
            FileStatus::Deleted => diff.deleted.push(path),
            FileStatus::Unchanged => diff.unchanged.push(path),
        }
    }

    diff
}

// ============================================================================
// Fast Diff (mtime/size comparison without hashing)
// ============================================================================

/// Result of fast file comparison (no hashing).
#[derive(Debug, Clone)]
pub struct FastDiffEntry {
    pub path: String,
    pub status: FileStatus,
}

/// Fast diff using mtime and size comparison (no hashing required).
///
/// Compares current filesystem state against a manifest using only
/// file metadata. This is much faster than hash comparison but may
/// miss content changes where mtime/size are unchanged.
///
/// # Arguments
/// * `root` - Root directory path
/// * `current_files` - List of current file paths (relative to root)
/// * `manifest` - Manifest to compare against
///
/// # Returns
/// Vector of FastDiffEntry for files that differ.
///
/// # Example
/// ```
/// use rusty_attachments_model::diff::fast_diff_files;
/// use std::path::Path;
///
/// let root = Path::new("/project/assets");
/// let files = vec!["file1.txt", "subdir/file2.txt"];
/// let manifest = Manifest::decode(json)?;
///
/// let changes = fast_diff_files(root, &files, &manifest)?;
/// for entry in changes {
///     println!("{}: {:?}", entry.path, entry.status);
/// }
/// ```
pub fn fast_diff_files(
    root: &Path,
    current_files: &[&str],
    manifest: &Manifest,
) -> std::io::Result<Vec<FastDiffEntry>> {
    let manifest_paths: HashMap<&str, &ManifestFilePath> = get_file_map(manifest)
        .into_iter()
        .filter(|(_, p)| !p.deleted)
        .collect();

    let mut results = Vec::new();
    let mut seen_paths = HashSet::new();

    for rel_path in current_files {
        seen_paths.insert(*rel_path);
        let full_path = root.join(rel_path);
        let metadata = std::fs::metadata(&full_path)?;

        let current_size = metadata.len();
        let current_mtime_us = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_micros() as i64))
            .unwrap_or(0);

        match manifest_paths.get(rel_path) {
            None => {
                results.push(FastDiffEntry {
                    path: rel_path.to_string(),
                    status: FileStatus::New,
                });
            }
            Some(manifest_entry) => {
                let size_differs = manifest_entry.size != Some(current_size);
                // Allow 1 microsecond tolerance for mtime comparison
                let mtime_differs = manifest_entry
                    .mtime
                    .map(|m| (m - current_mtime_us).abs() > 1)
                    .unwrap_or(true);

                if size_differs || mtime_differs {
                    results.push(FastDiffEntry {
                        path: rel_path.to_string(),
                        status: FileStatus::Modified,
                    });
                }
            }
        }
    }

    // Find deleted files
    for (path, _) in &manifest_paths {
        if !seen_paths.contains(path) {
            results.push(FastDiffEntry {
                path: path.to_string(),
                status: FileStatus::Deleted,
            });
        }
    }

    Ok(results)
}

// ============================================================================
// Create Diff Manifest
// ============================================================================

/// Create a diff manifest from comparing two snapshots.
///
/// The resulting manifest has:
/// - `manifest_type = Diff`
/// - `parent_manifest_hash` = hash of the parent manifest's canonical encoding
/// - New/modified entries with full content
/// - Deleted entries with `deleted = true`
///
/// # Arguments
/// * `parent` - The parent snapshot manifest
/// * `parent_encoded` - The canonical JSON encoding of the parent (for hashing)
/// * `current` - The current snapshot manifest
///
/// # Errors
/// Returns `VersionNotCompatibleError` if either manifest is v2023-03-03.
///
/// # Example
/// ```
/// use rusty_attachments_model::diff::create_diff_manifest;
///
/// let parent = Manifest::decode(parent_json)?;
/// let parent_encoded = parent.encode()?;
/// let current = Manifest::decode(current_json)?;
///
/// let diff = create_diff_manifest(&parent, &parent_encoded, &current)?;
/// println!("Diff has {} changes", diff.file_count());
/// ```
pub fn create_diff_manifest(
    parent: &Manifest,
    parent_encoded: &str,
    current: &Manifest,
) -> Result<Manifest, ManifestError> {
    // Compute parent manifest hash using xxh128 (from common crate)
    let parent_hash = hash_string(parent_encoded, HashAlgorithm::Xxh128);

    let diff_entries = compare_manifests(parent, current)?;

    let mut files: Vec<ManifestFilePath> = Vec::new();

    // Collect file entries
    for entry in &diff_entries {
        match entry.status {
            FileStatus::New | FileStatus::Modified => {
                if let Some(cur) = entry.current {
                    files.push(cur.clone());
                }
            }
            FileStatus::Deleted => {
                files.push(ManifestFilePath::deleted(entry.path));
            }
            FileStatus::Unchanged => {
                // Don't include unchanged files in diff
            }
        }
    }

    // Handle directory changes
    let parent_dirs: HashSet<&str> = get_dir_set(parent)?;
    let current_dirs: HashSet<&str> = get_dir_set(current)?;

    let mut dirs: Vec<ManifestDirectoryPath> = Vec::new();

    // New/existing directories from current
    for dir in get_dirs(current)? {
        if !parent_dirs.contains(dir.path.as_str()) {
            dirs.push(dir.clone());
        }
    }

    // Deleted directories
    for path in &parent_dirs {
        if !current_dirs.contains(path) {
            dirs.push(ManifestDirectoryPath::deleted(*path));
        }
    }

    // Calculate total size (excluding deleted and symlinks)
    let total_size: u64 = files
        .iter()
        .filter(|f| !f.deleted && f.symlink_target.is_none())
        .filter_map(|f| f.size)
        .sum();

    Ok(Manifest::V2025_12_04_beta(AssetManifest {
        hash_alg: current.hash_alg(),
        manifest_version: crate::version::ManifestVersion::V2025_12_04_beta,
        manifest_type: ManifestType::Diff,
        dirs,
        paths: files,
        total_size,
        parent_manifest_hash: Some(parent_hash),
    }))
}

// ============================================================================
// Apply Diff Manifest
// ============================================================================

/// Apply a diff manifest to a base snapshot, producing a new snapshot.
///
/// This is the inverse of `create_diff_manifest`:
/// - New entries are added
/// - Modified entries replace existing ones
/// - Deleted entries are removed
///
/// # Arguments
/// * `base` - The base snapshot manifest
/// * `diff` - The diff manifest to apply
///
/// # Returns
/// A new snapshot manifest with the diff applied.
///
/// # Errors
/// - Returns error if `diff` is not a diff manifest type.
/// - Returns `VersionNotCompatibleError` if base is v2023-03-03.
///
/// # Example
/// ```
/// use rusty_attachments_model::diff::apply_diff_manifest;
///
/// let base = Manifest::decode(base_json)?;
/// let diff = Manifest::decode(diff_json)?;
///
/// let result = apply_diff_manifest(&base, &diff)?;
/// // result is a snapshot with diff applied
/// ```
pub fn apply_diff_manifest(
    base: &Manifest,
    diff: &Manifest,
) -> Result<Manifest, ManifestError> {
    // Verify diff manifest type
    if diff.manifest_type() != ManifestType::Diff {
        return Err(ManifestError::Validation(ValidationError::InvalidManifestType {
            expected: ManifestType::Diff,
            found: diff.manifest_type(),
        }));
    }

    // Start with base files
    let mut files: HashMap<String, ManifestFilePath> = get_file_map(base)?
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();

    // Apply diff
    for diff_entry in get_files(diff)? {
        if diff_entry.deleted {
            files.remove(&diff_entry.path);
        } else {
            files.insert(diff_entry.path.clone(), diff_entry.clone());
        }
    }

    // Handle directories
    let mut dirs: HashMap<String, ManifestDirectoryPath> = get_dirs(base)?
        .iter()
        .map(|d| (d.path.clone(), d.clone()))
        .collect();

    for diff_dir in get_dirs(diff)? {
        if diff_dir.deleted {
            dirs.remove(&diff_dir.path);
        } else {
            dirs.insert(diff_dir.path.clone(), diff_dir.clone());
        }
    }

    // Calculate total size
    let total_size: u64 = files
        .values()
        .filter(|f| f.symlink_target.is_none())
        .filter_map(|f| f.size)
        .sum();

    Ok(Manifest::V2025_12_04_beta(AssetManifest {
        hash_alg: base.hash_alg(),
        manifest_version: crate::version::ManifestVersion::V2025_12_04_beta,
        manifest_type: ManifestType::Snapshot,
        dirs: dirs.into_values().collect(),
        paths: files.into_values().collect(),
        total_size,
        parent_manifest_hash: None,
    }))
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Extract file entries as a map from path to entry.
/// 
/// # Errors
/// Returns `VersionNotCompatibleError` for v2023-03-03 manifests.
fn get_file_map(manifest: &Manifest) -> Result<HashMap<&str, &ManifestFilePath>, VersionNotCompatibleError> {
    match manifest {
        // COMPAT: v2023-03-03 uses different path type, cannot be used with v2025 diff
        Manifest::V2023_03_03(_) => Err(VersionNotCompatibleError::new(
            "manifest diff with v2025 format",
            "v2025-12-04-beta",
        )),
        Manifest::V2025_12_04_beta(m) => {
            Ok(m.paths.iter().map(|p| (p.path.as_str(), p)).collect())
        }
    }
}

/// Get files slice from manifest.
/// 
/// # Errors
/// Returns `VersionNotCompatibleError` for v2023-03-03 manifests.
fn get_files(manifest: &Manifest) -> Result<&[ManifestFilePath], VersionNotCompatibleError> {
    match manifest {
        // COMPAT: v2023-03-03 uses different path type
        Manifest::V2023_03_03(_) => Err(VersionNotCompatibleError::new(
            "v2025 file entries",
            "v2025-12-04-beta",
        )),
        Manifest::V2025_12_04_beta(m) => Ok(&m.paths),
    }
}

/// Get directories slice from manifest.
/// 
/// # Errors
/// Returns `VersionNotCompatibleError` for v2023-03-03 manifests.
fn get_dirs(manifest: &Manifest) -> Result<&[ManifestDirectoryPath], VersionNotCompatibleError> {
    match manifest {
        // COMPAT: v2023-03-03 does not track directories
        Manifest::V2023_03_03(_) => Err(VersionNotCompatibleError::new(
            "directory entries",
            "v2025-12-04-beta",
        )),
        Manifest::V2025_12_04_beta(m) => Ok(&m.dirs),
    }
}

/// Get directory paths as a set.
fn get_dir_set(manifest: &Manifest) -> Result<HashSet<&str>, VersionNotCompatibleError> {
    Ok(get_dirs(manifest)?.iter().map(|d| d.path.as_str()).collect())
}

/// Compare content hashes between two file entries.
/// Handles both single hash and chunkhashes.
fn content_hashes_equal(a: &ManifestFilePath, b: &ManifestFilePath) -> bool {
    match (&a.hash, &b.hash, &a.chunkhashes, &b.chunkhashes) {
        (Some(h1), Some(h2), None, None) => h1 == h2,
        (None, None, Some(c1), Some(c2)) => c1 == c2,
        _ => false,
    }
}
```

---

## merge.rs - Complete Implementation

```rust
//! Manifest merge operations.
//!
//! This module provides functions for merging multiple manifests into one.
//! Based on Python implementation in `deadline-cloud/src/deadline/job_attachments/download.py`.

use std::collections::HashMap;

use crate::error::ManifestError;
use crate::hash::HashAlgorithm;
use crate::version::{ManifestType, ManifestVersion};
use crate::v2023_03_03::{AssetManifest as AssetManifest2023, ManifestPath as ManifestPath2023};
use crate::v2025_12_04::{
    AssetManifest as AssetManifest2025, ManifestDirectoryPath, ManifestFilePath,
};
use crate::Manifest;

/// Merge multiple manifests into a single manifest.
///
/// Later manifests override earlier ones (last-write-wins semantics).
/// All manifests must use the same hash algorithm.
///
/// # Arguments
/// * `manifests` - Slice of manifests to merge (order matters: later wins)
///
/// # Returns
/// Merged manifest, or None if input is empty.
///
/// # Errors
/// Returns error if manifests have different hash algorithms.
///
/// # Example
/// ```
/// use rusty_attachments_model::merge::merge_manifests;
///
/// let input_manifest = Manifest::decode(input_json)?;
/// let output_manifest = Manifest::decode(output_json)?;
///
/// let merged = merge_manifests(&[&input_manifest, &output_manifest])?;
/// if let Some(m) = merged {
///     println!("Merged {} files", m.file_count());
/// }
/// ```
pub fn merge_manifests(manifests: &[&Manifest]) -> Result<Option<Manifest>, ManifestError> {
    if manifests.is_empty() {
        return Ok(None);
    }

    if manifests.len() == 1 {
        return Ok(Some((*manifests[0]).clone()));
    }

    let hash_alg = manifests[0].hash_alg();
    let version = manifests[0].version();

    // Verify all manifests use same hash algorithm
    for manifest in manifests.iter().skip(1) {
        if manifest.hash_alg() != hash_alg {
            return Err(ManifestError::MergeHashAlgorithmMismatch {
                expected: hash_alg,
                found: manifest.hash_alg(),
            });
        }
    }

    // Dispatch to version-specific merge
    match version {
        ManifestVersion::V2023_03_03 => merge_v2023(manifests, hash_alg),
        ManifestVersion::V2025_12_04_beta => merge_v2025(manifests, hash_alg),
    }
}

/// Merge v2023-03-03 manifests.
fn merge_v2023(
    manifests: &[&Manifest],
    hash_alg: HashAlgorithm,
) -> Result<Option<Manifest>, ManifestError> {
    let mut merged_paths: HashMap<String, ManifestPath2023> = HashMap::new();

    for manifest in manifests {
        if let Manifest::V2023_03_03(m) = manifest {
            for path in &m.paths {
                merged_paths.insert(path.path.clone(), path.clone());
            }
        }
    }

    let paths: Vec<ManifestPath2023> = merged_paths.into_values().collect();
    let total_size: u64 = paths.iter().map(|p| p.size).sum();

    Ok(Some(Manifest::V2023_03_03(AssetManifest2023 {
        hash_alg,
        manifest_version: ManifestVersion::V2023_03_03,
        paths,
        total_size,
    })))
}

/// Merge v2025-12-04-beta manifests.
fn merge_v2025(
    manifests: &[&Manifest],
    hash_alg: HashAlgorithm,
) -> Result<Option<Manifest>, ManifestError> {
    let mut merged_files: HashMap<String, ManifestFilePath> = HashMap::new();
    let mut merged_dirs: HashMap<String, ManifestDirectoryPath> = HashMap::new();

    for manifest in manifests {
        if let Manifest::V2025_12_04_beta(m) = manifest {
            for file in &m.paths {
                merged_files.insert(file.path.clone(), file.clone());
            }
            for dir in &m.dirs {
                merged_dirs.insert(dir.path.clone(), dir.clone());
            }
        }
    }

    let paths: Vec<ManifestFilePath> = merged_files.into_values().collect();
    let dirs: Vec<ManifestDirectoryPath> = merged_dirs.into_values().collect();

    let total_size: u64 = paths
        .iter()
        .filter(|f| !f.deleted && f.symlink_target.is_none())
        .filter_map(|f| f.size)
        .sum();

    Ok(Some(Manifest::V2025_12_04_beta(AssetManifest2025 {
        hash_alg,
        manifest_version: ManifestVersion::V2025_12_04_beta,
        dirs,
        paths,
        total_size,
        manifest_type: ManifestType::Snapshot,
        parent_manifest_hash: None,
    })))
}

/// Merge manifests sorted by timestamp (oldest first).
///
/// This ensures chronological ordering where newer files overwrite older ones.
/// Used when merging output manifests from S3 where LastModified is available.
///
/// # Arguments
/// * `manifests_with_timestamps` - Mutable slice of (timestamp_millis, manifest) tuples
///
/// # Example
/// ```
/// use rusty_attachments_model::merge::merge_manifests_chronologically;
///
/// let mut manifests = vec![
///     (1703001200000i64, &manifest1),  // older
///     (1703001100000i64, &manifest2),  // oldest
///     (1703001300000i64, &manifest3),  // newest
/// ];
///
/// let merged = merge_manifests_chronologically(&mut manifests)?;
/// // manifest3's files win for any conflicts
/// ```
pub fn merge_manifests_chronologically(
    manifests_with_timestamps: &mut [(i64, &Manifest)],
) -> Result<Option<Manifest>, ManifestError> {
    if manifests_with_timestamps.is_empty() {
        return Ok(None);
    }

    // Sort by timestamp ascending (oldest first)
    manifests_with_timestamps.sort_by_key(|(ts, _)| *ts);

    // Extract manifests in sorted order
    let sorted_manifests: Vec<&Manifest> = manifests_with_timestamps
        .iter()
        .map(|(_, m)| *m)
        .collect();

    merge_manifests(&sorted_manifests)
}

// ============================================================================
// ManifestPathGroup - For Download Aggregation
// ============================================================================

/// Paths from multiple manifests grouped by hash algorithm.
///
/// Used for efficient download operations where files need to be
/// grouped by their hash algorithm for CAS retrieval.
///
/// # Example
/// ```
/// use rusty_attachments_model::merge::ManifestPathGroup;
///
/// let mut group = ManifestPathGroup::new();
/// group.add_manifest(&input_manifest);
/// group.add_manifest(&output_manifest);
///
/// println!("Total: {} files, {} bytes", group.file_count(), group.total_bytes);
///
/// for (hash_alg, files) in &group.files_by_hash_alg {
///     println!("{}: {} files", hash_alg, files.len());
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct ManifestPathGroup {
    pub total_bytes: u64,
    pub files_by_hash_alg: HashMap<HashAlgorithm, Vec<ManifestFilePath>>,
}

impl ManifestPathGroup {
    /// Create a new empty path group.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add all paths from a manifest to this group.
    pub fn add_manifest(&mut self, manifest: &Manifest) {
        let hash_alg = manifest.hash_alg();

        match manifest {
            Manifest::V2023_03_03(m) => {
                let files = self.files_by_hash_alg.entry(hash_alg).or_default();
                for path in &m.paths {
                    self.total_bytes += path.size;
                    // Convert v2023 path to v2025 format
                    files.push(ManifestFilePath::file(
                        &path.path,
                        &path.hash,
                        path.size,
                        path.mtime,
                    ));
                }
            }
            Manifest::V2025_12_04_beta(m) => {
                let files = self.files_by_hash_alg.entry(hash_alg).or_default();
                for path in &m.paths {
                    if !path.deleted {
                        if let Some(size) = path.size {
                            self.total_bytes += size;
                        }
                        files.push(path.clone());
                    }
                }
            }
        }
    }

    /// Combine another group into this one.
    pub fn combine(&mut self, other: &ManifestPathGroup) {
        self.total_bytes += other.total_bytes;

        for (hash_alg, paths) in &other.files_by_hash_alg {
            self.files_by_hash_alg
                .entry(*hash_alg)
                .or_default()
                .extend(paths.iter().cloned());
        }
    }

    /// Get all paths regardless of hash algorithm.
    pub fn all_paths(&self) -> Vec<&str> {
        let mut paths: Vec<&str> = self
            .files_by_hash_alg
            .values()
            .flat_map(|files| files.iter().map(|f| f.path.as_str()))
            .collect();
        paths.sort();
        paths
    }

    /// Total number of files across all hash algorithms.
    pub fn file_count(&self) -> usize {
        self.files_by_hash_alg.values().map(|v| v.len()).sum()
    }
}
```

---

## Error Types (additions to error.rs)

```rust
// Add to error.rs

#[derive(Debug, Error)]
pub enum ManifestError {
    // ... existing errors ...

    #[error("Cannot merge manifests with different hash algorithms: expected {expected}, found {found}")]
    MergeHashAlgorithmMismatch {
        expected: HashAlgorithm,
        found: HashAlgorithm,
    },
}

#[derive(Debug, Error)]
pub enum ValidationError {
    // ... existing errors ...

    #[error("Expected {expected:?} manifest type, found {found:?}")]
    InvalidManifestType {
        expected: ManifestType,
        found: ManifestType,
    },
}
```

---

## Updates to lib.rs

```rust
// Add to lib.rs

pub mod diff;
pub mod merge;

// Update Manifest impl
impl Manifest {
    // ... existing methods ...

    /// Get the manifest type (snapshot or diff).
    pub fn manifest_type(&self) -> ManifestType {
        match self {
            Manifest::V2023_03_03(_) => ManifestType::Snapshot, // v2023 is always snapshot
            Manifest::V2025_12_04_beta(m) => m.manifest_type,
        }
    }

    /// Get file entries (v2025 format).
    /// For v2023, returns empty slice (use paths() instead).
    pub fn files(&self) -> &[v2025_12_04::ManifestFilePath] {
        match self {
            Manifest::V2023_03_03(_) => &[],
            Manifest::V2025_12_04_beta(m) => &m.paths,
        }
    }

    /// Get directory entries.
    pub fn dirs(&self) -> &[v2025_12_04::ManifestDirectoryPath] {
        match self {
            Manifest::V2023_03_03(_) => &[],
            Manifest::V2025_12_04_beta(m) => &m.dirs,
        }
    }
}
```

---

## Cargo.toml Dependencies

```toml
# Add to crates/model/Cargo.toml

[dependencies]
rusty-attachments-common = { path = "../common" }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
thiserror = "1.0"

[dev-dependencies]
pretty_assertions = "1.0"
```

---

## Usage Examples

### Compare Two Manifests

```rust
use rusty_attachments_model::{Manifest, diff::{compare_manifests, to_manifest_diff, FileStatus}};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = Manifest::decode(include_str!("base.json"))?;
    let current = Manifest::decode(include_str!("current.json"))?;

    let entries = compare_manifests(&base, &current);
    let summary = to_manifest_diff(&entries);

    println!("New files: {:?}", summary.new);
    println!("Modified files: {:?}", summary.modified);
    println!("Deleted files: {:?}", summary.deleted);

    // Get detailed info for modified files
    for entry in entries.iter().filter(|e| e.status == FileStatus::Modified) {
        println!(
            "{}: {} -> {}",
            entry.path,
            entry.reference.and_then(|r| r.hash.as_deref()).unwrap_or("?"),
            entry.current.and_then(|c| c.hash.as_deref()).unwrap_or("?"),
        );
    }

    Ok(())
}
```

### Create and Apply Diff Manifest

```rust
use rusty_attachments_model::{Manifest, diff::{create_diff_manifest, apply_diff_manifest}};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let parent = Manifest::decode(parent_json)?;
    let parent_encoded = parent.encode()?;
    let current = Manifest::decode(current_json)?;

    // Create diff
    let diff = create_diff_manifest(&parent, &parent_encoded, &current)?;
    let diff_json = diff.encode()?;
    println!("Diff manifest: {}", diff_json);

    // Apply diff to reconstruct current
    let reconstructed = apply_diff_manifest(&parent, &diff)?;

    // Verify roundtrip
    assert_eq!(reconstructed.file_count(), current.file_count());

    Ok(())
}
```

### Merge Multiple Manifests

```rust
use rusty_attachments_model::{Manifest, merge::{merge_manifests, ManifestPathGroup}};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = Manifest::decode(input_json)?;
    let step_a_output = Manifest::decode(step_a_json)?;
    let step_b_output = Manifest::decode(step_b_json)?;

    // Merge with last-write-wins (step_b wins over step_a wins over input)
    let merged = merge_manifests(&[&input, &step_a_output, &step_b_output])?;

    if let Some(m) = merged {
        println!("Merged manifest has {} files", m.file_count());
    }

    // Or use ManifestPathGroup for download aggregation
    let mut group = ManifestPathGroup::new();
    group.add_manifest(&input);
    group.add_manifest(&step_a_output);
    group.add_manifest(&step_b_output);

    println!("Total: {} files, {} bytes", group.file_count(), group.total_bytes);

    Ok(())
}
```

### Fast Diff Against Filesystem

```rust
use rusty_attachments_model::{Manifest, diff::{fast_diff_files, FileStatus}};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = Manifest::decode(manifest_json)?;
    let root = Path::new("/project/assets");

    // Get current files from filesystem
    let current_files: Vec<&str> = vec![
        "file1.txt",
        "subdir/file2.txt",
        "new_file.txt",
    ];

    let changes = fast_diff_files(root, &current_files, &manifest)?;

    for entry in changes {
        match entry.status {
            FileStatus::New => println!("+ {}", entry.path),
            FileStatus::Modified => println!("M {}", entry.path),
            FileStatus::Deleted => println!("- {}", entry.path),
            _ => {}
        }
    }

    Ok(())
}
```

---

## Implementation Plan

### Phase 1: Core Types and Diff
- [ ] Add `FileStatus` enum to `diff.rs`
- [ ] Add `DiffEntry` and `ManifestDiff` structs
- [ ] Implement `compare_manifests()`
- [ ] Implement `to_manifest_diff()`
- [ ] Add error types to `error.rs`
- [ ] Unit tests for diff operations

### Phase 2: Fast Diff
- [ ] Add `FastDiffEntry` struct
- [ ] Implement `fast_diff_files()`
- [ ] Integration tests with filesystem

### Phase 3: Diff Manifest Creation
- [ ] Import `hash_string` from common crate
- [ ] Implement `create_diff_manifest()`
- [ ] Implement `apply_diff_manifest()`
- [ ] Roundtrip tests

### Phase 4: Merge Operations
- [ ] Implement `merge_manifests()`
- [ ] Implement `merge_v2023()` and `merge_v2025()` helpers
- [ ] Implement `merge_manifests_chronologically()`
- [ ] Unit tests for merge operations

### Phase 5: ManifestPathGroup
- [ ] Implement `ManifestPathGroup` struct
- [ ] Implement `add_manifest()`, `combine()`, `all_paths()`, `file_count()`
- [ ] Unit tests

### Phase 6: lib.rs Updates
- [ ] Add `manifest_type()` method to `Manifest`
- [ ] Add `files()` and `dirs()` methods to `Manifest`
- [ ] Re-export diff and merge modules
- [ ] Update documentation

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_identical_manifests() {
        let manifest = create_test_manifest();
        let entries = compare_manifests(&manifest, &manifest);
        assert!(entries.iter().all(|e| e.status == FileStatus::Unchanged));
    }

    #[test]
    fn test_compare_detects_new_file() {
        let base = create_manifest_with_files(&["a.txt"]);
        let current = create_manifest_with_files(&["a.txt", "b.txt"]);
        let diff = to_manifest_diff(&compare_manifests(&base, &current));
        assert_eq!(diff.new, vec!["b.txt"]);
    }

    #[test]
    fn test_compare_detects_deleted_file() {
        let base = create_manifest_with_files(&["a.txt", "b.txt"]);
        let current = create_manifest_with_files(&["a.txt"]);
        let diff = to_manifest_diff(&compare_manifests(&base, &current));
        assert_eq!(diff.deleted, vec!["b.txt"]);
    }

    #[test]
    fn test_compare_detects_modified_file() {
        let base = create_manifest_with_file("a.txt", "hash1");
        let current = create_manifest_with_file("a.txt", "hash2");
        let diff = to_manifest_diff(&compare_manifests(&base, &current));
        assert_eq!(diff.modified, vec!["a.txt"]);
    }

    #[test]
    fn test_merge_last_write_wins() {
        let m1 = create_manifest_with_file("test.txt", "hash1");
        let m2 = create_manifest_with_file("test.txt", "hash2");
        let merged = merge_manifests(&[&m1, &m2]).unwrap().unwrap();
        // m2's version should win
        let files = get_files(&merged);
        assert_eq!(files[0].hash, Some("hash2".to_string()));
    }

    #[test]
    fn test_merge_different_hash_alg_fails() {
        // Would need manifests with different hash algorithms
        // Currently only XXH128 is supported, so this is a placeholder
    }

    #[test]
    fn test_diff_roundtrip() {
        let parent = create_test_manifest();
        let current = create_modified_manifest();

        let parent_encoded = parent.encode().unwrap();
        let diff = create_diff_manifest(&parent, &parent_encoded, &current).unwrap();
        let reconstructed = apply_diff_manifest(&parent, &diff).unwrap();

        assert_eq!(reconstructed.file_count(), current.file_count());
    }

    #[test]
    fn test_manifest_path_group_aggregation() {
        let m1 = create_manifest_with_files(&["a.txt", "b.txt"]);
        let m2 = create_manifest_with_files(&["c.txt"]);

        let mut group = ManifestPathGroup::new();
        group.add_manifest(&m1);
        group.add_manifest(&m2);

        assert_eq!(group.file_count(), 3);
    }
}
```

---

## Related Documents

- [common.md](common.md) - Shared hash utilities (`hash_string`)
- [model-design.md](model-design.md) - Core manifest model design
- [storage-design.md](storage-design.md) - Storage abstraction for S3 operations

---

## Manifest Name Generation

When uploading manifests to S3, a deterministic naming convention is used to identify manifests by their source root. This ensures consistent naming across uploads and enables manifest deduplication.

### Naming Convention

Manifest names follow the pattern: `{hash_of_source_root}_{suffix}`

Where:
- `hash_of_source_root`: XXH128 hash of the source root path string (as bytes)
- `suffix`: Type identifier (e.g., `input`, `output`)

Example: `a1b2c3d4e5f6789012345678901234567890_input`

### Implementation

```rust
/// Generate a manifest file name from the source root path.
/// 
/// The name is deterministic based on the source root, allowing
/// consistent naming across uploads from the same location.
/// 
/// # Arguments
/// * `source_root` - The root path the manifest was created from
/// * `suffix` - Type suffix (e.g., "input", "output")
/// * `hash_alg` - Hash algorithm to use (typically XXH128)
/// 
/// # Returns
/// Manifest name in format: `{hash}_{suffix}`
/// 
/// # Note
/// The source_root is converted to a string using OS-specific separators
/// before hashing. This means the same directory will produce different
/// manifest names on Windows vs Unix. This is intentional - manifests
/// from different OS types should be distinguishable.
pub fn generate_manifest_name(
    source_root: &str,
    suffix: &str,
    hash_alg: HashAlgorithm,
) -> String {
    let root_hash = hash_data(source_root.as_bytes(), hash_alg);
    format!("{}_{}", root_hash, suffix)
}

/// Generate manifest name with hash algorithm from manifest.
pub fn generate_manifest_name_from_manifest(
    manifest: &Manifest,
    source_root: &str,
    suffix: &str,
) -> String {
    generate_manifest_name(source_root, suffix, manifest.hash_alg())
}
```

### Full Manifest Key Construction

The complete S3 key for a manifest combines the partial prefix with the generated name:

```rust
/// Build the full S3 key for an input manifest.
/// 
/// # Arguments
/// * `root_prefix` - S3 root prefix (e.g., "DeadlineCloud")
/// * `farm_id` - Farm identifier
/// * `queue_id` - Queue identifier  
/// * `guid` - Unique identifier for this upload session
/// * `manifest_name` - Generated manifest name
/// 
/// # Returns
/// Full S3 key: `{root_prefix}/Manifests/{farm_id}/{queue_id}/Inputs/{guid}/{manifest_name}`
pub fn build_input_manifest_key(
    root_prefix: &str,
    farm_id: &str,
    queue_id: &str,
    guid: &str,
    manifest_name: &str,
) -> String {
    format!(
        "{}/Manifests/{}/{}/Inputs/{}/{}",
        root_prefix, farm_id, queue_id, guid, manifest_name
    )
}

/// Build the partial manifest key (excludes root prefix and Manifests folder).
/// This is what gets stored in ManifestProperties.inputManifestPath.
/// 
/// # Returns
/// Partial key: `{farm_id}/{queue_id}/Inputs/{guid}/{manifest_name}`
pub fn build_partial_manifest_key(
    farm_id: &str,
    queue_id: &str,
    guid: &str,
    manifest_name: &str,
) -> String {
    format!(
        "{}/{}/Inputs/{}/{}",
        farm_id, queue_id, guid, manifest_name
    )
}
```

### Usage Example

```rust
use rusty_attachments_model::manifest_utils::{
    generate_manifest_name, build_partial_manifest_key
};
use uuid::Uuid;

// Generate manifest name from source root
let source_root = "/mnt/projects/job1";
let manifest_name = generate_manifest_name(source_root, "input", HashAlgorithm::Xxh128);
// Result: "a1b2c3d4e5f6789012345678901234567890_input"

// Build partial key for S3
let guid = Uuid::new_v4().to_string();
let partial_key = build_partial_manifest_key(
    "farm-123",
    "queue-456", 
    &guid,
    &manifest_name,
);
// Result: "farm-123/queue-456/Inputs/550e8400-e29b-41d4-a716-446655440000/a1b2c3d4..._input"
```

### Manifest Hash Computation

After encoding a manifest, compute its hash for the `inputManifestHash` field:

```rust
/// Compute the hash of an encoded manifest.
/// Used for ManifestProperties.inputManifestHash.
pub fn compute_manifest_hash(manifest_bytes: &[u8], hash_alg: HashAlgorithm) -> String {
    hash_data(manifest_bytes, hash_alg)
}

// Usage:
let manifest_bytes = manifest.encode()?.as_bytes();
let manifest_hash = compute_manifest_hash(manifest_bytes, manifest.hash_alg());
```

### Implementation Plan Addition

Add to Phase 6:
- [ ] Implement `generate_manifest_name()`
- [ ] Implement `build_input_manifest_key()` and `build_partial_manifest_key()`
- [ ] Implement `compute_manifest_hash()`
- [ ] Unit tests for manifest naming


---

## Output File Detection

When syncing outputs after job execution, we need to detect which files are new or modified since the input sync. This is done by tracking file modification times.

### Synced Assets Mtime Tracker

```rust
/// Tracks modification times of synced assets.
/// 
/// Used to detect which files have been created or modified during job execution.
/// Files are tracked by their absolute path, with mtime stored in nanoseconds.
#[derive(Debug, Clone, Default)]
pub struct SyncedAssetsMtime {
    /// Map of absolute file path -> mtime in nanoseconds
    mtimes: HashMap<String, i64>,
}

impl SyncedAssetsMtime {
    /// Create a new empty tracker.
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Record the mtime of a file at sync time.
    pub fn record(&mut self, path: &Path, mtime_ns: i64) {
        self.mtimes.insert(path.to_string_lossy().into_owned(), mtime_ns);
    }
    
    /// Record mtimes for all files in a manifest.
    /// 
    /// # Arguments
    /// * `local_root` - Local root directory where files are synced
    /// * `manifest` - The manifest containing file entries
    pub fn record_from_manifest(&mut self, local_root: &Path, manifest: &Manifest) {
        for file in manifest.files() {
            if file.deleted {
                continue;
            }
            let abs_path = local_root.join(&file.path);
            if let Ok(metadata) = abs_path.metadata() {
                if let Ok(mtime) = metadata.modified() {
                    let mtime_ns = mtime
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as i64)
                        .unwrap_or(0);
                    self.record(&abs_path, mtime_ns);
                }
            }
        }
    }
    
    /// Check if a file has been modified since it was synced.
    /// 
    /// Returns `true` if:
    /// - The file was not tracked (new file)
    /// - The file's current mtime is greater than the recorded mtime
    pub fn is_modified(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        match self.mtimes.get(path_str.as_ref()) {
            None => true, // New file
            Some(&recorded_mtime) => {
                if let Ok(metadata) = path.metadata() {
                    if let Ok(mtime) = metadata.modified() {
                        let current_mtime_ns = mtime
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as i64)
                            .unwrap_or(0);
                        return current_mtime_ns > recorded_mtime;
                    }
                }
                false
            }
        }
    }
    
    /// Get the recorded mtime for a path, if any.
    pub fn get_mtime(&self, path: &Path) -> Option<i64> {
        self.mtimes.get(&path.to_string_lossy().into_owned()).copied()
    }
}
```

### Output File Discovery

```rust
/// Information about an output file discovered during sync.
#[derive(Debug, Clone)]
pub struct OutputFile {
    /// File size in bytes
    pub file_size: u64,
    /// File hash (xxh128)
    pub file_hash: String,
    /// Relative path from local root (POSIX format)
    pub rel_path: String,
    /// Absolute path on local filesystem
    pub full_path: PathBuf,
    /// S3 key for CAS upload
    pub s3_key: String,
    /// Whether the file already exists in S3
    pub in_s3: bool,
}

/// Discover output files in the specified directories.
/// 
/// Walks the output directories and finds files that are new or modified
/// since the input sync (based on `synced_mtimes`).
/// 
/// # Arguments
/// * `output_dirs` - Output directories to scan (relative to local_root)
/// * `local_root` - Local root directory
/// * `session_dir` - Session directory for security validation
/// * `synced_mtimes` - Tracker with recorded mtimes from input sync
/// * `hash_alg` - Hash algorithm to use
/// * `cas_prefix` - S3 CAS prefix for key generation
/// 
/// # Returns
/// List of output files with their metadata and hashes
pub fn discover_output_files(
    output_dirs: &[String],
    local_root: &Path,
    session_dir: &Path,
    synced_mtimes: &SyncedAssetsMtime,
    hash_alg: HashAlgorithm,
    cas_prefix: &str,
) -> Result<Vec<OutputFile>, FileSystemError> {
    let mut output_files = Vec::new();
    
    for output_dir in output_dirs {
        let output_root = local_root.join(output_dir);
        
        // Skip if directory doesn't exist (another task might create it)
        if !output_root.is_dir() {
            continue;
        }
        
        // Walk the directory tree
        for entry in walkdir::WalkDir::new(&output_root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let file_path = entry.path();
            
            // Skip directories
            if file_path.is_dir() {
                continue;
            }
            
            // Check if file is new or modified
            if !synced_mtimes.is_modified(file_path) {
                continue;
            }
            
            // Security: resolve real path and validate within session directory
            let real_path = file_path.canonicalize()
                .map_err(|e| FileSystemError::IoError {
                    path: file_path.display().to_string(),
                    source: e,
                })?;
            
            if !is_path_within_directory(&real_path, session_dir) {
                // Skip files that resolve outside session directory
                continue;
            }
            
            // Get file metadata
            let metadata = real_path.metadata()
                .map_err(|e| FileSystemError::IoError {
                    path: real_path.display().to_string(),
                    source: e,
                })?;
            
            let file_size = metadata.len();
            
            // Compute hash
            let file_hash = hash_file(&real_path, hash_alg)?;
            
            // Build S3 key
            let s3_key = format!("{}/{}.{}", cas_prefix, file_hash, hash_alg.extension());
            
            // Compute relative path (POSIX format)
            let rel_path = file_path
                .strip_prefix(local_root)
                .map_err(|_| FileSystemError::PathOutsideRoot {
                    path: file_path.display().to_string(),
                    root: local_root.display().to_string(),
                })?;
            let rel_path_str = rel_path
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            
            output_files.push(OutputFile {
                file_size,
                file_hash,
                rel_path: rel_path_str,
                full_path: real_path,
                s3_key,
                in_s3: false, // Caller should check S3 existence
            });
        }
    }
    
    Ok(output_files)
}

/// Check if a path is within a directory (security validation).
fn is_path_within_directory(file_path: &Path, directory_path: &Path) -> bool {
    match (file_path.canonicalize(), directory_path.canonicalize()) {
        (Ok(real_file), Ok(real_dir)) => real_file.starts_with(&real_dir),
        _ => false,
    }
}
```

### Generate Output Manifest

```rust
/// Generate an output manifest from discovered output files.
/// 
/// # Arguments
/// * `output_files` - List of output files discovered
/// * `hash_alg` - Hash algorithm used
/// 
/// # Returns
/// A manifest containing the output files
pub fn generate_output_manifest(
    output_files: &[OutputFile],
    hash_alg: HashAlgorithm,
) -> Manifest {
    let paths: Vec<ManifestFilePath> = output_files
        .iter()
        .map(|file| {
            let mtime_us = file.full_path
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| (d.as_nanos() / 1000) as i64)
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            
            ManifestFilePath::file(
                &file.rel_path,
                &file.file_hash,
                file.file_size,
                mtime_us,
            )
        })
        .collect();
    
    let total_size: u64 = output_files.iter().map(|f| f.file_size).sum();
    
    Manifest::V2025_12_04_beta(AssetManifest {
        hash_alg,
        manifest_version: ManifestVersion::V2025_12_04_beta,
        dirs: vec![],
        paths,
        total_size,
        manifest_type: ManifestType::Snapshot,
        parent_manifest_hash: None,
    })
}
```

### Usage Example

```rust
use rusty_attachments_model::manifest_utils::{
    SyncedAssetsMtime, discover_output_files, generate_output_manifest,
};

// During input sync: record mtimes
let mut synced_mtimes = SyncedAssetsMtime::new();
for (local_root, manifest) in &merged_manifests_by_root {
    synced_mtimes.record_from_manifest(Path::new(local_root), manifest);
}

// After job execution: discover output files
let output_dirs = vec!["renders".to_string(), "cache".to_string()];
let output_files = discover_output_files(
    &output_dirs,
    &local_root,
    &session_dir,
    &synced_mtimes,
    HashAlgorithm::Xxh128,
    "DeadlineCloud/Data",
)?;

// Generate output manifest
if !output_files.is_empty() {
    let output_manifest = generate_output_manifest(&output_files, HashAlgorithm::Xxh128);
    
    // Upload manifest and files...
}
```


---

## Implementation Status

### ✅ Implemented

#### merge.rs (in `model` crate)
Location: `crates/model/src/merge.rs`

- [x] `merge_manifests()` - Merges multiple manifests with last-write-wins semantics
- [x] `merge_manifests_chronologically()` - Sorts by timestamp before merging
- [x] `merge_v2023_manifests()` - Version-specific helper for v2023
- [x] `merge_v2025_manifests()` - Version-specific helper for v2025
- [x] `ManifestError::MergeHashAlgorithmMismatch` - Error type
- [x] `ManifestError::MergeVersionMismatch` - Error type
- [x] Full test coverage (10 tests)

#### Directory-to-Manifest Diff (in `filesystem` crate)
Location: `crates/filesystem/src/diff.rs`

Note: This is a different implementation than the design's manifest-to-manifest diff.
It compares filesystem state against a manifest (useful for incremental uploads).

- [x] `DiffEngine` struct with `diff()` method
- [x] `DiffMode` enum (Fast/Hash)
- [x] `DiffOptions` struct
- [x] `DiffResult` struct (added/modified/deleted/unchanged)
- [x] `DiffStats` struct
- [x] `FileEntry` struct
- [x] `create_diff_manifest()` - Creates diff manifest with parent hash

### ❌ Not Implemented

#### diff.rs (in `model` crate) - Manifest-to-Manifest Comparison
The design specifies manifest-to-manifest comparison functions:

- [ ] `FileStatus` enum (Unchanged/New/Modified/Deleted)
- [ ] `DiffEntry` struct
- [ ] `ManifestDiff` struct
- [ ] `compare_manifests()` - Compare two manifests by hash
- [ ] `to_manifest_diff()` - Convert diff entries to summary
- [ ] `fast_diff_files()` - Fast diff using mtime/size against manifest
- [ ] `create_diff_manifest()` - Create diff manifest from two snapshots (model version)
- [ ] `apply_diff_manifest()` - Apply diff to base snapshot to reconstruct current

#### ManifestPathGroup
- [ ] `ManifestPathGroup` struct for download aggregation
- [ ] `add_manifest()` method
- [ ] `combine()` method
- [ ] `all_paths()` method
- [ ] `file_count()` method

#### Output File Detection
- [ ] `SyncedAssetsMtime` struct
- [ ] `record()` method
- [ ] `record_from_manifest()` method
- [ ] `is_modified()` method
- [ ] `get_mtime()` method

#### Output File Discovery
- [ ] `OutputFile` struct
- [ ] `discover_output_files()` function
- [ ] `generate_output_manifest()` function
- [ ] `is_path_within_directory()` helper

#### Manifest Naming (may be in storage crate)
- [x] `compute_manifest_name_hash()` - Implemented in `storage/src/manifest_storage.rs`
- [x] `compute_root_path_hash()` - Implemented in `storage/src/manifest_storage.rs`
- [x] `format_input_manifest_s3_key()` - Implemented in `storage/src/manifest_storage.rs`
- [ ] `compute_manifest_hash()` - Hash of encoded manifest bytes

### Notes

1. The design's `diff.rs` (manifest-to-manifest) and the implemented `filesystem/src/diff.rs` (directory-to-manifest) serve different purposes:
   - **Design's diff.rs**: Compare two manifests to detect changes between versions
   - **Implemented diff.rs**: Compare filesystem state against a manifest for incremental uploads

2. The `ManifestPathGroup` functionality may be partially covered by the download operations in `storage/src/manifest_storage.rs` which groups manifests by asset root.

3. Output file detection (`SyncedAssetsMtime`) is needed for worker-side output sync operations.
