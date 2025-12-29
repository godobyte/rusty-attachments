//! Tauri command handlers.

use std::path::PathBuf;

use tauri::Window;

use ja_deadline_utils::{submit_bundle_attachments, AssetReferences, BundleSubmitOptions};
use rusty_attachments_filesystem::{FileSystemScanner, GlobFilter, SnapshotOptions};
use rusty_attachments_model::{Manifest, ManifestVersion};
use rusty_attachments_storage::{StorageClient, StorageSettings};
use rusty_attachments_storage_crt::CrtStorageClient;

use crate::error::CommandError;
use crate::progress::TauriProgressCallback;
use crate::types::{
    DirectoryEntry, JobAttachmentConfig, ManifestFileEntry, ParsedManifest,
    S3ObjectEntry, SnapshotConfig, SnapshotResult, SubmitResult,
};

/// Parse manifest version string to enum.
fn parse_manifest_version(version: &str) -> Result<ManifestVersion, CommandError> {
    match version {
        "v2023-03-03" => Ok(ManifestVersion::V2023_03_03),
        "v2025-12-04-beta" => Ok(ManifestVersion::V2025_12_04_beta),
        _ => Err(CommandError::invalid_version(format!(
            "Invalid manifest version: {}. Use 'v2023-03-03' or 'v2025-12-04-beta'",
            version
        ))),
    }
}

/// Build GlobFilter from include/exclude patterns.
fn build_glob_filter(
    include_patterns: &[String],
    exclude_patterns: &[String],
) -> Result<Option<GlobFilter>, CommandError> {
    if include_patterns.is_empty() && exclude_patterns.is_empty() {
        return Ok(None);
    }

    let filter: GlobFilter = GlobFilter::with_patterns(
        include_patterns.to_vec(),
        exclude_patterns.to_vec(),
    )?;

    Ok(Some(filter))
}

/// List contents of a directory for file browser UI.
#[tauri::command]
pub async fn browse_directory(path: String) -> Result<Vec<DirectoryEntry>, CommandError> {
    let dir_path: PathBuf = if path.is_empty() {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else {
        PathBuf::from(&path)
    };

    if !dir_path.exists() {
        return Err(CommandError::invalid_path(format!(
            "Path does not exist: {}",
            dir_path.display()
        )));
    }

    if !dir_path.is_dir() {
        return Err(CommandError::invalid_path(format!(
            "Path is not a directory: {}",
            dir_path.display()
        )));
    }

    let mut entries: Vec<DirectoryEntry> = Vec::new();

    let read_dir = std::fs::read_dir(&dir_path)?;
    for entry_result in read_dir {
        let entry = entry_result?;
        let metadata = entry.metadata()?;

        let modified: Option<u64> = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        entries.push(DirectoryEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size: if metadata.is_file() {
                Some(metadata.len())
            } else {
                None
            },
            modified,
        });
    }

    // Sort: directories first, then by name
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(entries)
}

/// List S3 objects under a prefix (for bucket browser).
#[tauri::command]
pub async fn browse_s3_prefix(
    config: JobAttachmentConfig,
    prefix: String,
) -> Result<Vec<S3ObjectEntry>, CommandError> {
    let settings = StorageSettings {
        region: config.region.clone(),
        ..Default::default()
    };

    let client: CrtStorageClient = CrtStorageClient::new(settings)
        .await
        .map_err(|e| CommandError::storage_error(format!("Failed to create client: {}", e)))?;

    // Build the full prefix
    let full_prefix: String = if prefix.is_empty() {
        format!("{}/Manifests/", config.root_prefix)
    } else {
        prefix.clone()
    };

    let objects = client
        .list_objects(&config.bucket, &full_prefix)
        .await?;

    // Extract unique "folders" at this level and direct files
    let prefix_len: usize = full_prefix.len();
    let mut seen_prefixes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries: Vec<S3ObjectEntry> = Vec::new();

    for obj in objects {
        let relative: &str = &obj.key[prefix_len..];

        if let Some(slash_pos) = relative.find('/') {
            // This is a nested object - extract the folder name
            let folder_name: &str = &relative[..slash_pos];
            let folder_key: String = format!("{}{}/", full_prefix, folder_name);

            if seen_prefixes.insert(folder_key.clone()) {
                entries.push(S3ObjectEntry {
                    key: folder_key,
                    name: folder_name.to_string(),
                    is_prefix: true,
                    size: None,
                    last_modified: None,
                    is_manifest: false,
                });
            }
        } else if !relative.is_empty() {
            // Direct file at this level
            let is_manifest: bool = relative.ends_with("_input") || relative.ends_with("_output");

            entries.push(S3ObjectEntry {
                key: obj.key.clone(),
                name: relative.to_string(),
                is_prefix: false,
                size: Some(obj.size),
                last_modified: obj.last_modified.map(|t| t as u64),
                is_manifest,
            });
        }
    }

    // Sort: prefixes first, then by name
    entries.sort_by(|a, b| match (a.is_prefix, b.is_prefix) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(entries)
}

/// Download and parse a manifest from S3.
#[tauri::command]
pub async fn fetch_manifest(
    config: JobAttachmentConfig,
    s3_key: String,
) -> Result<ParsedManifest, CommandError> {
    let settings = StorageSettings {
        region: config.region.clone(),
        ..Default::default()
    };

    let client: CrtStorageClient = CrtStorageClient::new(settings)
        .await
        .map_err(|e| CommandError::storage_error(format!("Failed to create client: {}", e)))?;

    // Download manifest bytes
    let bytes: Vec<u8> = client.get_object(&config.bucket, &s3_key).await?;

    let content: String = String::from_utf8(bytes)
        .map_err(|e| CommandError::io_error(format!("Invalid UTF-8: {}", e)))?;

    // Parse manifest
    let manifest: Manifest = Manifest::decode(&content)
        .map_err(|e| CommandError::io_error(format!("Failed to parse manifest: {}", e)))?;

    // Try to get asset root from metadata
    let metadata = client.head_object_with_metadata(&config.bucket, &s3_key).await?;
    let asset_root: Option<String> = metadata
        .and_then(|m| m.user_metadata.get("asset-root").cloned());

    // Convert to our response type
    let parsed: ParsedManifest = convert_manifest_to_parsed(&manifest, asset_root);

    Ok(parsed)
}

/// Convert internal Manifest to ParsedManifest for frontend.
fn convert_manifest_to_parsed(manifest: &Manifest, asset_root: Option<String>) -> ParsedManifest {
    let mut files: Vec<ManifestFileEntry> = Vec::new();
    let mut dir_count: usize = 0;

    match manifest {
        Manifest::V2023_03_03(m) => {
            for path in &m.paths {
                files.push(ManifestFileEntry {
                    path: path.path.clone(),
                    size: Some(path.size),
                    mtime: Some(path.mtime),
                    hash: Some(path.hash.clone()),
                    executable: false,
                    entry_type: "file".to_string(),
                    symlink_target: None,
                    chunk_count: None,
                });
            }
        }
        Manifest::V2025_12_04_beta(m) => {
            dir_count = m.dirs.len();

            for file in &m.files {
                let entry_type: String = if file.delete {
                    "deleted".to_string()
                } else if file.symlink_target.is_some() {
                    "symlink".to_string()
                } else {
                    "file".to_string()
                };

                let chunk_count: Option<usize> = file.chunkhashes.as_ref().map(|c| c.len());

                files.push(ManifestFileEntry {
                    path: file.name.clone(),
                    size: file.size,
                    mtime: file.mtime,
                    hash: file.hash.clone(),
                    executable: file.runnable,
                    entry_type,
                    symlink_target: file.symlink_target.clone(),
                    chunk_count,
                });
            }
        }
    }

    ParsedManifest {
        version: manifest.version().as_str().to_string(),
        hash_alg: manifest.hash_alg().as_str().to_string(),
        total_size: manifest.total_size(),
        file_count: manifest.file_count(),
        dir_count,
        asset_root,
        files,
    }
}

/// Create a manifest from a directory without uploading.
#[tauri::command]
pub async fn create_snapshot(
    config: SnapshotConfig,
    window: Window,
) -> Result<SnapshotResult, CommandError> {
    let root_path: PathBuf = PathBuf::from(&config.root_path);

    if !root_path.exists() {
        return Err(CommandError::invalid_path(format!(
            "Root path does not exist: {}",
            root_path.display()
        )));
    }

    let version: ManifestVersion = parse_manifest_version(&config.manifest_version)?;
    let filter: GlobFilter = build_glob_filter(&config.include_patterns, &config.exclude_patterns)?
        .unwrap_or_default();

    let input_files: Option<Vec<PathBuf>> = if config.input_files.is_empty() {
        None
    } else {
        Some(config.input_files.iter().map(PathBuf::from).collect())
    };

    let options = SnapshotOptions {
        root: root_path,
        input_files,
        version,
        filter,
        ..Default::default()
    };

    let scanner: FileSystemScanner = FileSystemScanner::new();
    let progress_cb = TauriProgressCallback::new(window, "scanning");

    let manifest = scanner.snapshot(&options, Some(&progress_cb))?;

    let manifest_json: String = manifest
        .encode()
        .map_err(|e| CommandError::io_error(format!("Failed to encode manifest: {}", e)))?;

    Ok(SnapshotResult {
        version: manifest.version().as_str().to_string(),
        file_count: manifest.file_count(),
        total_size: manifest.total_size(),
        manifest_json,
    })
}

/// Submit a job bundle: hash files, upload to CAS, upload manifest.
#[tauri::command]
pub async fn submit_bundle(
    ja_config: JobAttachmentConfig,
    snapshot_config: SnapshotConfig,
    output_directories: Vec<String>,
    window: Window,
) -> Result<SubmitResult, CommandError> {
    let s3_location = ja_config.to_s3_location();
    let manifest_location = ja_config.to_manifest_location();

    let input_filenames: Vec<PathBuf> = if snapshot_config.input_files.is_empty() {
        vec![PathBuf::from(&snapshot_config.root_path)]
    } else {
        snapshot_config.input_files.iter().map(PathBuf::from).collect()
    };

    let asset_references = AssetReferences {
        input_filenames,
        output_directories: output_directories.iter().map(PathBuf::from).collect(),
        referenced_paths: vec![],
    };

    let version: ManifestVersion = parse_manifest_version(&snapshot_config.manifest_version)?;
    let glob_filter: Option<GlobFilter> = build_glob_filter(
        &snapshot_config.include_patterns,
        &snapshot_config.exclude_patterns,
    )?;

    let options = BundleSubmitOptions {
        manifest_version: version,
        glob_filter,
        ..Default::default()
    };

    let settings = StorageSettings {
        region: ja_config.region.clone(),
        ..Default::default()
    };

    let client: CrtStorageClient = CrtStorageClient::new(settings)
        .await
        .map_err(|e| CommandError::storage_error(format!("Failed to create storage client: {}", e)))?;

    let hash_progress = TauriProgressCallback::new(window, "hashing");

    let result = submit_bundle_attachments(
        &client,
        &s3_location,
        &manifest_location,
        &asset_references,
        None,
        &options,
        Some(&hash_progress),
        None,
    )
    .await?;

    let attachments_json: String = result
        .attachments
        .to_json()
        .map_err(|e| CommandError::io_error(format!("Failed to serialize attachments: {}", e)))?;

    Ok(SubmitResult {
        attachments_json,
        hashing_stats: result.hashing_stats.into(),
        upload_stats: result.upload_stats.into(),
    })
}

/// Cancel an in-progress operation.
#[tauri::command]
pub async fn cancel_operation(_operation_id: String) -> Result<(), CommandError> {
    // TODO: Implement cancellation token registry
    Ok(())
}
