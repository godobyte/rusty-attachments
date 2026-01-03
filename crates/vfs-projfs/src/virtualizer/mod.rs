//! ProjFS virtualizer implementation.
//!
//! This module provides the ProjFS callback implementations and
//! manages the virtualization lifecycle.

#[cfg(target_os = "windows")]
mod projfs;

#[cfg(target_os = "windows")]
pub use projfs::WritableProjFs;

// Non-Windows stub
#[cfg(not(target_os = "windows"))]
pub struct WritableProjFs;

#[cfg(not(target_os = "windows"))]
impl WritableProjFs {
    pub fn new(
        _manifest: &rusty_attachments_model::Manifest,
        _storage: std::sync::Arc<dyn rusty_attachments_vfs::FileStore>,
        _options: crate::options::ProjFsOptions,
        _write_options: crate::options::ProjFsWriteOptions,
    ) -> Result<Self, crate::error::ProjFsError> {
        Err(crate::error::ProjFsError::Manifest(
            "ProjFS is only available on Windows".to_string(),
        ))
    }
}
