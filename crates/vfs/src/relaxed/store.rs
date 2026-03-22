//! RelaxedFileStore trait for resolving relaxed consistency files.

use async_trait::async_trait;

use crate::VfsError;

use super::types::{RelaxedFileKey, RelaxedResolution, RequestPriority};

/// Trait for resolving relaxed consistency files.
///
/// Implementations check S3 for completion markers and enqueue upload requests
/// via SQS when a file has not yet been uploaded.
#[async_trait]
pub trait RelaxedFileStore: Send + Sync {
    /// Check if a relaxed file has been uploaded and resolve its CAS location.
    /// If not uploaded, enqueue an upload request.
    ///
    /// # Arguments
    /// * `key` - The relaxed file key (root_id + path_key).
    /// * `priority` - Request priority (affects which SQS queue is used).
    ///
    /// # Returns
    /// The resolution status of the file.
    async fn resolve(
        &self,
        key: &RelaxedFileKey,
        priority: RequestPriority,
    ) -> Result<RelaxedResolution, VfsError>;

    /// Poll for a previously requested file's availability.
    /// Does not re-enqueue if already pending.
    ///
    /// # Arguments
    /// * `key` - The relaxed file key.
    ///
    /// # Returns
    /// The current resolution status.
    async fn poll(&self, key: &RelaxedFileKey) -> Result<RelaxedResolution, VfsError>;
}
