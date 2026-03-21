# Manifest Utilities Design Summary

**Full doc:** `design/manifest-utils.md`  
**Status:** ✅ IMPLEMENTED in `crates/model/src/operations/` and `crates/model/src/merge.rs`

## Purpose
Composable manifest operations: diff, merge, compose, filter, subtree, partition, join.

## Diff Operations

### Compute Diff Manifest
```rust
fn compute_diff_manifest(
    parent: &Snapshot,
    current: &Snapshot,
    parent_manifest_hash: impl Into<String>,
    options: &DiffOptions,
) -> SnapshotDiff;

fn compute_abs_diff_manifest(
    parent: &AbsSnapshot,
    current: &AbsSnapshot,
    parent_manifest_hash: impl Into<String>,
    options: &DiffOptions,
) -> AbsSnapshotDiff;

struct DiffOptions {
    ignore_hashes: bool,       // Compare by mtime/size only
    preserve_runnable: bool,   // Keep runnable flag from parent
}
```

Uses typed wrappers (Snapshot, AbsSnapshot) for compile-time path style safety.

## Compose Operations

### Compose Manifests (later wins)
```rust
fn compose_manifests(manifests: &[&Snapshot]) -> Option<Snapshot>;
fn compose_abs_manifests(manifests: &[&AbsSnapshot]) -> Option<AbsSnapshot>;
```

### Apply Diffs to Base
```rust
fn apply_diffs(base: &Snapshot, diffs: &[&SnapshotDiff]) -> Snapshot;
fn apply_abs_diffs(base: &AbsSnapshot, diffs: &[&AbsSnapshotDiff]) -> AbsSnapshot;
```

Internally uses a trie for efficient path-based merging.

## Filter Operations

```rust
fn filter_manifest<F>(manifest: &Snapshot, predicate: F) -> Snapshot;
fn filter_abs_manifest<F>(manifest: &AbsSnapshot, predicate: F) -> AbsSnapshot;

struct IncludeExcludeFilter {
    fn new(include: &[String], exclude: &[String]) -> Result<Self, PatternError>;
    fn matches(&self, path: &str) -> bool;
    fn as_predicate(&self) -> impl Fn(&ManifestFilePath) -> bool;
}
```

## Subtree / Join / Partition

```rust
// Extract subtree as relative manifest
fn subtree_manifest(manifest: &AbsSnapshot, subtree: &str) -> Snapshot;
fn subtree_rel_manifest(manifest: &Snapshot, subtree: &str) -> Snapshot;

// Prepend prefix (inverse of subtree)
fn join_manifest(manifest: &Snapshot, prefix: &str) -> JoinResult;  // Abs or Rel
fn join_to_absolute(manifest: &Snapshot, prefix: &str) -> AbsSnapshot;
fn join_to_relative(manifest: &Snapshot, prefix: &str) -> Snapshot;

// Split into (root, Snapshot) pairs
fn partition_manifest(manifest: &AbsSnapshot, roots: Option<&[String]>) -> Vec<(String, Snapshot)>;
```

Partition auto-detects roots (POSIX: longest common prefix, Windows: group by drive letter).

## Merge Operations

### Basic Merge
```rust
fn merge_manifests(manifests: &[&Manifest]) -> Result<Option<Manifest>, ManifestError>;
```
Later manifests override earlier (last-write-wins).

### Chronological Merge
```rust
fn merge_manifests_chronologically(
    manifests_with_timestamps: &mut [(i64, &Manifest)],
) -> Result<Option<Manifest>, ManifestError>;
```
Sorts by timestamp, then merges (newest wins for conflicts).

## ManifestPathGroup

For efficient download aggregation:
```rust
struct ManifestPathGroup {
    total_bytes: u64,
    files_by_hash_alg: HashMap<HashAlgorithm, Vec<ManifestFilePath>>,
}

impl ManifestPathGroup {
    fn add_manifest(&mut self, manifest: &Manifest);
    fn combine(&mut self, other: &ManifestPathGroup);
    fn all_paths(&self) -> Vec<&str>;
    fn file_count(&self) -> usize;
}
```

## When to Read Full Doc
- Implementing manifest comparison
- Understanding diff manifest format
- Merge semantics and ordering
- Download path aggregation
