//! INode primitives for the virtual filesystem.
//!
//! This module provides the core data structures for representing files,
//! directories, and symlinks in the VFS.

mod dir;
mod file;
mod manager;
mod symlink;
mod types;

pub use dir::INodeDir;
pub use file::{FileContent, INodeFile};
pub use manager::INodeManager;
pub use symlink::INodeSymlink;
pub use types::{INode, INodeId, INodeType, ROOT_INODE};
