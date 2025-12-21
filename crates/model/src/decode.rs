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
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [
                {"path": "test.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
            ],
            "totalSize": 100
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        assert_eq!(manifest.version(), ManifestVersion::V2023_03_03);
        assert_eq!(manifest.file_count(), 1);
    }

    #[test]
    fn test_decode_v2025_12_04_beta() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2025-12-04-beta",
            "dirs": [{"path": "subdir"}],
            "files": [
                {"path": "subdir/test.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
            ],
            "totalSize": 100
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        assert_eq!(manifest.version(), ManifestVersion::V2025_12_04_beta);
        assert_eq!(manifest.file_count(), 1);
    }

    #[test]
    fn test_decode_unknown_version() {
        let json: &str = r#"{"manifestVersion": "1900-01-01"}"#;
        let result: Result<Manifest, ManifestError> = decode_manifest(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_missing_manifest_version() {
        let json: &str = r#"{"hashAlg": "xxh128"}"#;
        let result: Result<Manifest, ManifestError> = decode_manifest(json);
        assert!(matches!(result, Err(ManifestError::UnknownVersion(_))));
    }

    // ==================== Unicode character tests ====================

    #[test]
    fn test_decode_unicode_carriage_return() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [{"path": "\r", "hash": "CarriageReturn", "size": 1, "mtime": 1679079744833848}],
            "totalSize": 1
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2023_03_03(m) = manifest {
            assert_eq!(m.paths[0].path, "\r");
        } else {
            panic!("Expected V2023_03_03 manifest");
        }
    }

    #[test]
    fn test_decode_unicode_control_char() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [{"path": "\u0080", "hash": "Control", "size": 1, "mtime": 1679079344833348}],
            "totalSize": 1
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2023_03_03(m) = manifest {
            assert_eq!(m.paths[0].path, "\u{0080}");
        } else {
            panic!("Expected V2023_03_03 manifest");
        }
    }

    #[test]
    fn test_decode_unicode_euro_sign() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [{"path": "€", "hash": "EuroSign", "size": 1, "mtime": 1679079344836848}],
            "totalSize": 1
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2023_03_03(m) = manifest {
            assert_eq!(m.paths[0].path, "€");
        } else {
            panic!("Expected V2023_03_03 manifest");
        }
    }

    #[test]
    fn test_decode_unicode_emoji() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [{"path": "😀", "hash": "EmojiGrinningFace", "size": 1, "mtime": 1679579344833848}],
            "totalSize": 1
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2023_03_03(m) = manifest {
            assert_eq!(m.paths[0].path, "😀");
        } else {
            panic!("Expected V2023_03_03 manifest");
        }
    }

    #[test]
    fn test_decode_unicode_hebrew() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [{"path": "דּ", "hash": "HebrewLetterDaletWithDagesh", "size": 1, "mtime": 1679039344833848}],
            "totalSize": 1
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2023_03_03(m) = manifest {
            assert_eq!(m.paths[0].path, "דּ");
        } else {
            panic!("Expected V2023_03_03 manifest");
        }
    }

    #[test]
    fn test_decode_unicode_latin_diaeresis() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2023-03-03",
            "paths": [{"path": "ö", "hash": "LatinSmallLetterOWithDiaeresis", "size": 1, "mtime": 1679079344833848}],
            "totalSize": 1
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2023_03_03(m) = manifest {
            assert_eq!(m.paths[0].path, "ö");
        } else {
            panic!("Expected V2023_03_03 manifest");
        }
    }

    // ==================== v2025 format tests ====================

    #[test]
    fn test_decode_v2025_with_chunked_file() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2025-12-04-beta",
            "dirs": [],
            "files": [
                {"path": "large.bin", "chunkhashes": ["hash1", "hash2"], "size": 314572801, "mtime": 1234567890}
            ],
            "totalSize": 314572801
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2025_12_04_beta(m) = manifest {
            assert!(m.paths[0].chunkhashes.is_some());
            assert_eq!(m.paths[0].chunkhashes.as_ref().unwrap().len(), 2);
        } else {
            panic!("Expected V2025_12_04_beta manifest");
        }
    }

    #[test]
    fn test_decode_v2025_with_symlink() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2025-12-04-beta",
            "dirs": [],
            "files": [
                {"path": "link.txt", "symlink_target": "target.txt"}
            ],
            "totalSize": 0
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2025_12_04_beta(m) = manifest {
            assert_eq!(m.paths[0].symlink_target, Some("target.txt".to_string()));
        } else {
            panic!("Expected V2025_12_04_beta manifest");
        }
    }

    #[test]
    fn test_decode_v2025_with_runnable() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "manifestVersion": "2025-12-04-beta",
            "dirs": [],
            "files": [
                {"path": "script.sh", "hash": "abc123", "size": 512, "mtime": 1234567890, "runnable": true}
            ],
            "totalSize": 512
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2025_12_04_beta(m) = manifest {
            assert!(m.paths[0].runnable);
        } else {
            panic!("Expected V2025_12_04_beta manifest");
        }
    }
}
