//! Manifest version and type enums.

use serde::{Deserialize, Serialize};

/// Supported manifest format versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum ManifestVersion {
    #[serde(rename = "2023-03-03")]
    V2023_03_03,
    #[serde(rename = "2025-12-04-beta")]
    V2025_12_04_beta,
}

impl ManifestVersion {
    /// Get the string representation of the version.
    pub fn as_str(&self) -> &'static str {
        match self {
            ManifestVersion::V2023_03_03 => "2023-03-03",
            ManifestVersion::V2025_12_04_beta => "2025-12-04-beta",
        }
    }
}

impl std::fmt::Display for ManifestVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Type of manifest (snapshot vs diff).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManifestType {
    Snapshot,
    Diff,
}

impl Default for ManifestType {
    fn default() -> Self {
        ManifestType::Snapshot
    }
}

/// Content-Type values for S3 storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum ManifestContentType {
    Snapshot2025_12_04_beta,
    Diff2025_12_04_beta,
}

impl ManifestContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ManifestContentType::Snapshot2025_12_04_beta => {
                "application/x-deadline-manifest-2025-12-04-beta"
            }
            ManifestContentType::Diff2025_12_04_beta => {
                "application/x-deadline-manifest-diff-2025-12-04-beta"
            }
        }
    }
}
