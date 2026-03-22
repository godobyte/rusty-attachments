//! Configuration loading for relaxed consistency launch options.
//!
//! Parses a JSON config file that declares relaxed roots, SQS queue
//! identifiers, and polling parameters. Shared across all platform
//! launchers (FUSE, FSKit, ProjFS).

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::options::RelaxedConsistencyOptions;
use crate::VfsError;

use super::markers::RelaxedRootConfig;

/// Top-level config file for relaxed consistency launch.
///
/// Loaded from a JSON file passed via `--relaxed-roots <path>`.
///
/// # Example JSON
///
/// ```json
/// {
///   "roots": [
///     {
///       "rootId": "a1b2c3d4e5",
///       "sourcePath": "/mnt/shared/assets",
///       "mountPath": "assets"
///     }
///   ],
///   "sqsRegion": "us-west-2",
///   "farmId": "farm-abc123",
///   "queueId": "queue-def456",
///   "pollIntervalSecs": 30,
///   "maxWaitTimeoutSecs": 1800
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaxedLaunchConfig {
    /// Relaxed root declarations.
    pub roots: Vec<RelaxedRootConfig>,
    /// AWS region for SQS queues.
    #[serde(default = "default_region")]
    pub sqs_region: String,
    /// Deadline farm ID (for SQS queue name).
    pub farm_id: String,
    /// Deadline queue ID (for SQS queue name).
    pub queue_id: String,
    /// Poll interval in seconds (default: 30).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Max wait timeout in seconds (default: 1800 = 30 min).
    #[serde(default = "default_max_wait_timeout")]
    pub max_wait_timeout_secs: u64,
    /// Batch poll size (default: 1000).
    #[serde(default = "default_batch_poll_size")]
    pub batch_poll_size: usize,
}

fn default_region() -> String {
    "us-west-2".to_string()
}

fn default_poll_interval() -> u64 {
    30
}

fn default_max_wait_timeout() -> u64 {
    1800
}

fn default_batch_poll_size() -> usize {
    1000
}

/// Load a relaxed consistency config from a JSON file.
///
/// # Arguments
/// * `path` - Path to the JSON config file.
///
/// # Returns
/// The parsed config, or a VfsError if the file can't be read or parsed.
pub fn load_relaxed_config(path: &Path) -> Result<RelaxedLaunchConfig, VfsError> {
    let json: String = std::fs::read_to_string(path).map_err(|e| {
        VfsError::MountFailed(format!(
            "Failed to read relaxed roots config {}: {}",
            path.display(),
            e
        ))
    })?;

    let config: RelaxedLaunchConfig = serde_json::from_str(&json).map_err(|e| {
        VfsError::MountFailed(format!(
            "Failed to parse relaxed roots config {}: {}",
            path.display(),
            e
        ))
    })?;

    if config.roots.is_empty() {
        return Err(VfsError::MountFailed(
            "Relaxed roots config has no roots declared".to_string(),
        ));
    }

    Ok(config)
}

/// Validate that relaxed consistency is only used with VFS (VIRTUAL) file system mode.
///
/// Relaxed consistency requires VFS to intercept file reads — in COPIED mode,
/// files are downloaded to disk before the job runs, so there is no interception
/// point for on-demand fetching.
///
/// # Arguments
/// * `file_system_mode` - The job attachment file system mode ("COPIED" or "VIRTUAL").
/// * `has_relaxed_roots` - Whether relaxed roots are configured.
///
/// # Returns
/// Ok(()) if the combination is valid, or a VfsError describing the conflict.
pub fn validate_relaxed_requires_vfs(
    file_system_mode: &str,
    has_relaxed_roots: bool,
) -> Result<(), VfsError> {
    if has_relaxed_roots && file_system_mode != "VIRTUAL" {
        return Err(VfsError::MountFailed(format!(
            "Relaxed consistency roots require fileSystem mode \"VIRTUAL\" (VFS), \
             but got \"{}\". Only VFS can intercept file reads for on-demand fetching.",
            file_system_mode
        )));
    }
    Ok(())
}

/// Convert a RelaxedLaunchConfig into VfsOptions-compatible RelaxedConsistencyOptions.
///
/// # Arguments
/// * `config` - The parsed launch config.
///
/// # Returns
/// A `RelaxedConsistencyOptions` ready to pass to `VfsOptions::with_relaxed()`.
pub fn to_relaxed_options(config: &RelaxedLaunchConfig) -> RelaxedConsistencyOptions {
    RelaxedConsistencyOptions {
        poll_interval: Duration::from_secs(config.poll_interval_secs),
        max_wait_timeout: Duration::from_secs(config.max_wait_timeout_secs),
        batch_poll_size: config.batch_poll_size,
        roots: config.roots.clone(),
    }
}

/// Build the SQS queue name for a given farm/queue/priority.
///
/// # Arguments
/// * `farm_id` - Deadline farm ID.
/// * `queue_id` - Deadline queue ID.
/// * `priority` - "high" or "async".
///
/// # Returns
/// The SQS queue name string.
pub fn sqs_queue_name(farm_id: &str, queue_id: &str, priority: &str) -> String {
    format!(
        "deadline-{}-{}-file-requests-{}",
        farm_id, queue_id, priority
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_config(json: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(json.as_bytes()).unwrap();
        f
    }

    #[test]
    fn test_load_full_config() {
        let json: &str = r#"{
            "roots": [
                {
                    "rootId": "abc123",
                    "sourcePath": "/mnt/shared",
                    "mountPath": "shared"
                }
            ],
            "sqsRegion": "eu-west-1",
            "farmId": "farm-1",
            "queueId": "queue-2",
            "pollIntervalSecs": 15,
            "maxWaitTimeoutSecs": 600,
            "batchPollSize": 500
        }"#;

        let f = write_config(json);
        let config: RelaxedLaunchConfig = load_relaxed_config(f.path()).unwrap();

        assert_eq!(config.roots.len(), 1);
        assert_eq!(config.roots[0].root_id, "abc123");
        assert_eq!(config.roots[0].source_path, "/mnt/shared");
        assert_eq!(config.roots[0].mount_path, "shared");
        assert_eq!(config.sqs_region, "eu-west-1");
        assert_eq!(config.farm_id, "farm-1");
        assert_eq!(config.queue_id, "queue-2");
        assert_eq!(config.poll_interval_secs, 15);
        assert_eq!(config.max_wait_timeout_secs, 600);
        assert_eq!(config.batch_poll_size, 500);
    }

    #[test]
    fn test_load_config_defaults() {
        let json: &str = r#"{
            "roots": [
                {
                    "rootId": "abc123",
                    "sourcePath": "/mnt/shared",
                    "mountPath": "shared"
                }
            ],
            "farmId": "farm-1",
            "queueId": "queue-2"
        }"#;

        let f = write_config(json);
        let config: RelaxedLaunchConfig = load_relaxed_config(f.path()).unwrap();

        assert_eq!(config.sqs_region, "us-west-2");
        assert_eq!(config.poll_interval_secs, 30);
        assert_eq!(config.max_wait_timeout_secs, 1800);
        assert_eq!(config.batch_poll_size, 1000);
    }

    #[test]
    fn test_load_config_empty_roots_fails() {
        let json: &str = r#"{
            "roots": [],
            "farmId": "farm-1",
            "queueId": "queue-2"
        }"#;

        let f = write_config(json);
        let result = load_relaxed_config(f.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_missing_file() {
        let result = load_relaxed_config(Path::new("/nonexistent/config.json"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_invalid_json() {
        let f = write_config("not json at all");
        let result = load_relaxed_config(f.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_to_relaxed_options() {
        let config = RelaxedLaunchConfig {
            roots: vec![RelaxedRootConfig {
                root_id: "abc".to_string(),
                source_path: "/mnt/shared".to_string(),
                mount_path: "shared".to_string(),
                file_system_location_name: None,
            }],
            sqs_region: "us-west-2".to_string(),
            farm_id: "farm-1".to_string(),
            queue_id: "queue-1".to_string(),
            poll_interval_secs: 10,
            max_wait_timeout_secs: 300,
            batch_poll_size: 200,
        };

        let opts: RelaxedConsistencyOptions = to_relaxed_options(&config);
        assert_eq!(opts.poll_interval, Duration::from_secs(10));
        assert_eq!(opts.max_wait_timeout, Duration::from_secs(300));
        assert_eq!(opts.batch_poll_size, 200);
        assert_eq!(opts.roots.len(), 1);
        assert_eq!(opts.roots[0].mount_path, "shared");
    }

    #[test]
    fn test_sqs_queue_name() {
        let name: String = sqs_queue_name("farm-abc", "queue-def", "high");
        assert_eq!(name, "deadline-farm-abc-queue-def-file-requests-high");
    }

    #[test]
    fn test_multiple_roots() {
        let json: &str = r#"{
            "roots": [
                {
                    "rootId": "root1",
                    "sourcePath": "/mnt/assets",
                    "mountPath": "assets"
                },
                {
                    "rootId": "root2",
                    "sourcePath": "/mnt/textures",
                    "mountPath": "textures"
                }
            ],
            "farmId": "farm-1",
            "queueId": "queue-1"
        }"#;

        let f = write_config(json);
        let config: RelaxedLaunchConfig = load_relaxed_config(f.path()).unwrap();
        assert_eq!(config.roots.len(), 2);
        assert_eq!(config.roots[0].mount_path, "assets");
        assert_eq!(config.roots[1].mount_path, "textures");
    }

    #[test]
    fn test_config_roundtrip() {
        let config = RelaxedLaunchConfig {
            roots: vec![RelaxedRootConfig {
                root_id: "abc".to_string(),
                source_path: "/mnt/shared".to_string(),
                mount_path: "shared".to_string(),
                file_system_location_name: None,
            }],
            sqs_region: "us-east-1".to_string(),
            farm_id: "farm-1".to_string(),
            queue_id: "queue-1".to_string(),
            poll_interval_secs: 30,
            max_wait_timeout_secs: 1800,
            batch_poll_size: 1000,
        };

        let json: String = serde_json::to_string_pretty(&config).unwrap();
        let parsed: RelaxedLaunchConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.farm_id, "farm-1");
        assert_eq!(parsed.roots[0].root_id, "abc");
    }

    #[test]
    fn test_validate_relaxed_requires_vfs_virtual_ok() {
        let result = validate_relaxed_requires_vfs("VIRTUAL", true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_relaxed_requires_vfs_copied_with_relaxed_fails() {
        let result = validate_relaxed_requires_vfs("COPIED", true);
        assert!(result.is_err());
        let err_msg: String = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("VIRTUAL"));
        assert!(err_msg.contains("COPIED"));
    }

    #[test]
    fn test_validate_relaxed_requires_vfs_copied_without_relaxed_ok() {
        // COPIED mode is fine when there are no relaxed roots
        let result = validate_relaxed_requires_vfs("COPIED", false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_relaxed_requires_vfs_virtual_without_relaxed_ok() {
        // VIRTUAL mode without relaxed roots is also fine (pure strong consistency VFS)
        let result = validate_relaxed_requires_vfs("VIRTUAL", false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mixed_mapped_and_unmapped_roots() {
        // Case 1: mapped root (with storage profile) + Case 2: unmapped root (dynamic)
        let json: &str = r#"{
            "roots": [
                {
                    "rootId": "a1b2c3d4e5",
                    "sourcePath": "/mnt/shared/assets",
                    "mountPath": "/mnt/worker/assets",
                    "fileSystemLocationName": "StudioAssets"
                },
                {
                    "rootId": "f0e1d2c3b4",
                    "sourcePath": "/mnt/shared/scratch",
                    "mountPath": "assetroot-f0e1d2c3b4"
                }
            ],
            "farmId": "farm-1",
            "queueId": "queue-1"
        }"#;

        let f = write_config(json);
        let config: RelaxedLaunchConfig = load_relaxed_config(f.path()).unwrap();

        // Mapped root (with storage profile)
        assert_eq!(config.roots[0].mount_path, "/mnt/worker/assets");
        assert_eq!(
            config.roots[0].file_system_location_name.as_deref(),
            Some("StudioAssets")
        );

        // Unmapped root (dynamic, no storage profile)
        assert_eq!(config.roots[1].mount_path, "assetroot-f0e1d2c3b4");
        assert!(config.roots[1].file_system_location_name.is_none());

        // Both should produce valid options
        let opts: RelaxedConsistencyOptions = to_relaxed_options(&config);
        assert_eq!(opts.roots.len(), 2);
    }
}
