//! Error types for Tauri commands.

use serde::Serialize;

/// Error type that can be returned from Tauri commands.
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
    pub kind: ErrorKind,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidPath,
    InvalidVersion,
    IoError,
    StorageError,
}

impl CommandError {
    pub fn invalid_path(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            kind: ErrorKind::InvalidPath,
        }
    }

    pub fn invalid_version(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            kind: ErrorKind::InvalidVersion,
        }
    }

    pub fn io_error(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            kind: ErrorKind::IoError,
        }
    }

    pub fn storage_error(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            kind: ErrorKind::StorageError,
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<std::io::Error> for CommandError {
    fn from(err: std::io::Error) -> Self {
        Self::io_error(err.to_string())
    }
}

impl From<rusty_attachments_filesystem::FileSystemError> for CommandError {
    fn from(err: rusty_attachments_filesystem::FileSystemError) -> Self {
        Self {
            message: err.to_string(),
            kind: ErrorKind::IoError,
        }
    }
}

impl From<rusty_attachments_storage::StorageError> for CommandError {
    fn from(err: rusty_attachments_storage::StorageError) -> Self {
        Self::storage_error(err.to_string())
    }
}

impl From<ja_deadline_utils::BundleSubmitError> for CommandError {
    fn from(err: ja_deadline_utils::BundleSubmitError) -> Self {
        Self::storage_error(err.to_string())
    }
}
