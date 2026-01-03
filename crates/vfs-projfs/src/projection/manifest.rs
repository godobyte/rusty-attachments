//! Manifest projection - in-memory tree from manifest.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use rusty_attachments_model::{HashAlgorithm, Manifest};

use crate::error::ProjFsError;
use crate::projection::folder::FolderData;
use crate::projection::types::{ContentHash, FileData, ProjectedFileInfo, SymlinkData};

/// In-memory projection of manifest contents.
///
/// Builds a tree structure from the manifest for fast enumeration and lookup.
/// All data is kept in memory - manifest files are typically small enough
/// that this is practical.
pub struct ManifestProjection {
    /// Root folder data.
    root: RwLock<FolderData>,
    /// Hash algorithm used by manifest.
    hash_algorithm: HashAlgorithm,
    /// Path to folder data cache for fast lookup.
    path_cache: RwLock<HashMap<String, Arc<[ProjectedFileInfo]>>>,
}

impl ManifestProjection {
    /// Build projection from manifest.
    ///
    /// # Arguments
    /// * `manifest` - Manifest to project (V1 or V2)
    ///
    /// # Returns
    /// New projection with sorted entries.
    pub fn from_manifest(manifest: &Manifest) -> Result<Self, ProjFsError> {
        let hash_algorithm: HashAlgorithm = manifest.hash_alg();

        let mut root = FolderData::root();

        match manifest {
            Manifest::V2023_03_03(m) => {
                Self::build_from_v1(&mut root, m)?;
            }
            Manifest::V2025_12_04_beta(m) => {
                Self::build_from_v2(&mut root, m)?;
            }
        }

        // Sort all entries
        root.sort_all();

        Ok(Self {
            root: RwLock::new(root),
            hash_algorithm,
            path_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Build projection from V1 manifest.
    ///
    /// # Arguments
    /// * `root` - Root folder to populate
    /// * `manifest` - V1 manifest
    fn build_from_v1(
        root: &mut FolderData,
        manifest: &rusty_attachments_model::v2023_03_03::AssetManifest,
    ) -> Result<(), ProjFsError> {
        for entry in &manifest.paths {
            let path: &str = &entry.path;
            let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

            if components.is_empty() {
                continue;
            }

            // Navigate to parent folder, creating as needed
            let mut current_folder: &mut FolderData = root;
            for component in &components[..components.len() - 1] {
                current_folder = current_folder.get_or_create_subfolder(component);
            }

            // Add file to parent folder
            let file_name: &str = components[components.len() - 1];
            let mtime: SystemTime = UNIX_EPOCH + Duration::from_secs(entry.mtime as u64);

            current_folder.add_file(FileData {
                name: file_name.to_string(),
                size: entry.size,
                content_hash: ContentHash::Single(entry.hash.clone()),
                mtime,
                executable: false, // V1 doesn't track executable
            });
        }

        Ok(())
    }

    /// Build projection from V2 manifest.
    ///
    /// # Arguments
    /// * `root` - Root folder to populate
    /// * `manifest` - V2 manifest
    fn build_from_v2(
        root: &mut FolderData,
        manifest: &rusty_attachments_model::v2025_12_04::AssetManifest,
    ) -> Result<(), ProjFsError> {
        // Create explicit directories first
        for dir in &manifest.dirs {
            if dir.delete {
                continue;
            }

            let path: &str = &dir.name;
            let components: Vec<&str> = path.split('/').filter(|s: &&str| !s.is_empty()).collect();

            if components.is_empty() {
                continue;
            }

            // Navigate to parent, creating as needed
            let mut current_folder: &mut FolderData = root;
            for component in &components {
                current_folder = current_folder.get_or_create_subfolder(component);
            }
        }

        // Add files and symlinks
        for entry in &manifest.files {
            if entry.delete {
                continue;
            }

            let path: &str = &entry.name;
            let components: Vec<&str> = path.split('/').filter(|s: &&str| !s.is_empty()).collect();

            if components.is_empty() {
                continue;
            }

            // Navigate to parent folder
            let mut current_folder: &mut FolderData = root;
            for component in &components[..components.len() - 1] {
                current_folder = current_folder.get_or_create_subfolder(component);
            }

            let file_name: &str = components[components.len() - 1];
            let mtime: SystemTime = UNIX_EPOCH + Duration::from_secs(
                entry.mtime.unwrap_or(0).max(0) as u64
            );

            // Check if symlink
            if let Some(ref target) = entry.symlink_target {
                current_folder.add_symlink(SymlinkData {
                    name: file_name.to_string(),
                    target: target.clone(),
                });
            } else {
                // Regular file
                let content_hash: ContentHash = if let Some(ref chunks) = entry.chunkhashes {
                    ContentHash::Chunked(chunks.clone())
                } else {
                    ContentHash::Single(entry.hash.clone().unwrap_or_default())
                };

                current_folder.add_file(FileData {
                    name: file_name.to_string(),
                    size: entry.size.unwrap_or(0),
                    content_hash,
                    mtime,
                    executable: entry.runnable,
                });
            }
        }

        Ok(())
    }

    /// Get projected items for a folder path.
    ///
    /// Fast path - all data is in memory.
    ///
    /// # Arguments
    /// * `relative_path` - Path relative to virtualization root
    ///
    /// # Returns
    /// Arc-wrapped slice of projected file info (shared, no cloning).
    pub fn get_projected_items(&self, relative_path: &str) -> Option<Arc<[ProjectedFileInfo]>> {
        // Normalize path
        let normalized: String = Self::normalize_path(relative_path);

        // Check cache first
        {
            let cache = self.path_cache.read();
            if let Some(items) = cache.get(&normalized) {
                return Some(Arc::clone(items));
            }
        }

        // Navigate to folder
        let folder: Option<Arc<[ProjectedFileInfo]>> = {
            let root = self.root.read();
            let target_folder: &FolderData = if normalized.is_empty() {
                &root
            } else {
                Self::navigate_to_folder(&root, &normalized)?
            };

            Some(target_folder.children().to_projected_info())
        };

        // Cache result
        if let Some(ref items) = folder {
            self.path_cache
                .write()
                .insert(normalized, Arc::clone(items));
        }

        folder
    }

    /// Check if a path is projected and get its info.
    ///
    /// # Arguments
    /// * `relative_path` - Path to check
    ///
    /// # Returns
    /// (canonical_name, is_folder) if path exists in projection.
    pub fn is_path_projected(&self, relative_path: &str) -> Option<(String, bool)> {
        let normalized: String = Self::normalize_path(relative_path);

        if normalized.is_empty() {
            // Root directory
            return Some((String::new(), true));
        }

        let root = self.root.read();
        let components: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();

        if components.is_empty() {
            return Some((String::new(), true));
        }

        // Navigate to parent
        let parent_folder: &FolderData = if components.len() == 1 {
            &root
        } else {
            let parent_path: String = components[..components.len() - 1].join("/");
            Self::navigate_to_folder(&root, &parent_path)?
        };

        // Find child
        let child_name: &str = components[components.len() - 1];
        let entry = parent_folder.find_child(child_name)?;

        Some((entry.name().to_string(), entry.is_folder()))
    }

    /// Get file info for placeholder creation.
    ///
    /// # Arguments
    /// * `relative_path` - Path to file
    ///
    /// # Returns
    /// File info including size and content hash.
    pub fn get_file_info(&self, relative_path: &str) -> Option<ProjectedFileInfo> {
        let normalized: String = Self::normalize_path(relative_path);
        let root = self.root.read();
        let components: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();

        if components.is_empty() {
            return None;
        }

        // Navigate to parent
        let parent_folder: &FolderData = if components.len() == 1 {
            &root
        } else {
            let parent_path: String = components[..components.len() - 1].join("/");
            Self::navigate_to_folder(&root, &parent_path)?
        };

        // Find file
        let file_name: &str = components[components.len() - 1];
        let entry = parent_folder.find_child(file_name)?;

        match entry {
            crate::projection::types::FolderEntry::File(f) => Some(ProjectedFileInfo::file(
                f.name.clone(),
                f.size,
                f.content_hash.primary_hash().to_string(),
                f.mtime,
                f.executable,
            )),
            crate::projection::types::FolderEntry::Symlink(s) => {
                Some(ProjectedFileInfo::symlink(s.name.clone(), s.target.clone()))
            }
            _ => None,
        }
    }

    /// Get hash algorithm used by manifest.
    pub fn hash_algorithm(&self) -> HashAlgorithm {
        self.hash_algorithm
    }

    /// Get content hash info for a file (including chunk hashes for V2 chunked files).
    ///
    /// # Arguments
    /// * `relative_path` - Path to file
    ///
    /// # Returns
    /// ContentHash (Single or Chunked) if file exists.
    pub fn get_content_hash(&self, relative_path: &str) -> Option<ContentHash> {
        let normalized: String = Self::normalize_path(relative_path);
        let root = self.root.read();
        let components: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();

        if components.is_empty() {
            return None;
        }

        // Navigate to parent
        let parent_folder: &FolderData = if components.len() == 1 {
            &root
        } else {
            let parent_path: String = components[..components.len() - 1].join("/");
            Self::navigate_to_folder(&root, &parent_path)?
        };

        // Find file
        let file_name: &str = components[components.len() - 1];
        let entry = parent_folder.find_child(file_name)?;

        match entry {
            crate::projection::types::FolderEntry::File(f) => Some(f.content_hash.clone()),
            _ => None,
        }
    }

    /// Get reference to root folder for iteration.
    ///
    /// # Returns
    /// Reference to the root folder data.
    pub fn root(&self) -> parking_lot::RwLockReadGuard<'_, FolderData> {
        self.root.read()
    }

    /// Normalize path (remove leading/trailing slashes, convert backslashes).
    ///
    /// # Arguments
    /// * `path` - Path to normalize
    ///
    /// # Returns
    /// Normalized path.
    fn normalize_path(path: &str) -> String {
        path.trim_matches('/')
            .trim_matches('\\')
            .replace('\\', "/")
    }

    /// Navigate to a folder by path.
    ///
    /// # Arguments
    /// * `root` - Root folder
    /// * `path` - Path to navigate to
    ///
    /// # Returns
    /// Reference to target folder if found.
    fn navigate_to_folder<'a>(root: &'a FolderData, path: &str) -> Option<&'a FolderData> {
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        let mut current: &FolderData = root;
        for component in components {
            let entry: &crate::projection::types::FolderEntry = current.find_child(component)?;
            match entry {
                crate::projection::types::FolderEntry::Folder(f) => {
                    current = f.as_ref();
                }
                _ => return None, // Not a folder
            }
        }

        Some(current)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use rusty_attachments_model::v2023_03_03::{AssetManifest as ManifestV1, ManifestPath};
    use rusty_attachments_model::ManifestVersion;

    fn create_test_manifest_v1() -> Manifest {
        let manifest = ManifestV1 {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2023_03_03,
            total_size: 300,
            paths: vec![
                ManifestPath {
                    path: "file1.txt".to_string(),
                    hash: "hash1".to_string(),
                    size: 100,
                    mtime: 1000,
                },
                ManifestPath {
                    path: "dir1/file2.txt".to_string(),
                    hash: "hash2".to_string(),
                    size: 200,
                    mtime: 2000,
                },
            ],
        };
        Manifest::V2023_03_03(manifest)
    }

    #[test]
    fn test_manifest_projection_from_v1() {
        let manifest = create_test_manifest_v1();
        let projection = ManifestProjection::from_manifest(&manifest).unwrap();

        // Check root items
        let root_items: Arc<[ProjectedFileInfo]> = projection.get_projected_items("").unwrap();
        assert_eq!(root_items.len(), 2); // file1.txt and dir1

        // Check dir1 items
        let dir1_items: Arc<[ProjectedFileInfo]> = projection.get_projected_items("dir1").unwrap();
        assert_eq!(dir1_items.len(), 1);
        assert_eq!(dir1_items[0].name.as_ref(), "file2.txt");
    }

    #[test]
    fn test_is_path_projected() {
        let manifest = create_test_manifest_v1();
        let projection = ManifestProjection::from_manifest(&manifest).unwrap();

        // Root exists
        assert!(projection.is_path_projected("").is_some());

        // File exists
        let (name, is_folder) = projection.is_path_projected("file1.txt").unwrap();
        assert_eq!(name, "file1.txt");
        assert!(!is_folder);

        // Directory exists
        let (name, is_folder) = projection.is_path_projected("dir1").unwrap();
        assert_eq!(name, "dir1");
        assert!(is_folder);

        // Nested file exists
        let (name, is_folder) = projection.is_path_projected("dir1/file2.txt").unwrap();
        assert_eq!(name, "file2.txt");
        assert!(!is_folder);

        // Non-existent path
        assert!(projection.is_path_projected("nonexistent").is_none());
    }

    #[test]
    fn test_get_file_info() {
        let manifest = create_test_manifest_v1();
        let projection = ManifestProjection::from_manifest(&manifest).unwrap();

        let info: ProjectedFileInfo = projection.get_file_info("file1.txt").unwrap();
        assert_eq!(info.name.as_ref(), "file1.txt");
        assert_eq!(info.size, 100);
        assert!(!info.is_folder);
        assert!(info.content_hash.is_some());

        // Non-existent file
        assert!(projection.get_file_info("nonexistent.txt").is_none());

        // Directory (not a file)
        assert!(projection.get_file_info("dir1").is_none());
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(ManifestProjection::normalize_path(""), "");
        assert_eq!(ManifestProjection::normalize_path("/"), "");
        assert_eq!(ManifestProjection::normalize_path("\\"), "");
        assert_eq!(ManifestProjection::normalize_path("/path/to/file"), "path/to/file");
        assert_eq!(ManifestProjection::normalize_path("path\\to\\file"), "path/to/file");
        assert_eq!(ManifestProjection::normalize_path("/path/to/file/"), "path/to/file");
    }

    #[test]
    fn test_path_cache() {
        let manifest = create_test_manifest_v1();
        let projection = ManifestProjection::from_manifest(&manifest).unwrap();

        // First access - populates cache
        let items1: Arc<[ProjectedFileInfo]> = projection.get_projected_items("").unwrap();

        // Second access - should hit cache
        let items2: Arc<[ProjectedFileInfo]> = projection.get_projected_items("").unwrap();

        // Should be the same Arc (pointer equality)
        assert!(Arc::ptr_eq(&items1, &items2));
    }

    #[test]
    fn test_case_insensitive_lookup() {
        let manifest = create_test_manifest_v1();
        let projection = ManifestProjection::from_manifest(&manifest).unwrap();

        // Original case
        assert!(projection.is_path_projected("file1.txt").is_some());

        // Different case
        assert!(projection.is_path_projected("FILE1.TXT").is_some());
        assert!(projection.is_path_projected("File1.Txt").is_some());
    }

    #[test]
    fn test_deeply_nested_paths() {
        let manifest = ManifestV1 {
            hash_alg: HashAlgorithm::Xxh128,
            manifest_version: ManifestVersion::V2023_03_03,
            total_size: 100,
            paths: vec![ManifestPath {
                path: "a/b/c/d/e/file.txt".to_string(),
                hash: "hash".to_string(),
                size: 100,
                mtime: 1000,
            }],
        };
        let manifest = Manifest::V2023_03_03(manifest);
        let projection = ManifestProjection::from_manifest(&manifest).unwrap();

        // Check each level exists
        assert!(projection.is_path_projected("a").is_some());
        assert!(projection.is_path_projected("a/b").is_some());
        assert!(projection.is_path_projected("a/b/c").is_some());
        assert!(projection.is_path_projected("a/b/c/d").is_some());
        assert!(projection.is_path_projected("a/b/c/d/e").is_some());
        assert!(projection.is_path_projected("a/b/c/d/e/file.txt").is_some());
    }

    #[test]
    fn test_get_content_hash_single() {
        let manifest = create_test_manifest_v1();
        let projection = ManifestProjection::from_manifest(&manifest).unwrap();

        let hash = projection.get_content_hash("file1.txt").unwrap();
        assert!(matches!(hash, ContentHash::Single(h) if h == "hash1"));

        // Non-existent file
        assert!(projection.get_content_hash("nonexistent.txt").is_none());

        // Directory (not a file)
        assert!(projection.get_content_hash("dir1").is_none());
    }

    #[test]
    fn test_get_content_hash_chunked_v2() {
        use rusty_attachments_model::v2025_12_04::{AssetManifest as ManifestV2, ManifestFilePath};

        let chunked_file = ManifestFilePath::chunked(
            "large_file.bin",
            vec![
                "chunk0_hash".to_string(),
                "chunk1_hash".to_string(),
                "chunk2_hash".to_string(),
            ],
            600 * 1024 * 1024, // 600MB
            1000,
        );

        let manifest = ManifestV2::snapshot(vec![], vec![chunked_file]);
        let manifest = Manifest::V2025_12_04_beta(manifest);
        let projection = ManifestProjection::from_manifest(&manifest).unwrap();

        let hash = projection.get_content_hash("large_file.bin").unwrap();
        match hash {
            ContentHash::Chunked(hashes) => {
                assert_eq!(hashes.len(), 3);
                assert_eq!(hashes[0], "chunk0_hash");
                assert_eq!(hashes[1], "chunk1_hash");
                assert_eq!(hashes[2], "chunk2_hash");
            }
            ContentHash::Single(_) => panic!("Expected chunked hash"),
        }
    }
}
