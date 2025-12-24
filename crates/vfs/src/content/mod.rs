//! Content retrieval for the virtual filesystem.
//!
//! This module provides traits and implementations for fetching file content
//! from various backends (S3 CAS, disk cache, etc.).

mod store;

pub use store::FileStore;
