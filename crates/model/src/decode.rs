//! Manifest decoding with version detection.

use serde_json::Value;

use crate::error::ManifestError;
use crate::v2025_12::{AssetManifest, ManifestDirectoryPath, ManifestFilePath};
use crate::version::{ManifestVersion, SpecVersion};
use crate::Manifest;

/// Decode a manifest from JSON string, auto-detecting version.
///
/// # Arguments
/// * `json` - JSON string to decode.
///
/// # Returns
/// Decoded manifest.
///
/// # Errors
/// Returns error if JSON is invalid or version is unknown.
pub fn decode_manifest(json: &str) -> Result<Manifest, ManifestError> {
    let data: Value = serde_json::from_str(json)?;

    // Check for v2025-12 format (has specificationVersion)
    if let Some(spec_str) = data.get("specificationVersion").and_then(|v| v.as_str()) {
        let spec_str_owned: String = spec_str.to_string();
        return decode_v2025_12(data, &spec_str_owned);
    }

    // Check for legacy manifestVersion field
    let version_str: &str = data
        .get("manifestVersion")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ManifestError::UnknownVersion("missing manifestVersion".to_string()))?;

    let version: ManifestVersion =
        serde_json::from_value(Value::String(version_str.to_string()))
            .map_err(|_| ManifestError::UnknownVersion(version_str.to_string()))?;

    match version {
        ManifestVersion::V2023_03_03 => {
            let manifest = crate::v2023_03_03::AssetManifest::decode(data)?;
            Ok(Manifest::V2023_03_03(manifest))
        }
        ManifestVersion::V2025_12 => {
            // Legacy v2025-12 without specificationVersion - assume relative snapshot
            decode_v2025_12(data, SpecVersion::REL_SNAPSHOT.spec_string())
        }
    }
}

/// Decode a v2025-12 manifest from JSON.
fn decode_v2025_12(data: Value, spec_str: &str) -> Result<Manifest, ManifestError> {
    let spec_version: SpecVersion = SpecVersion::from_spec_string(spec_str)
        .ok_or_else(|| ManifestError::UnknownSpecVersion(spec_str.to_string()))?;

    // Build directory index for $N/ expansion
    let dirs_array: &Vec<Value> = data
        .get("dirs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ManifestError::UnknownVersion("missing dirs array".to_string()))?;

    let dir_index: Vec<String> = build_dir_index(dirs_array)?;

    // Decode directories
    let dirs: Vec<ManifestDirectoryPath> = dirs_array
        .iter()
        .map(|d| decode_directory_entry(d, &dir_index))
        .collect::<Result<Vec<_>, _>>()?;

    // Decode files
    let files_array: &Vec<Value> = data
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ManifestError::UnknownVersion("missing files array".to_string()))?;

    let files: Vec<ManifestFilePath> = files_array
        .iter()
        .map(|f| decode_file_entry(f, &dir_index))
        .collect::<Result<Vec<_>, _>>()?;

    // Get parent manifest hash for diff manifests
    let parent_manifest_hash: Option<String> = data
        .get("parentManifestHash")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let manifest: AssetManifest =
        AssetManifest::with_spec(dirs, files, spec_version, parent_manifest_hash);

    Ok(Manifest::V2025_12(manifest))
}

/// Build directory index for $N/ expansion.
fn build_dir_index(dirs_array: &[Value]) -> Result<Vec<String>, ManifestError> {
    let mut index: Vec<String> = Vec::with_capacity(dirs_array.len());

    for dir in dirs_array {
        let path: &str = dir
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ManifestError::UnknownVersion("directory missing path".to_string()))?;

        // Expand $N/ references
        let expanded: String = expand_path_reference(path, &index)?;
        index.push(expanded);
    }

    Ok(index)
}

/// Expand $N/ path references using directory index.
fn expand_path_reference(path: &str, dir_index: &[String]) -> Result<String, ManifestError> {
    if path.starts_with('$') {
        if let Some(slash_pos) = path.find('/') {
            let idx_str: &str = &path[1..slash_pos];
            let idx: usize = idx_str.parse().map_err(|_| {
                ManifestError::UnknownVersion(format!("invalid dir index: {}", path))
            })?;

            if idx >= dir_index.len() {
                return Err(ManifestError::UnknownVersion(format!(
                    "dir index out of bounds: {}",
                    path
                )));
            }

            let rest: &str = &path[slash_pos + 1..];
            return Ok(format!("{}/{}", dir_index[idx], rest));
        }
    }
    Ok(path.to_string())
}

/// Decode a directory entry from JSON.
fn decode_directory_entry(
    data: &Value,
    dir_index: &[String],
) -> Result<ManifestDirectoryPath, ManifestError> {
    let path: &str = data
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ManifestError::UnknownVersion("directory missing path".to_string()))?;

    let expanded_path: String = expand_path_reference(path, dir_index)?;
    let deleted: bool = data
        .get("deleted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(ManifestDirectoryPath {
        path: expanded_path,
        deleted,
    })
}

/// Decode a file entry from JSON.
fn decode_file_entry(
    data: &Value,
    dir_index: &[String],
) -> Result<ManifestFilePath, ManifestError> {
    let path: &str = data
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ManifestError::UnknownVersion("file missing path".to_string()))?;

    let expanded_path: String = expand_path_reference(path, dir_index)?;

    let hash: Option<String> = data
        .get("hash")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let size: Option<u64> = data.get("size").and_then(|v| v.as_u64());

    let mtime: Option<i64> = data.get("mtime").and_then(|v| v.as_i64());

    let runnable: bool = data
        .get("runnable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let chunkhashes: Option<Vec<String>> = data.get("chunkhashes").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|h| h.as_str().map(|s| s.to_string()))
                .collect()
        })
    });

    let symlink_target: Option<String> = data
        .get("symlink")
        .and_then(|v| v.get("target"))
        .and_then(|v| v.as_str())
        .map(|s| expand_path_reference(s, dir_index))
        .transpose()?;

    let deleted: bool = data
        .get("deleted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(ManifestFilePath {
        path: expanded_path,
        hash,
        size,
        mtime,
        runnable,
        chunkhashes,
        symlink_target,
        deleted,
    })
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
    fn test_decode_v2025_12_rel_snapshot() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "specificationVersion": "relative-manifest-snapshot-beta-2025-12",
            "dirs": [{"path": "subdir"}],
            "files": [
                {"path": "$0/test.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
            ],
            "totalSize": 100
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        assert_eq!(manifest.version(), ManifestVersion::V2025_12);
        assert_eq!(manifest.spec_version(), Some(SpecVersion::REL_SNAPSHOT));
        assert_eq!(manifest.file_count(), 1);

        if let Manifest::V2025_12(m) = manifest {
            assert_eq!(m.files[0].path, "subdir/test.txt");
        }
    }

    #[test]
    fn test_decode_v2025_12_abs_snapshot() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "specificationVersion": "absolute-manifest-snapshot-beta-2025-12",
            "dirs": [],
            "files": [
                {"path": "/home/user/test.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
            ],
            "totalSize": 100
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        assert_eq!(manifest.spec_version(), Some(SpecVersion::ABS_SNAPSHOT));
        assert!(manifest.is_absolute());
    }

    #[test]
    fn test_decode_v2025_12_diff() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "specificationVersion": "relative-manifest-diff-beta-2025-12",
            "dirs": [],
            "files": [
                {"path": "deleted.txt", "deleted": true}
            ],
            "parentManifestHash": "parent_hash_123",
            "totalSize": 0
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        assert_eq!(manifest.spec_version(), Some(SpecVersion::REL_DIFF));

        if let Manifest::V2025_12(m) = manifest {
            assert!(m.is_diff());
            assert_eq!(m.parent_manifest_hash, Some("parent_hash_123".to_string()));
            assert!(m.files[0].deleted);
        }
    }

    #[test]
    fn test_decode_v2025_12_with_chunked_file() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "specificationVersion": "relative-manifest-snapshot-beta-2025-12",
            "dirs": [],
            "files": [
                {"path": "large.bin", "chunkhashes": ["hash1", "hash2"], "size": 314572801, "mtime": 1234567890}
            ],
            "totalSize": 314572801
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2025_12(m) = manifest {
            assert!(m.files[0].chunkhashes.is_some());
            assert_eq!(m.files[0].chunkhashes.as_ref().unwrap().len(), 2);
        }
    }

    #[test]
    fn test_decode_v2025_12_with_symlink() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "specificationVersion": "relative-manifest-snapshot-beta-2025-12",
            "dirs": [],
            "files": [
                {"path": "link.txt", "symlink": {"target": "target.txt"}}
            ],
            "totalSize": 0
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2025_12(m) = manifest {
            assert_eq!(m.files[0].symlink_target, Some("target.txt".to_string()));
        }
    }

    #[test]
    fn test_decode_v2025_12_unhashed_file() {
        let json: &str = r#"{
            "hashAlg": "xxh128",
            "specificationVersion": "relative-manifest-snapshot-beta-2025-12",
            "dirs": [],
            "files": [
                {"path": "test.txt", "size": 100, "mtime": 1234567890}
            ],
            "totalSize": 100
        }"#;

        let manifest: Manifest = decode_manifest(json).unwrap();
        if let Manifest::V2025_12(m) = manifest {
            assert!(m.files[0].hash.is_none());
            assert!(m.files[0].is_unhashed());
        }
    }

    #[test]
    fn test_decode_unknown_version() {
        let json: &str = r#"{"manifestVersion": "1900-01-01"}"#;
        let result: Result<Manifest, ManifestError> = decode_manifest(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_unknown_spec_version() {
        let json: &str = r#"{
            "specificationVersion": "invalid-spec-version",
            "dirs": [],
            "files": [],
            "totalSize": 0
        }"#;
        let result: Result<Manifest, ManifestError> = decode_manifest(json);
        assert!(matches!(result, Err(ManifestError::UnknownSpecVersion(_))));
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
        }
    }
}
