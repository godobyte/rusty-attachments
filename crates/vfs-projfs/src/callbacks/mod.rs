//! Coordination layer between virtualizer and projection.
//!
//! This module handles dirty state tracking and coordinates between
//! the ProjFS virtualizer and the manifest projection.

mod background;
mod modified_paths;
mod path_registry;
mod stats;
mod vfs_callbacks;

pub use modified_paths::{ModificationSummary, ModifiedPathsDatabase};
pub use path_registry::PathRegistry;
pub use stats::{ProjFsStats, ProjFsStatsCollector};
pub use vfs_callbacks::VfsCallbacks;
