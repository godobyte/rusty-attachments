//! Content retrieval for the virtual filesystem.
//!
//! This module provides traits and implementations for fetching file content
//! from various backends (S3 CAS, disk cache, etc.).
//!
//! # Storage Integration
//!
//! The `StorageClientAdapter` bridges the storage crate's `StorageClient` trait
//! with the VFS `FileStore` trait, enabling S3 CAS access through the existing
//! storage infrastructure.

mod s3_adapter;
mod store;

pub use s3_adapter::StorageClientAdapter;
pub use store::{FileStore, MemoryFileStore};
