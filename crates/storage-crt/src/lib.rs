//! AWS SDK S3 backend for rusty-attachments storage.
//!
//! This crate provides `StorageClient` implementations using the AWS SDK for Rust.
//! Two backends are available:
//!
//! - `CrtStorageClient` - Basic AWS SDK S3 client
//! - `TransferManagerClient` - High-performance client using S3 Transfer Manager
//!   (requires `transfer-manager` feature, enabled by default)
//!
//! # Example
//!
//! ```ignore
//! use rusty_attachments_storage_crt::DefaultClient;
//! use rusty_attachments_storage::{StorageSettings, UploadOrchestrator, S3Location};
//!
//! let settings = StorageSettings::default();
//! let client = DefaultClient::new(settings).await?;
//!
//! let location = S3Location::new("my-bucket", "DeadlineCloud", "Data", "Manifests");
//! let orchestrator = UploadOrchestrator::new(&client, location);
//! ```

mod client;
mod config;
mod error;

#[cfg(feature = "transfer-manager")]
mod transfer_manager;

pub use client::CrtStorageClient;
pub use config::S3Config;
pub use error::CrtError;

#[cfg(feature = "transfer-manager")]
pub use transfer_manager::TransferManagerClient;

/// Default client type based on enabled features.
///
/// With `transfer-manager` (default): uses `TransferManagerClient` for
/// automatic multipart uploads and parallel downloads.
///
/// Without `transfer-manager`: falls back to `CrtStorageClient`.
#[cfg(feature = "transfer-manager")]
pub type DefaultClient = TransferManagerClient;

#[cfg(not(feature = "transfer-manager"))]
pub type DefaultClient = CrtStorageClient;
