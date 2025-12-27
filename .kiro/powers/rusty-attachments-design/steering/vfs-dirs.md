# VFS Directory Operations Design Summary

**Full doc:** `design/vfs-dirs.md`  
**Status:** ✅ IMPLEMENTED in `crates/vfs/`

## Purpose
Support for `mkdir` and `rmdir` FUSE operations with dirty tracking for diff manifest generation.

## Architecture

```
Layer 3: WritableVfs
         - mkdir() → create directory in dirty layer
         - rmdir() → mark directory deleted in dirty layer

Layer 2: DirtyDirManager
         - dirty_dirs: HashMap<INodeId, DirtyDir>
         - create_dir(), delete_dir()
         - get_dirty_dir_entries() for diff manifest

Layer 1: INodeManager (extended)
         - remove_child(), remove_inode()
```

## Key Types

### DirtyDir
```rust
enum DirtyDirState { New, Deleted }

struct DirtyDir {
    inode_id: INodeId,
    rel_path: String,
    state: DirtyDirState,
    mtime: SystemTime,
}

struct DirtyDirEntry {
    path: String,
    state: DirtyDirState,
}
```

### DirtyDirManager
```rust
impl DirtyDirManager {
    fn new(inodes: Arc<INodeManager>, original_dirs: HashSet<String>) -> Self;
    fn is_dirty(&self, inode_id: INodeId) -> bool;
    fn get_state(&self, inode_id: INodeId) -> Option<DirtyDirState>;
    fn create_dir(&self, parent_id: INodeId, name: &str) -> Result<INodeId, VfsError>;
    fn delete_dir(&self, parent_id: INodeId, name: &str) -> Result<(), VfsError>;
    fn get_dirty_dir_entries(&self) -> Vec<DirtyDirEntry>;
    fn clear(&self);
}
```

### INodeManager Extensions
```rust
impl INodeManager {
    fn remove_child(&self, parent_id: INodeId, name: &str) -> Result<(), VfsError>;
    fn remove_inode(&self, id: INodeId) -> Result<(), VfsError>;
}
```

## FUSE Operations

### mkdir
1. Validate name (non-empty, no `/`)
2. `dirty_dir_manager.create_dir(parent, name)`
3. Return FileAttr with FileType::Directory

### rmdir
1. Validate name (not `.` or `..`)
2. Check directory is empty
3. `dirty_dir_manager.delete_dir(parent, name)`
4. Return success

## Edge Cases

### Create Directory That Was Deleted
```
Original: dirs = ["a", "a/b"]
1. rmdir("a/b")  → dirty_dirs: {b_id: Deleted}
2. mkdir("a/b")  → dirty_dirs: {} (delete entry removed)
Result: No change in diff manifest
```

### Delete Newly Created Directory
```
1. mkdir("new_dir")  → dirty_dirs: {id: New}
2. rmdir("new_dir")  → dirty_dirs: {} (entry removed)
Result: No change in diff manifest
```

### Delete Non-Empty Directory
```
rmdir("dir_with_files") → Error: ENOTEMPTY
```

## Diff Manifest Integration

Updated `DirtySummary`:
```rust
struct DirtySummary {
    new_count: usize,
    modified_count: usize,
    deleted_count: usize,
    new_dir_count: usize,      // NEW
    deleted_dir_count: usize,  // NEW
}
```

Export includes directory changes:
```rust
for entry in dirty_dir_entries {
    match entry.state {
        DirtyDirState::New => dirs.push(ManifestDirectoryPath { path, deleted: false }),
        DirtyDirState::Deleted => dirs.push(ManifestDirectoryPath { path, deleted: true }),
    }
}
```

## Error Types
```rust
enum VfsError {
    AlreadyExists(String),
    NotADirectory(INodeId),
    DirectoryNotEmpty(String),
    NotFound(String),
    // ... existing variants
}
```

## Thread Safety

| Operation | Lock Type | Duration |
|-----------|-----------|----------|
| `is_dirty()` | Read | Brief |
| `get_state()` | Read | Brief |
| `create_dir()` | Write | Brief |
| `delete_dir()` | Write | Brief |
| `get_dirty_dir_entries()` | Read | Brief |
| `clear()` | Write | Brief |

## When to Read Full Doc
- Implementing directory FUSE operations
- Understanding dirty directory tracking
- Edge case handling details
- Thread safety considerations
