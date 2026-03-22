//! Path resolution between worker-side and submitter-side paths for relaxed roots.
//!
//! When storage profiles are in use, the VFS sees files at the worker-side path
//! but the upload agent needs the submitter-side path. This module provides the
//! translation layer.

use std::collections::HashMap;

use super::markers::{FileUploadRequest, RelaxedRootConfig};
use super::types::{RelaxedFileKey, RequestPriority};

/// Resolves root_id to source (submitter-side) paths for SQS message construction.
///
/// The VFS operates on worker-side paths (from `mount_path`), but the upload agent
/// needs submitter-side paths (from `source_path`) to locate files on the on-prem
/// network storage. This struct holds the mapping.
#[derive(Debug, Clone)]
pub struct RootPathResolver {
    /// Map of root_id → RelaxedRootConfig.
    roots: HashMap<String, RelaxedRootConfig>,
}

impl RootPathResolver {
    /// Create a resolver from a list of relaxed root configs.
    ///
    /// # Arguments
    /// * `configs` - The relaxed root configurations.
    pub fn new(configs: &[RelaxedRootConfig]) -> Self {
        let roots: HashMap<String, RelaxedRootConfig> = configs
            .iter()
            .map(|c| (c.root_id.clone(), c.clone()))
            .collect();
        Self { roots }
    }

    /// Get the submitter-side source path for a root_id.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    ///
    /// # Returns
    /// The source path on the submitter's machine, or None if the root_id is unknown.
    pub fn source_path(&self, root_id: &str) -> Option<&str> {
        self.roots.get(root_id).map(|c| c.source_path.as_str())
    }

    /// Get the worker-side mount path for a root_id.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    ///
    /// # Returns
    /// The mount path within the VFS, or None if the root_id is unknown.
    pub fn mount_path(&self, root_id: &str) -> Option<&str> {
        self.roots.get(root_id).map(|c| c.mount_path.as_str())
    }

    /// Get the full config for a root_id.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    pub fn get_config(&self, root_id: &str) -> Option<&RelaxedRootConfig> {
        self.roots.get(root_id)
    }

    /// Check if a root_id is known.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    pub fn contains(&self, root_id: &str) -> bool {
        self.roots.contains_key(root_id)
    }

    /// Get the number of registered roots.
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    /// Build a FileUploadRequest with the correct source_root_path translation.
    ///
    /// The `key.relative_path` is the path-mapping invariant — identical on both
    /// submitter and worker sides. The `source_root_path` is looked up from the
    /// root config so the upload agent can resolve the file on the on-prem storage.
    ///
    /// # Arguments
    /// * `key` - The relaxed file key (contains root_id and relative_path).
    /// * `priority` - Request priority.
    /// * `bucket` - S3 bucket name.
    /// * `root_prefix` - S3 root prefix.
    /// * `job_id` - Job ID for auditing.
    ///
    /// # Returns
    /// A `FileUploadRequest` with the submitter-side `source_root_path`, or None
    /// if the root_id is unknown.
    pub fn build_upload_request(
        &self,
        key: &RelaxedFileKey,
        priority: RequestPriority,
        bucket: &str,
        root_prefix: &str,
        job_id: &str,
    ) -> Option<FileUploadRequest> {
        let config: &RelaxedRootConfig = self.roots.get(&key.root_id)?;

        let priority_str: &str = match priority {
            RequestPriority::High => "high",
            RequestPriority::AsyncEventual => "async_eventual",
        };

        Some(FileUploadRequest {
            version: "2026-03-21".to_string(),
            root_id: key.root_id.clone(),
            source_root_path: config.source_path.clone(),
            relative_path: key.relative_path.clone(),
            path_key: key.path_key.clone(),
            bucket: bucket.to_string(),
            root_prefix: root_prefix.to_string(),
            job_id: job_id.to_string(),
            requested_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            priority: priority_str.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_configs() -> Vec<RelaxedRootConfig> {
        vec![
            RelaxedRootConfig {
                root_id: "root_mapped".to_string(),
                source_path: "/mnt/shared/assets".to_string(),
                mount_path: "/mnt/worker/assets".to_string(),
                file_system_location_name: Some("StudioAssets".to_string()),
            },
            RelaxedRootConfig {
                root_id: "root_dynamic".to_string(),
                source_path: "/mnt/shared/scratch".to_string(),
                mount_path: "assetroot-f0e1d2c3b4".to_string(),
                file_system_location_name: None,
            },
        ]
    }

    #[test]
    fn test_source_path_lookup() {
        let resolver = RootPathResolver::new(&make_configs());
        assert_eq!(
            resolver.source_path("root_mapped"),
            Some("/mnt/shared/assets")
        );
        assert_eq!(
            resolver.source_path("root_dynamic"),
            Some("/mnt/shared/scratch")
        );
        assert_eq!(resolver.source_path("unknown"), None);
    }

    #[test]
    fn test_mount_path_lookup() {
        let resolver = RootPathResolver::new(&make_configs());
        assert_eq!(
            resolver.mount_path("root_mapped"),
            Some("/mnt/worker/assets")
        );
        assert_eq!(
            resolver.mount_path("root_dynamic"),
            Some("assetroot-f0e1d2c3b4")
        );
    }

    #[test]
    fn test_build_upload_request_mapped_root() {
        // Case 1: Storage profile mapped root.
        // Worker sees /mnt/worker/assets/textures/diffuse.png
        // Upload agent needs /mnt/shared/assets as source_root_path
        let resolver = RootPathResolver::new(&make_configs());

        let key = RelaxedFileKey {
            root_id: "root_mapped".to_string(),
            relative_path: "textures/diffuse.png".to_string(),
            path_key: "abc123".to_string(),
        };

        let req: FileUploadRequest = resolver
            .build_upload_request(
                &key,
                RequestPriority::High,
                "my-bucket",
                "DeadlineCloud",
                "job-123",
            )
            .expect("should build request for known root");

        // The source_root_path must be the SUBMITTER's path, not the worker's
        assert_eq!(req.source_root_path, "/mnt/shared/assets");
        assert_eq!(req.relative_path, "textures/diffuse.png");
        assert_eq!(req.root_id, "root_mapped");
        assert_eq!(req.priority, "high");

        // Upload agent will resolve: /mnt/shared/assets + textures/diffuse.png
        // = /mnt/shared/assets/textures/diffuse.png ✓
    }

    #[test]
    fn test_build_upload_request_dynamic_root() {
        // Case 2: No storage profile, dynamic mount path.
        // Worker sees assetroot-f0e1d2c3b4/data/scene.blend
        // Upload agent needs /mnt/shared/scratch as source_root_path
        let resolver = RootPathResolver::new(&make_configs());

        let key = RelaxedFileKey {
            root_id: "root_dynamic".to_string(),
            relative_path: "data/scene.blend".to_string(),
            path_key: "def456".to_string(),
        };

        let req: FileUploadRequest = resolver
            .build_upload_request(
                &key,
                RequestPriority::AsyncEventual,
                "my-bucket",
                "DeadlineCloud",
                "job-456",
            )
            .expect("should build request for known root");

        assert_eq!(req.source_root_path, "/mnt/shared/scratch");
        assert_eq!(req.relative_path, "data/scene.blend");
        assert_eq!(req.priority, "async_eventual");
    }

    #[test]
    fn test_build_upload_request_unknown_root() {
        let resolver = RootPathResolver::new(&make_configs());

        let key = RelaxedFileKey {
            root_id: "unknown_root".to_string(),
            relative_path: "file.txt".to_string(),
            path_key: "xyz789".to_string(),
        };

        let result = resolver.build_upload_request(
            &key,
            RequestPriority::High,
            "bucket",
            "prefix",
            "job-1",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_relative_path_is_invariant_across_mapping() {
        // The key insight: relative_path is the SAME on both sides.
        // Only the root prefix changes between submitter and worker.
        let resolver = RootPathResolver::new(&make_configs());

        let relative: &str = "project/shots/sh010/textures/hero_diffuse.exr";

        let key = RelaxedFileKey {
            root_id: "root_mapped".to_string(),
            relative_path: relative.to_string(),
            path_key: "test_key".to_string(),
        };

        let req: FileUploadRequest = resolver
            .build_upload_request(
                &key,
                RequestPriority::High,
                "bucket",
                "prefix",
                "job-1",
            )
            .unwrap();

        // Worker path would be: /mnt/worker/assets/project/shots/sh010/textures/hero_diffuse.exr
        // Submitter path is:    /mnt/shared/assets/project/shots/sh010/textures/hero_diffuse.exr
        // The relative_path is identical in both:
        assert_eq!(req.relative_path, relative);
        // And the source_root_path is the submitter's root:
        assert_eq!(req.source_root_path, "/mnt/shared/assets");
    }

    #[test]
    fn test_root_count() {
        let resolver = RootPathResolver::new(&make_configs());
        assert_eq!(resolver.root_count(), 2);
    }

    #[test]
    fn test_contains() {
        let resolver = RootPathResolver::new(&make_configs());
        assert!(resolver.contains("root_mapped"));
        assert!(resolver.contains("root_dynamic"));
        assert!(!resolver.contains("nonexistent"));
    }
}
