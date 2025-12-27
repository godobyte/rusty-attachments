# Rusty Attachments: VFS Directory Operations Design

**Status: ✅ IMPLEMENTED**

## Overview

This document extends the writable VFS design with support for directory creation (`mkdir`) and deletion (`rmdir`). These operations integrate with the existing dirty file tracking and diff manifest export systems.

## Goals

1. **Create directories**: Support `mkdir` FUSE operation for new directories
2. **Delete empty directories**: Support `rmdir` FUSE operation for empty directories
3. **Diff manifest integration**: Track directory changes for export
4. **Consistent semantics**: Follow POSIX directory semantics

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Layer 3: FUSE Interface                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  WritableVfs (fuser::Filesystem)                                    │    │
│  │    - mkdir() → create directory in dirty layer                      │    │
│  │    - rmdir() → mark directory deleted in dirty layer                │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Layer 2: Write Layer                                 │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  DirtyDirManager (NEW)                                              │    │
│  │    - dirty_dirs: HashMap<INodeId, DirtyDir>                         │    │
│  │    - create_dir() → create new directory                            │    │
│  │    - delete_dir() → mark directory deleted                          │    │
│  │    - get_dirty_dir_entries() → for diff manifest                    │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  DirtyFileManager (existing)                                        │    │
│  │    - dirty_files: HashMap<INodeId, DirtyFile>                       │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Layer 1: INode Layer                                 │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  INodeManager (extended)                                            │    │
│  │    - remove_child() → remove child from parent directory            │    │
│  │    - remove_inode() → remove inode from maps                        │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Project Structure Changes

```
crates/vfs/src/
├── write/
│   ├── mod.rs            # Add: DirtyDirManager export
│   ├── dirty.rs          # Existing: DirtyFileManager
│   ├── dirty_dir.rs      # NEW: DirtyDir, DirtyDirManager
│   └── ...
│
├── inode/
│   ├── manager.rs        # Extend: remove_child(), remove_inode()
│   └── dir.rs            # Extend: remove_child()
│
└── fuse_writable.rs      # Extend: mkdir(), rmdir() implementations
```

---

## Core Types

### DirtyDir

Represents a directory that has been created or deleted.

```rust
/// State of a dirty directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyDirState {
    /// Directory is newly created (not in original manifest).
    New,
    /// Directory has been deleted.
    Deleted,
}

/// A directory that has been created or deleted.
///
/// Unlike files, directories don't have content to track.
/// We only need to track their existence state for diff manifests.
#[derive(Debug)]
pub struct DirtyDir {
    /// Inode ID of this directory.
    inode_id: INodeId,
    /// Relative path within the VFS.
    rel_path: String,
    /// Current state of the directory.
    state: DirtyDirState,
    /// Modification time.
    mtime: SystemTime,
}

impl DirtyDir {
    /// Create a new dirty directory entry.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID of the directory
    /// * `rel_path` - Relative path within VFS
    /// * `state` - Initial state (New or Deleted)
    pub fn new(inode_id: INodeId, rel_path: String, state: DirtyDirState) -> Self {
        Self {
            inode_id,
            rel_path,
            state,
            mtime: SystemTime::now(),
        }
    }

    /// Get the inode ID.
    pub fn inode_id(&self) -> INodeId {
        self.inode_id
    }

    /// Get relative path.
    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }

    /// Get current state.
    pub fn state(&self) -> DirtyDirState {
        self.state
    }

    /// Get modification time.
    pub fn mtime(&self) -> SystemTime {
        self.mtime
    }

    /// Mark directory as deleted.
    pub fn mark_deleted(&mut self) {
        self.state = DirtyDirState::Deleted;
        self.mtime = SystemTime::now();
    }
}
```

### DirtyDirEntry

Summary for diff manifest export.

```rust
/// Summary of a dirty directory for export.
#[derive(Debug, Clone)]
pub struct DirtyDirEntry {
    /// Relative path.
    pub path: String,
    /// Current state.
    pub state: DirtyDirState,
}
```

---

### DirtyDirManager

Manages dirty directories.

```rust
/// Manages dirty (created/deleted) directories.
///
/// Tracks directory changes for diff manifest generation.
pub struct DirtyDirManager {
    /// Map of inode ID to dirty directory.
    dirty_dirs: RwLock<HashMap<INodeId, DirtyDir>>,
    /// Reference to inode manager.
    inodes: Arc<INodeManager>,
    /// Set of original directory paths from manifest.
    original_dirs: RwLock<HashSet<String>>,
}

impl DirtyDirManager {
    /// Create a new dirty directory manager.
    ///
    /// # Arguments
    /// * `inodes` - Inode manager for metadata
    /// * `original_dirs` - Set of directory paths from original manifest
    pub fn new(inodes: Arc<INodeManager>, original_dirs: HashSet<String>) -> Self {
        Self {
            dirty_dirs: RwLock::new(HashMap::new()),
            inodes,
            original_dirs: RwLock::new(original_dirs),
        }
    }

    /// Check if a directory is dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    pub fn is_dirty(&self, inode_id: INodeId) -> bool {
        self.dirty_dirs.read().unwrap().contains_key(&inode_id)
    }

    /// Get dirty directory state if dirty.
    ///
    /// # Arguments
    /// * `inode_id` - Inode ID to check
    ///
    /// # Returns
    /// State if dirty, None otherwise.
    pub fn get_state(&self, inode_id: INodeId) -> Option<DirtyDirState> {
        let guard = self.dirty_dirs.read().unwrap();
        guard.get(&inode_id).map(|d| d.state())
    }

    /// Create a new directory.
    ///
    /// # Arguments
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Name of the new directory
    ///
    /// # Returns
    /// Inode ID of the new directory.
    pub fn create_dir(&self, parent_id: INodeId, name: &str) -> Result<INodeId, VfsError> {
        // Get parent path
        let parent_inode: Arc<dyn INode> = self
            .inodes
            .get(parent_id)
            .ok_or(VfsError::InodeNotFound(parent_id))?;

        if parent_inode.inode_type() != INodeType::Directory {
            return Err(VfsError::NotADirectory(parent_id));
        }

        // Build new directory path
        let parent_path: &str = parent_inode.path();
        let new_path: String = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        };

        // Check if directory already exists
        if self.inodes.get_by_path(&new_path).is_some() {
            return Err(VfsError::AlreadyExists(new_path));
        }

        // Create directory in inode manager
        let new_id: INodeId = self.inodes.add_directory(&new_path);

        // Check if this was an original directory that was deleted and is being recreated
        let is_original: bool = self.original_dirs.read().unwrap().contains(&new_path);

        if !is_original {
            // Track as new directory
            let dirty = DirtyDir::new(new_id, new_path, DirtyDirState::New);
            self.dirty_dirs.write().unwrap().insert(new_id, dirty);
        } else {
            // Was deleted, now recreated - remove from dirty tracking
            // (effectively undoing the delete)
            self.dirty_dirs.write().unwrap().remove(&new_id);
        }

        Ok(new_id)
    }

    /// Delete an empty directory.
    ///
    /// # Arguments
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Name of the directory to delete
    pub fn delete_dir(&self, parent_id: INodeId, name: &str) -> Result<(), VfsError> {
        // Find the directory
        let parent_inode: Arc<dyn INode> = self
            .inodes
            .get(parent_id)
            .ok_or(VfsError::InodeNotFound(parent_id))?;

        let dir_id: INodeId = self
            .inodes
            .get_dir_children(parent_id)
            .ok_or(VfsError::NotADirectory(parent_id))?
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
            .ok_or(VfsError::NotFound(name.to_string()))?;

        let dir_inode: Arc<dyn INode> = self
            .inodes
            .get(dir_id)
            .ok_or(VfsError::InodeNotFound(dir_id))?;

        // Verify it's a directory
        if dir_inode.inode_type() != INodeType::Directory {
            return Err(VfsError::NotADirectory(dir_id));
        }

        // Check if directory is empty
        let children: Vec<(String, INodeId)> = self
            .inodes
            .get_dir_children(dir_id)
            .ok_or(VfsError::NotADirectory(dir_id))?;

        if !children.is_empty() {
            return Err(VfsError::DirectoryNotEmpty(dir_inode.path().to_string()));
        }

        let dir_path: String = dir_inode.path().to_string();

        // Check if this is an original directory or a newly created one
        let is_original: bool = self.original_dirs.read().unwrap().contains(&dir_path);
        let was_new: bool = self
            .dirty_dirs
            .read()
            .unwrap()
            .get(&dir_id)
            .map(|d| d.state() == DirtyDirState::New)
            .unwrap_or(false);

        if was_new {
            // Newly created directory being deleted - just remove from tracking
            self.dirty_dirs.write().unwrap().remove(&dir_id);
        } else if is_original {
            // Original directory being deleted - track as deleted
            let dirty = DirtyDir::new(dir_id, dir_path.clone(), DirtyDirState::Deleted);
            self.dirty_dirs.write().unwrap().insert(dir_id, dirty);
        }
        // else: not original and not new - shouldn't happen, but safe to ignore

        // Remove from inode manager
        self.inodes.remove_child(parent_id, name)?;
        self.inodes.remove_inode(dir_id)?;

        Ok(())
    }

    /// Get all dirty directory entries for diff manifest generation.
    ///
    /// # Returns
    /// Vector of dirty directory entries.
    pub fn get_dirty_dir_entries(&self) -> Vec<DirtyDirEntry> {
        let guard = self.dirty_dirs.read().unwrap();
        guard
            .values()
            .map(|dirty| DirtyDirEntry {
                path: dirty.rel_path().to_string(),
                state: dirty.state(),
            })
            .collect()
    }

    /// Clear all dirty state after successful export.
    pub fn clear(&self) {
        self.dirty_dirs.write().unwrap().clear();
    }
}
```

---

## INodeManager Extensions

### INodeDir Extension

```rust
impl INodeDir {
    /// Remove a child entry from this directory.
    ///
    /// # Arguments
    /// * `name` - Child entry name to remove
    ///
    /// # Returns
    /// The removed child inode ID, or None if not found.
    pub fn remove_child(&self, name: &str) -> Option<INodeId> {
        let mut children: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
            self.children.write().unwrap();
        children.remove(name)
    }
}
```

### INodeManager Extension

```rust
impl INodeManager {
    /// Remove a child from a parent directory.
    ///
    /// # Arguments
    /// * `parent_id` - Parent directory inode ID
    /// * `name` - Child name to remove
    ///
    /// # Returns
    /// Ok if removed, Err if parent not found or not a directory.
    pub fn remove_child(&self, parent_id: INodeId, name: &str) -> Result<(), VfsError> {
        let parent: Arc<dyn INode> = self
            .get(parent_id)
            .ok_or(VfsError::InodeNotFound(parent_id))?;

        let parent_dir: &INodeDir = parent
            .as_any()
            .downcast_ref::<INodeDir>()
            .ok_or(VfsError::NotADirectory(parent_id))?;

        parent_dir.remove_child(name);
        Ok(())
    }

    /// Remove an inode from the manager.
    ///
    /// # Arguments
    /// * `id` - Inode ID to remove
    ///
    /// # Returns
    /// Ok if removed, Err if not found.
    pub fn remove_inode(&self, id: INodeId) -> Result<(), VfsError> {
        // Get path before removing
        let path: String = {
            let inode: Arc<dyn INode> = self
                .get(id)
                .ok_or(VfsError::InodeNotFound(id))?;
            inode.path().to_string()
        };

        // Remove from inodes map
        {
            let mut inodes: std::sync::RwLockWriteGuard<'_, HashMap<INodeId, Arc<dyn INode>>> =
                self.inodes.write().unwrap();
            inodes.remove(&id);
        }

        // Remove from path index
        {
            let mut path_index: std::sync::RwLockWriteGuard<'_, HashMap<String, INodeId>> =
                self.path_index.write().unwrap();
            path_index.remove(&path);
        }

        Ok(())
    }
}
```

---

## FUSE Implementation

### mkdir

```rust
impl fuser::Filesystem for WritableVfs {
    fn mkdir(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name_str: &str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        // Validate name
        if name_str.is_empty() || name_str.contains('/') {
            reply.error(libc::EINVAL);
            return;
        }

        // Create directory
        match self.dirty_dir_manager.create_dir(parent, name_str) {
            Ok(new_id) => {
                // Build file attributes
                let attr = FileAttr {
                    ino: new_id,
                    size: 0,
                    blocks: 0,
                    atime: SystemTime::now(),
                    mtime: SystemTime::now(),
                    ctime: SystemTime::now(),
                    crtime: SystemTime::now(),
                    kind: FileType::Directory,
                    perm: (mode & 0o7777) as u16,
                    nlink: 2, // . and parent's link
                    uid: req.uid(),
                    gid: req.gid(),
                    rdev: 0,
                    blksize: 512,
                    flags: 0,
                };

                reply.entry(&TTL, &attr, 0);
            }
            Err(VfsError::AlreadyExists(_)) => {
                reply.error(libc::EEXIST);
            }
            Err(VfsError::NotADirectory(_)) => {
                reply.error(libc::ENOTDIR);
            }
            Err(e) => {
                tracing::error!("mkdir failed: {}", e);
                reply.error(libc::EIO);
            }
        }
    }
}
```

### rmdir

```rust
impl fuser::Filesystem for WritableVfs {
    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str: &str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        // Validate name
        if name_str.is_empty() || name_str == "." || name_str == ".." {
            reply.error(libc::EINVAL);
            return;
        }

        // Delete directory
        match self.dirty_dir_manager.delete_dir(parent, name_str) {
            Ok(()) => {
                reply.ok();
            }
            Err(VfsError::NotFound(_)) => {
                reply.error(libc::ENOENT);
            }
            Err(VfsError::NotADirectory(_)) => {
                reply.error(libc::ENOTDIR);
            }
            Err(VfsError::DirectoryNotEmpty(_)) => {
                reply.error(libc::ENOTEMPTY);
            }
            Err(e) => {
                tracing::error!("rmdir failed: {}", e);
                reply.error(libc::EIO);
            }
        }
    }
}
```

---

## Diff Manifest Integration

### Updated DiffManifestExporter

The diff manifest export must include directory changes:

```rust
impl DiffManifestExporter for WritableVfs {
    async fn export_diff_manifest(
        &self,
        parent_manifest: &Manifest,
        parent_encoded: &str,
    ) -> Result<Manifest, VfsError> {
        // ... existing file export logic ...

        // Collect dirty directory entries
        let dirty_dir_entries: Vec<DirtyDirEntry> = 
            self.dirty_dir_manager.get_dirty_dir_entries();

        let mut dirs: Vec<ManifestDirectoryPath> = Vec::new();

        for entry in dirty_dir_entries {
            match entry.state {
                DirtyDirState::New => {
                    dirs.push(ManifestDirectoryPath {
                        path: entry.path,
                        deleted: false,
                    });
                }
                DirtyDirState::Deleted => {
                    dirs.push(ManifestDirectoryPath {
                        path: entry.path,
                        deleted: true,
                    });
                }
            }
        }

        // ... combine with file entries and return manifest ...
    }

    fn clear_dirty(&self) -> Result<(), VfsError> {
        self.dirty_manager.clear();
        self.dirty_dir_manager.clear();
        Ok(())
    }

    fn dirty_summary(&self) -> DirtySummary {
        let file_entries: Vec<DirtyEntry> = self.dirty_manager.get_dirty_entries();
        let dir_entries: Vec<DirtyDirEntry> = self.dirty_dir_manager.get_dirty_dir_entries();

        let mut summary = DirtySummary::default();

        for entry in file_entries {
            match entry.state {
                DirtyState::New => summary.new_count += 1,
                DirtyState::Modified => summary.modified_count += 1,
                DirtyState::Deleted => summary.deleted_count += 1,
            }
        }

        // Add directory counts
        for entry in dir_entries {
            match entry.state {
                DirtyDirState::New => summary.new_dir_count += 1,
                DirtyDirState::Deleted => summary.deleted_dir_count += 1,
            }
        }

        summary
    }
}
```

### Updated DirtySummary

```rust
/// Summary of dirty file and directory counts.
#[derive(Debug, Clone, Default)]
pub struct DirtySummary {
    pub new_count: usize,
    pub modified_count: usize,
    pub deleted_count: usize,
    pub new_dir_count: usize,
    pub deleted_dir_count: usize,
}

impl DirtySummary {
    /// Total number of dirty files.
    pub fn total_files(&self) -> usize {
        self.new_count + self.modified_count + self.deleted_count
    }

    /// Total number of dirty directories.
    pub fn total_dirs(&self) -> usize {
        self.new_dir_count + self.deleted_dir_count
    }

    /// Total number of dirty entries.
    pub fn total(&self) -> usize {
        self.total_files() + self.total_dirs()
    }

    /// Returns true if there are any dirty entries.
    pub fn has_changes(&self) -> bool {
        self.total() > 0
    }
}
```

---

## Error Types

### New VfsError Variants

```rust
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    // ... existing variants ...

    #[error("Path already exists: {0}")]
    AlreadyExists(String),

    #[error("Not a directory: inode {0}")]
    NotADirectory(INodeId),

    #[error("Directory not empty: {0}")]
    DirectoryNotEmpty(String),

    #[error("Not found: {0}")]
    NotFound(String),
}
```

---

## Edge Cases

### Create Directory That Was Deleted

If a directory from the original manifest was deleted and then recreated:

```
Original manifest: dirs = ["a", "a/b"]

1. rmdir("a/b")     → dirty_dirs: {b_id: Deleted}
2. mkdir("a/b")     → dirty_dirs: {} (delete entry removed)

Result: No change in diff manifest (directory exists in both)
```

### Delete Newly Created Directory

If a newly created directory is deleted:

```
1. mkdir("new_dir")  → dirty_dirs: {id: New}
2. rmdir("new_dir")  → dirty_dirs: {} (entry removed)

Result: No change in diff manifest (never existed in original)
```

### Nested Directory Creation

Creating nested directories requires creating parents first:

```
mkdir("a/b/c") when "a" doesn't exist:
  → Error: ENOENT (parent doesn't exist)

Correct sequence:
  mkdir("a")
  mkdir("a/b")
  mkdir("a/b/c")
```

### Delete Non-Empty Directory

```
rmdir("dir_with_files")
  → Error: ENOTEMPTY
```

---

## Materialized Cache Integration

Directories don't have content, so the `MaterializedCache` doesn't need changes. However, when creating files in new directories, the cache automatically creates parent directories:

```rust
// In MaterializedCache::write_file()
if let Some(parent) = full_path.parent() {
    std::fs::create_dir_all(parent)?;  // Creates any missing directories
}
```

For deleted directories, the cache doesn't need explicit handling since:
1. Files in the directory must be deleted first (POSIX semantics)
2. Empty directories in the cache are harmless

---

## Thread Safety

The `DirtyDirManager` follows the same concurrency model as `DirtyFileManager`:

| Operation | Lock Type | Duration | Notes |
|-----------|-----------|----------|-------|
| `is_dirty()` | Read | Brief | Check only |
| `get_state()` | Read | Brief | State lookup |
| `create_dir()` | Write | Brief | Insert entry |
| `delete_dir()` | Write | Brief | Update/remove entry |
| `get_dirty_dir_entries()` | Read | Brief | Clone entries |
| `clear()` | Write | Brief | Clear map |

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_new_directory() {
        let inodes = Arc::new(INodeManager::new());
        let manager = DirtyDirManager::new(inodes.clone(), HashSet::new());

        let id = manager.create_dir(ROOT_INODE, "new_dir").unwrap();
        
        assert!(manager.is_dirty(id));
        assert_eq!(manager.get_state(id), Some(DirtyDirState::New));
    }

    #[test]
    fn test_delete_empty_directory() {
        let inodes = Arc::new(INodeManager::new());
        let original_dirs: HashSet<String> = ["existing"].iter().map(|s| s.to_string()).collect();
        let manager = DirtyDirManager::new(inodes.clone(), original_dirs);

        // Add existing directory to inodes
        inodes.add_directory("existing");

        // Delete it
        manager.delete_dir(ROOT_INODE, "existing").unwrap();

        // Should be tracked as deleted
        let entries = manager.get_dirty_dir_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, DirtyDirState::Deleted);
    }

    #[test]
    fn test_delete_non_empty_directory_fails() {
        let inodes = Arc::new(INodeManager::new());
        let manager = DirtyDirManager::new(inodes.clone(), HashSet::new());

        // Create directory with a file
        inodes.add_directory("dir");
        inodes.add_file("dir/file.txt", 100, 0, FileContent::SingleHash("hash".into()), HashAlgorithm::Xxh128, false);

        // Try to delete - should fail
        let result = manager.delete_dir(ROOT_INODE, "dir");
        assert!(matches!(result, Err(VfsError::DirectoryNotEmpty(_))));
    }

    #[test]
    fn test_recreate_deleted_directory() {
        let inodes = Arc::new(INodeManager::new());
        let original_dirs: HashSet<String> = ["orig_dir"].iter().map(|s| s.to_string()).collect();
        let manager = DirtyDirManager::new(inodes.clone(), original_dirs);

        inodes.add_directory("orig_dir");

        // Delete original directory
        manager.delete_dir(ROOT_INODE, "orig_dir").unwrap();
        assert_eq!(manager.get_dirty_dir_entries().len(), 1);

        // Recreate it
        manager.create_dir(ROOT_INODE, "orig_dir").unwrap();
        
        // Should no longer be dirty (back to original state)
        assert_eq!(manager.get_dirty_dir_entries().len(), 0);
    }
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_mkdir_rmdir_fuse_operations() {
    // Mount writable VFS
    // mkdir /mnt/vfs/new_dir
    // Verify directory exists
    // rmdir /mnt/vfs/new_dir
    // Verify directory removed
    // Export diff manifest
    // Verify no directory changes (created then deleted)
}
```

---

## Implementation Checklist

- [x] Add `DirtyDirState` enum to `write/dirty_dir.rs`
- [x] Add `DirtyDir` struct to `write/dirty_dir.rs`
- [x] Add `DirtyDirEntry` struct to `write/dirty_dir.rs`
- [x] Add `DirtyDirManager` to `write/dirty_dir.rs`
- [x] Add `remove_child()` to `INodeDir`
- [x] Add `remove_child()` to `INodeManager`
- [x] Add `remove_inode()` to `INodeManager`
- [x] Add new error variants to `VfsError`
- [x] Implement `mkdir()` in `WritableVfs`
- [x] Implement `rmdir()` in `WritableVfs`
- [x] Update `DirtySummary` with directory counts
- [x] Update `clear_dirty()` to clear directory state
- [x] Add unit tests for `DirtyDirManager`
- [ ] Add integration tests for FUSE operations
- [ ] Update `export_diff_manifest()` to include directories (currently returns error "not yet implemented")

---

## Implementation Notes

### Differences from Design

1. **DirtyDirManager additional methods**: The implementation includes extra helper methods not in the original design:
   - `is_new_dir(inode_id)` - Check if an inode is a newly created directory
   - `get_new_dirs_in_parent(parent_id)` - Get new directories in a parent for readdir
   - `lookup_new_dir(parent_id, name)` - Look up a new directory by parent and name

2. **Recreate deleted directory handling**: The implementation handles the case where a deleted original directory is recreated by looking for existing deleted entries by path (not just by inode ID), since the recreated directory gets a new inode ID.

3. **DiffManifestExporter**: The `export_diff_manifest()` method is not yet fully implemented - it returns an error "Diff manifest export not yet implemented". The `dirty_summary()` method correctly includes directory counts.

4. **FUSE lookup integration**: The `lookup()` method in `WritableVfs` checks for new directories via `lookup_new_dir()` and deleted directories via `get_state()`.

5. **FUSE readdir integration**: The `readdir()` method includes new directories via `get_new_dirs_in_parent()` and filters out deleted directories.
