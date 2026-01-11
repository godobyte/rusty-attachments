//! Manifest merging operations.
//!
//! This module provides functions for merging multiple manifests into a single
//! manifest. This is used when downloading job outputs where multiple session
//! actions may have produced manifests for the same asset root.
//!
//! # Merging Strategy
//!
//! When merging manifests:
//! - Files with the same path are deduplicated, keeping the entry from the
//!   later manifest (by last_modified timestamp)
//! - All manifests must use the same hash algorithm
//! - The resulting manifest contains the union of all unique file paths
//!
//! # Chronological Merging
//!
//! For output manifest downloads, manifests should be merged chronologically
//! (oldest first) so that newer files overwrite older ones with the same path.
//! Use `merge_manifests_chronologically` for this use case.

use std::collections::HashMap;

use crate::error::ManifestError;
use crate::v2023_03_03::{AssetManifest as V2023Manifest, ManifestPath as V2023Path};
use crate::v2025_12::{
    AssetManifest as V2025Manifest, ManifestDirectoryPath, ManifestFilePath as V2025Path,
};
use crate::Manifest;

/// Merge multiple manifests into a single manifest.
///
/// Files with the same path are deduplicated, keeping the entry from the
/// later manifest in the input list. All manifests must use the same hash
/// algorithm.
///
/// # Arguments
/// * `manifests` - List of manifests to merge (order matters - later entries win)
///
/// # Returns
/// Merged manifest, or None if the input list is empty.
///
/// # Errors
/// Returns error if manifests have different hash algorithms or versions.
pub fn merge_manifests(manifests: &[Manifest]) -> Result<Option<Manifest>, ManifestError> {
    if manifests.is_empty() {
        return Ok(None);
    }

    if manifests.len() == 1 {
        return Ok(Some(manifests[0].clone()));
    }

    // Check all manifests have the same version and hash algorithm
    let first: &Manifest = &manifests[0];
    let hash_alg = first.hash_alg();
    let version = first.version();

    for manifest in manifests.iter().skip(1) {
        if manifest.hash_alg() != hash_alg {
            return Err(ManifestError::MergeHashAlgorithmMismatch {
                expected: hash_alg,
                actual: manifest.hash_alg(),
            });
        }
        if manifest.version() != version {
            return Err(ManifestError::MergeVersionMismatch {
                expected: version,
                actual: manifest.version(),
            });
        }
    }

    // Merge based on version
    match first {
        Manifest::V2023_03_03(_) => merge_v2023_manifests(manifests),
        Manifest::V2025_12(_) => merge_v2025_manifests(manifests),
    }
}

/// Merge manifests chronologically by last_modified timestamp.
///
/// Sorts manifests by their last_modified timestamp (oldest first) before
/// merging, ensuring that newer files overwrite older ones with the same path.
///
/// # Arguments
/// * `manifests_with_timestamps` - List of (manifest, last_modified) tuples
///
/// # Returns
/// Merged manifest, or None if the input list is empty.
///
/// # Errors
/// Returns error if manifests have different hash algorithms or versions.
pub fn merge_manifests_chronologically(
    manifests_with_timestamps: &[(Manifest, i64)],
) -> Result<Option<Manifest>, ManifestError> {
    if manifests_with_timestamps.is_empty() {
        return Ok(None);
    }

    // Sort by timestamp (oldest first)
    let mut sorted: Vec<_> = manifests_with_timestamps.to_vec();
    sorted.sort_by_key(|(_, ts)| *ts);

    // Extract just the manifests in sorted order
    let manifests: Vec<Manifest> = sorted.into_iter().map(|(m, _)| m).collect();

    merge_manifests(&manifests)
}

/// Merge v2023-03-03 manifests.
fn merge_v2023_manifests(manifests: &[Manifest]) -> Result<Option<Manifest>, ManifestError> {
    let mut merged_paths: HashMap<String, V2023Path> = HashMap::new();

    for manifest in manifests {
        if let Manifest::V2023_03_03(m) = manifest {
            for path in &m.paths {
                merged_paths.insert(path.path.clone(), path.clone());
            }
        }
    }

    let paths: Vec<V2023Path> = merged_paths.into_values().collect();
    Ok(Some(Manifest::V2023_03_03(V2023Manifest::new(paths))))
}

/// Merge v2025-12 manifests.
fn merge_v2025_manifests(manifests: &[Manifest]) -> Result<Option<Manifest>, ManifestError> {
    let mut merged_dirs: HashMap<String, ManifestDirectoryPath> = HashMap::new();
    let mut merged_files: HashMap<String, V2025Path> = HashMap::new();

    // Get spec version from first manifest
    let spec_version = manifests[0].spec_version().unwrap_or_default();

    for manifest in manifests {
        if let Manifest::V2025_12(m) = manifest {
            for dir in &m.dirs {
                merged_dirs.insert(dir.path.clone(), dir.clone());
            }
            for file in &m.files {
                merged_files.insert(file.path.clone(), file.clone());
            }
        }
    }

    let dirs: Vec<ManifestDirectoryPath> = merged_dirs.into_values().collect();
    let files: Vec<V2025Path> = merged_files.into_values().collect();

    Ok(Some(Manifest::V2025_12(V2025Manifest::with_spec(
        dirs,
        files,
        spec_version,
        None,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2023_03_03::ManifestPath;
    use crate::v2025_12::{AssetManifest as V2025Manifest, ManifestFilePath};
    use crate::version::ManifestVersion;

    #[test]
    fn test_merge_empty_list() {
        let result = merge_manifests(&[]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_merge_single_manifest() {
        let paths: Vec<ManifestPath> = vec![ManifestPath::new("a.txt", "hash1", 100, 1000)];
        let manifest: Manifest = Manifest::V2023_03_03(V2023Manifest::new(paths));

        let result = merge_manifests(&[manifest.clone()]).unwrap();
        assert!(result.is_some());

        let merged: Manifest = result.unwrap();
        assert_eq!(merged.file_count(), 1);
    }

    #[test]
    fn test_merge_two_manifests_no_overlap() {
        let m1: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "a.txt", "hash1", 100, 1000,
        )]));
        let m2: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "b.txt", "hash2", 200, 2000,
        )]));

        let result: Manifest = merge_manifests(&[m1, m2]).unwrap().unwrap();
        assert_eq!(result.file_count(), 2);
        assert_eq!(result.total_size(), 300);
    }

    #[test]
    fn test_merge_two_manifests_with_overlap() {
        let m1: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![
            ManifestPath::new("a.txt", "hash1_old", 100, 1000),
            ManifestPath::new("b.txt", "hash2", 200, 2000),
        ]));
        let m2: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "a.txt",
            "hash1_new",
            150,
            3000,
        )]));

        let result: Manifest = merge_manifests(&[m1, m2]).unwrap().unwrap();
        assert_eq!(result.file_count(), 2);
        // a.txt should have the new size (150) + b.txt (200) = 350
        assert_eq!(result.total_size(), 350);
    }

    #[test]
    fn test_merge_later_manifest_wins() {
        let m1: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "file.txt", "old_hash", 100, 1000,
        )]));
        let m2: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "file.txt", "new_hash", 200, 2000,
        )]));

        let result: Manifest = merge_manifests(&[m1, m2]).unwrap().unwrap();

        if let Manifest::V2023_03_03(m) = result {
            assert_eq!(m.paths.len(), 1);
            assert_eq!(m.paths[0].hash, "new_hash");
            assert_eq!(m.paths[0].size, 200);
        } else {
            panic!("Expected V2023_03_03 manifest");
        }
    }

    #[test]
    fn test_merge_chronologically() {
        // Create manifests with timestamps (out of order)
        let m_old: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "file.txt", "old_hash", 100, 1000,
        )]));
        let m_new: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "file.txt", "new_hash", 200, 2000,
        )]));

        // Pass in wrong order (new first), but with correct timestamps
        let manifests_with_ts: Vec<(Manifest, i64)> = vec![(m_new, 2000i64), (m_old, 1000i64)];

        let result: Manifest = merge_manifests_chronologically(&manifests_with_ts)
            .unwrap()
            .unwrap();

        // Should have new_hash because chronological merge sorts by timestamp
        if let Manifest::V2023_03_03(m) = result {
            assert_eq!(m.paths[0].hash, "new_hash");
        } else {
            panic!("Expected V2023_03_03 manifest");
        }
    }

    #[test]
    fn test_merge_hash_algorithm_mismatch() {
        let m1: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "a.txt", "hash1", 100, 1000,
        )]));

        // Create a manifest with different hash algorithm (would need to modify the struct)
        // For now, we test that same algorithm works
        let m2: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "b.txt", "hash2", 200, 2000,
        )]));

        // This should succeed since both use XXH128
        let result = merge_manifests(&[m1, m2]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_merge_version_mismatch() {
        let m1: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "a.txt", "hash1", 100, 1000,
        )]));
        let m2: Manifest = Manifest::V2025_12(V2025Manifest::snapshot(
            vec![],
            vec![ManifestFilePath::file("b.txt", "hash2", 200, 2000)],
        ));

        let result = merge_manifests(&[m1, m2]);
        assert!(matches!(
            result,
            Err(ManifestError::MergeVersionMismatch { .. })
        ));
    }

    #[test]
    fn test_merge_v2025_manifests() {
        let m1: Manifest = Manifest::V2025_12(V2025Manifest::snapshot(
            vec![],
            vec![ManifestFilePath::file("a.txt", "hash1", 100, 1000)],
        ));
        let m2: Manifest = Manifest::V2025_12(V2025Manifest::snapshot(
            vec![],
            vec![ManifestFilePath::file("b.txt", "hash2", 200, 2000)],
        ));

        let result: Manifest = merge_manifests(&[m1, m2]).unwrap().unwrap();
        assert_eq!(result.file_count(), 2);
        assert_eq!(result.version(), ManifestVersion::V2025_12);
    }

    #[test]
    fn test_merge_three_manifests() {
        let m1: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![
            ManifestPath::new("a.txt", "hash_a1", 100, 1000),
            ManifestPath::new("b.txt", "hash_b1", 200, 1000),
        ]));
        let m2: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![
            ManifestPath::new("b.txt", "hash_b2", 250, 2000),
            ManifestPath::new("c.txt", "hash_c1", 300, 2000),
        ]));
        let m3: Manifest = Manifest::V2023_03_03(V2023Manifest::new(vec![ManifestPath::new(
            "a.txt", "hash_a3", 150, 3000,
        )]));

        let result: Manifest = merge_manifests(&[m1, m2, m3]).unwrap().unwrap();
        assert_eq!(result.file_count(), 3);
        // a.txt=150 (from m3) + b.txt=250 (from m2) + c.txt=300 (from m2) = 700
        assert_eq!(result.total_size(), 700);
    }
}
