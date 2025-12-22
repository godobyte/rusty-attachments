# Rusty Attachments: Storage Profiles & File System Locations

**Status: ✅ IMPLEMENTED** - See `crates/profiles/`

## Overview

Storage Profiles define how files are organized and handled based on their location. File System Locations within a profile classify paths as either LOCAL (uploaded with the job) or SHARED (accessible to workers, not uploaded).

This design enables:
- Skipping uploads for files on shared storage
- Grouping files by their storage location for path mapping
- Supporting multi-root asset structures

---

## Dependencies

This module uses path utilities from the `common` crate:

```rust
use rusty_attachments_common::{lexical_normalize, to_absolute, PathError};
```

---

## Data Structures

### File System Location Type

```rust
/// Classification of a file system location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSystemLocationType {
    /// Files are local to the submitter and must be uploaded.
    Local,
    /// Files are on shared storage accessible to workers (skip upload).
    Shared,
}
```

### File System Location

```rust
/// A named file system location with a root path and type.
#[derive(Debug, Clone)]
pub struct FileSystemLocation {
    /// Human-readable name for this location.
    pub name: String,
    /// Root path of this location.
    pub path: String,
    /// Whether this location is local or shared.
    pub location_type: FileSystemLocationType,
}
```

### Storage Profile

```rust
/// A storage profile containing multiple file system locations.
#[derive(Debug, Clone, Default)]
pub struct StorageProfile {
    /// List of file system locations in this profile.
    pub file_system_locations: Vec<FileSystemLocation>,
}

impl StorageProfile {
    /// Get all LOCAL type locations as a map of path -> name.
    pub fn local_locations(&self) -> HashMap<&str, &str> {
        self.file_system_locations
            .iter()
            .filter(|loc| loc.location_type == FileSystemLocationType::Local)
            .map(|loc| (loc.path.as_str(), loc.name.as_str()))
            .collect()
    }

    /// Get all SHARED type locations as a map of path -> name.
    pub fn shared_locations(&self) -> HashMap<&str, &str> {
        self.file_system_locations
            .iter()
            .filter(|loc| loc.location_type == FileSystemLocationType::Shared)
            .map(|loc| (loc.path.as_str(), loc.name.as_str()))
            .collect()
    }

    /// Check if a path is under any SHARED location.
    pub fn is_shared(&self, path: &Path) -> bool {
        self.file_system_locations
            .iter()
            .filter(|loc| loc.location_type == FileSystemLocationType::Shared)
            .any(|loc| path.starts_with(&loc.path))
    }

    /// Find the most specific LOCAL location containing a path.
    /// Returns (location_path, location_name) or None.
    pub fn find_local_location(&self, path: &Path) -> Option<(&str, &str)> {
        self.file_system_locations
            .iter()
            .filter(|loc| loc.location_type == FileSystemLocationType::Local)
            .filter(|loc| path.starts_with(&loc.path))
            .max_by_key(|loc| loc.path.len())
            .map(|loc| (loc.path.as_str(), loc.name.as_str()))
    }
}
```

---

## Asset Root Grouping

Files are grouped by their common root path, considering storage profile locations. This is essential for:
- Proper path mapping between submitter and worker
- Associating outputs with the correct input root
- Tracking file system location names for storage profile integration

### Grouping Algorithm

The grouping algorithm processes paths in this order:

1. **Filter SHARED paths**: Any path under a SHARED location is excluded (not uploaded)
2. **Match LOCAL locations**: Find the most specific LOCAL location containing each path
3. **Group by root**: Paths matching the same LOCAL location are grouped together
4. **Fallback grouping**: Paths not matching any LOCAL location are grouped by top-level directory
5. **Compute common root**: For each group, find the common ancestor path

### Asset Root Group

```rust
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
```

### Why Three Path Types?

- **Inputs**: Files that exist and will be hashed/uploaded. These become manifest entries.
- **Outputs**: Directories where the job will write results. Not uploaded, but tracked for download.
- **References**: Paths that may not exist yet but need to be associated with an asset root. Used for path mapping when the actual files will be created during job execution.

### Grouping Logic

```rust
/// Group paths by asset root, respecting storage profile locations.
pub fn group_asset_paths(
    input_paths: &[PathBuf],
    output_paths: &[PathBuf],
    referenced_paths: &[PathBuf],
    storage_profile: Option<&StorageProfile>,
) -> Vec<AssetRootGroup> {
    let local_locations = storage_profile
        .map(|p| p.local_locations())
        .unwrap_or_default();
    let shared_locations = storage_profile
        .map(|p| p.shared_locations())
        .unwrap_or_default();

    let mut groupings: HashMap<String, AssetRootGroup> = HashMap::new();

    for path in input_paths {
        let abs_path = path.canonicalize().unwrap_or_else(|_| path.clone());

        // Skip files under SHARED locations
        if shared_locations.keys().any(|shared| abs_path.starts_with(shared)) {
            continue;
        }

        // Find matching LOCAL location (most specific wins)
        let (root_key, location_name) = find_root_for_path(
            &abs_path,
            &local_locations,
            &mut groupings,
        );

        let group = groupings.entry(root_key.clone()).or_insert_with(|| {
            AssetRootGroup {
                file_system_location_name: location_name.map(String::from),
                ..Default::default()
            }
        });
        group.inputs.insert(abs_path);
    }

    // Similar logic for output_paths and referenced_paths...
    // (outputs and references follow same grouping but go into different sets)

    // Compute final root_path as common path of all entries in each group
    for group in groupings.values_mut() {
        let all_paths: Vec<&Path> = group.inputs.iter()
            .chain(group.outputs.iter())
            .chain(group.references.iter())
            .map(|p| p.as_path())
            .collect();

        if let Some(common) = common_path(&all_paths) {
            group.root_path = if common.is_file() {
                common.parent().unwrap_or(&common).to_string_lossy().into()
            } else {
                common.to_string_lossy().into()
            };
        }
    }

    groupings.into_values().collect()
}

/// Find the root key for grouping a path.
fn find_root_for_path<'a>(
    abs_path: &Path,
    local_locations: &HashMap<&'a str, &'a str>,
    groupings: &mut HashMap<String, AssetRootGroup>,
) -> (String, Option<&'a str>) {
    // Find most specific LOCAL location containing this path
    let matched = local_locations
        .iter()
        .filter(|(loc_path, _)| abs_path.starts_with(loc_path))
        .max_by_key(|(loc_path, _)| loc_path.len());

    if let Some((loc_path, loc_name)) = matched {
        (loc_path.to_string(), Some(*loc_name))
    } else {
        // No LOCAL location match - use top-level directory
        let top_dir = abs_path.components().next()
            .map(|c| c.as_os_str().to_string_lossy().into())
            .unwrap_or_default();
        (top_dir, None)
    }
}
```

---

## Usage Example

```rust
use rusty_attachments_storage::{
    StorageProfile, FileSystemLocation, FileSystemLocationType,
    group_asset_paths,
};

// Define storage profile
let profile = StorageProfile {
    file_system_locations: vec![
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
    ],
};

let inputs = vec![
    PathBuf::from("/mnt/projects/job1/scene.blend"),
    PathBuf::from("/mnt/projects/job1/textures/wood.png"),
    PathBuf::from("/mnt/shared/library/hdri.exr"),  // Will be skipped
];

let outputs = vec![
    PathBuf::from("/mnt/projects/job1/renders"),
];

let groups = group_asset_paths(&inputs, &outputs, &[], Some(&profile));

// Result: One group with root_path="/mnt/projects/job1"
// - inputs: [scene.blend, textures/wood.png]
// - outputs: [renders]
// - file_system_location_name: Some("ProjectFiles")
// Note: hdri.exr is skipped because it's under SHARED location
```

---

## Missing Input Path Handling

When processing input paths, some paths may not exist on the filesystem. The handling depends on the `require_paths_exist` option:

### Validation Mode

```rust
/// Options for path validation during grouping
#[derive(Debug, Clone, Copy, Default)]
pub struct PathValidationMode {
    /// If true, missing input paths cause an error.
    /// If false, missing paths are demoted to referenced_paths.
    pub require_paths_exist: bool,
}

/// Errors for misconfigured inputs
#[derive(Debug, thiserror::Error)]
pub enum PathGroupingError {
    #[error("Missing input files:\n{}", .missing.join("\n"))]
    MissingInputFiles { missing: Vec<String> },
    
    #[error("Directories specified as input files:\n{}", .directories.join("\n"))]
    DirectoriesAsFiles { directories: Vec<String> },
    
    #[error("Misconfigured inputs:\n{message}")]
    MisconfiguredInputs { 
        message: String,
        missing: Vec<String>,
        directories: Vec<String>,
    },
}
```

### Grouping with Validation

```rust
/// Result of path grouping with validation info
#[derive(Debug, Clone)]
pub struct PathGroupingResult {
    /// Successfully grouped asset roots
    pub groups: Vec<AssetRootGroup>,
    /// Paths that were demoted to references (when require_paths_exist=false)
    pub demoted_to_references: Vec<PathBuf>,
    /// Paths under SHARED locations that were skipped
    pub skipped_shared: Vec<PathBuf>,
}

/// Group paths with validation and error handling.
pub fn group_asset_paths_validated(
    input_paths: &[PathBuf],
    output_paths: &[PathBuf],
    referenced_paths: &[PathBuf],
    storage_profile: Option<&StorageProfile>,
    validation: PathValidationMode,
) -> Result<PathGroupingResult, PathGroupingError> {
    let mut missing_paths: Vec<PathBuf> = Vec::new();
    let mut directory_paths: Vec<PathBuf> = Vec::new();
    let mut demoted_to_references: Vec<PathBuf> = Vec::new();
    let mut valid_inputs: Vec<PathBuf> = Vec::new();
    let mut augmented_references: Vec<PathBuf> = referenced_paths.to_vec();
    
    // Validate each input path
    for path in input_paths {
        let abs_path = normalize_path(path);
        
        if !abs_path.exists() {
            if validation.require_paths_exist {
                missing_paths.push(abs_path);
            } else {
                // Demote to reference - will be associated with an asset root
                // but won't be hashed/uploaded
                demoted_to_references.push(abs_path.clone());
                augmented_references.push(abs_path);
            }
            continue;
        }
        
        if abs_path.is_dir() {
            // Directories cannot be input files (they should be in output_paths)
            directory_paths.push(abs_path);
            continue;
        }
        
        valid_inputs.push(abs_path);
    }
    
    // Report errors if validation is strict
    if validation.require_paths_exist && (!missing_paths.is_empty() || !directory_paths.is_empty()) {
        return Err(PathGroupingError::MisconfiguredInputs {
            message: "Job submission contains missing input files or directories specified as files.".into(),
            missing: missing_paths.iter().map(|p| p.display().to_string()).collect(),
            directories: directory_paths.iter().map(|p| p.display().to_string()).collect(),
        });
    }
    
    // Proceed with grouping using valid inputs
    let groups = group_asset_paths(
        &valid_inputs,
        output_paths,
        &augmented_references,
        storage_profile,
    );
    
    Ok(PathGroupingResult {
        groups,
        demoted_to_references,
        skipped_shared: Vec::new(), // Populated during grouping
    })
}

/// Normalize a path: absolute without resolving symlinks, with .. removed.
/// Uses utilities from common crate.
fn normalize_path(path: &Path) -> PathBuf {
    use rusty_attachments_common::{to_absolute, lexical_normalize};
    
    let abs = to_absolute(path).unwrap_or_else(|_| path.to_path_buf());
    lexical_normalize(&abs)
}
```

### Use Cases

1. **Strict validation** (`require_paths_exist: true`):
   - Used during final job submission
   - All input files must exist
   - Directories in input list cause errors
   - Returns `PathGroupingError` on any issues

2. **Lenient validation** (`require_paths_exist: false`):
   - Used during job preview/dry-run
   - Missing files are demoted to `referenced_paths`
   - Allows partial job setup before all files exist
   - Logs warnings but continues processing

---

## Integration with File System Module

The `SnapshotOptions` in `file_system.md` should accept an optional storage profile:

```rust
pub struct SnapshotOptions {
    // ... existing fields ...
    
    /// Optional storage profile for path classification.
    pub storage_profile: Option<StorageProfile>,
}
```

When a storage profile is provided:
1. Files under SHARED locations are excluded from the manifest
2. Files are grouped by their LOCAL location for proper path mapping
3. The `file_system_location_name` is recorded for each group

---

## Path Mapping Rule Generation

When downloading outputs from jobs submitted with a different storage profile, we need to generate path mapping rules to translate paths from the job's storage profile to the local machine's storage profile.

### Storage Profile with OS Family

```rust
/// Operating system family for a storage profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageProfileOsFamily {
    Windows,
    Linux,
    Macos,
}

impl StorageProfileOsFamily {
    /// Convert to PathFormat for path mapping.
    pub fn to_path_format(&self) -> PathFormat {
        match self {
            StorageProfileOsFamily::Windows => PathFormat::Windows,
            StorageProfileOsFamily::Linux | StorageProfileOsFamily::Macos => PathFormat::Posix,
        }
    }
}

/// Extended storage profile with identifier and OS family.
/// 
/// This matches the structure returned by Deadline Cloud APIs:
/// - `deadline.get_storage_profile()`
/// - `deadline.get_storage_profile_for_queue()`
#[derive(Debug, Clone)]
pub struct StorageProfileWithId {
    /// Unique identifier for this storage profile.
    pub storage_profile_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Operating system family.
    pub os_family: StorageProfileOsFamily,
    /// File system locations in this profile.
    pub file_system_locations: Vec<FileSystemLocation>,
}
```

### Generate Path Mapping Rules

```rust
use crate::path_mapping::{PathMappingRule, PathFormat};

/// Generate path mapping rules between source and destination storage profiles.
/// 
/// Creates a rule for each file system location name shared between profiles,
/// regardless of type (LOCAL vs SHARED). This allows mapping paths from a job
/// submitted with one storage profile to a machine with a different profile.
/// 
/// # Arguments
/// * `source_profile` - The storage profile of the job submitter
/// * `destination_profile` - The storage profile of the local machine
/// 
/// # Returns
/// A list of path mapping rules. Empty if profiles are identical or share no
/// location names.
/// 
/// # Example
/// ```
/// use rusty_attachments_storage::{
///     StorageProfileWithId, StorageProfileOsFamily, FileSystemLocation,
///     FileSystemLocationType, generate_path_mapping_rules,
/// };
/// 
/// let source = StorageProfileWithId {
///     storage_profile_id: "profile-windows".into(),
///     display_name: "Windows Workstation".into(),
///     os_family: StorageProfileOsFamily::Windows,
///     file_system_locations: vec![
///         FileSystemLocation {
///             name: "ProjectFiles".into(),
///             path: "Z:\\Projects".into(),
///             location_type: FileSystemLocationType::Local,
///         },
///     ],
/// };
/// 
/// let destination = StorageProfileWithId {
///     storage_profile_id: "profile-linux".into(),
///     display_name: "Linux Render Node".into(),
///     os_family: StorageProfileOsFamily::Linux,
///     file_system_locations: vec![
///         FileSystemLocation {
///             name: "ProjectFiles".into(),
///             path: "/mnt/projects".into(),
///             location_type: FileSystemLocationType::Local,
///         },
///     ],
/// };
/// 
/// let rules = generate_path_mapping_rules(&source, &destination);
/// // rules[0]: Z:\Projects -> /mnt/projects (Windows format)
/// ```
pub fn generate_path_mapping_rules(
    source_profile: &StorageProfileWithId,
    destination_profile: &StorageProfileWithId,
) -> Vec<PathMappingRule> {
    // If profiles are identical, no transformation needed
    if source_profile.storage_profile_id == destination_profile.storage_profile_id {
        return vec![];
    }
    
    // Build lookup map for destination locations by name
    let dest_locations: HashMap<&str, &str> = destination_profile
        .file_system_locations
        .iter()
        .map(|loc| (loc.name.as_str(), loc.path.as_str()))
        .collect();
    
    let source_format = source_profile.os_family.to_path_format();
    
    // Create a rule for each shared location name
    source_profile
        .file_system_locations
        .iter()
        .filter_map(|src_loc| {
            dest_locations.get(src_loc.name.as_str()).map(|dest_path| {
                PathMappingRule::new(source_format, &src_loc.path, *dest_path)
            })
        })
        .collect()
}
```

### Usage with Incremental Download

```rust
use rusty_attachments_storage::{
    generate_path_mapping_rules, PathMappingApplier,
    StorageProfileWithId,
};

// Load storage profiles from Deadline Cloud API
let job_storage_profile: StorageProfileWithId = /* from deadline.get_storage_profile_for_queue() */;
let local_storage_profile: StorageProfileWithId = /* local machine's profile */;

// Generate rules
let rules = generate_path_mapping_rules(&job_storage_profile, &local_storage_profile);

// Create applier for efficient batch transformation
let applier = if rules.is_empty() {
    None
} else {
    Some(PathMappingApplier::new(&rules)?)
};

// Use applier when resolving manifest paths
let resolved_paths = resolve_manifest_paths(
    &manifest,
    &root_path,
    job_storage_profile.os_family.to_path_format(),
    applier.as_ref(),
)?;
```

---

## Related Documents

- [common.md](common.md) - Shared path utilities (`lexical_normalize`, `to_absolute`)
- [file_system.md](file_system.md) - Snapshot and diff operations
- [job-submission.md](job-submission.md) - Converting manifests to job attachments format
- [path-mapping.md](path-mapping.md) - Path mapping rules and applier
