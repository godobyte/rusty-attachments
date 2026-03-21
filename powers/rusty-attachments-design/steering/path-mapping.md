# Path Mapping Design Summary

**Full doc:** `design/path-mapping.md`  
**Status:** Partially implemented (path utilities in `crates/common/`, PathFormat in `crates/ja-deadline-utils/`)

## Purpose
Translate paths between source (submitter) and destination (worker) for cross-platform and cross-profile scenarios.

## Key Types

```rust
enum PathFormat { Windows, Posix }

struct PathMappingRule {
    source_path_format: PathFormat,
    source_path: String,
    destination_path: String,
}
```

## Path Mapping Rule

```rust
impl PathMappingRule {
    fn matches(&self, path: &str) -> bool;
    fn apply(&self, path: &str) -> Option<String>;
    fn hashed_source_path(&self, hash_alg: HashAlgorithm) -> String;
}
```

## Dynamic Path Mapping

For jobs without storage profiles:
```rust
fn get_unique_dest_dir_name(source_root: &str) -> String;
// Returns: "assetroot-{20_char_hex}" using SHAKE-256

fn generate_dynamic_path_mapping(
    session_dir: &Path,
    manifests: &[ManifestProperties],
) -> HashMap<String, PathMappingRule>;
```

## PathMappingApplier (Trie-Based)

Efficient batch transformation using prefix trie:
```rust
struct PathMappingApplier {
    fn new(rules: &[PathMappingRule]) -> Result<Self, PathMappingError>;
    fn transform(&self, path: &str) -> Option<PathBuf>;
    fn strict_transform(&self, path: &str) -> Result<PathBuf, PathMappingError>;
}
```

O(path_depth) lookup instead of O(rules × path_length).

## Manifest Path Resolution

```rust
struct ResolvedManifestPath {
    manifest_entry: ManifestFilePath,
    local_path: PathBuf,
}

fn resolve_manifest_paths(
    manifest: &Manifest,
    root_path: &str,
    source_path_format: PathFormat,
    applier: Option<&PathMappingApplier>,
) -> Result<ResolvedManifestPaths, PathMappingError>;
```

## Implementation Status

### ✅ Implemented (in common crate)
- `to_posix_path()`, `from_posix_path()`
- `normalize_for_manifest()`, `is_within_root()`
- `lexical_normalize()`, `to_absolute()`

### ✅ Implemented (in ja-deadline-utils)
- `PathFormat` enum (Windows, Posix) with `PathFormat::host()`

### ❌ Not Implemented
- `PathMappingRule` (Rust-side, exists as Python binding wrapper)
- `PathMappingApplier` (trie-based)
- Dynamic mapping generation
- `resolve_manifest_paths()`

## When to Read Full Doc
- Cross-platform path handling
- Storage profile path mapping
- Trie-based applier implementation
- Session directory mapping
