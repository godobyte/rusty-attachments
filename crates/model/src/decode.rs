//! Manifest decoding with version detection.

use serde_json::Value;

use crate::error::ManifestError;
use crate::version::ManifestVersion;
use crate::Manifest;

/// Decode a manifest from JSON string, auto-detecting version.
pub fn decode_manifest(json: &str) -> Result<Manifest, ManifestError> {
    let data: Value = serde_json::from_str(json)?;

    let version_str = data
        .get("manifestVersion")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ManifestError::UnknownVersion("missing manifestVersion".to_string()))?;

    let version: ManifestVersion = serde_json::from_value(Value::String(version_str.to_string()))
        .map_err(|_| ManifestError::UnknownVersion(version_str.to_string()))?;

    match version {
        ManifestVersion::V2023_03_03 => {
            let manifest = crate::v2023_03_03::AssetManifest::decode(data)?;
            Ok(Manifest::V2023_03_03(manifest))
        }
        ManifestVersion::V2025_12_04_beta => {
            let manifest = crate::v2025_12_04::AssetManifest::decode(data)?;
            manifest.validate()?;
            Ok(Manifest::V2025_12_04_beta(manifest))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_v2023_03_03() {
        let json = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [
                {"path": "test.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
            ],
            "totalSize": 100
        }"#;

        let manifest = decode_manifest(json).unwrap();
        assert_eq!(manifest.version(), ManifestVersion::V2023_03_03);
        assert_eq!(manifest.file_count(), 1);
    }

    #[test]
    fn test_decode_v2025_12_04_beta() {
        let json = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2025-12-04-beta",
            "dirs": [{"path": "subdir"}],
            "files": [
                {"path": "subdir/test.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
            ],
            "totalSize": 100
        }"#;

        let manifest = decode_manifest(json).unwrap();
        assert_eq!(manifest.version(), ManifestVersion::V2025_12_04_beta);
        assert_eq!(manifest.file_count(), 1);
    }

    #[test]
    fn test_decode_unknown_version() {
        let json = r#"{"manifestVersion": "1900-01-01"}"#;
        let result = decode_manifest(json);
        assert!(result.is_err());
    }
}
