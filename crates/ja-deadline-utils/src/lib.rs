//! Deadline Cloud job attachment utilities.
//!
//! This crate provides Deadline Cloud-specific job attachment utilities,
//! composing the generic storage primitives from other crates into high-level
//! operations for:
//!
//! - **Job Submission** - Upload attachments and build `Attachments` payload for CreateJob API
//! - **Worker Sync** - Download inputs and upload outputs during job execution (future)
//!
//! # Example
//!
//! ```ignore
//! use ja_deadline_utils::{
//!     submit_bundle_attachments, AssetReferences, BundleSubmitOptions,
//! };
//! use rusty_attachments_storage::{S3Location, ManifestLocation};
//!
//! async fn submit_job<C: StorageClient>(client: &C) -> Result<(), Box<dyn std::error::Error>> {
//!     let s3_location = S3Location::new("my-bucket", "DeadlineCloud", "Data", "Manifests");
//!     let manifest_location = ManifestLocation::new("my-bucket", "DeadlineCloud", "farm-123", "queue-456");
//!
//!     let asset_references = AssetReferences {
//!         input_filenames: vec![PathBuf::from("/projects/job1/scene.blend")],
//!         output_directories: vec![PathBuf::from("/projects/job1/renders")],
//!         referenced_paths: vec![],
//!     };
//!
//!     let result = submit_bundle_attachments(
//!         client,
//!         &s3_location,
//!         &manifest_location,
//!         &asset_references,
//!         None,
//!         &BundleSubmitOptions::default(),
//!         None,
//!         None,
//!     ).await?;
//!
//!     println!("Attachments: {}", result.attachments.to_json_pretty()?);
//!     Ok(())
//! }
//! ```

mod bundle_submit;
mod conversion;
mod error;
mod types;
mod worker_sync;

pub use bundle_submit::{
    submit_bundle_attachments, BundleSubmitOptions, BundleSubmitResult, SummaryStatistics,
};
pub use conversion::{build_attachments, build_manifest_properties};
pub use error::BundleSubmitError;
pub use types::{
    AssetReferences, AssetRootManifest, Attachments, ManifestProperties, PathFormat,
};
