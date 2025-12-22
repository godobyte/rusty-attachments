# Rusty Attachments: Path Mapping Design

## Overview

Path mapping enables translation between source paths (where files originated) and destination paths (where files should be placed). This is essential for:

1. **Job Submission**: Mapping submitter paths to worker session directories
2. **Download**: Mapping manifest asset roots to local destinations
3. **Cross-Platform**: Handling Windows ↔ POSIX path format differences

---

## Dependencies

This module uses path utilities from the `common` crate:

```rust
use rusty_attachments_common::{
    normalize_for_manifest, from_posix_path, to_posix_path,
    hash_bytes, PathError,
};
```

---

## Data Structures

### Path Format

```rust
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
        { PathFormat::Windows }
        #[cfg(not(windows))]
        { PathFormat::Posix }
    }
    
    /// Get the path separator for this format.
    pub fn separator(&self) -> char {
        match self {
            PathFormat::Windows => '\\',
            PathFormat::Posix => '/',
        }
    }
}
```

### Path Mapping Rule

```rust
/// A rule for mapping source paths to destination paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMappingRule {
    /// The path format of the source path (windows or posix).
    pub source_path_format: PathFormat,
    /// The source path to match against.
    pub source_path: String,
    /// The destination path to transform to.
    pub destination_path: String,
}

impl PathMappingRule {
    /// Create a new path mapping rule.
    pub fn new(
        source_path_format: PathFormat,
        source_path: impl Into<String>,
        destination_path: impl Into<String>,
    ) -> Self {
        Self {
            source_path_format,
            source_path: source_path.into(),
            destination_path: destination_path.into(),
        }
    }
    
    /// Check if a path matches this rule's source path.
    pub fn matches(&self, path: &str) -> bool {
        self.normalize_for_comparison(path)
            .starts_with(&self.normalize_for_comparison(&self.source_path))
    }
    
    /// Apply this rule to transform a path.
    /// Returns None if the path doesn't match this rule.
    pub fn apply(&self, path: &str) -> Option<String> {
        let normalized_path = self.normalize_for_comparison(path);
        let normalized_source = self.normalize_for_comparison(&self.source_path);
        
        if let Some(relative) = normalized_path.strip_prefix(&normalized_source) {
            let relative = relative.trim_start_matches('/').trim_start_matches('\\');
            if relative.is_empty() {
                Some(self.destination_path.clone())
            } else {
                Some(format!("{}/{}", self.destination_path, relative))
            }
        } else {
            None
        }
    }
    
    /// Normalize a path for comparison (convert separators, lowercase on Windows).
    fn normalize_for_comparison(&self, path: &str) -> String {
        let normalized = path.replace('\\', "/");
        if self.source_path_format == PathFormat::Windows {
            normalized.to_lowercase()
        } else {
            normalized
        }
    }
    
    /// Compute a hash of the source path for manifest naming.
    pub fn hashed_source_path(&self, hash_alg: HashAlgorithm) -> String {
        hash_data(self.source_path.as_bytes(), hash_alg)
    }
}
```

---

## Unique Destination Directory Naming

When downloading files without a storage profile, we generate unique directory names to prevent path collisions across different asset roots.

### Algorithm

```rust
use sha3::{Shake256, digest::{Update, ExtendableOutput, XofReader}};

/// Generate a unique destination directory name from a source root path.
/// 
/// Uses SHAKE-256 to create a short, deterministic hash of the source path.
/// Format: `assetroot-{20_char_hex}`
/// 
/// # Example
/// ```
/// let dir_name = get_unique_dest_dir_name("/mnt/projects/job1");
/// // Returns something like: "assetroot-a1b2c3d4e5f6g7h8i9j0"
/// ```
pub fn get_unique_dest_dir_name(source_root: &str) -> String {
    let mut hasher = Shake256::default();
    hasher.update(source_root.as_bytes());
    
    let mut reader = hasher.finalize_xof();
    let mut hash_bytes = [0u8; 10]; // 10 bytes = 20 hex chars
    reader.read(&mut hash_bytes);
    
    format!("assetroot-{}", hex::encode(hash_bytes))
}
```

---

## Dynamic Path Mapping Generation

For job submissions without storage profiles, we generate path mappings dynamically based on the session directory.

### Generate Dynamic Mappings

```rust
/// Generate path mapping rules for manifests without file system location names.
/// 
/// For each manifest that doesn't have a `fileSystemLocationName`, creates a
/// mapping from the original root path to a unique subdirectory under the
/// session directory.
/// 
/// # Arguments
/// * `session_dir` - The worker's session directory
/// * `manifests` - List of manifest properties from job attachments
/// 
/// # Returns
/// Map of original root path → PathMappingRule
pub fn generate_dynamic_path_mapping(
    session_dir: &Path,
    manifests: &[ManifestProperties],
) -> HashMap<String, PathMappingRule> {
    let mut mappings = HashMap::new();
    
    for manifest in manifests {
        // Skip manifests that have a file system location (handled by storage profiles)
        if manifest.file_system_location_name.is_some() {
            continue;
        }
        
        let dir_name = get_unique_dest_dir_name(&manifest.root_path);
        let local_root = session_dir.join(&dir_name);
        
        mappings.insert(
            manifest.root_path.clone(),
            PathMappingRule::new(
                manifest.root_path_format,
                &manifest.root_path,
                local_root.to_string_lossy(),
            ),
        );
    }
    
    mappings
}
```

### Resolve Local Destination

```rust
/// Resolve the local destination for a manifest.
/// 
/// Checks dynamic mappings first, then falls back to storage profile mappings.
/// 
/// # Arguments
/// * `manifest` - The manifest properties to resolve
/// * `dynamic_mappings` - Mappings generated by `generate_dynamic_path_mapping`
/// * `storage_profile_mappings` - Mappings from storage profile (source → dest)
/// 
/// # Returns
/// The local destination path, or error if no mapping found.
pub fn get_local_destination(
    manifest: &ManifestProperties,
    dynamic_mappings: &HashMap<String, PathMappingRule>,
    storage_profile_mappings: &HashMap<String, String>,
) -> Result<String, PathMappingError> {
    if manifest.file_system_location_name.is_some() {
        // Use storage profile mapping
        storage_profile_mappings
            .get(&manifest.root_path)
            .cloned()
            .ok_or_else(|| PathMappingError::NoMappingFound {
                root_path: manifest.root_path.clone(),
            })
    } else {
        // Use dynamic mapping
        dynamic_mappings
            .get(&manifest.root_path)
            .map(|rule| rule.destination_path.clone())
            .ok_or_else(|| PathMappingError::NoMappingFound {
                root_path: manifest.root_path.clone(),
            })
    }
}
```

---

## Path Transformation

Path transformation utilities are provided by the `common` crate. The path-mapping module provides higher-level wrappers:

### Transform Manifest Path to Local Path

```rust
use rusty_attachments_common::from_posix_path;

/// Transform a relative path from a manifest to a local filesystem path.
/// 
/// This is a convenience wrapper around `from_posix_path` from common.
/// 
/// # Arguments
/// * `manifest_path` - Relative path from manifest (always uses `/` separators)
/// * `local_root` - Local root directory
/// 
/// # Returns
/// Absolute local path
pub fn manifest_path_to_local(manifest_path: &str, local_root: &Path) -> PathBuf {
    from_posix_path(manifest_path, local_root)
}

/// Transform a local filesystem path to a manifest-relative path.
/// 
/// # Arguments
/// * `local_path` - Absolute local path
/// * `local_root` - Local root directory
/// 
/// # Returns
/// Relative path using forward slashes (manifest format)
pub fn local_path_to_manifest(
    local_path: &Path,
    local_root: &Path,
) -> Result<String, PathMappingError> {
    use rusty_attachments_common::{normalize_for_manifest, PathError};
    
    normalize_for_manifest(local_path, local_root)
        .map_err(|e| match e {
            PathError::PathOutsideRoot { path, root } => {
                PathMappingError::PathOutsideRoot { path, root }
            }
            _ => PathMappingError::InvalidPath { 
                path: local_path.display().to_string() 
            },
        })
}
```

---

## Cross-Platform Path Handling

### Convert Output Directory Paths

When processing output directories from manifests, handle path format differences:

```rust
/// Convert an output directory path to the current platform format.
/// 
/// # Arguments
/// * `output_dir` - Output directory path from manifest
/// * `source_format` - Path format the directory was created with
/// 
/// # Returns
/// Path converted to current platform format
pub fn convert_output_dir_path(
    output_dir: &str,
    source_format: PathFormat,
) -> String {
    let current_format = PathFormat::host();
    
    if source_format == current_format {
        return output_dir.to_string();
    }
    
    match source_format {
        PathFormat::Windows => output_dir.replace('\\', "/"),
        PathFormat::Posix => output_dir.replace('/', "\\"),
    }
}
```

---

## Path Mapping Applier

For efficient batch path transformation, we provide a trie-based applier that matches paths against multiple rules in O(path_depth) time.

### Why a Trie?

When transforming many paths (e.g., all files in a manifest), checking each path against each rule is O(paths × rules × path_length). A trie reduces this to O(paths × path_depth), which is significant when:
- Many rules exist (multiple file system locations)
- Many paths need transformation (large manifests)

### Implementation

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf, PurePosixPath, PureWindowsPath};

/// Trie node for path mapping.
struct TrieNode {
    /// Destination path if this node represents a complete rule.
    destination: Option<PathBuf>,
    /// Child nodes keyed by path component.
    children: HashMap<String, TrieNode>,
}

impl TrieNode {
    fn new() -> Self {
        Self {
            destination: None,
            children: HashMap::new(),
        }
    }
}

/// Efficient path mapper using trie for rule matching.
/// 
/// Handles case-insensitive matching for Windows source paths while
/// preserving case in the output.
/// 
/// # Algorithm
/// 
/// 1. Each source path is split into components using the appropriate
///    path parser (PureWindowsPath or PurePosixPath).
/// 2. Components are inserted into a trie, with the destination stored
///    at the terminal node.
/// 3. When transforming a path, we traverse the trie matching components.
///    The longest matching prefix wins.
/// 4. Remaining path components are appended to the destination.
/// 
/// # Example
/// ```
/// use rusty_attachments_storage::path_mapping::{PathMappingRule, PathMappingApplier, PathFormat};
/// 
/// let rules = vec![
///     PathMappingRule::new(PathFormat::Windows, r"Z:\Projects", "/mnt/projects"),
///     PathMappingRule::new(PathFormat::Windows, r"Z:\Projects\Special", "/mnt/special"),
/// ];
/// 
/// let applier = PathMappingApplier::new(&rules)?;
/// 
/// // Most specific rule wins
/// assert_eq!(
///     applier.transform(r"Z:\Projects\Special\data.txt"),
///     Some(PathBuf::from("/mnt/special/data.txt"))
/// );
/// 
/// // Less specific rule for other paths
/// assert_eq!(
///     applier.transform(r"Z:\Projects\Other\file.txt"),
///     Some(PathBuf::from("/mnt/projects/Other/file.txt"))
/// );
/// 
/// // Case-insensitive matching for Windows
/// assert_eq!(
///     applier.transform(r"z:\projects\other\file.txt"),
///     Some(PathBuf::from("/mnt/projects/other/file.txt"))
/// );
/// ```
pub struct PathMappingApplier {
    /// Source path format (determines parsing and case sensitivity).
    source_path_format: PathFormat,
    /// Root of the trie.
    trie: TrieNode,
}

impl PathMappingApplier {
    /// Create a new path mapping applier from a list of rules.
    /// 
    /// # Errors
    /// Returns error if rules have inconsistent source path formats.
    pub fn new(rules: &[PathMappingRule]) -> Result<Self, PathMappingError> {
        if rules.is_empty() {
            return Ok(Self {
                source_path_format: PathFormat::Posix,
                trie: TrieNode::new(),
            });
        }
        
        let source_path_format = rules[0].source_path_format;
        
        // Verify all rules have the same source format
        for rule in rules.iter().skip(1) {
            if rule.source_path_format != source_path_format {
                return Err(PathMappingError::InconsistentSourceFormats {
                    expected: source_path_format,
                    found: rule.source_path_format,
                });
            }
        }
        
        let mut trie = TrieNode::new();
        
        for rule in rules {
            let parts = Self::split_path(&rule.source_path, source_path_format);
            let mut node = &mut trie;
            
            for part in parts {
                let key = Self::normalize_part(&part, source_path_format);
                node = node.children.entry(key).or_insert_with(TrieNode::new);
            }
            
            node.destination = Some(PathBuf::from(&rule.destination_path));
        }
        
        Ok(Self { source_path_format, trie })
    }
    
    /// Transform a path using the mapping rules.
    /// 
    /// Returns `None` if no rule matches the path.
    pub fn transform(&self, path: &str) -> Option<PathBuf> {
        let parts = Self::split_path(path, self.source_path_format);
        
        let mut matched_destination: Option<&PathBuf> = None;
        let mut matched_remaining_start: usize = 0;
        
        let mut node = &self.trie;
        
        for (i, part) in parts.iter().enumerate() {
            let key = Self::normalize_part(part, self.source_path_format);
            
            match node.children.get(&key) {
                Some(child) => {
                    // Record match if this node has a destination
                    if child.destination.is_some() {
                        matched_destination = child.destination.as_ref();
                        matched_remaining_start = i + 1;
                    }
                    node = child;
                }
                None => break,
            }
        }
        
        matched_destination.map(|dest| {
            let remaining: Vec<&str> = parts[matched_remaining_start..].iter()
                .map(|s| s.as_str())
                .collect();
            
            if remaining.is_empty() {
                dest.clone()
            } else {
                dest.join(remaining.join("/"))
            }
        })
    }
    
    /// Transform a path, returning an error if no rule matches.
    pub fn strict_transform(&self, path: &str) -> Result<PathBuf, PathMappingError> {
        self.transform(path).ok_or_else(|| PathMappingError::NoRuleMatched {
            path: path.to_string(),
        })
    }
    
    /// Get the source path format for this applier.
    pub fn source_path_format(&self) -> PathFormat {
        self.source_path_format
    }
    
    /// Split a path into components using the appropriate parser.
    fn split_path(path: &str, format: PathFormat) -> Vec<String> {
        match format {
            PathFormat::Windows => {
                // Use a simple split that handles both \ and /
                path.replace('/', "\\")
                    .split('\\')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            }
            PathFormat::Posix => {
                path.split('/')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            }
        }
    }
    
    /// Normalize a path component for trie lookup.
    /// Windows paths are case-insensitive, POSIX are case-sensitive.
    fn normalize_part(part: &str, format: PathFormat) -> String {
        match format {
            PathFormat::Windows => part.to_lowercase(),
            PathFormat::Posix => part.to_string(),
        }
    }
}
```

---

## Manifest Path Resolution

When downloading files, manifest paths (which are relative) need to be converted to absolute local paths. This involves:

1. Joining the relative path with the asset root
2. Normalizing using the source path format
3. Applying path mapping (if storage profiles differ)

### Data Structures

```rust
/// A manifest path resolved to an absolute local destination.
#[derive(Debug, Clone)]
pub struct ResolvedManifestPath {
    /// The original manifest file entry.
    pub manifest_entry: ManifestFilePath,
    /// The absolute local path where the file should be downloaded.
    pub local_path: PathBuf,
}

/// Result of resolving manifest paths.
#[derive(Debug, Clone, Default)]
pub struct ResolvedManifestPaths {
    /// Successfully resolved paths.
    pub resolved: Vec<ResolvedManifestPath>,
    /// Paths that could not be mapped (no matching rule).
    pub unmapped: Vec<UnmappedPath>,
}

/// A path that could not be mapped.
#[derive(Debug, Clone)]
pub struct UnmappedPath {
    /// The original manifest path.
    pub manifest_path: String,
    /// The absolute source path (after joining with root).
    pub absolute_source_path: String,
}
```

### Resolution Function

```rust
/// Resolve manifest paths to absolute local paths.
/// 
/// For each file in the manifest:
/// 1. Join the relative path with `root_path` using `source_path_format` separators
/// 2. Normalize the resulting path
/// 3. Apply path mapping if `applier` is provided
/// 
/// # Arguments
/// * `manifest` - The manifest containing relative file paths
/// * `root_path` - The asset root path (from manifest metadata)
/// * `source_path_format` - Path format of the source machine (from storage profile)
/// * `applier` - Optional path mapping applier for cross-profile downloads
/// 
/// # Returns
/// Resolved paths and any paths that couldn't be mapped.
/// 
/// # Example
/// ```
/// use rusty_attachments_storage::path_mapping::{
///     resolve_manifest_paths, PathMappingApplier, PathFormat,
/// };
/// 
/// let manifest = download_manifest(&client, bucket, key).await?;
/// let root_path = "Z:\\Projects\\Job1";
/// let source_format = PathFormat::Windows;
/// 
/// // Without path mapping (same storage profile)
/// let result = resolve_manifest_paths(&manifest, root_path, source_format, None)?;
/// 
/// // With path mapping (different storage profiles)
/// let applier = PathMappingApplier::new(&rules)?;
/// let result = resolve_manifest_paths(&manifest, root_path, source_format, Some(&applier))?;
/// 
/// for resolved in result.resolved {
///     println!("{} -> {}", resolved.manifest_entry.path, resolved.local_path.display());
/// }
/// 
/// for unmapped in result.unmapped {
///     eprintln!("WARNING: No mapping for {}", unmapped.absolute_source_path);
/// }
/// ```
pub fn resolve_manifest_paths(
    manifest: &Manifest,
    root_path: &str,
    source_path_format: PathFormat,
    applier: Option<&PathMappingApplier>,
) -> Result<ResolvedManifestPaths, PathMappingError> {
    let mut result = ResolvedManifestPaths::default();
    
    let files = match manifest {
        Manifest::V2023_03_03(m) => {
            // Convert v2023 paths to v2025 format for uniform handling
            m.paths.iter()
                .map(|p| ManifestFilePath {
                    path: p.path.clone(),
                    hash: Some(p.hash.clone()),
                    size: Some(p.size),
                    mtime: Some(p.mtime),
                    runnable: false,
                    chunkhashes: None,
                    symlink_target: None,
                    deleted: false,
                })
                .collect::<Vec<_>>()
        }
        Manifest::V2025_12_04_beta(m) => m.paths.clone(),
    };
    
    for entry in files {
        // Skip deleted entries
        if entry.deleted {
            continue;
        }
        
        // Join relative path with root using source format
        let absolute_source = join_path(root_path, &entry.path, source_path_format);
        
        // Apply path mapping if provided
        let local_path = if let Some(applier) = applier {
            match applier.transform(&absolute_source) {
                Some(mapped) => mapped,
                None => {
                    result.unmapped.push(UnmappedPath {
                        manifest_path: entry.path.clone(),
                        absolute_source_path: absolute_source,
                    });
                    continue;
                }
            }
        } else {
            PathBuf::from(&absolute_source)
        };
        
        result.resolved.push(ResolvedManifestPath {
            manifest_entry: entry,
            local_path,
        });
    }
    
    Ok(result)
}

/// Join a root path with a relative path using the specified format.
fn join_path(root: &str, relative: &str, format: PathFormat) -> String {
    let separator = format.separator();
    let root = root.trim_end_matches(|c| c == '/' || c == '\\');
    let relative = relative.trim_start_matches(|c| c == '/' || c == '\\');
    
    // Normalize the relative path separators to match the format
    let relative = match format {
        PathFormat::Windows => relative.replace('/', "\\"),
        PathFormat::Posix => relative.replace('\\', "/"),
    };
    
    format!("{}{}{}", root, separator, relative)
}
```

---

## Error Types

Path mapping errors extend the common `PathError`:

```rust
use rusty_attachments_common::PathError;

#[derive(Debug, thiserror::Error)]
pub enum PathMappingError {
    /// Path-related errors (from common crate)
    #[error(transparent)]
    Path(#[from] PathError),
    
    #[error("No path mapping rule found for root path: {root_path}")]
    NoMappingFound { root_path: String },
    
    #[error("Path {path} is outside root {root}")]
    PathOutsideRoot { path: String, root: String },
    
    #[error("Invalid path format: {path}")]
    InvalidPath { path: String },
    
    #[error("No path mapping rule matched path: {path}")]
    NoRuleMatched { path: String },
    
    #[error("Inconsistent source path formats in rules: expected {expected:?}, found {found:?}")]
    InconsistentSourceFormats { expected: PathFormat, found: PathFormat },
}
```

---

## Usage Examples

### Job Submission Path Mapping

```rust
use rusty_attachments_storage::path_mapping::{
    generate_dynamic_path_mapping, get_local_destination, PathMappingRule,
};

// On worker: generate mappings for session
let session_dir = Path::new("/var/lib/deadline/session-123");
let dynamic_mappings = generate_dynamic_path_mapping(session_dir, &attachments.manifests);

// Resolve destination for each manifest
for manifest in &attachments.manifests {
    let local_dest = get_local_destination(
        manifest,
        &dynamic_mappings,
        &storage_profile_mappings,
    )?;
    
    println!("Manifest root {} -> local {}", manifest.root_path, local_dest);
}

// Convert mappings to job submission format
let path_mapping_rules: Vec<_> = dynamic_mappings
    .values()
    .map(|rule| serde_json::json!({
        "sourcePathFormat": rule.source_path_format,
        "sourcePath": rule.source_path,
        "destinationPath": rule.destination_path,
    }))
    .collect();
```

### Download Path Resolution

```rust
use rusty_attachments_storage::path_mapping::manifest_path_to_local;

let manifest = download_manifest(&client, bucket, key).await?;
let local_root = Path::new("/var/lib/deadline/session-123/assetroot-abc123");

for file in manifest.files() {
    let local_path = manifest_path_to_local(
        &file.path,
        local_root,
        manifest_properties.root_path_format,
    );
    
    // Download file to local_path
    download_file(&client, &file.hash, &local_path).await?;
}
```

---

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── storage/
│   │   └── src/
│   │       ├── path_mapping/
│   │       │   ├── mod.rs           # Public API
│   │       │   ├── rule.rs          # PathMappingRule
│   │       │   ├── dynamic.rs       # Dynamic mapping generation
│   │       │   ├── transform.rs     # Path transformation utilities
│   │       │   └── error.rs         # Error types
│   │       └── ...
```

---

## Related Documents

- [common.md](common.md) - Shared path utilities (`from_posix_path`, `normalize_for_manifest`)
- [storage-profiles.md](storage-profiles.md) - Storage profile and file system location types, `generate_path_mapping_rules()`
- [job-submission.md](job-submission.md) - Converting manifests to job attachments format
- [manifest-storage.md](manifest-storage.md) - Manifest upload/download operations
- [storage-design.md](storage-design.md) - Download orchestrator with `download_to_resolved_paths()`


---

## Implementation Status

### ✅ Implemented

#### Path Utilities (in `common` crate)
Location: `crates/common/src/path_utils.rs`

- [x] `to_posix_path()` - Convert path to forward-slash format
- [x] `from_posix_path()` - Convert manifest path to local path with OS separators
- [x] `normalize_for_manifest()` - Normalize path relative to root for manifest storage
- [x] `is_within_root()` - Security check for path containment
- [x] `lexical_normalize()` - Normalize path without filesystem access
- [x] `to_absolute()` - Convert to absolute path

### ❌ Not Implemented

#### PathFormat Enum
- [ ] `PathFormat` enum (Windows/Posix)
- [ ] `PathFormat::host()` - Get current platform format
- [ ] `PathFormat::separator()` - Get path separator

#### PathMappingRule
- [ ] `PathMappingRule` struct
- [ ] `matches()` - Check if path matches rule
- [ ] `apply()` - Transform path using rule
- [ ] `normalize_for_comparison()` - Normalize for matching
- [ ] `hashed_source_path()` - Hash source path for naming

#### Dynamic Path Mapping
- [ ] `get_unique_dest_dir_name()` - Generate unique directory name using SHAKE-256
- [ ] `generate_dynamic_path_mapping()` - Generate mappings for manifests without storage profiles
- [ ] `get_local_destination()` - Resolve local destination for manifest

#### Path Transformation Wrappers
- [ ] `manifest_path_to_local()` - Wrapper around `from_posix_path`
- [ ] `local_path_to_manifest()` - Wrapper around `normalize_for_manifest`
- [ ] `convert_output_dir_path()` - Convert output directory to current platform format

#### PathMappingApplier (Trie-based)
- [ ] `PathMappingApplier` struct with trie
- [ ] `new()` - Build trie from rules
- [ ] `transform()` - Transform path using trie lookup
- [ ] `strict_transform()` - Transform with error on no match
- [ ] `split_path()` - Split path into components
- [ ] `normalize_part()` - Normalize component for lookup

#### Manifest Path Resolution
- [ ] `ResolvedManifestPath` struct
- [ ] `ResolvedManifestPaths` struct
- [ ] `UnmappedPath` struct
- [ ] `resolve_manifest_paths()` - Resolve manifest paths to local paths
- [ ] `join_path()` - Join root with relative path

#### Error Types
- [ ] `PathMappingError` enum
- [ ] `NoMappingFound` variant
- [ ] `PathOutsideRoot` variant
- [ ] `InvalidPath` variant
- [ ] `NoRuleMatched` variant
- [ ] `InconsistentSourceFormats` variant

### Notes

1. The basic path utilities (`to_posix_path`, `from_posix_path`, etc.) are implemented in the `common` crate and are used by the filesystem crate.

2. The path mapping module (`storage/src/path_mapping/`) does not exist yet. This is needed for:
   - Cross-platform path translation (Windows ↔ POSIX)
   - Storage profile path mapping
   - Dynamic session directory mapping

3. The `PathMappingApplier` with trie-based lookup is an optimization for batch path transformation. A simpler linear implementation could be done first.

4. Dependencies needed for full implementation:
   - `sha3` crate for SHAKE-256 (unique directory naming)
   - Or use existing `xxhash` for simpler hashing
