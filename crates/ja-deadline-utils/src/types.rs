//! Job attachment types for Deadline Cloud API.

use std::path::PathBuf;

use rusty_attachments_model::Manifest;
use serde::{Deserialize, Serialize};

/// Operating system path format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathFormat {
    Windows,
    Posix,
}

impl PathFormat {
    /// Get the path format for the current host.
    pub fn host() -> Self {
        #[cfg(windows)]
        {
            PathFormat::Windows
        }
        #[cfg(not(windows))]
        {
            PathFormat::Posix
        }
    }
}

/// Properties of a single manifest for job submission.
///
/// Serializes to the format expected by Deadline Cloud CreateJob API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestProperties {
    /// Root path the manifest was created from.
    pub root_path: String,
    /// Path format of the root path.
    pub root_path_format: PathFormat,
    /// S3 key path to the uploaded manifest (partial, excludes root prefix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_manifest_path: Option<String>,
    /// Hash of the manifest content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_manifest_hash: Option<String>,
    /// Relative paths of output directories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_relative_directories: Option<Vec<String>>,
    /// File system location name (from storage profile).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_system_location_name: Option<String>,
}

/// Job attachments payload for CreateJob API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachments {
    /// List of manifest properties.
    pub manifests: Vec<ManifestProperties>,
    /// File system mode: "COPIED" or "VIRTUAL".
    pub file_system: String,
}

impl Default for Attachments {
    fn default() -> Self {
        Self {
            manifests: Vec::new(),
            file_system: "COPIED".into(),
        }
    }
}

impl Attachments {
    /// Serialize to JSON for API submission.
    ///
    /// # Returns
    /// JSON string representation.
    ///
    /// # Errors
    /// Returns error if serialization fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serialize to pretty JSON for debugging.
    ///
    /// # Returns
    /// Pretty-printed JSON string.
    ///
    /// # Errors
    /// Returns error if serialization fails.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// A manifest with its associated metadata for a single asset root.
///
/// Used as intermediate representation during bundle submit.
#[derive(Debug, Clone)]
pub struct AssetRootManifest {
    /// Root path for this manifest.
    pub root_path: String,
    /// The manifest (None if no input files for this root).
    pub asset_manifest: Option<Manifest>,
    /// Output directories (absolute paths).
    pub outputs: Vec<PathBuf>,
    /// File system location name (from storage profile).
    pub file_system_location_name: Option<String>,
}

/// Asset references parsed from job bundle's asset_references.yaml/json.
#[derive(Debug, Clone, Default)]
pub struct AssetReferences {
    /// Input files to upload.
    pub input_filenames: Vec<PathBuf>,
    /// Output directories to track.
    pub output_directories: Vec<PathBuf>,
    /// Referenced paths (may not exist).
    pub referenced_paths: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_format_host() {
        let format: PathFormat = PathFormat::host();
        #[cfg(windows)]
        assert_eq!(format, PathFormat::Windows);
        #[cfg(not(windows))]
        assert_eq!(format, PathFormat::Posix);
    }

    #[test]
    fn test_attachments_default() {
        let attachments: Attachments = Attachments::default();
        assert!(attachments.manifests.is_empty());
        assert_eq!(attachments.file_system, "COPIED");
    }

    #[test]
    fn test_attachments_to_json() {
        let attachments: Attachments = Attachments {
            manifests: vec![ManifestProperties {
                root_path: "/projects/job1".into(),
                root_path_format: PathFormat::Posix,
                input_manifest_path: Some("farm-123/queue-456/input".into()),
                input_manifest_hash: Some("abc123".into()),
                output_relative_directories: Some(vec!["renders".into()]),
                file_system_location_name: Some("ProjectFiles".into()),
            }],
            file_system: "COPIED".into(),
        };

        let json: String = attachments.to_json().unwrap();
        assert!(json.contains("rootPath"));
        assert!(json.contains("/projects/job1"));
        assert!(json.contains("posix"));
    }

    #[test]
    fn test_manifest_properties_skip_none_fields() {
        let props: ManifestProperties = ManifestProperties {
            root_path: "/test".into(),
            root_path_format: PathFormat::Posix,
            input_manifest_path: None,
            input_manifest_hash: None,
            output_relative_directories: None,
            file_system_location_name: None,
        };

        let json: String = serde_json::to_string(&props).unwrap();
        assert!(!json.contains("inputManifestPath"));
        assert!(!json.contains("inputManifestHash"));
        assert!(!json.contains("outputRelativeDirectories"));
        assert!(!json.contains("fileSystemLocationName"));
    }
}
