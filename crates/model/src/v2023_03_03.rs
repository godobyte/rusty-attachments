//! v2023-03-03 manifest format (original format).
//!
//! This format supports:
//! - File entries with path, hash, size, mtime
//! - XXH128 hashing algorithm
//! - Canonical JSON encoding

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::hash::HashAlgorithm;
use crate::version::ManifestVersion;

/// File entry in v2023-03-03 manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPath {
    /// Relative file path within the manifest.
    pub path: String,
    /// XXH128 hash of file content.
    pub hash: String,
    /// File size in bytes.
    pub size: u64,
    /// Modification time in microseconds since epoch.
    pub mtime: i64,
}

impl ManifestPath {
    /// Create a new manifest path entry.
    pub fn new(path: impl Into<String>, hash: impl Into<String>, size: u64, mtime: i64) -> Self {
        Self {
            path: path.into(),
            hash: hash.into(),
            size,
            mtime,
        }
    }
}

/// Asset manifest v2023-03-03.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetManifest {
    /// Hashing algorithm used for file hashes.
    pub hash_alg: HashAlgorithm,
    /// Manifest format version.
    pub manifest_version: ManifestVersion,
    /// List of file entries.
    pub paths: Vec<ManifestPath>,
    /// Total size of all files in bytes.
    pub total_size: u64,
}

impl AssetManifest {
    /// Create a new v2023-03-03 manifest.
    pub fn new(paths: Vec<ManifestPath>) -> Self {
        let total_size = paths.iter().map(|p| p.size).sum();
        Self {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2023_03_03,
            paths,
            total_size,
        }
    }

    /// Encode to canonical JSON string.
    pub fn encode(&self) -> Result<String, ManifestError> {
        let mut manifest = self.clone();
        // Sort paths by UTF-16 BE encoding for canonical output
        manifest.paths.sort_by(|a, b| {
            let a_bytes: Vec<u16> = a.path.encode_utf16().collect();
            let b_bytes: Vec<u16> = b.path.encode_utf16().collect();
            a_bytes.cmp(&b_bytes)
        });

        // Build the output dict with sorted keys
        let output = serde_json::json!({
            "hashAlg": manifest.hash_alg,
            "manifestVersion": manifest.manifest_version,
            "paths": manifest.paths.iter().map(|p| {
                serde_json::json!({
                    "hash": p.hash,
                    "mtime": p.mtime,
                    "path": p.path,
                    "size": p.size,
                })
            }).collect::<Vec<_>>(),
            "totalSize": manifest.total_size,
        });

        Ok(serde_json::to_string(&output)?)
    }

    /// Decode from JSON value.
    pub fn decode(data: serde_json::Value) -> Result<Self, ManifestError> {
        Ok(serde_json::from_value(data)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_path_new() {
        let path = ManifestPath::new("test/file.txt", "abc123", 1024, 1234567890);
        assert_eq!(path.path, "test/file.txt");
        assert_eq!(path.hash, "abc123");
        assert_eq!(path.size, 1024);
        assert_eq!(path.mtime, 1234567890);
    }

    #[test]
    fn test_asset_manifest_new() {
        let paths = vec![
            ManifestPath::new("a.txt", "hash1", 100, 1000),
            ManifestPath::new("b.txt", "hash2", 200, 2000),
        ];
        let manifest = AssetManifest::new(paths);

        assert_eq!(manifest.hash_alg, HashAlgorithm::Xxh128);
        assert_eq!(manifest.manifest_version, ManifestVersion::V2023_03_03);
        assert_eq!(manifest.total_size, 300);
        assert_eq!(manifest.paths.len(), 2);
    }

    #[test]
    fn test_encode_produces_canonical_json() {
        let paths = vec![
            ManifestPath::new("b.txt", "hash2", 200, 2000),
            ManifestPath::new("a.txt", "hash1", 100, 1000),
        ];
        let manifest = AssetManifest::new(paths);
        let encoded = manifest.encode().unwrap();

        // Should be sorted by path
        assert!(encoded.contains(r#""path":"a.txt"#));
        // No whitespace in canonical JSON
        assert!(!encoded.contains(" "));
    }
}
