# Storage Profiles Design Summary

**Full doc:** `design/storage-profiles.md`  
**Status:** ✅ IMPLEMENTED in `crates/profiles/`

## Purpose
Define how files are organized based on location - LOCAL (uploaded) vs SHARED (skipped).

## Key Types

```rust
enum FileSystemLocationType {
    Local,   // Files uploaded with job
    Shared,  // Files accessible to workers, not uploaded
}

struct FileSystemLocation {
    name: String,
    path: String,
    location_type: FileSystemLocationType,
}

struct StorageProfile {
    file_system_locations: Vec<FileSystemLocation>,
}

struct AssetRootGroup {
    root_path: String,
    inputs: HashSet<PathBuf>,      // Files to upload
    outputs: HashSet<PathBuf>,     // Output directories
    references: HashSet<PathBuf>,  // May not exist yet
    file_system_location_name: Option<String>,
}
```

## Grouping Algorithm

1. Filter SHARED paths (excluded from upload)
2. Match LOCAL locations (most specific wins)
3. Group by root (same LOCAL location → same group)
4. Fallback: group by top-level directory
5. Compute common root for each group

## Key Functions

```rust
fn group_asset_paths(
    input_paths: &[PathBuf],
    output_paths: &[PathBuf],
    referenced_paths: &[PathBuf],
    storage_profile: Option<&StorageProfile>,
) -> Vec<AssetRootGroup>;

fn group_asset_paths_validated(
    ...,
    validation: PathValidationMode,
) -> Result<PathGroupingResult, PathGroupingError>;
```

## Path Validation Modes
- `require_paths_exist: true`: Error on missing files (job submission)
- `require_paths_exist: false`: Demote missing to references (preview/dry-run)

## Path Mapping Generation

```rust
fn generate_path_mapping_rules(
    source_profile: &StorageProfileWithId,
    destination_profile: &StorageProfileWithId,
) -> Vec<PathMappingRule>;
```

Creates rules for cross-profile downloads (e.g., Windows submitter → Linux worker).

## When to Read Full Doc
- Implementing storage profile handling
- Understanding path grouping logic
- Cross-platform path mapping
- Validation mode behavior
