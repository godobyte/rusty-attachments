//! Conversion functions for manifest to API format.

use crate::types::{AssetRootManifest, Attachments, ManifestProperties, PathFormat};

/// Convert an uploaded manifest into ManifestProperties for job submission.
///
/// # Arguments
/// * `asset_root_manifest` - The manifest with its metadata
/// * `partial_manifest_key` - S3 key (partial, without root prefix)
/// * `manifest_hash` - Hash of the manifest content
///
/// # Returns
/// ManifestProperties ready for inclusion in Attachments.
pub fn build_manifest_properties(
    asset_root_manifest: &AssetRootManifest,
    partial_manifest_key: Option<&str>,
    manifest_hash: Option<&str>,
) -> ManifestProperties {
    let output_rel_paths: Option<Vec<String>> = if asset_root_manifest.outputs.is_empty() {
        None
    } else {
        Some(
            asset_root_manifest
                .outputs
                .iter()
                .filter_map(|path| {
                    path.strip_prefix(&asset_root_manifest.root_path)
                        .ok()
                        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                })
                .collect(),
        )
    };

    ManifestProperties {
        root_path: asset_root_manifest.root_path.clone(),
        root_path_format: PathFormat::host(),
        input_manifest_path: partial_manifest_key.map(String::from),
        input_manifest_hash: manifest_hash.map(String::from),
        output_relative_directories: output_rel_paths,
        file_system_location_name: asset_root_manifest.file_system_location_name.clone(),
    }
}

/// Convert a list of manifest properties into an Attachments payload.
///
/// # Arguments
/// * `manifest_properties` - List of manifest properties
/// * `file_system_mode` - "COPIED" or "VIRTUAL"
///
/// # Returns
/// Attachments payload ready for CreateJob API.
pub fn build_attachments(
    manifest_properties: Vec<ManifestProperties>,
    file_system_mode: &str,
) -> Attachments {
    Attachments {
        manifests: manifest_properties,
        file_system: file_system_mode.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_build_manifest_properties_with_inputs() {
        let manifest: AssetRootManifest = AssetRootManifest {
            root_path: "/projects/job1".into(),
            asset_manifest: None,
            outputs: vec![],
            file_system_location_name: Some("ProjectFiles".into()),
        };

        let props: ManifestProperties = build_manifest_properties(
            &manifest,
            Some("farm-123/queue-456/input"),
            Some("abc123def456"),
        );

        assert_eq!(props.root_path, "/projects/job1");
        assert_eq!(props.input_manifest_path, Some("farm-123/queue-456/input".into()));
        assert_eq!(props.input_manifest_hash, Some("abc123def456".into()));
        assert!(props.output_relative_directories.is_none());
        assert_eq!(props.file_system_location_name, Some("ProjectFiles".into()));
    }

    #[test]
    fn test_build_manifest_properties_with_outputs() {
        let manifest: AssetRootManifest = AssetRootManifest {
            root_path: "/projects/job1".into(),
            asset_manifest: None,
            outputs: vec![
                PathBuf::from("/projects/job1/renders"),
                PathBuf::from("/projects/job1/cache/temp"),
            ],
            file_system_location_name: None,
        };

        let props: ManifestProperties = build_manifest_properties(&manifest, None, None);

        assert!(props.input_manifest_path.is_none());
        assert!(props.input_manifest_hash.is_none());

        let outputs: Vec<String> = props.output_relative_directories.unwrap();
        assert_eq!(outputs.len(), 2);
        assert!(outputs.contains(&"renders".to_string()));
        assert!(outputs.contains(&"cache/temp".to_string()));
    }

    #[test]
    fn test_build_manifest_properties_output_not_under_root() {
        let manifest: AssetRootManifest = AssetRootManifest {
            root_path: "/projects/job1".into(),
            asset_manifest: None,
            outputs: vec![
                PathBuf::from("/projects/job1/renders"),
                PathBuf::from("/other/path"), // Not under root
            ],
            file_system_location_name: None,
        };

        let props: ManifestProperties = build_manifest_properties(&manifest, None, None);

        // Only the path under root should be included
        let outputs: Vec<String> = props.output_relative_directories.unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(outputs.contains(&"renders".to_string()));
    }

    #[test]
    fn test_build_attachments() {
        let props: Vec<ManifestProperties> = vec![ManifestProperties {
            root_path: "/projects/job1".into(),
            root_path_format: PathFormat::Posix,
            input_manifest_path: Some("path/to/manifest".into()),
            input_manifest_hash: Some("hash123".into()),
            output_relative_directories: None,
            file_system_location_name: None,
        }];

        let attachments: Attachments = build_attachments(props, "COPIED");

        assert_eq!(attachments.manifests.len(), 1);
        assert_eq!(attachments.file_system, "COPIED");
    }

    #[test]
    fn test_build_attachments_virtual_mode() {
        let attachments: Attachments = build_attachments(vec![], "VIRTUAL");
        assert_eq!(attachments.file_system, "VIRTUAL");
    }
}
