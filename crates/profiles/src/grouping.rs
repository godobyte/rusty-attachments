//! Asset path grouping by storage profile locations.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusty_attachments_common::{lexical_normalize, to_absolute};
use thiserror::Error;

use crate::types::StorageProfile;

/// A group of paths sharing a common asset root.
#[derive(Debug, Clone, Default)]
pub struct AssetRootGroup {
    /// The common root path for this group.
    pub root_path: String,
    /// Input files to be uploaded.
    pub inputs: HashSet<PathBuf>,
    /// Output directories (tracked but not uploaded).
    pub outputs: HashSet<PathBuf>,
    /// Referenced paths (may not exist, associated with this root).
    pub references: HashSet<PathBuf>,
    /// File system location name (if matched to a LOCAL location).
    pub file_system_location_name: Option<String>,
}

/// Options for path validation during grouping.
#[derive(Debug, Clone, Copy, Default)]
pub struct PathValidationMode {
    /// If true, missing input paths cause an error.
    /// If false, missing paths are demoted to referenced_paths.
    pub require_paths_exist: bool,
}

/// Errors for misconfigured inputs.
#[derive(Debug, Error)]
pub enum PathGroupingError {
    /// Input files that do not exist.
    #[error("Missing input files:\n{}", format_paths(.missing))]
    MissingInputFiles { missing: Vec<String> },

    /// Directories specified as input files.
    #[error("Directories specified as input files:\n{}", format_paths(.directories))]
    DirectoriesAsFiles { directories: Vec<String> },

    /// Combined error for multiple issues.
    #[error("{message}\n{}", format_misconfigured(.missing, .directories))]
    MisconfiguredInputs {
        message: String,
        missing: Vec<String>,
        directories: Vec<String>,
    },
}

/// Format a list of paths for error display.
fn format_paths(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| format!("\t{}", p))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format misconfigured inputs for error display.
fn format_misconfigured(missing: &[String], directories: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("Missing input files:\n{}", format_paths(missing)));
    }
    if !directories.is_empty() {
        parts.push(format!(
            "Directories classified as files:\n{}",
            format_paths(directories)
        ));
    }
    parts.join("\n")
}

/// Result of path grouping with validation info.
#[derive(Debug, Clone)]
pub struct PathGroupingResult {
    /// Successfully grouped asset roots.
    pub groups: Vec<AssetRootGroup>,
    /// Paths that were demoted to references (when require_paths_exist=false).
    pub demoted_to_references: Vec<PathBuf>,
    /// Paths under SHARED locations that were skipped.
    pub skipped_shared: Vec<PathBuf>,
}

/// Group paths by asset root, respecting storage profile locations.
///
/// # Arguments
/// * `input_paths` - Files to be uploaded
/// * `output_paths` - Output directories (tracked but not uploaded)
/// * `referenced_paths` - Paths that may not exist but need association
/// * `storage_profile` - Optional storage profile for path classification
///
/// # Returns
/// List of asset root groups, sorted by root path.
pub fn group_asset_paths(
    input_paths: &[PathBuf],
    output_paths: &[PathBuf],
    referenced_paths: &[PathBuf],
    storage_profile: Option<&StorageProfile>,
) -> Vec<AssetRootGroup> {
    let local_locations: HashMap<&str, &str> = storage_profile
        .map(|p| p.local_locations())
        .unwrap_or_default();
    let shared_locations: HashMap<&str, &str> = storage_profile
        .map(|p| p.shared_locations())
        .unwrap_or_default();

    let mut groupings: HashMap<String, AssetRootGroup> = HashMap::new();

    // Process input paths
    for path in input_paths {
        let abs_path: PathBuf = normalize_path(path);

        // Skip files under SHARED locations
        if is_under_shared(&abs_path, &shared_locations) {
            continue;
        }

        let (root_key, location_name): (String, Option<&str>) =
            find_root_for_path(&abs_path, &local_locations, &groupings);

        let group: &mut AssetRootGroup =
            groupings.entry(root_key).or_insert_with(|| AssetRootGroup {
                file_system_location_name: location_name.map(String::from),
                ..Default::default()
            });
        group.inputs.insert(abs_path);
    }

    // Process output paths
    for path in output_paths {
        let abs_path: PathBuf = normalize_path(path);

        if is_under_shared(&abs_path, &shared_locations) {
            continue;
        }

        let (root_key, location_name): (String, Option<&str>) =
            find_root_for_path(&abs_path, &local_locations, &groupings);

        let group: &mut AssetRootGroup =
            groupings.entry(root_key).or_insert_with(|| AssetRootGroup {
                file_system_location_name: location_name.map(String::from),
                ..Default::default()
            });
        group.outputs.insert(abs_path);
    }

    // Process referenced paths
    for path in referenced_paths {
        let abs_path: PathBuf = normalize_path(path);

        if is_under_shared(&abs_path, &shared_locations) {
            continue;
        }

        let (root_key, location_name): (String, Option<&str>) =
            find_root_for_path(&abs_path, &local_locations, &groupings);

        let group: &mut AssetRootGroup =
            groupings.entry(root_key).or_insert_with(|| AssetRootGroup {
                file_system_location_name: location_name.map(String::from),
                ..Default::default()
            });
        group.references.insert(abs_path);
    }

    // Compute final root_path as common path of all entries in each group
    for group in groupings.values_mut() {
        let all_paths: Vec<&Path> = group
            .inputs
            .iter()
            .chain(group.outputs.iter())
            .chain(group.references.iter())
            .map(|p| p.as_path())
            .collect();

        if let Some(common) = common_path(&all_paths) {
            group.root_path = if common.is_file() {
                common
                    .parent()
                    .unwrap_or(&common)
                    .to_string_lossy()
                    .into_owned()
            } else {
                common.to_string_lossy().into_owned()
            };
        }
    }

    let mut groups: Vec<AssetRootGroup> = groupings.into_values().collect();
    groups.sort_by(|a, b| {
        a.root_path.cmp(&b.root_path).then_with(|| {
            a.file_system_location_name
                .cmp(&b.file_system_location_name)
        })
    });
    groups
}

/// Group paths with validation and error handling.
///
/// # Arguments
/// * `input_paths` - Files to be uploaded
/// * `output_paths` - Output directories
/// * `referenced_paths` - Paths that may not exist
/// * `storage_profile` - Optional storage profile
/// * `validation` - Validation mode settings
///
/// # Returns
/// Grouping result with validation info, or error if validation fails.
///
/// # Errors
/// Returns `PathGroupingError` if validation is strict and paths are missing/misconfigured.
pub fn group_asset_paths_validated(
    input_paths: &[PathBuf],
    output_paths: &[PathBuf],
    referenced_paths: &[PathBuf],
    storage_profile: Option<&StorageProfile>,
    validation: PathValidationMode,
) -> Result<PathGroupingResult, PathGroupingError> {
    let shared_locations: HashMap<&str, &str> = storage_profile
        .map(|p| p.shared_locations())
        .unwrap_or_default();

    let mut missing_paths: Vec<PathBuf> = Vec::new();
    let mut directory_paths: Vec<PathBuf> = Vec::new();
    let mut demoted_to_references: Vec<PathBuf> = Vec::new();
    let mut skipped_shared: Vec<PathBuf> = Vec::new();
    let mut valid_inputs: Vec<PathBuf> = Vec::new();
    let mut augmented_references: Vec<PathBuf> = referenced_paths.to_vec();

    // Validate each input path
    for path in input_paths {
        let abs_path: PathBuf = normalize_path(path);

        // Track shared paths that are skipped
        if is_under_shared(&abs_path, &shared_locations) {
            skipped_shared.push(abs_path);
            continue;
        }

        if !abs_path.exists() {
            if validation.require_paths_exist {
                missing_paths.push(abs_path);
            } else {
                demoted_to_references.push(abs_path.clone());
                augmented_references.push(abs_path);
            }
            continue;
        }

        if abs_path.is_dir() {
            directory_paths.push(abs_path);
            continue;
        }

        valid_inputs.push(abs_path);
    }

    // Report errors if validation is strict
    if validation.require_paths_exist && (!missing_paths.is_empty() || !directory_paths.is_empty())
    {
        return Err(PathGroupingError::MisconfiguredInputs {
            message:
                "Job submission contains missing input files or directories specified as files."
                    .into(),
            missing: missing_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            directories: directory_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        });
    }

    // Proceed with grouping using valid inputs
    let groups: Vec<AssetRootGroup> = group_asset_paths(
        &valid_inputs,
        output_paths,
        &augmented_references,
        storage_profile,
    );

    Ok(PathGroupingResult {
        groups,
        demoted_to_references,
        skipped_shared,
    })
}

/// Normalize a path: absolute without resolving symlinks, with .. removed.
fn normalize_path(path: &Path) -> PathBuf {
    let abs: PathBuf = to_absolute(path).unwrap_or_else(|_| path.to_path_buf());
    lexical_normalize(&abs)
}

/// Check if a path is under any shared location.
fn is_under_shared(abs_path: &Path, shared_locations: &HashMap<&str, &str>) -> bool {
    shared_locations
        .keys()
        .any(|shared| abs_path.starts_with(shared))
}

/// Find the root key for grouping a path.
///
/// Returns (root_key, location_name) where:
/// - root_key is the grouping key (LOCAL location path or top-level directory)
/// - location_name is the file system location name if matched
fn find_root_for_path<'a>(
    abs_path: &Path,
    local_locations: &HashMap<&'a str, &'a str>,
    #[cfg_attr(not(windows), allow(unused_variables))] groupings: &HashMap<String, AssetRootGroup>,
) -> (String, Option<&'a str>) {
    // Find most specific LOCAL location containing this path
    let matched: Option<(&&str, &&str)> = local_locations
        .iter()
        .filter(|(loc_path, _)| abs_path.starts_with(*loc_path))
        .max_by_key(|(loc_path, _)| loc_path.len());

    if let Some((loc_path, loc_name)) = matched {
        ((*loc_path).to_string(), Some(*loc_name))
    } else {
        // No LOCAL location match - use top-level directory
        let top_dir: String = abs_path
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();

        // Check if this top_dir already exists (case-insensitive on Windows)
        #[cfg(windows)]
        let top_dir_key: String = {
            let lower: String = top_dir.to_lowercase();
            if groupings.keys().any(|k| k.to_lowercase() == lower) {
                lower
            } else {
                top_dir
            }
        };

        #[cfg(not(windows))]
        let top_dir_key: String = top_dir;

        (top_dir_key, None)
    }
}

/// Find the common path prefix of a list of paths.
fn common_path(paths: &[&Path]) -> Option<PathBuf> {
    if paths.is_empty() {
        return None;
    }

    if paths.len() == 1 {
        return Some(paths[0].to_path_buf());
    }

    // Use Path components for safe comparison instead of string slicing
    let first_components: Vec<_> = paths[0].components().collect();

    // Find the longest common prefix by comparing components
    let mut common_count: usize = first_components.len();

    for path in &paths[1..] {
        let components: Vec<_> = path.components().collect();
        let matching: usize = first_components
            .iter()
            .zip(components.iter())
            .take_while(|(a, b)| a == b)
            .count();
        common_count = common_count.min(matching);
    }

    if common_count == 0 {
        return None;
    }

    // Build the common path from components
    let common: PathBuf = first_components[..common_count].iter().collect();

    // If the common path is a file (all paths share the same file), return its parent
    // This happens when paths.len() == 1 case is already handled above, so this
    // means we have multiple paths that share a common prefix
    if common.is_file() {
        common.parent().map(|p| p.to_path_buf())
    } else if common.as_os_str().is_empty() {
        #[cfg(unix)]
        return Some(PathBuf::from("/"));
        #[cfg(windows)]
        return None;
    } else {
        Some(common)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSystemLocation, FileSystemLocationType};
    use std::fs;
    use tempfile::TempDir;

    fn create_test_profile() -> StorageProfile {
        StorageProfile::with_locations(vec![
            FileSystemLocation {
                name: "ProjectFiles".into(),
                path: "/mnt/projects".into(),
                location_type: FileSystemLocationType::Local,
            },
            FileSystemLocation {
                name: "SharedAssets".into(),
                path: "/mnt/shared".into(),
                location_type: FileSystemLocationType::Shared,
            },
        ])
    }

    #[test]
    fn test_group_asset_paths_empty() {
        let groups: Vec<AssetRootGroup> = group_asset_paths(&[], &[], &[], None);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_group_asset_paths_single_input() {
        let temp_dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = temp_dir.path().join("test.txt");
        fs::write(&file_path, "test").unwrap();

        let groups: Vec<AssetRootGroup> = group_asset_paths(&[file_path.clone()], &[], &[], None);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].inputs.len(), 1);
        assert!(groups[0].outputs.is_empty());
        assert!(groups[0].references.is_empty());
    }

    #[test]
    fn test_group_asset_paths_with_outputs() {
        let temp_dir: TempDir = TempDir::new().unwrap();
        let file_path: PathBuf = temp_dir.path().join("input.txt");
        let output_dir: PathBuf = temp_dir.path().join("outputs");
        fs::write(&file_path, "test").unwrap();
        fs::create_dir(&output_dir).unwrap();

        let groups: Vec<AssetRootGroup> =
            group_asset_paths(&[file_path.clone()], &[output_dir.clone()], &[], None);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].inputs.len(), 1);
        assert_eq!(groups[0].outputs.len(), 1);
    }

    #[test]
    fn test_group_asset_paths_skips_shared() {
        let profile: StorageProfile = create_test_profile();

        let shared_file: PathBuf = PathBuf::from("/mnt/shared/asset.exr");
        let local_file: PathBuf = PathBuf::from("/mnt/projects/scene.blend");

        let groups: Vec<AssetRootGroup> =
            group_asset_paths(&[shared_file, local_file], &[], &[], Some(&profile));

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].inputs.len(), 1);
        assert!(groups[0]
            .inputs
            .iter()
            .any(|p| p.to_string_lossy().contains("projects")));
    }

    #[test]
    fn test_group_asset_paths_with_local_location() {
        let profile: StorageProfile = create_test_profile();

        let file1: PathBuf = PathBuf::from("/mnt/projects/job1/scene.blend");
        let file2: PathBuf = PathBuf::from("/mnt/projects/job1/textures/wood.png");
        let output: PathBuf = PathBuf::from("/mnt/projects/job1/renders");

        let groups: Vec<AssetRootGroup> =
            group_asset_paths(&[file1, file2], &[output], &[], Some(&profile));

        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].file_system_location_name,
            Some("ProjectFiles".into())
        );
        assert_eq!(groups[0].inputs.len(), 2);
        assert_eq!(groups[0].outputs.len(), 1);
    }

    #[test]
    fn test_group_asset_paths_multiple_groups() {
        let profile: StorageProfile = StorageProfile::with_locations(vec![
            FileSystemLocation {
                name: "Project1".into(),
                path: "/projects/proj1".into(),
                location_type: FileSystemLocationType::Local,
            },
            FileSystemLocation {
                name: "Project2".into(),
                path: "/projects/proj2".into(),
                location_type: FileSystemLocationType::Local,
            },
        ]);

        let file1: PathBuf = PathBuf::from("/projects/proj1/scene.blend");
        let file2: PathBuf = PathBuf::from("/projects/proj2/scene.blend");

        let groups: Vec<AssetRootGroup> =
            group_asset_paths(&[file1, file2], &[], &[], Some(&profile));

        assert_eq!(groups.len(), 2);

        let proj1_group: Option<&AssetRootGroup> = groups
            .iter()
            .find(|g| g.file_system_location_name == Some("Project1".into()));
        let proj2_group: Option<&AssetRootGroup> = groups
            .iter()
            .find(|g| g.file_system_location_name == Some("Project2".into()));

        assert!(proj1_group.is_some());
        assert!(proj2_group.is_some());
    }

    #[test]
    fn test_group_asset_paths_validated_missing_files_strict() {
        let missing_file: PathBuf = PathBuf::from("/nonexistent/file.txt");

        let result: Result<PathGroupingResult, PathGroupingError> = group_asset_paths_validated(
            &[missing_file],
            &[],
            &[],
            None,
            PathValidationMode {
                require_paths_exist: true,
            },
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            PathGroupingError::MisconfiguredInputs { missing, .. } => {
                assert!(!missing.is_empty());
            }
            _ => panic!("Expected MisconfiguredInputs error"),
        }
    }

    #[test]
    fn test_group_asset_paths_validated_missing_files_lenient() {
        let missing_file: PathBuf = PathBuf::from("/nonexistent/file.txt");

        let result: Result<PathGroupingResult, PathGroupingError> = group_asset_paths_validated(
            &[missing_file.clone()],
            &[],
            &[],
            None,
            PathValidationMode {
                require_paths_exist: false,
            },
        );

        assert!(result.is_ok());
        let grouping_result: PathGroupingResult = result.unwrap();
        assert_eq!(grouping_result.demoted_to_references.len(), 1);
    }

    #[test]
    fn test_group_asset_paths_validated_directory_as_input() {
        let temp_dir: TempDir = TempDir::new().unwrap();
        let dir_path: PathBuf = temp_dir.path().to_path_buf();

        let result: Result<PathGroupingResult, PathGroupingError> = group_asset_paths_validated(
            &[dir_path],
            &[],
            &[],
            None,
            PathValidationMode {
                require_paths_exist: true,
            },
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            PathGroupingError::MisconfiguredInputs { directories, .. } => {
                assert!(!directories.is_empty());
            }
            _ => panic!("Expected MisconfiguredInputs error"),
        }
    }

    #[test]
    fn test_group_asset_paths_validated_tracks_skipped_shared() {
        let profile: StorageProfile = create_test_profile();
        let shared_file: PathBuf = PathBuf::from("/mnt/shared/asset.exr");

        let result: Result<PathGroupingResult, PathGroupingError> = group_asset_paths_validated(
            &[shared_file],
            &[],
            &[],
            Some(&profile),
            PathValidationMode {
                require_paths_exist: false,
            },
        );

        assert!(result.is_ok());
        let grouping_result: PathGroupingResult = result.unwrap();
        assert_eq!(grouping_result.skipped_shared.len(), 1);
    }

    #[test]
    fn test_common_path_single() {
        let paths: Vec<&Path> = vec![Path::new("/a/b/c")];
        let result: Option<PathBuf> = common_path(&paths);
        assert_eq!(result, Some(PathBuf::from("/a/b/c")));
    }

    #[test]
    fn test_common_path_multiple() {
        let paths: Vec<&Path> = vec![Path::new("/a/b/c"), Path::new("/a/b/d")];
        let result: Option<PathBuf> = common_path(&paths);
        assert_eq!(result, Some(PathBuf::from("/a/b")));
    }

    #[test]
    fn test_common_path_empty() {
        let paths: Vec<&Path> = vec![];
        let result: Option<PathBuf> = common_path(&paths);
        assert_eq!(result, None);
    }

    #[test]
    fn test_common_path_no_common() {
        let paths: Vec<&Path> = vec![Path::new("/a/b"), Path::new("/x/y")];
        let result: Option<PathBuf> = common_path(&paths);
        assert_eq!(result, Some(PathBuf::from("/")));
    }

    #[test]
    fn test_common_path_utf8_paths() {
        // Test with multi-byte UTF-8 characters (Japanese)
        let paths: Vec<&Path> = vec![
            Path::new("/プロジェクト/ファイル1.txt"),
            Path::new("/プロジェクト/ファイル2.txt"),
        ];
        let result: Option<PathBuf> = common_path(&paths);
        assert_eq!(result, Some(PathBuf::from("/プロジェクト")));
    }

    #[test]
    fn test_common_path_utf8_partial_match() {
        // Test where common prefix ends mid-character boundary
        let paths: Vec<&Path> = vec![Path::new("/日本語/テスト"), Path::new("/日本/別のパス")];
        let result: Option<PathBuf> = common_path(&paths);
        // Should find common prefix up to last separator before divergence
        assert_eq!(result, Some(PathBuf::from("/")));
    }

    #[test]
    fn test_common_path_mixed_ascii_utf8() {
        let paths: Vec<&Path> = vec![
            Path::new("/projects/日本語/file.txt"),
            Path::new("/projects/日本語/other.txt"),
        ];
        let result: Option<PathBuf> = common_path(&paths);
        assert_eq!(result, Some(PathBuf::from("/projects/日本語")));
    }
}
