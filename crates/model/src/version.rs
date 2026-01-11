//! Manifest version, path style, and specification version types.

use serde::{Deserialize, Serialize};

/// Supported manifest format versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum ManifestVersion {
    #[serde(rename = "2023-03-03")]
    V2023_03_03,
    #[serde(rename = "2025-12")]
    V2025_12,
}

impl ManifestVersion {
    /// Get the string representation of the version.
    pub fn as_str(&self) -> &'static str {
        match self {
            ManifestVersion::V2023_03_03 => "2023-03-03",
            ManifestVersion::V2025_12 => "2025-12",
        }
    }
}

impl std::fmt::Display for ManifestVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Type of manifest (snapshot vs diff).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ManifestType {
    #[default]
    Snapshot,
    Diff,
}

/// Path style for v2025-12 manifests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PathStyle {
    /// Paths are absolute filesystem paths (e.g., `/home/user/project/file.txt`).
    Absolute,
    /// Paths are relative to manifest root (e.g., `project/file.txt`).
    #[default]
    Relative,
}

/// Combined specification version for v2025-12 format.
///
/// Encodes both path style (absolute/relative) and manifest type (snapshot/diff)
/// into a single specification version string for JSON serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpecVersion {
    pub path_style: PathStyle,
    pub manifest_type: ManifestType,
}

impl SpecVersion {
    /// Absolute-path snapshot manifest.
    pub const ABS_SNAPSHOT: Self = Self {
        path_style: PathStyle::Absolute,
        manifest_type: ManifestType::Snapshot,
    };

    /// Absolute-path diff manifest.
    pub const ABS_DIFF: Self = Self {
        path_style: PathStyle::Absolute,
        manifest_type: ManifestType::Diff,
    };

    /// Relative-path snapshot manifest.
    pub const REL_SNAPSHOT: Self = Self {
        path_style: PathStyle::Relative,
        manifest_type: ManifestType::Snapshot,
    };

    /// Relative-path diff manifest.
    pub const REL_DIFF: Self = Self {
        path_style: PathStyle::Relative,
        manifest_type: ManifestType::Diff,
    };

    /// Get the specification version string for JSON encoding.
    pub fn spec_string(&self) -> &'static str {
        match (self.path_style, self.manifest_type) {
            (PathStyle::Absolute, ManifestType::Snapshot) => {
                "absolute-manifest-snapshot-beta-2025-12"
            }
            (PathStyle::Absolute, ManifestType::Diff) => "absolute-manifest-diff-beta-2025-12",
            (PathStyle::Relative, ManifestType::Snapshot) => {
                "relative-manifest-snapshot-beta-2025-12"
            }
            (PathStyle::Relative, ManifestType::Diff) => "relative-manifest-diff-beta-2025-12",
        }
    }

    /// Parse specification version string.
    ///
    /// # Arguments
    /// * `s` - Specification version string from JSON.
    ///
    /// # Returns
    /// `Some(SpecVersion)` if valid, `None` otherwise.
    pub fn from_spec_string(s: &str) -> Option<Self> {
        match s {
            "absolute-manifest-snapshot-beta-2025-12" => Some(Self::ABS_SNAPSHOT),
            "absolute-manifest-diff-beta-2025-12" => Some(Self::ABS_DIFF),
            "relative-manifest-snapshot-beta-2025-12" => Some(Self::REL_SNAPSHOT),
            "relative-manifest-diff-beta-2025-12" => Some(Self::REL_DIFF),
            _ => None,
        }
    }

    /// Create a new SpecVersion.
    pub fn new(path_style: PathStyle, manifest_type: ManifestType) -> Self {
        Self {
            path_style,
            manifest_type,
        }
    }

    /// Check if this is an absolute-path manifest.
    pub fn is_absolute(&self) -> bool {
        self.path_style == PathStyle::Absolute
    }

    /// Check if this is a relative-path manifest.
    pub fn is_relative(&self) -> bool {
        self.path_style == PathStyle::Relative
    }

    /// Check if this is a snapshot manifest.
    pub fn is_snapshot(&self) -> bool {
        self.manifest_type == ManifestType::Snapshot
    }

    /// Check if this is a diff manifest.
    pub fn is_diff(&self) -> bool {
        self.manifest_type == ManifestType::Diff
    }
}

impl Default for SpecVersion {
    fn default() -> Self {
        Self::REL_SNAPSHOT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spec_version_roundtrip() {
        let versions: [SpecVersion; 4] = [
            SpecVersion::ABS_SNAPSHOT,
            SpecVersion::ABS_DIFF,
            SpecVersion::REL_SNAPSHOT,
            SpecVersion::REL_DIFF,
        ];

        for version in versions {
            let s: &str = version.spec_string();
            let parsed: SpecVersion = SpecVersion::from_spec_string(s).unwrap();
            assert_eq!(version, parsed);
        }
    }

    #[test]
    fn test_spec_version_strings() {
        assert_eq!(
            SpecVersion::ABS_SNAPSHOT.spec_string(),
            "absolute-manifest-snapshot-beta-2025-12"
        );
        assert_eq!(
            SpecVersion::ABS_DIFF.spec_string(),
            "absolute-manifest-diff-beta-2025-12"
        );
        assert_eq!(
            SpecVersion::REL_SNAPSHOT.spec_string(),
            "relative-manifest-snapshot-beta-2025-12"
        );
        assert_eq!(
            SpecVersion::REL_DIFF.spec_string(),
            "relative-manifest-diff-beta-2025-12"
        );
    }

    #[test]
    fn test_spec_version_from_invalid_string() {
        assert!(SpecVersion::from_spec_string("invalid").is_none());
        assert!(SpecVersion::from_spec_string("2025-12-04-beta").is_none());
    }

    #[test]
    fn test_spec_version_predicates() {
        assert!(SpecVersion::ABS_SNAPSHOT.is_absolute());
        assert!(SpecVersion::ABS_SNAPSHOT.is_snapshot());
        assert!(!SpecVersion::ABS_SNAPSHOT.is_relative());
        assert!(!SpecVersion::ABS_SNAPSHOT.is_diff());

        assert!(SpecVersion::REL_DIFF.is_relative());
        assert!(SpecVersion::REL_DIFF.is_diff());
    }
}
