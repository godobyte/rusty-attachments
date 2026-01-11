//! Error types for ProjFS VFS.

use std::fmt;

use rusty_attachments_vfs::VfsError;

/// Errors that can occur during ProjFS operations.
#[derive(Debug)]
pub enum ProjFsError {
    /// VFS error from shared primitives.
    Vfs(VfsError),

    /// ProjFS API error.
    ProjFsApi {
        /// Operation that failed.
        operation: String,
        /// HRESULT error code.
        hresult: i32,
    },

    /// Virtualization root path error.
    InvalidRootPath(String),

    /// Virtualization already started.
    AlreadyStarted,

    /// Virtualization not started.
    NotStarted,

    /// IO error.
    Io(std::io::Error),

    /// Manifest error.
    Manifest(String),

    /// Path conversion error (UTF-16 <-> UTF-8).
    PathConversion(String),
}

impl fmt::Display for ProjFsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjFsError::Vfs(e) => write!(f, "VFS error: {}", e),
            ProjFsError::ProjFsApi { operation, hresult } => {
                write!(
                    f,
                    "ProjFS API error in {}: HRESULT 0x{:08X}",
                    operation, hresult
                )
            }
            ProjFsError::InvalidRootPath(path) => {
                write!(f, "Invalid virtualization root path: {}", path)
            }
            ProjFsError::AlreadyStarted => write!(f, "Virtualization already started"),
            ProjFsError::NotStarted => write!(f, "Virtualization not started"),
            ProjFsError::Io(e) => write!(f, "IO error: {}", e),
            ProjFsError::Manifest(msg) => write!(f, "Manifest error: {}", msg),
            ProjFsError::PathConversion(msg) => write!(f, "Path conversion error: {}", msg),
        }
    }
}

impl std::error::Error for ProjFsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProjFsError::Vfs(e) => Some(e),
            ProjFsError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<VfsError> for ProjFsError {
    fn from(e: VfsError) -> Self {
        ProjFsError::Vfs(e)
    }
}

impl From<std::io::Error> for ProjFsError {
    fn from(e: std::io::Error) -> Self {
        ProjFsError::Io(e)
    }
}
