//! Typed manifest wrappers for compile-time path style and manifest type enforcement.
//!
//! These newtypes wrap `Manifest` and enforce constraints at the type level:
//! - `Snapshot` - Relative-path snapshot manifest
//! - `SnapshotDiff` - Relative-path diff manifest
//! - `AbsSnapshot` - Absolute-path snapshot manifest
//! - `AbsSnapshotDiff` - Absolute-path diff manifest

use std::path::Path;

use crate::error::ManifestTypeError;
use crate::version::{ManifestType, PathStyle, SpecVersion};
use crate::{v2025_12, Manifest};

/// Relative-path snapshot manifest.
///
/// Spec: `relative-manifest-snapshot-beta-2025-12`
#[derive(Debug, Clone)]
pub struct Snapshot(pub(crate) Manifest);

impl Snapshot {
    /// Create from manifest, validating constraints.
    ///
    /// # Errors
    /// Returns error if manifest is not a relative-path snapshot.
    pub fn try_from(manifest: Manifest) -> Result<Self, ManifestTypeError> {
        validate_spec(&manifest, PathStyle::Relative, ManifestType::Snapshot)?;
        Ok(Self(manifest))
    }

    /// Create a new snapshot from v2025-12 data.
    pub fn new(
        dirs: Vec<v2025_12::ManifestDirectoryPath>,
        files: Vec<v2025_12::ManifestFilePath>,
    ) -> Self {
        Self(Manifest::V2025_12(v2025_12::AssetManifest::snapshot(
            dirs, files,
        )))
    }

    /// Convert to absolute paths by prepending a root.
    ///
    /// # Arguments
    /// * `root` - Root path to prepend to all paths.
    pub fn to_absolute(self, root: &Path) -> AbsSnapshot {
        let manifest: Manifest = transform_to_absolute(self.0, root, SpecVersion::ABS_SNAPSHOT);
        AbsSnapshot(manifest)
    }

    /// Access inner manifest.
    pub fn inner(&self) -> &Manifest {
        &self.0
    }

    /// Consume and return inner manifest.
    pub fn into_inner(self) -> Manifest {
        self.0
    }

    /// Get the spec version.
    pub fn spec_version(&self) -> SpecVersion {
        SpecVersion::REL_SNAPSHOT
    }
}

/// Relative-path diff manifest.
///
/// Spec: `relative-manifest-diff-beta-2025-12`
#[derive(Debug, Clone)]
pub struct SnapshotDiff(pub(crate) Manifest);

impl SnapshotDiff {
    /// Create from manifest, validating constraints.
    ///
    /// # Errors
    /// Returns error if manifest is not a relative-path diff.
    pub fn try_from(manifest: Manifest) -> Result<Self, ManifestTypeError> {
        validate_spec(&manifest, PathStyle::Relative, ManifestType::Diff)?;
        Ok(Self(manifest))
    }

    /// Create a new diff from v2025-12 data.
    pub fn new(
        dirs: Vec<v2025_12::ManifestDirectoryPath>,
        files: Vec<v2025_12::ManifestFilePath>,
        parent_hash: impl Into<String>,
    ) -> Self {
        Self(Manifest::V2025_12(v2025_12::AssetManifest::diff(
            dirs,
            files,
            parent_hash,
        )))
    }

    /// Get parent manifest hash (required for diff manifests).
    pub fn parent_hash(&self) -> &str {
        match &self.0 {
            Manifest::V2025_12(m) => m.parent_manifest_hash.as_deref().unwrap(),
            _ => unreachable!("SnapshotDiff only wraps V2025_12"),
        }
    }

    /// Convert to absolute paths by prepending a root.
    pub fn to_absolute(self, root: &Path) -> AbsSnapshotDiff {
        let manifest: Manifest = transform_to_absolute(self.0, root, SpecVersion::ABS_DIFF);
        AbsSnapshotDiff(manifest)
    }

    /// Access inner manifest.
    pub fn inner(&self) -> &Manifest {
        &self.0
    }

    /// Consume and return inner manifest.
    pub fn into_inner(self) -> Manifest {
        self.0
    }

    /// Get the spec version.
    pub fn spec_version(&self) -> SpecVersion {
        SpecVersion::REL_DIFF
    }
}

/// Absolute-path snapshot manifest.
///
/// Spec: `absolute-manifest-snapshot-beta-2025-12`
#[derive(Debug, Clone)]
pub struct AbsSnapshot(pub(crate) Manifest);

impl AbsSnapshot {
    /// Create from manifest, validating constraints.
    ///
    /// # Errors
    /// Returns error if manifest is not an absolute-path snapshot.
    pub fn try_from(manifest: Manifest) -> Result<Self, ManifestTypeError> {
        validate_spec(&manifest, PathStyle::Absolute, ManifestType::Snapshot)?;
        Ok(Self(manifest))
    }

    /// Create a new absolute snapshot from v2025-12 data.
    pub fn new(
        dirs: Vec<v2025_12::ManifestDirectoryPath>,
        files: Vec<v2025_12::ManifestFilePath>,
    ) -> Self {
        Self(Manifest::V2025_12(v2025_12::AssetManifest::abs_snapshot(
            dirs, files,
        )))
    }

    /// Convert to relative paths by stripping a root prefix.
    ///
    /// # Arguments
    /// * `root` - Root path to strip from all paths.
    pub fn to_relative(self, root: &Path) -> Snapshot {
        let manifest: Manifest = transform_to_relative(self.0, root, SpecVersion::REL_SNAPSHOT);
        Snapshot(manifest)
    }

    /// Access inner manifest.
    pub fn inner(&self) -> &Manifest {
        &self.0
    }

    /// Consume and return inner manifest.
    pub fn into_inner(self) -> Manifest {
        self.0
    }

    /// Get the spec version.
    pub fn spec_version(&self) -> SpecVersion {
        SpecVersion::ABS_SNAPSHOT
    }

    /// Get the files in this manifest.
    pub fn files(&self) -> &[v2025_12::ManifestFilePath] {
        match &self.0 {
            Manifest::V2025_12(m) => &m.files,
            _ => unreachable!("AbsSnapshot only wraps V2025_12"),
        }
    }

    /// Get the directories in this manifest.
    pub fn dirs(&self) -> &[v2025_12::ManifestDirectoryPath] {
        match &self.0 {
            Manifest::V2025_12(m) => &m.dirs,
            _ => unreachable!("AbsSnapshot only wraps V2025_12"),
        }
    }
}

/// Absolute-path diff manifest.
///
/// Spec: `absolute-manifest-diff-beta-2025-12`
#[derive(Debug, Clone)]
pub struct AbsSnapshotDiff(pub(crate) Manifest);

impl AbsSnapshotDiff {
    /// Create from manifest, validating constraints.
    ///
    /// # Errors
    /// Returns error if manifest is not an absolute-path diff.
    pub fn try_from(manifest: Manifest) -> Result<Self, ManifestTypeError> {
        validate_spec(&manifest, PathStyle::Absolute, ManifestType::Diff)?;
        Ok(Self(manifest))
    }

    /// Create a new absolute diff from v2025-12 data.
    pub fn new(
        dirs: Vec<v2025_12::ManifestDirectoryPath>,
        files: Vec<v2025_12::ManifestFilePath>,
        parent_hash: impl Into<String>,
    ) -> Self {
        Self(Manifest::V2025_12(v2025_12::AssetManifest::abs_diff(
            dirs,
            files,
            parent_hash,
        )))
    }

    /// Get parent manifest hash (required for diff manifests).
    pub fn parent_hash(&self) -> &str {
        match &self.0 {
            Manifest::V2025_12(m) => m.parent_manifest_hash.as_deref().unwrap(),
            _ => unreachable!("AbsSnapshotDiff only wraps V2025_12"),
        }
    }

    /// Convert to relative paths by stripping a root prefix.
    pub fn to_relative(self, root: &Path) -> SnapshotDiff {
        let manifest: Manifest = transform_to_relative(self.0, root, SpecVersion::REL_DIFF);
        SnapshotDiff(manifest)
    }

    /// Access inner manifest.
    pub fn inner(&self) -> &Manifest {
        &self.0
    }

    /// Consume and return inner manifest.
    pub fn into_inner(self) -> Manifest {
        self.0
    }

    /// Get the spec version.
    pub fn spec_version(&self) -> SpecVersion {
        SpecVersion::ABS_DIFF
    }

    /// Get the files in this manifest.
    pub fn files(&self) -> &[v2025_12::ManifestFilePath] {
        match &self.0 {
            Manifest::V2025_12(m) => &m.files,
            _ => unreachable!("AbsSnapshotDiff only wraps V2025_12"),
        }
    }

    /// Get the directories in this manifest.
    pub fn dirs(&self) -> &[v2025_12::ManifestDirectoryPath] {
        match &self.0 {
            Manifest::V2025_12(m) => &m.dirs,
            _ => unreachable!("AbsSnapshotDiff only wraps V2025_12"),
        }
    }
}

/// Validate that a manifest matches expected path style and manifest type.
fn validate_spec(
    manifest: &Manifest,
    expected_path_style: PathStyle,
    expected_manifest_type: ManifestType,
) -> Result<(), ManifestTypeError> {
    match manifest {
        Manifest::V2023_03_03(_) => Err(ManifestTypeError::LegacyVersionNotSupported),
        Manifest::V2025_12(m) => {
            if m.path_style() != expected_path_style {
                return Err(ManifestTypeError::PathStyleMismatch {
                    expected: expected_path_style,
                    actual: m.path_style(),
                });
            }
            if m.manifest_type() != expected_manifest_type {
                return Err(ManifestTypeError::TypeMismatch {
                    expected: expected_manifest_type,
                    actual: m.manifest_type(),
                });
            }
            Ok(())
        }
    }
}

/// Transform a manifest to absolute paths by prepending a root.
fn transform_to_absolute(manifest: Manifest, root: &Path, new_spec: SpecVersion) -> Manifest {
    match manifest {
        Manifest::V2023_03_03(_) => {
            unreachable!("Cannot transform V2023_03_03 to absolute")
        }
        Manifest::V2025_12(m) => {
            let root_str: String = root.to_string_lossy().to_string();
            let root_prefix: &str = if root_str.ends_with('/') {
                &root_str
            } else {
                &format!("{}/", root_str)
            };

            let dirs: Vec<v2025_12::ManifestDirectoryPath> = m
                .dirs
                .into_iter()
                .map(|d| v2025_12::ManifestDirectoryPath {
                    path: format!("{}{}", root_prefix, d.path),
                    deleted: d.deleted,
                })
                .collect();

            let files: Vec<v2025_12::ManifestFilePath> = m
                .files
                .into_iter()
                .map(|f| v2025_12::ManifestFilePath {
                    path: format!("{}{}", root_prefix, f.path),
                    ..f
                })
                .collect();

            Manifest::V2025_12(v2025_12::AssetManifest::with_spec(
                dirs,
                files,
                new_spec,
                m.parent_manifest_hash,
            ))
        }
    }
}

/// Transform a manifest to relative paths by stripping a root prefix.
fn transform_to_relative(manifest: Manifest, root: &Path, new_spec: SpecVersion) -> Manifest {
    match manifest {
        Manifest::V2023_03_03(_) => {
            unreachable!("Cannot transform V2023_03_03 to relative")
        }
        Manifest::V2025_12(m) => {
            let root_str: String = root.to_string_lossy().to_string();
            let root_prefix: String = if root_str.ends_with('/') {
                root_str
            } else {
                format!("{}/", root_str)
            };

            let dirs: Vec<v2025_12::ManifestDirectoryPath> = m
                .dirs
                .into_iter()
                .map(|d| v2025_12::ManifestDirectoryPath {
                    path: d
                        .path
                        .strip_prefix(&root_prefix)
                        .unwrap_or(&d.path)
                        .to_string(),
                    deleted: d.deleted,
                })
                .collect();

            let files: Vec<v2025_12::ManifestFilePath> = m
                .files
                .into_iter()
                .map(|f| v2025_12::ManifestFilePath {
                    path: f
                        .path
                        .strip_prefix(&root_prefix)
                        .unwrap_or(&f.path)
                        .to_string(),
                    ..f
                })
                .collect();

            Manifest::V2025_12(v2025_12::AssetManifest::with_spec(
                dirs,
                files,
                new_spec,
                m.parent_manifest_hash,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_snapshot_creation() {
        let snapshot: Snapshot = Snapshot::new(vec![], vec![]);
        assert_eq!(snapshot.spec_version(), SpecVersion::REL_SNAPSHOT);
    }

    #[test]
    fn test_abs_snapshot_creation() {
        let snapshot: AbsSnapshot = AbsSnapshot::new(vec![], vec![]);
        assert_eq!(snapshot.spec_version(), SpecVersion::ABS_SNAPSHOT);
    }

    #[test]
    fn test_snapshot_to_absolute() {
        let files: Vec<v2025_12::ManifestFilePath> = vec![v2025_12::ManifestFilePath::file(
            "test.txt", "abc123", 100, 1234567890,
        )];
        let snapshot: Snapshot = Snapshot::new(vec![], files);

        let root: PathBuf = PathBuf::from("/home/user/project");
        let abs_snapshot: AbsSnapshot = snapshot.to_absolute(&root);

        assert_eq!(abs_snapshot.spec_version(), SpecVersion::ABS_SNAPSHOT);
        assert_eq!(abs_snapshot.files()[0].path, "/home/user/project/test.txt");
    }

    #[test]
    fn test_abs_snapshot_to_relative() {
        let files: Vec<v2025_12::ManifestFilePath> = vec![v2025_12::ManifestFilePath::file(
            "/home/user/project/test.txt",
            "abc123",
            100,
            1234567890,
        )];
        let abs_snapshot: AbsSnapshot = AbsSnapshot::new(vec![], files);

        let root: PathBuf = PathBuf::from("/home/user/project");
        let snapshot: Snapshot = abs_snapshot.to_relative(&root);

        assert_eq!(snapshot.spec_version(), SpecVersion::REL_SNAPSHOT);
        if let Manifest::V2025_12(m) = snapshot.inner() {
            assert_eq!(m.files[0].path, "test.txt");
        }
    }

    #[test]
    fn test_diff_parent_hash() {
        let diff: SnapshotDiff = SnapshotDiff::new(vec![], vec![], "parent_hash_123");
        assert_eq!(diff.parent_hash(), "parent_hash_123");
    }

    #[test]
    fn test_try_from_wrong_type() {
        let manifest: Manifest =
            Manifest::V2025_12(v2025_12::AssetManifest::abs_snapshot(vec![], vec![]));

        // Try to create Snapshot from AbsSnapshot - should fail
        let result: Result<Snapshot, ManifestTypeError> = Snapshot::try_from(manifest);
        assert!(matches!(
            result,
            Err(ManifestTypeError::PathStyleMismatch { .. })
        ));
    }

    #[test]
    fn test_try_from_legacy_version() {
        let manifest: Manifest =
            Manifest::V2023_03_03(crate::v2023_03_03::AssetManifest::new(vec![]));

        let result: Result<Snapshot, ManifestTypeError> = Snapshot::try_from(manifest);
        assert!(matches!(
            result,
            Err(ManifestTypeError::LegacyVersionNotSupported)
        ));
    }
}
