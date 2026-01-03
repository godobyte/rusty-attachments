//! Sendable wrapper for ProjFS context.
//!
//! ProjFS allows certain operations from any thread, so we need
//! a Send+Sync wrapper for the context handle.

use windows::Win32::Storage::ProjectedFileSystem::PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT;

/// ProjFS context wrapper that is Send + Sync.
///
/// # Safety
///
/// ProjFS documentation states that `PrjCompleteCommand` and `PrjWriteFileData`
/// can be called from any thread, so the context handle is safe to send
/// across threads for these operations.
#[derive(Clone, Copy)]
pub struct SendableContext(pub PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT);

unsafe impl Send for SendableContext {}

unsafe impl Sync for SendableContext {}

impl SendableContext {
    /// Create a new sendable context wrapper.
    ///
    /// # Arguments
    /// * `context` - ProjFS virtualization context
    ///
    /// # Safety
    /// Caller must ensure the context is valid for the lifetime of this wrapper.
    pub fn new(context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT) -> Self {
        Self(context)
    }

    /// Get the inner context.
    pub fn inner(&self) -> PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT {
        self.0
    }
}
