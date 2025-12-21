//! File system operations for rusty-attachments.
//!
//! This crate provides directory scanning and manifest creation:
//! - `GlobFilter` - Include/exclude pattern matching
//! - `expand_input_paths()` - Directory-to-file expansion
//! - `validate_input_paths()` - Path validation
//! - `StatCache` - File stat caching (LRU)
//! - `FileSystemScanner` - Snapshot and diff operations

pub mod error;
pub mod expand;
pub mod glob;
pub mod scanner;
pub mod stat_cache;
pub mod symlink;

// Re-export main types
pub use error::FileSystemError;
pub use expand::{expand_input_paths, validate_input_paths, ExpandedInputPaths, ValidatedInputPaths};
pub use glob::GlobFilter;
pub use scanner::{FileSystemScanner, ScanPhase, ScanProgress, SnapshotOptions};
pub use stat_cache::{StatCache, StatResult};
pub use symlink::{validate_symlink, SymlinkInfo};
