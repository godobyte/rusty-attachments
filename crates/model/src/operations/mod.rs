//! Composable manifest operations.
//!
//! This module provides the core operations for working with manifests,
//! following the Python Snapshots composable operations design:
//!
//! - `collect` - Filesystem scanning to unhashed manifest
//! - `hash` - Compute hashes for unhashed entries
//! - `diff` - Compute difference between manifests
//! - `compose` - Combine manifests (later entries override earlier)
//! - `filter` - Filter manifest entries by predicate
//! - `subtree` - Extract subtree as relative manifest
//! - `partition` - Split manifest into (root, RelManifest) pairs
//! - `join` - Prepend prefix to paths (inverse of subtree)

pub mod compose;
pub mod diff;
pub mod filter;
pub mod join;
pub mod partition;
pub mod subtree;

pub use compose::compose_manifests;
pub use diff::compute_diff_manifest;
pub use filter::{filter_manifest, IncludeExcludeFilter};
pub use join::join_manifest;
pub use partition::partition_manifest;
pub use subtree::subtree_manifest;
