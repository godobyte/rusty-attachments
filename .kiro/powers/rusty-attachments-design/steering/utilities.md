# Utilities Design Summary

**Full doc:** `design/utilities.md`  
**Status:** Not yet implemented (planned for CLI support)

## Purpose
Utility functions for CLI and application-level operations, particularly for known asset path handling.

## Known Asset Path Filtering

Removes redundant paths where one is a prefix of another using a TRIE:

```rust
fn filter_redundant_known_paths(known_asset_paths: &[String]) -> Vec<String>;

// Example:
// Input:  ["/mnt/projects", "/mnt/projects/job1", "/mnt/shared"]
// Output: ["/mnt/projects", "/mnt/shared"]
```

### Algorithm
1. Sort paths shortest to longest (prefixes first)
2. Insert into TRIE by path components
3. Detect if another path is already a prefix
4. Filter out paths with existing prefixes

### Simple Alternative
For small lists (<100 paths), O(n²) implementation:
```rust
fn filter_redundant_known_paths_simple(known_asset_paths: &[String]) -> Vec<String>;
```

## Path Classification

Classify paths as known or unknown for warning generation:

```rust
struct PathClassification {
    known_paths: HashSet<String>,    // Under known locations
    unknown_paths: HashSet<String>,  // Outside all known locations
}

fn classify_paths(
    paths: &[impl AsRef<str>],
    known_prefixes: &[String],
) -> PathClassification;
```

## Collect Known Asset Paths

Aggregate from multiple sources:

```rust
struct KnownAssetPathSources<'a> {
    explicit_paths: &'a [String],           // CLI arguments
    job_bundle_dir: Option<&'a Path>,       // Always known
    storage_profile: Option<&'a StorageProfile>,  // LOCAL locations
    config_paths: &'a [String],             // Config file
    parameter_paths: &'a [String],          // PATH-type job params
}

fn collect_known_asset_paths(sources: &KnownAssetPathSources<'_>) -> Vec<String>;
```

## Warning Message Generation

```rust
fn generate_message_for_asset_paths(
    unknown_paths: &[String],
    known_prefixes: &[String],
) -> Option<String>;

fn format_path_classification_summary(
    classification: &PathClassification,
    known_prefixes: &[String],
) -> String;
```

Output format:
```
Found 2 path(s) outside of known asset locations:
  - /home/user/downloads/texture.png
  - /tmp/cache/model.obj

Known asset locations:
  - /mnt/projects

Consider adding these paths to your storage profile or using --known-asset-path.
```

## When to Read Full Doc
- Implementing CLI path validation
- Understanding TRIE-based filtering
- Warning message formatting
- Known path source aggregation
