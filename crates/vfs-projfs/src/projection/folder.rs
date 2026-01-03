//! Folder data structures with ProjFS sorting.

use std::sync::Arc;

use crate::projection::types::{FolderEntry, ProjectedFileInfo};
use crate::util::prj_file_name_compare;

/// Sorted collection of folder entries.
///
/// Maintains entries in ProjFS sort order for efficient enumeration.
#[derive(Clone, Debug)]
pub struct SortedFolderEntries {
    /// Entries sorted by name using PrjFileNameCompare order.
    entries: Vec<FolderEntry>,
}

impl SortedFolderEntries {
    /// Create empty sorted entries.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add an entry and maintain sort order.
    ///
    /// # Arguments
    /// * `entry` - Entry to add
    pub fn add(&mut self, entry: FolderEntry) {
        self.entries.push(entry);
    }

    /// Sort all entries using ProjFS collation.
    pub fn sort(&mut self) {
        self.entries
            .sort_by(|a, b| prj_file_name_compare(a.name(), b.name()));
    }

    /// Get entries as slice.
    pub fn entries(&self) -> &[FolderEntry] {
        &self.entries
    }

    /// Get entry count.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Convert to ProjectedFileInfo for enumeration.
    ///
    /// # Returns
    /// Arc-wrapped slice of projected file info (shared, no cloning).
    pub fn to_projected_info(&self) -> Arc<[ProjectedFileInfo]> {
        let infos: Vec<ProjectedFileInfo> = self
            .entries
            .iter()
            .map(|entry| match entry {
                FolderEntry::File(f) => ProjectedFileInfo::file(
                    f.name.clone(),
                    f.size,
                    f.content_hash.primary_hash().to_string(),
                    f.mtime,
                    f.executable,
                ),
                FolderEntry::Folder(f) => ProjectedFileInfo::folder(f.name().to_string()),
                FolderEntry::Symlink(s) => ProjectedFileInfo::symlink(s.name.clone(), s.target.clone()),
            })
            .collect();

        Arc::from(infos.into_boxed_slice())
    }
}

impl Default for SortedFolderEntries {
    fn default() -> Self {
        Self::new()
    }
}

/// Data about a folder in the projection.
#[derive(Clone, Debug)]
pub struct FolderData {
    /// Folder name (empty for root).
    name: String,
    /// Sorted child entries.
    children: SortedFolderEntries,
    /// Whether this folder is included in projection.
    #[allow(dead_code)]
    is_included: bool,
}

impl FolderData {
    /// Create a new folder.
    ///
    /// # Arguments
    /// * `name` - Folder name (empty for root)
    pub fn new(name: String) -> Self {
        Self {
            name,
            children: SortedFolderEntries::new(),
            is_included: true,
        }
    }

    /// Create root folder.
    pub fn root() -> Self {
        Self::new(String::new())
    }

    /// Get folder name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get children.
    pub fn children(&self) -> &SortedFolderEntries {
        &self.children
    }

    /// Get mutable children.
    #[allow(dead_code)]
    pub fn children_mut(&mut self) -> &mut SortedFolderEntries {
        &mut self.children
    }

    /// Check if included in projection.
    #[allow(dead_code)]
    pub fn is_included(&self) -> bool {
        self.is_included
    }

    /// Sort all children recursively.
    pub fn sort_all(&mut self) {
        self.children.sort();

        // Recursively sort subfolders
        for entry in &mut self.children.entries {
            if let FolderEntry::Folder(ref mut folder) = entry {
                folder.sort_all();
            }
        }
    }

    /// Find a child by name (case-insensitive).
    ///
    /// # Arguments
    /// * `name` - Child name to find
    ///
    /// # Returns
    /// Reference to child entry if found.
    pub fn find_child(&self, name: &str) -> Option<&FolderEntry> {
        self.children
            .entries()
            .iter()
            .find(|e| e.name().eq_ignore_ascii_case(name))
    }

    /// Get or create a subfolder by name.
    ///
    /// # Arguments
    /// * `name` - Subfolder name
    ///
    /// # Returns
    /// Mutable reference to the subfolder.
    pub fn get_or_create_subfolder(&mut self, name: &str) -> &mut FolderData {
        // Check if folder already exists
        let existing_idx: Option<usize> = self
            .children
            .entries
            .iter()
            .position(|e| matches!(e, FolderEntry::Folder(f) if f.name().eq_ignore_ascii_case(name)));

        let idx: usize = match existing_idx {
            Some(idx) => idx,
            None => {
                // Create new folder
                let new_folder = FolderData::new(name.to_string());
                self.children.add(FolderEntry::Folder(Box::new(new_folder)));
                self.children.entries.len() - 1
            }
        };

        // Return reference to the folder at idx
        if let FolderEntry::Folder(ref mut folder) = self.children.entries[idx] {
            folder.as_mut()
        } else {
            unreachable!("Expected folder at index")
        }
    }

    /// Add a file to this folder.
    ///
    /// # Arguments
    /// * `file` - File data to add
    pub fn add_file(&mut self, file: crate::projection::types::FileData) {
        self.children.add(FolderEntry::File(file));
    }

    /// Add a symlink to this folder.
    ///
    /// # Arguments
    /// * `symlink` - Symlink data to add
    pub fn add_symlink(&mut self, symlink: crate::projection::types::SymlinkData) {
        self.children.add(FolderEntry::Symlink(symlink));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::types::{ContentHash, FileData};
    use std::time::SystemTime;

    #[test]
    fn test_sorted_folder_entries_empty() {
        let entries = SortedFolderEntries::new();
        assert!(entries.is_empty());
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_sorted_folder_entries_sort() {
        let mut entries = SortedFolderEntries::new();

        entries.add(FolderEntry::File(FileData {
            name: "zebra.txt".to_string(),
            size: 100,
            content_hash: ContentHash::Single("hash1".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        }));

        entries.add(FolderEntry::File(FileData {
            name: "apple.txt".to_string(),
            size: 200,
            content_hash: ContentHash::Single("hash2".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        }));

        entries.sort();

        assert_eq!(entries.entries()[0].name(), "apple.txt");
        assert_eq!(entries.entries()[1].name(), "zebra.txt");
    }

    #[test]
    fn test_folder_data_root() {
        let root = FolderData::root();
        assert_eq!(root.name(), "");
        assert!(root.is_included());
        assert!(root.children().is_empty());
    }

    #[test]
    fn test_folder_data_add_file() {
        let mut folder = FolderData::new("test".to_string());

        folder.add_file(FileData {
            name: "file.txt".to_string(),
            size: 100,
            content_hash: ContentHash::Single("hash".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        });

        assert_eq!(folder.children().len(), 1);
        assert_eq!(folder.children().entries()[0].name(), "file.txt");
    }

    #[test]
    fn test_folder_data_get_or_create_subfolder() {
        let mut folder = FolderData::new("parent".to_string());

        let subfolder1 = folder.get_or_create_subfolder("child");
        subfolder1.add_file(FileData {
            name: "file.txt".to_string(),
            size: 100,
            content_hash: ContentHash::Single("hash".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        });

        // Get same subfolder again
        let subfolder2 = folder.get_or_create_subfolder("child");
        assert_eq!(subfolder2.children().len(), 1);

        // Should only have one child folder
        assert_eq!(folder.children().len(), 1);
    }

    #[test]
    fn test_folder_data_find_child() {
        let mut folder = FolderData::new("test".to_string());

        folder.add_file(FileData {
            name: "file.txt".to_string(),
            size: 100,
            content_hash: ContentHash::Single("hash".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        });

        assert!(folder.find_child("file.txt").is_some());
        assert!(folder.find_child("FILE.TXT").is_some()); // Case-insensitive
        assert!(folder.find_child("nonexistent.txt").is_none());
    }

    #[test]
    fn test_folder_data_sort_all() {
        let mut root = FolderData::root();

        // Add files in unsorted order
        root.add_file(FileData {
            name: "z.txt".to_string(),
            size: 100,
            content_hash: ContentHash::Single("hash1".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        });

        root.add_file(FileData {
            name: "a.txt".to_string(),
            size: 200,
            content_hash: ContentHash::Single("hash2".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        });

        // Add subfolder with unsorted files
        let subfolder = root.get_or_create_subfolder("subdir");
        subfolder.add_file(FileData {
            name: "y.txt".to_string(),
            size: 300,
            content_hash: ContentHash::Single("hash3".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        });

        subfolder.add_file(FileData {
            name: "b.txt".to_string(),
            size: 400,
            content_hash: ContentHash::Single("hash4".to_string()),
            mtime: SystemTime::UNIX_EPOCH,
            executable: false,
        });

        // Sort all
        root.sort_all();

        // Check root is sorted
        let root_entries: &[FolderEntry] = root.children().entries();
        assert_eq!(root_entries[0].name(), "a.txt");
        assert_eq!(root_entries[1].name(), "subdir");
        assert_eq!(root_entries[2].name(), "z.txt");

        // Check subfolder is sorted
        if let FolderEntry::Folder(ref subfolder) = root_entries[1] {
            let sub_entries: &[FolderEntry] = subfolder.children().entries();
            assert_eq!(sub_entries[0].name(), "b.txt");
            assert_eq!(sub_entries[1].name(), "y.txt");
        } else {
            panic!("Expected folder");
        }
    }

    #[test]
    fn test_content_hash_single() {
        let hash = ContentHash::Single("abc123".to_string());
        assert_eq!(hash.primary_hash(), "abc123");
        assert!(!hash.is_chunked());
        assert_eq!(hash.chunk_count(), 1);
    }

    #[test]
    fn test_content_hash_chunked() {
        let hash = ContentHash::Chunked(vec![
            "chunk1".to_string(),
            "chunk2".to_string(),
            "chunk3".to_string(),
        ]);
        assert_eq!(hash.primary_hash(), "chunk1");
        assert!(hash.is_chunked());
        assert_eq!(hash.chunk_count(), 3);
    }

    #[test]
    fn test_projected_file_info_file() {
        let info = ProjectedFileInfo::file(
            "test.txt".to_string(),
            1024,
            "hash123".to_string(),
            SystemTime::UNIX_EPOCH,
            false,
        );

        assert_eq!(info.name.as_ref(), "test.txt");
        assert_eq!(info.size, 1024);
        assert!(!info.is_folder);
        assert!(info.content_hash.is_some());
        assert!(info.symlink_target.is_none());
    }

    #[test]
    fn test_projected_file_info_folder() {
        let info = ProjectedFileInfo::folder("mydir".to_string());

        assert_eq!(info.name.as_ref(), "mydir");
        assert_eq!(info.size, 0);
        assert!(info.is_folder);
        assert!(info.content_hash.is_none());
        assert!(info.symlink_target.is_none());
    }

    #[test]
    fn test_projected_file_info_symlink() {
        let info = ProjectedFileInfo::symlink("link".to_string(), "/target/path".to_string());

        assert_eq!(info.name.as_ref(), "link");
        assert!(!info.is_folder);
        assert!(info.content_hash.is_none());
        assert_eq!(info.symlink_target.as_ref().unwrap().as_ref(), "/target/path");
    }
}
