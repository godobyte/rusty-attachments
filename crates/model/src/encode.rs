//! Canonical JSON encoding for manifests.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::error::ManifestError;
use crate::v2025_12_04::{AssetManifest, ManifestDirectoryPath, ManifestFilePath};
use crate::version::ManifestType;

/// Encode a v2025-12-04-beta manifest to canonical JSON with directory compression.
pub fn encode_v2025_12_04(manifest: &AssetManifest) -> Result<String, ManifestError> {
    // Sort and deduplicate directories by full path
    let mut unique_dirs: Vec<&ManifestDirectoryPath> = Vec::new();
    let mut seen_paths: HashMap<&str, usize> = HashMap::new();

    let mut sorted_dirs: Vec<_> = manifest.dirs.iter().collect();
    sorted_dirs.sort_by(|a, b| a.path.cmp(&b.path));

    for dir in sorted_dirs {
        if !seen_paths.contains_key(dir.path.as_str()) {
            seen_paths.insert(&dir.path, unique_dirs.len());
            unique_dirs.push(dir);
        }
    }

    // Build directory index: path -> index
    let dir_index: HashMap<&str, usize> = unique_dirs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.path.as_str(), i))
        .collect();

    // Encode directories with $N/ compression
    let dirs_json: Vec<Value> = unique_dirs
        .iter()
        .map(|d| {
            let encoded_name = encode_path_with_dir_index(&d.path, &dir_index);
            let mut entry = json!({"name": encoded_name});
            if d.deleted {
                entry["delete"] = json!(true);
            }
            entry
        })
        .collect();

    // Sort files by UTF-16 BE encoding
    let mut sorted_files: Vec<_> = manifest.paths.iter().collect();
    sorted_files.sort_by(|a, b| {
        let a_bytes: Vec<u16> = a.path.encode_utf16().collect();
        let b_bytes: Vec<u16> = b.path.encode_utf16().collect();
        a_bytes.cmp(&b_bytes)
    });

    // Encode files with $N/ compression
    let files_json: Vec<Value> = sorted_files
        .iter()
        .map(|f| encode_file_entry(f, &dir_index))
        .collect();

    // Build manifest dict
    let mut manifest_dict = json!({
        "dirs": dirs_json,
        "files": files_json,
        "hashAlg": manifest.hash_alg,
        "manifestVersion": manifest.manifest_version,
        "totalSize": manifest.total_size,
    });

    if manifest.manifest_type == ManifestType::Diff {
        if let Some(ref parent_hash) = manifest.parent_manifest_hash {
            manifest_dict["parentManifestHash"] = json!(parent_hash);
        }
    }

    Ok(serde_json::to_string(&manifest_dict)?)
}

/// Encode a path using directory index compression ($N/ references).
fn encode_path_with_dir_index(path: &str, dir_index: &HashMap<&str, usize>) -> String {
    if let Some(last_slash) = path.rfind('/') {
        let dir_path = &path[..last_slash];
        let name = &path[last_slash + 1..];

        if let Some(&idx) = dir_index.get(dir_path) {
            return format!("${}/{}", idx, name);
        }
    }
    path.to_string()
}

/// Encode a file entry to JSON with directory compression.
fn encode_file_entry(file: &ManifestFilePath, dir_index: &HashMap<&str, usize>) -> Value {
    let encoded_name = encode_path_with_dir_index(&file.path, dir_index);
    let mut entry = json!({"name": encoded_name});

    // Add content field (exactly one of: hash, chunkhashes, symlink)
    if let Some(ref hash) = file.hash {
        entry["hash"] = json!(hash);
    } else if let Some(ref chunks) = file.chunkhashes {
        entry["chunkhashes"] = json!(chunks);
    } else if let Some(ref target) = file.symlink_target {
        let encoded_target = encode_path_with_dir_index(target, dir_index);
        entry["symlink"] = json!({"name": encoded_target});
    }

    // Add metadata (only for non-deleted, non-symlink entries)
    if !file.deleted && file.symlink_target.is_none() {
        if let Some(size) = file.size {
            entry["size"] = json!(size);
        }
        if let Some(mtime) = file.mtime {
            entry["mtime"] = json!(mtime);
        }
        if file.runnable {
            entry["runnable"] = json!(true);
        }
    }

    if file.deleted {
        entry["delete"] = json!(true);
    }

    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2025_12_04::{ManifestDirectoryPath, ManifestFilePath};

    #[test]
    fn test_encode_path_with_dir_index() {
        let mut dir_index = HashMap::new();
        dir_index.insert("subdir", 0);
        dir_index.insert("subdir/nested", 1);

        assert_eq!(
            encode_path_with_dir_index("subdir/file.txt", &dir_index),
            "$0/file.txt"
        );
        assert_eq!(
            encode_path_with_dir_index("subdir/nested/deep.txt", &dir_index),
            "$1/deep.txt"
        );
        assert_eq!(
            encode_path_with_dir_index("root.txt", &dir_index),
            "root.txt"
        );
    }

    #[test]
    fn test_encode_manifest() {
        let dirs = vec![ManifestDirectoryPath::new("subdir")];
        let paths = vec![ManifestFilePath::file(
            "subdir/test.txt",
            "abc123",
            100,
            1234567890,
        )];
        let manifest = AssetManifest::snapshot(dirs, paths);

        let encoded = encode_v2025_12_04(&manifest).unwrap();
        assert!(encoded.contains("$0/test.txt"));
    }
}
