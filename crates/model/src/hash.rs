//! Hash algorithm definitions.

use serde::{Deserialize, Serialize};

/// Supported hashing algorithms for file content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HashAlgorithm {
    #[serde(rename = "xxh128")]
    Xxh128,
}

impl HashAlgorithm {
    /// Get the string representation of the algorithm.
    pub fn as_str(&self) -> &'static str {
        match self {
            HashAlgorithm::Xxh128 => "xxh128",
        }
    }

    /// Get the file extension used in CAS storage.
    pub fn extension(&self) -> &'static str {
        match self {
            HashAlgorithm::Xxh128 => "xxh128",
        }
    }
}

impl std::fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Chunk size for large files (256MB = 256 * 1024 * 1024 bytes).
pub const FILE_CHUNK_SIZE_BYTES: u64 = 256 * 1024 * 1024;
