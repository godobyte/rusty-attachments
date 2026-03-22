//! Relaxed consistency support for on-demand file fetching.
//!
//! This module extends the VFS to support files that are not pre-uploaded
//! to S3 CAS. Instead, they are fetched on-demand from on-premises storage
//! via an upload agent that listens to SQS queues.
//!
//! # Architecture
//!
//! ```text
//! VFS read() → RelaxedFileStore::resolve() → check S3 marker
//!                                           → if missing: enqueue SQS
//!                                           → poll until available
//!                                           → promote INode to SingleHash
//! ```

pub mod markers;
pub mod memory_store;
pub mod pending_tracker;
pub mod store;
pub mod types;
pub mod utils;

pub use markers::{
    FileUploadRequest, MarkerEnvelope, RelaxedRootConfig, UploadCompletionMarker,
    UploadFailureMarker,
};
pub use memory_store::MemoryRelaxedStore;
pub use pending_tracker::PendingFileTracker;
pub use store::RelaxedFileStore;
pub use types::{RelaxedFileKey, RelaxedResolution, RequestPriority};
pub use utils::{relaxed_file_key, relaxed_relative_path, s3_marker_key};
