//! Coordination layer between virtualizer and projection.
//!
//! This module handles dirty state tracking and coordinates between
//! the ProjFS virtualizer and the manifest projection.

mod background;
mod vfs_callbacks;

pub use background::{BackgroundTask, BackgroundTaskRunner};
pub use vfs_callbacks::VfsCallbacks;
