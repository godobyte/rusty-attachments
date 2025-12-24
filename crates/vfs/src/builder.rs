//! Builder for constructing VFS from manifests.

use rusty_attachments_model::{v2023_03_03, v2025_12_04, HashAlgorithm, Manifest};

use crate::inode::{FileContent, INodeManager};

/// Build an INodeManager from a manifest.
///
/// # Arguments
/// * `manifest` - The manifest to build from
///
/// # Returns
/// An INodeManager populated with the manifest entries.
pub fn build_from_manifest(manifest: &Manifest) -> INodeManager {
    match manifest {
        Manifest::V2023_03_03(m) => build_from_v1(m),
        Manifest::V2025_12_04_beta(m) => build_from_v2(m),
    }
}

/// Build INode tree from V1 manifest.
///
/// # Arguments
/// * `manifest` - V1 manifest
///
/// # Returns
/// An INodeManager populated with the manifest entries.
fn build_from_v1(manifest: &v2023_03_03::AssetManifest) -> INodeManager {
    let manager: INodeManager = INodeManager::new();

    for entry in &manifest.paths {
        let content: FileContent = FileContent::SingleHash(entry.hash.clone());
        manager.add_file(
            &entry.path,
            entry.size,
            entry.mtime,
            content,
            manifest.hash_alg,
            false, // V1 doesn't support executable flag
        );
    }

    manager
}

/// Build INode tree from V2 manifest.
///
/// # Arguments
/// * `manifest` - V2 manifest
///
/// # Returns
/// An INodeManager populated with the manifest entries.
fn build_from_v2(manifest: &v2025_12_04::AssetManifest) -> INodeManager {
    let manager: INodeManager = INodeManager::new();

    // Create explicit directories first
    for dir in &manifest.dirs {
        if !dir.deleted {
            manager.add_directory(&dir.path);
        }
    }

    // Process file entries
    for entry in &manifest.paths {
        // Skip deleted entries
        if entry.deleted {
            continue;
        }

        // Handle symlinks
        if let Some(ref target) = entry.symlink_target {
            manager.add_symlink(&entry.path, target);
            continue;
        }

        // Handle chunked files
        if let Some(ref chunkhashes) = entry.chunkhashes {
            let content: FileContent = FileContent::Chunked(chunkhashes.clone());
            manager.add_file(
                &entry.path,
                entry.size.unwrap_or(0),
                entry.mtime.unwrap_or(0),
                content,
                manifest.hash_alg,
                entry.runnable,
            );
            continue;
        }

        // Handle regular files
        if let Some(ref hash) = entry.hash {
            let content: FileContent = FileContent::SingleHash(hash.clone());
            manager.add_file(
                &entry.path,
                entry.size.unwrap_or(0),
                entry.mtime.unwrap_or(0),
                content,
                manifest.hash_alg,
                entry.runnable,
            );
        }
    }

    manager
}

/// Get the hash algorithm from a manifest.
///
/// # Arguments
/// * `manifest` - The manifest
///
/// # Returns
/// The hash algorithm used by the manifest.
pub fn get_hash_algorithm(manifest: &Manifest) -> HashAlgorithm {
    manifest.hash_alg()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inode::{INode, INodeType, ROOT_INODE};
    use std::sync::Arc;

    #[test]
    fn test_build_from_v1() {
        let paths: Vec<v2023_03_03::ManifestPath> = vec![
            v2023_03_03::ManifestPath::new("file1.txt", "hash1", 100, 1000),
            v2023_03_03::ManifestPath::new("dir/file2.txt", "hash2", 200, 2000),
        ];
        let manifest: v2023_03_03::AssetManifest = v2023_03_03::AssetManifest::new(paths);

        let manager: INodeManager = build_from_v1(&manifest);

        // Check root exists
        let root: Arc<dyn INode> = manager.get(ROOT_INODE).unwrap();
        assert_eq!(root.inode_type(), INodeType::Directory);

        // Check files
        let file1: Arc<dyn INode> = manager.get_by_path("file1.txt").unwrap();
        assert_eq!(file1.name(), "file1.txt");
        assert_eq!(file1.size(), 100);
        assert_eq!(file1.inode_type(), INodeType::File);

        let file2: Arc<dyn INode> = manager.get_by_path("dir/file2.txt").unwrap();
        assert_eq!(file2.name(), "file2.txt");
        assert_eq!(file2.size(), 200);

        // Check directory was created
        let dir: Arc<dyn INode> = manager.get_by_path("dir").unwrap();
        assert_eq!(dir.inode_type(), INodeType::Directory);
    }

    #[test]
    fn test_build_from_v2_with_symlink() {
        let dirs: Vec<v2025_12_04::ManifestDirectoryPath> =
            vec![v2025_12_04::ManifestDirectoryPath::new("subdir")];
        let paths: Vec<v2025_12_04::ManifestFilePath> = vec![
            v2025_12_04::ManifestFilePath::file("file.txt", "hash1", 100, 1000),
            v2025_12_04::ManifestFilePath::symlink("link.txt", "file.txt"),
        ];
        let manifest: v2025_12_04::AssetManifest = v2025_12_04::AssetManifest::snapshot(dirs, paths);

        let manager: INodeManager = build_from_v2(&manifest);

        // Check file
        let file: Arc<dyn INode> = manager.get_by_path("file.txt").unwrap();
        assert_eq!(file.inode_type(), INodeType::File);

        // Check symlink
        let link: Arc<dyn INode> = manager.get_by_path("link.txt").unwrap();
        assert_eq!(link.inode_type(), INodeType::Symlink);

        // Check explicit directory
        let subdir: Arc<dyn INode> = manager.get_by_path("subdir").unwrap();
        assert_eq!(subdir.inode_type(), INodeType::Directory);
    }

    #[test]
    fn test_build_from_v2_with_chunked() {
        let chunkhashes: Vec<String> = vec!["chunk1".to_string(), "chunk2".to_string()];
        let size: u64 = 300 * 1024 * 1024; // 300MB
        let paths: Vec<v2025_12_04::ManifestFilePath> =
            vec![v2025_12_04::ManifestFilePath::chunked("large.bin", chunkhashes, size, 1000)];
        let manifest: v2025_12_04::AssetManifest =
            v2025_12_04::AssetManifest::snapshot(vec![], paths);

        let manager: INodeManager = build_from_v2(&manifest);

        let file: Arc<dyn INode> = manager.get_by_path("large.bin").unwrap();
        assert_eq!(file.inode_type(), INodeType::File);
        assert_eq!(file.size(), size);
    }

    #[test]
    fn test_build_from_v2_skips_deleted() {
        let paths: Vec<v2025_12_04::ManifestFilePath> = vec![
            v2025_12_04::ManifestFilePath::file("keep.txt", "hash1", 100, 1000),
            v2025_12_04::ManifestFilePath::deleted("deleted.txt"),
        ];
        let manifest: v2025_12_04::AssetManifest =
            v2025_12_04::AssetManifest::diff(vec![], paths, "parent_hash");

        let manager: INodeManager = build_from_v2(&manifest);

        assert!(manager.get_by_path("keep.txt").is_some());
        assert!(manager.get_by_path("deleted.txt").is_none());
    }
}
