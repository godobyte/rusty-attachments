# Windows Virtual Filesystem: ProjFS Design

**Status: 📋 DESIGN (Revised)**

## Overview

This document describes the design for `rusty-attachments-vfs-projfs`, a Windows-native virtual filesystem using Microsoft's Projected File System (ProjFS). The design is heavily influenced by [VFSForGit](https://github.com/microsoft/VFSForGit), a production-grade ProjFS implementation.

### Why ProjFS?

| Aspect | ProjFS | Alternatives |
|--------|--------|--------------|
| **License** | Windows component (commercial safe) | WinFSP: GPLv3 ❌, Dokan: LGPL |
| **Installation** | Enable Windows feature only | Requires third-party driver |
| **Model** | Placeholder/hydration (ideal for CAS) | Traditional VFS |
| **Production Use** | OneDrive, VFS for Git | Limited |

---

## Architecture Overview

Following VFSForGit's layered architecture:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              ProjFS Driver                                   │
│                         (prjflt.sys - Windows)                              │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                          PRJ_CALLBACKS (user-mode)
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│  Layer 3: Platform Virtualizer                                              │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │  WindowsFileSystemVirtualizer (implements IRequiredCallbacks)         │  │
│  │    - Receives ProjFS callbacks                                        │  │
│  │    - Manages enumeration sessions (ConcurrentDictionary<Guid, ...>)   │  │
│  │    - Manages async commands (ConcurrentDictionary<int, ...>)          │  │
│  │    - Dispatches to worker threads for I/O                             │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                          FileSystemCallbacks trait
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│  Layer 2: Callbacks & Coordination                                          │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │  FileSystemCallbacks                                                  │  │
│  │    - Coordinates between virtualizer and projection                   │  │
│  │    - Tracks modified paths (for diff manifest)                        │  │
│  │    - Background task queue for non-blocking operations                │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                          ManifestProjection trait
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│  Layer 1: Projection (Backing Store)                                        │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │  ManifestProjection                                                   │  │
│  │    - In-memory tree from manifest (V1 or V2)                          │  │
│  │    - Sorted folder entries for enumeration                            │  │
│  │    - Case-insensitive lookup                                          │  │
│  │    - Content hash → S3 CAS mapping                                    │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
┌─────────────────────────────────────────────────────────────────────────────┐
│  Layer 0: Storage & Primitives (existing crates)                            │
│    StorageClient, MemoryPool, DirtyFileManager, DirtyDirManager             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Key Design Decisions from VFSForGit

### 1. Separation of Concerns

VFSForGit separates:
- **Virtualizer** (`WindowsFileSystemVirtualizer`): Platform-specific ProjFS callbacks
- **Callbacks** (`FileSystemCallbacks`): Coordination and state management
- **Projection** (`GitIndexProjection`): Backing store representation

We adopt the same pattern:
- **Virtualizer**: `ProjFsVirtualizer` - ProjFS callback handling
- **Callbacks**: `VfsCallbacks` - Coordination layer
- **Projection**: `ManifestProjection` - Manifest-based backing store

### 2. Fast Path / Slow Path Pattern

VFSForGit uses a critical optimization:

```csharp
// Fast path: data already in memory → sync return
if (TryGetProjectedItemsFromMemory(virtualPath, out projectedItems)) {
    activeEnumerations.TryAdd(enumerationId, new ActiveEnumeration(projectedItems));
    return HResult.Ok;
}

// Slow path: need I/O → async return
TryScheduleFileOrNetworkRequest(...);
return HResult.Pending;
```

We adopt this pattern:
- **Fast path**: Manifest data is always in memory → sync return for enumeration/placeholder
- **Slow path**: S3 fetch for file data → async return with `ERROR_IO_PENDING`

### 3. Pre-Loading Enumeration Data

VFSForGit loads ALL enumeration data in `StartDirectoryEnumerationCallback`, not in `GetDirectoryEnumerationCallback`. This ensures:
- `GetDirectoryEnumerationCallback` is always fast (no I/O)
- Multiple calls to `GetDirectoryEnumerationCallback` don't re-fetch data
- Enumeration state is consistent throughout the session

**Our approach:** Same pattern. The entire manifest is loaded into `ManifestProjection` at startup. `StartDirectoryEnumerationCallback` retrieves the pre-sorted children from memory and stores them in `ActiveEnumeration`. No I/O is ever needed for enumeration.

### 4. Worker Thread Pool for I/O

VFSForGit uses a dedicated worker thread pool (`BlockingCollection<FileOrNetworkRequest>`) for operations that may block on I/O. This prevents ProjFS worker threads from being exhausted.

**Our approach:** We reuse the `AsyncExecutor` from `crates/vfs` (same as FUSE). Instead of a custom worker pool:
- `AsyncExecutor` runs a dedicated Tokio runtime in a background thread
- ProjFS callbacks call `executor.block_on()` which blocks on a oneshot channel (condvar wait)
- Async work (S3 fetches) runs on the executor's Tokio runtime
- This is simpler than VFSForGit's approach and shares code with FUSE

### 5. Background Task Queue

Non-critical operations (tracking modified paths, updating databases) are queued to a background task runner, keeping callbacks fast.

---

## Data Structures

### Layer 1: ManifestProjection


```rust
use std::collections::HashMap;
use std::sync::Arc;

/// In-memory projection of manifest contents.
///
/// Analogous to VFSForGit's `GitIndexProjection`. Builds a tree structure
/// from the manifest for fast enumeration and lookup. All data is kept
/// in memory for fast access - manifest files are typically small enough
/// that this is practical.
///
/// # Thread Safety
/// Uses interior mutability with RwLock for concurrent read access.
/// Write access only needed during initialization or manifest updates.
pub struct ManifestProjection {
    /// Root folder data.
    root: FolderData,
    /// Manifest version for compatibility checks.
    manifest_version: ManifestVersion,
    /// Hash algorithm used by manifest.
    hash_algorithm: HashAlgorithm,
}

/// Data about a folder in the projection.
///
/// Analogous to VFSForGit's `FolderData`.
pub struct FolderData {
    /// Sorted child entries (files and subfolders).
    /// Sorted using ProjFS collation order for fast enumeration.
    children: SortedFolderEntries,
    /// Whether this folder is included in projection.
    /// Used for sparse/partial projections.
    is_included: bool,
}

/// Sorted collection of folder entries.
///
/// Analogous to VFSForGit's `SortedFolderEntries`.
/// Maintains entries in ProjFS sort order for efficient enumeration.
pub struct SortedFolderEntries {
    /// Entries sorted by name using PrjFileNameCompare order.
    entries: Vec<FolderEntry>,
}

/// Entry in a folder (file or subfolder).
///
/// Analogous to VFSForGit's `FolderEntryData`.
pub enum FolderEntry {
    File(FileData),
    Folder(Box<FolderData>),
    Symlink(SymlinkData),
}

/// Data about a file in the projection.
///
/// Analogous to VFSForGit's `FileData`.
pub struct FileData {
    /// File name (not full path).
    name: String,
    /// File size in bytes.
    size: u64,
    /// Content hash for CAS lookup.
    content_hash: ContentHash,
    /// Modification time.
    mtime: i64,
}

/// Content hash - either single hash or chunked.
pub enum ContentHash {
    /// Single hash for entire file (V1 and small V2 files).
    Single(String),
    /// Chunk hashes for large files (V2 only, >256MB).
    Chunked(Vec<String>),
}

/// Data about a symlink (V2 manifest only).
pub struct SymlinkData {
    /// Symlink name.
    name: String,
    /// Target path.
    target: String,
}

impl ManifestProjection {
    /// Build projection from manifest.
    ///
    /// # Arguments
    /// * `manifest` - Manifest to project (V1 or V2)
    ///
    /// # Returns
    /// New projection with sorted entries.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        // Implementation builds tree from manifest paths
        // Entries are sorted using ProjFS collation order
        todo!()
    }

    /// Get projected items for a folder path.
    ///
    /// # Arguments
    /// * `relative_path` - Path relative to virtualization root
    ///
    /// # Returns
    /// List of projected file info for enumeration.
    pub fn get_projected_items(&self, relative_path: &str) -> Option<Vec<ProjectedFileInfo>> {
        todo!()
    }

    /// Check if a path is projected and get its info.
    ///
    /// # Arguments
    /// * `relative_path` - Path to check
    ///
    /// # Returns
    /// (canonical_name, is_folder) if path exists in projection.
    pub fn is_path_projected(&self, relative_path: &str) -> Option<(String, bool)> {
        todo!()
    }

    /// Get file info for placeholder creation.
    ///
    /// # Arguments
    /// * `relative_path` - Path to file
    ///
    /// # Returns
    /// File info including size and content hash.
    pub fn get_file_info(&self, relative_path: &str) -> Option<ProjectedFileInfo> {
        todo!()
    }
}

/// Projected file/folder info for enumeration and placeholders.
///
/// Analogous to VFSForGit's `ProjectedFileInfo`.
#[derive(Clone)]
pub struct ProjectedFileInfo {
    /// File or folder name.
    pub name: String,
    /// Size in bytes (0 for folders).
    pub size: u64,
    /// Whether this is a folder.
    pub is_folder: bool,
    /// Content hash for files (None for folders).
    pub content_hash: Option<String>,
    /// Symlink target (None for files/folders).
    pub symlink_target: Option<String>,
}
```

### Layer 2: VfsCallbacks

```rust
use std::collections::HashSet;
use std::sync::Arc;
use parking_lot::RwLock;

/// Coordination layer between virtualizer and projection.
///
/// Analogous to VFSForGit's `FileSystemCallbacks`.
/// Tracks modified paths and coordinates background operations.
///
/// # Note on Placeholder Database
/// VFSForGit maintains a placeholder database to track which files have been
/// materialized on disk and their content hashes. This is needed because:
/// - Git index changes require updating/invalidating placeholders
/// - Git operations need to know what's on disk vs. virtual
///
/// For job attachments, we don't need this because:
/// - The manifest is static during a job run (no backing store changes)
/// - Dirty state is tracked via DirtyFileManager/DirtyDirManager
/// - ProjFS itself tracks placeholder state internally
pub struct VfsCallbacks {
    /// Manifest projection (backing store).
    projection: Arc<ManifestProjection>,
    /// Modified paths for diff manifest generation.
    modified_paths: RwLock<ModifiedPathsDatabase>,
    /// Background task queue.
    background_tasks: BackgroundTaskRunner,
    /// Storage client for S3 CAS access.
    storage: Arc<dyn StorageClient>,
    /// Memory pool for caching fetched content.
    memory_pool: Arc<MemoryPool>,
    /// Dirty file manager (COW layer).
    dirty_files: Arc<DirtyFileManager>,
    /// Dirty directory manager.
    dirty_dirs: Arc<DirtyDirManager>,
}

impl VfsCallbacks {
    /// Get projected items for enumeration.
    ///
    /// Fast path - all data is in memory.
    ///
    /// # Arguments
    /// * `relative_path` - Directory path
    ///
    /// # Returns
    /// List of items to enumerate.
    pub fn get_projected_items(&self, relative_path: &str) -> Vec<ProjectedFileInfo> {
        let mut items: Vec<ProjectedFileInfo> = self.projection
            .get_projected_items(relative_path)
            .unwrap_or_default();

        // Add new files/dirs from dirty managers
        // Remove deleted items
        // This matches FUSE/FSKit dirty handling logic
        self.apply_dirty_state(&mut items, relative_path);

        items
    }

    /// Check if path is projected.
    ///
    /// # Arguments
    /// * `relative_path` - Path to check
    ///
    /// # Returns
    /// (canonical_name, is_folder) if exists.
    pub fn is_path_projected(
        &self,
        relative_path: &str,
    ) -> Option<(String, bool)> {
        // Check manifest projection first
        if let Some(result) = self.projection.is_path_projected(relative_path) {
            // Check if deleted
            if !self.is_deleted(relative_path) {
                return Some(result);
            }
        }

        // Check new files/dirs from dirty managers
        self.check_dirty_paths(relative_path)
    }

    /// Fetch file content from S3 CAS.
    ///
    /// Slow path - requires network I/O.
    ///
    /// # Arguments
    /// * `content_hash` - Hash of content to fetch
    /// * `offset` - Byte offset
    /// * `length` - Bytes to read
    ///
    /// # Returns
    /// File content bytes.
    pub async fn fetch_file_content(
        &self,
        content_hash: &str,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, VfsError> {
        // Check dirty manager first (COW files)
        // Then check memory pool
        // Finally fetch from S3
        todo!()
    }

    // --- Notification handlers (queued to background) ---

    /// Called when a new file is created.
    pub fn on_file_created(&self, relative_path: &str) {
        self.background_tasks.enqueue(BackgroundTask::FileCreated(
            relative_path.to_string(),
        ));
    }

    /// Called when a file is modified.
    pub fn on_file_modified(&self, relative_path: &str) {
        self.background_tasks.enqueue(BackgroundTask::FileModified(
            relative_path.to_string(),
        ));
    }

    /// Called when a file is deleted.
    pub fn on_file_deleted(&self, relative_path: &str) {
        self.background_tasks.enqueue(BackgroundTask::FileDeleted(
            relative_path.to_string(),
        ));
    }

    /// Called when a file is renamed.
    pub fn on_file_renamed(&self, old_path: &str, new_path: &str) {
        self.background_tasks.enqueue(BackgroundTask::FileRenamed {
            old_path: old_path.to_string(),
            new_path: new_path.to_string(),
        });
    }

    /// Called when a file is hydrated.
    pub fn on_file_hydrated(&self, relative_path: &str) {
        // Telemetry/metrics
    }
}

/// Background task types.
enum BackgroundTask {
    FileCreated(String),
    FileModified(String),
    FileDeleted(String),
    FileRenamed { old_path: String, new_path: String },
    FolderCreated(String),
    FolderDeleted(String),
}
```

### Layer 3: ProjFsVirtualizer

```rust
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use vfs::{AsyncExecutor, ExecutorConfig};
use windows::Win32::Storage::ProjectedFileSystem::*;

/// ProjFS virtualizer implementing required callbacks.
///
/// Analogous to VFSForGit's `WindowsFileSystemVirtualizer`.
/// Uses the same `AsyncExecutor` pattern as FUSE for bridging
/// sync callbacks to async I/O.
pub struct ProjFsVirtualizer {
    /// Callbacks for coordination.
    callbacks: Arc<VfsCallbacks>,
    /// Async executor - same pattern as FUSE.
    /// Dedicated runtime in background thread, blocks on oneshot channels.
    executor: Arc<AsyncExecutor>,
    /// Active enumeration sessions.
    /// Key: enumeration GUID, Value: session state.
    active_enumerations: RwLock<HashMap<GUID, ActiveEnumeration>>,
    /// ProjFS virtualization context.
    context: RwLock<PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT>,
    /// Configuration options.
    options: ProjFsOptions,
}

/// Active enumeration session.
///
/// Analogous to VFSForGit's `ActiveEnumeration`.
/// Holds pre-loaded enumeration data and current position.
pub struct ActiveEnumeration {
    /// Pre-loaded items (sorted).
    items: Vec<ProjectedFileInfo>,
    /// Current position in items.
    index: usize,
    /// Captured search expression (wildcard filter).
    filter: Option<String>,
    /// Whether filter has been captured.
    filter_captured: bool,
}

impl ActiveEnumeration {
    /// Create new enumeration with pre-loaded items.
    ///
    /// # Arguments
    /// * `items` - Pre-sorted list of items to enumerate
    pub fn new(items: Vec<ProjectedFileInfo>) -> Self {
        Self {
            items,
            index: 0,
            filter: None,
            filter_captured: false,
        }
    }

    /// Get current item if valid.
    pub fn current(&self) -> Option<&ProjectedFileInfo> {
        self.items.get(self.index)
    }

    /// Check if current position is valid.
    pub fn is_current_valid(&self) -> bool {
        self.index < self.items.len()
    }

    /// Move to next item (skipping filtered items).
    pub fn move_next(&mut self) -> bool {
        self.index += 1;
        self.skip_filtered();
        self.is_current_valid()
    }

    /// Restart enumeration with new filter.
    pub fn restart(&mut self, filter: Option<String>) {
        self.index = 0;
        self.filter = filter;
        self.filter_captured = true;
        self.skip_filtered();
    }

    /// Try to save filter (only on first call).
    pub fn try_save_filter(&mut self, filter: Option<String>) -> bool {
        if !self.filter_captured {
            self.filter = filter;
            self.filter_captured = true;
            self.skip_filtered();
            true
        } else {
            false
        }
    }

    /// Skip items that don't match filter.
    fn skip_filtered(&mut self) {
        while self.is_current_valid() {
            if self.matches_filter(self.current().unwrap()) {
                break;
            }
            self.index += 1;
        }
    }

    /// Check if item matches current filter.
    fn matches_filter(&self, item: &ProjectedFileInfo) -> bool {
        match &self.filter {
            None => true,
            Some(pattern) if pattern.is_empty() => true,
            Some(pattern) => {
                // Use PrjFileNameMatch for wildcard matching
                unsafe { prj_file_name_match(&item.name, pattern) }
            }
        }
    }
}
```

---

## Callback Implementations

### StartDirectoryEnumerationCallback

```rust
impl ProjFsVirtualizer {
    /// Start directory enumeration callback.
    ///
    /// Pre-loads ALL enumeration data (fast path - no I/O needed).
    /// This ensures GetDirectoryEnumerationCallback is always fast.
    unsafe extern "system" fn start_dir_enum_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);
        let relative_path: String = wstr_to_string((*callback_data).FilePathName);

        // Fast path: get projected items from memory
        // VfsCallbacks.get_projected_items handles dirty state
        let items: Vec<ProjectedFileInfo> = this.callbacks.get_projected_items(&relative_path);

        // Create enumeration session with pre-loaded data
        let enumeration = ActiveEnumeration::new(items);

        // Store session
        if !this.active_enumerations.write().try_insert(*enumeration_id, enumeration).is_ok() {
            return E_FAIL;
        }

        S_OK
    }
}
```

### GetDirectoryEnumerationCallback

```rust
impl ProjFsVirtualizer {
    /// Get directory enumeration callback.
    ///
    /// Iterates pre-loaded data (always fast - no I/O).
    unsafe extern "system" fn get_dir_enum_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
        search_expression: PCWSTR,
        dir_entry_buffer_handle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
    ) -> HRESULT {
        let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);

        let mut enumerations = this.active_enumerations.write();
        let enumeration: &mut ActiveEnumeration = match enumerations.get_mut(&*enumeration_id) {
            Some(e) => e,
            None => return E_FAIL,
        };

        // Handle restart scan flag
        let restart: bool = ((*callback_data).Flags & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN) != 0;
        let filter: Option<String> = if search_expression.is_null() {
            None
        } else {
            Some(wstr_to_string(search_expression))
        };

        if restart {
            enumeration.restart(filter);
        } else {
            enumeration.try_save_filter(filter);
        }

        // Fill buffer with entries
        let mut entries_added: usize = 0;

        while enumeration.is_current_valid() {
            let item: &ProjectedFileInfo = enumeration.current().unwrap();

            let result: HRESULT = fill_dir_entry_buffer(
                dir_entry_buffer_handle,
                item,
            );

            if result == HRESULT::from(ERROR_INSUFFICIENT_BUFFER) {
                if entries_added == 0 {
                    // Buffer can't fit even one entry
                    return result;
                }
                // Buffer full, return what we have
                break;
            }

            if result.is_err() {
                return result;
            }

            entries_added += 1;
            enumeration.move_next();
        }

        S_OK
    }
}
```

### GetPlaceholderInfoCallback

```rust
impl ProjFsVirtualizer {
    /// Get placeholder info callback.
    ///
    /// Fast path for manifest data, returns sync.
    unsafe extern "system" fn get_placeholder_info_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
    ) -> HRESULT {
        let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);
        let relative_path: String = wstr_to_string((*callback_data).FilePathName);

        // Check if path is projected (fast - in memory)
        let (canonical_name, is_folder) = match this.callbacks.is_path_projected(&relative_path) {
            Some(result) => result,
            None => return HRESULT::from(ERROR_FILE_NOT_FOUND),
        };

        // Get file info for placeholder
        let file_info: ProjectedFileInfo = match this.callbacks.projection.get_file_info(&relative_path) {
            Some(info) => info,
            None => return HRESULT::from(ERROR_FILE_NOT_FOUND),
        };

        // Build path with canonical case from backing store
        let parent_path: &str = get_parent_path(&relative_path);
        let canonical_path: String = if parent_path.is_empty() {
            canonical_name
        } else {
            format!("{}\\{}", parent_path, canonical_name)
        };

        // Write placeholder info
        write_placeholder_info(
            *this.context.read(),
            &canonical_path,
            &file_info,
        )
    }
}
```

### GetFileDataCallback

```rust
impl ProjFsVirtualizer {
    /// Get file data callback.
    ///
    /// Uses AsyncExecutor.block_on() - blocks on oneshot channel, not Tokio.
    /// This is safe to call from ProjFS callback threads.
    unsafe extern "system" fn get_file_data_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        byte_offset: u64,
        length: u32,
    ) -> HRESULT {
        let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);

        // Extract data from callback
        let relative_path: String = wstr_to_string((*callback_data).FilePathName);
        let data_stream_id: GUID = (*callback_data).DataStreamId;
        let context = SendableContext((*callback_data).NamespaceVirtualizationContext);

        // Get content hash from version info or projection
        let content_hash: String = match extract_content_hash(&(*callback_data).VersionInfo) {
            Some(hash) => hash,
            None => match this.callbacks.projection.get_file_info(&relative_path) {
                Some(info) => info.content_hash.unwrap_or_default(),
                None => return HRESULT::from(ERROR_FILE_NOT_FOUND),
            },
        };

        // Clone for async closure
        let callbacks: Arc<VfsCallbacks> = this.callbacks.clone();

        // Use executor.block_on - blocks on oneshot channel, NOT Tokio
        // This is the same pattern as FUSE
        let result = this.executor.block_on_cancellable(async move {
            // Fetch content (may hit memory pool or S3)
            let data: Vec<u8> = callbacks
                .fetch_file_content(&content_hash, byte_offset, length)
                .await?;

            // Write to ProjFS with aligned buffer
            write_file_data_aligned(context.0, &data_stream_id, &data, byte_offset)?;

            callbacks.on_file_hydrated(&relative_path);
            Ok::<(), VfsError>(())
        });

        match result {
            Ok(Ok(())) => S_OK,
            Ok(Err(e)) => e.to_hresult(),
            Err(ExecutorError::Timeout { .. }) => HRESULT::from(ERROR_TIMEOUT),
            Err(ExecutorError::Cancelled) => HRESULT::from(ERROR_OPERATION_ABORTED),
            Err(ExecutorError::Shutdown) => HRESULT::from(ERROR_SHUTDOWN_IN_PROGRESS),
            Err(ExecutorError::TaskPanicked) => E_FAIL,
        }
    }
}

/// ProjFS context wrapper that is Send.
///
/// # Safety
/// ProjFS allows PrjCompleteCommand/PrjWriteFileData from any thread.
#[derive(Clone, Copy)]
struct SendableContext(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT);

unsafe impl Send for SendableContext {}
unsafe impl Sync for SendableContext {}
```

### CancelCommandCallback

```rust
impl ProjFsVirtualizer {
    /// Cancel command callback.
    ///
    /// Signals cancellation via the executor's built-in cancellation.
    unsafe extern "system" fn cancel_command_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
    ) {
        let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);
        
        // The executor's cancel_all() triggers ExecutorError::Cancelled
        // for any block_on_cancellable() calls in progress
        this.executor.cancel_all();
    }
}
```

---

## Notification Handling


Following VFSForGit's pattern, notifications are queued to a background task runner to keep callbacks fast:

```rust
impl ProjFsVirtualizer {
    /// Notification callback.
    ///
    /// Queues work to background - returns immediately.
    unsafe extern "system" fn notification_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        is_directory: BOOLEAN,
        notification: PRJ_NOTIFICATION,
        destination_file_name: PCWSTR,
        _operation_parameters: *mut PRJ_NOTIFICATION_PARAMETERS,
    ) -> HRESULT {
        let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);
        let relative_path: String = wstr_to_string((*callback_data).FilePathName);

        match notification {
            PRJ_NOTIFICATION_NEW_FILE_CREATED => {
                if is_directory.as_bool() {
                    this.callbacks.on_folder_created(&relative_path);
                } else {
                    this.callbacks.on_file_created(&relative_path);
                }
            }

            PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_MODIFIED => {
                this.callbacks.on_file_modified(&relative_path);
            }

            PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_DELETED => {
                if is_directory.as_bool() {
                    this.callbacks.on_folder_deleted(&relative_path);
                } else {
                    this.callbacks.on_file_deleted(&relative_path);
                }
            }

            PRJ_NOTIFICATION_FILE_RENAMED => {
                let dest_path: String = if destination_file_name.is_null() {
                    String::new()
                } else {
                    wstr_to_string(destination_file_name)
                };
                this.callbacks.on_file_renamed(&relative_path, &dest_path);
            }

            PRJ_NOTIFICATION_PRE_DELETE => {
                // Pre-operation: can veto by returning error
                // For now, allow all deletes
            }

            PRJ_NOTIFICATION_PRE_RENAME => {
                // Pre-operation: can veto by returning error
                // For now, allow all renames
            }

            _ => {}
        }

        S_OK
    }
}
```

---

## Async Execution (Reusing FUSE Pattern)

Instead of a custom worker pool, we reuse the `AsyncExecutor` from `crates/vfs`:

```rust
use vfs::{AsyncExecutor, ExecutorConfig, ExecutorError};

impl ProjFsVirtualizer {
    pub fn new(
        manifest: &Manifest,
        storage: Arc<dyn StorageClient>,
        options: ProjFsOptions,
    ) -> Result<Self, VfsError> {
        // Create executor with same config as FUSE
        let executor_config = ExecutorConfig::default()
            .with_worker_threads(options.worker_thread_count)
            .with_default_timeout(options.default_timeout);
        
        let executor = Arc::new(AsyncExecutor::new(executor_config));

        // ... rest of initialization
    }
}
```

The `AsyncExecutor` provides:
- `block_on(future)` - Block on async work (safe from any thread)
- `block_on_timeout(future, duration)` - With timeout
- `block_on_cancellable(future)` - With cancellation support
- `cancel_all()` - Cancel all in-flight operations
```

---

## Background Task Runner

```rust
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// Background task runner for non-critical operations.
///
/// Analogous to VFSForGit's `BackgroundFileSystemTaskRunner`.
pub struct BackgroundTaskRunner {
    /// Task queue.
    queue: Arc<(Mutex<VecDeque<BackgroundTask>>, Condvar)>,
    /// Worker thread.
    thread: Option<thread::JoinHandle<()>>,
    /// Shutdown flag.
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl BackgroundTaskRunner {
    /// Create new background task runner.
    ///
    /// # Arguments
    /// * `handler` - Function to handle tasks
    pub fn new<F>(handler: F) -> Self
    where
        F: Fn(BackgroundTask) + Send + 'static,
    {
        let queue = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let queue_clone = queue.clone();
        let shutdown_clone = shutdown.clone();

        let thread = thread::spawn(move || {
            loop {
                let task: Option<BackgroundTask> = {
                    let (lock, cvar) = &*queue_clone;
                    let mut queue = lock.lock().unwrap();

                    while queue.is_empty() {
                        if shutdown_clone.load(std::sync::atomic::Ordering::SeqCst) {
                            return;
                        }
                        queue = cvar.wait(queue).unwrap();
                    }

                    queue.pop_front()
                };

                if let Some(task) = task {
                    handler(task);
                }
            }
        });

        Self {
            queue,
            thread: Some(thread),
            shutdown,
        }
    }

    /// Enqueue a task.
    ///
    /// # Arguments
    /// * `task` - Task to enqueue
    pub fn enqueue(&self, task: BackgroundTask) {
        let (lock, cvar) = &*self.queue;
        lock.lock().unwrap().push_back(task);
        cvar.notify_one();
    }

    /// Check if queue is empty.
    pub fn is_empty(&self) -> bool {
        let (lock, _) = &*self.queue;
        lock.lock().unwrap().is_empty()
    }

    /// Shutdown the runner.
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        let (_, cvar) = &*self.queue;
        cvar.notify_all();

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
```

---

## Dirty State Handling

Matching FUSE/FSKit patterns for consistency:

```rust
impl VfsCallbacks {
    /// Apply dirty state to enumeration items.
    ///
    /// # Arguments
    /// * `items` - Items from projection (modified in place)
    /// * `parent_path` - Parent directory path
    fn apply_dirty_state(&self, items: &mut Vec<ProjectedFileInfo>, parent_path: &str) {
        let mut seen_names: HashSet<String> = HashSet::new();

        // Remove deleted items, update modified items
        items.retain_mut(|item| {
            let item_path: String = if parent_path.is_empty() {
                item.name.clone()
            } else {
                format!("{}\\{}", parent_path, item.name)
            };

            // Check if deleted
            if self.dirty_files.is_deleted(&item_path) || self.dirty_dirs.is_deleted(&item_path) {
                return false;
            }

            // Update size/mtime if modified
            if let Some(new_size) = self.dirty_files.get_size(&item_path) {
                item.size = new_size;
            }

            seen_names.insert(item.name.to_lowercase());
            true
        });

        // Add new files
        for (name, size) in self.dirty_files.get_new_files_in_dir(parent_path) {
            let name_lower: String = name.to_lowercase();
            if !seen_names.contains(&name_lower) {
                items.push(ProjectedFileInfo {
                    name,
                    size,
                    is_folder: false,
                    content_hash: None,
                    symlink_target: None,
                });
                seen_names.insert(name_lower);
            }
        }

        // Add new directories
        for name in self.dirty_dirs.get_new_dirs_in_parent(parent_path) {
            let name_lower: String = name.to_lowercase();
            if !seen_names.contains(&name_lower) {
                items.push(ProjectedFileInfo {
                    name,
                    size: 0,
                    is_folder: true,
                    content_hash: None,
                    symlink_target: None,
                });
                seen_names.insert(name_lower);
            }
        }

        // Re-sort after modifications
        items.sort_by(|a, b| prj_file_name_compare(&a.name, &b.name));
    }
}
```

---

## Configuration

```rust
use std::time::Duration;

/// ProjFS virtualizer configuration.
#[derive(Debug, Clone)]
pub struct ProjFsOptions {
    /// Virtualization root path.
    pub root_path: PathBuf,
    /// Instance GUID (unique per mount).
    pub instance_guid: GUID,
    /// Number of worker threads for async executor.
    /// Default: 4 (same as FUSE AsyncExecutor default).
    pub worker_thread_count: usize,
    /// Default timeout for async operations.
    /// Default: None (no timeout).
    pub default_timeout: Option<Duration>,
    /// ProjFS pool thread count.
    /// Default: 0 (let ProjFS decide).
    pub pool_thread_count: u32,
    /// ProjFS concurrent thread count.
    /// Default: 0 (let ProjFS decide).
    pub concurrent_thread_count: u32,
    /// Memory pool configuration.
    pub memory_pool: MemoryPoolConfig,
    /// Notifications to receive.
    pub notifications: NotificationMask,
}

impl Default for ProjFsOptions {
    fn default() -> Self {
        Self {
            root_path: PathBuf::new(),
            instance_guid: GUID::new().unwrap(),
            worker_thread_count: 4,  // Match FUSE default
            default_timeout: None,
            pool_thread_count: 0,
            concurrent_thread_count: 0,
            memory_pool: MemoryPoolConfig::default(),
            notifications: NotificationMask::for_writable(),
        }
    }
}

/// Notification mask.
#[derive(Debug, Clone, Default)]
pub struct NotificationMask {
    pub new_file_created: bool,
    pub file_modified: bool,
    pub file_deleted: bool,
    pub file_renamed: bool,
    pub pre_delete: bool,
    pub pre_rename: bool,
}

impl NotificationMask {
    /// Mask for writable VFS (track all modifications).
    pub fn for_writable() -> Self {
        Self {
            new_file_created: true,
            file_modified: true,
            file_deleted: true,
            file_renamed: true,
            pre_delete: false,
            pre_rename: false,
        }
    }
}
```

---

## Lifecycle

```rust
impl ProjFsVirtualizer {
    /// Create new virtualizer.
    ///
    /// # Arguments
    /// * `manifest` - Manifest to project
    /// * `storage` - Storage client for S3 CAS
    /// * `options` - Configuration options
    pub fn new(
        manifest: &Manifest,
        storage: Arc<dyn StorageClient>,
        options: ProjFsOptions,
    ) -> Result<Self, VfsError> {
        // Build projection from manifest
        let projection = Arc::new(ManifestProjection::from_manifest(manifest));

        // Create callbacks layer
        let callbacks = Arc::new(VfsCallbacks::new(
            projection,
            storage,
            options.memory_pool.clone(),
        ));

        // Create async executor - same pattern as FUSE
        let executor_config = ExecutorConfig::default()
            .with_worker_threads(options.worker_thread_count)
            .with_default_timeout(options.default_timeout);
        let executor = Arc::new(AsyncExecutor::new(executor_config));

        Ok(Self {
            callbacks,
            executor,
            active_enumerations: RwLock::new(HashMap::new()),
            context: RwLock::new(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT::default()),
            options,
        })
    }

    /// Start virtualization.
    pub fn start(&self) -> Result<(), VfsError> {
        // Ensure directory exists
        std::fs::create_dir_all(&self.options.root_path)?;

        unsafe {
            // Mark as virtualization root
            PrjMarkDirectoryAsPlaceholder(
                &self.options.root_path,
                PCWSTR::null(),
                std::ptr::null(),
                &self.options.instance_guid,
            ).ok()?;

            // Set up callbacks
            let callbacks = PRJ_CALLBACKS {
                StartDirectoryEnumerationCallback: Some(Self::start_dir_enum_cb),
                EndDirectoryEnumerationCallback: Some(Self::end_dir_enum_cb),
                GetDirectoryEnumerationCallback: Some(Self::get_dir_enum_cb),
                GetPlaceholderInfoCallback: Some(Self::get_placeholder_info_cb),
                GetFileDataCallback: Some(Self::get_file_data_cb),
                QueryFileNameCallback: Some(Self::query_file_name_cb),
                NotificationCallback: Some(Self::notification_cb),
                CancelCommandCallback: Some(Self::cancel_command_cb),
            };

            // Start virtualization
            let mut context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT = std::mem::zeroed();
            PrjStartVirtualizing(
                &self.options.root_path,
                &callbacks,
                self as *const _ as *const c_void,
                std::ptr::null(), // Use default options
                &mut context,
            ).ok()?;

            *self.context.write() = context;
        }

        Ok(())
    }

    /// Stop virtualization.
    pub fn stop(&self) {
        // Cancel any in-flight async operations
        self.executor.cancel_all();

        let context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT = *self.context.read();
        if !context.is_invalid() {
            unsafe {
                PrjStopVirtualizing(context);
            }
        }
    }
}

impl Drop for ProjFsVirtualizer {
    fn drop(&mut self) {
        self.stop();
        // AsyncExecutor handles its own cleanup on drop
    }
}
```

---

## Manifest Version Support

| Feature | V1 (v2023-03-03) | V2 (v2025-12-04-beta) |
|---------|------------------|----------------------|
| Files | `hash`, `size`, `mtime` | Same + optional `chunkhashes` |
| Directories | Implicit from paths | Explicit `dirs` array |
| Symlinks | Not supported | `symlink_target` field |
| Chunking | Not supported | Files >256MB use chunks |

```rust
impl ManifestProjection {
    /// Build projection from manifest.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        match manifest {
            Manifest::V2023_03_03(m) => Self::from_v1(m),
            Manifest::V2025_12_04_beta(m) => Self::from_v2(m),
        }
    }

    fn from_v1(m: &ManifestV1) -> Self {
        let mut root = FolderData::new();

        for entry in &m.paths {
            // V1: All entries are files, directories implicit
            root.add_file_path(&entry.path, FileData {
                name: get_filename(&entry.path),
                size: entry.size,
                content_hash: ContentHash::Single(entry.hash.clone()),
                mtime: entry.mtime,
            });
        }

        root.sort_all();

        Self {
            root,
            manifest_version: ManifestVersion::V2023_03_03,
            hash_algorithm: m.hash_alg,
        }
    }

    fn from_v2(m: &ManifestV2) -> Self {
        let mut root = FolderData::new();

        // V2: Create explicit directories first
        for dir in &m.dirs {
            if !dir.deleted {
                root.add_directory(&dir.name);
            }
        }

        // Then add files/symlinks
        for entry in &m.paths {
            if entry.deleted {
                continue;
            }

            if let Some(ref target) = entry.symlink_target {
                root.add_symlink_path(&entry.path, SymlinkData {
                    name: get_filename(&entry.path),
                    target: target.clone(),
                });
            } else {
                let content_hash: ContentHash = match &entry.chunkhashes {
                    Some(chunks) => ContentHash::Chunked(chunks.clone()),
                    None => ContentHash::Single(entry.hash.clone()),
                };

                root.add_file_path(&entry.path, FileData {
                    name: get_filename(&entry.path),
                    size: entry.size,
                    content_hash,
                    mtime: entry.mtime,
                });
            }
        }

        root.sort_all();

        Self {
            root,
            manifest_version: ManifestVersion::V2025_12_04_beta,
            hash_algorithm: m.hash_alg,
        }
    }
}
```

---

## Project Structure

```
crates/vfs-projfs/
├── Cargo.toml
├── src/
│   ├── lib.rs                  # Public API
│   ├── error.rs                # VfsError
│   ├── options.rs              # ProjFsOptions
│   │
│   ├── projection/             # Layer 1: Backing store
│   │   ├── mod.rs
│   │   ├── manifest.rs         # ManifestProjection
│   │   ├── folder.rs           # FolderData, SortedFolderEntries
│   │   └── file.rs             # FileData, ProjectedFileInfo
│   │
│   ├── callbacks/              # Layer 2: Coordination
│   │   ├── mod.rs
│   │   ├── vfs_callbacks.rs    # VfsCallbacks
│   │   ├── modified_paths.rs   # ModifiedPathsDatabase
│   │   └── background.rs       # BackgroundTaskRunner
│   │
│   ├── virtualizer/            # Layer 3: ProjFS
│   │   ├── mod.rs
│   │   ├── projfs.rs           # ProjFsVirtualizer
│   │   ├── enumeration.rs      # ActiveEnumeration
│   │   ├── sendable.rs         # SendableContext wrapper
│   │   └── callbacks/          # ProjFS callback implementations
│   │       ├── mod.rs
│   │       ├── enumeration.rs
│   │       ├── placeholder.rs
│   │       ├── file_data.rs
│   │       └── notification.rs
│   │
│   └── util/                   # Utilities
│       ├── mod.rs
│       ├── wstr.rs             # Wide string conversion
│       ├── filetime.rs         # FILETIME conversion
│       └── compare.rs          # PrjFileNameCompare wrapper
│
└── examples/
    └── mount.rs

# Note: AsyncExecutor is reused from crates/vfs/src/executor.rs
# No custom worker pool needed - same pattern as FUSE
```

---

## Cross-Platform Comparison

| Aspect | FUSE (Linux) | FSKit (macOS) | ProjFS (Windows) |
|--------|--------------|---------------|------------------|
| Callback Model | Sync | Async trait | Sync |
| Async Bridge | `AsyncExecutor` | Native | `AsyncExecutor` (shared) |
| Blocking Mechanism | `oneshot::blocking_recv()` | N/A | `oneshot::blocking_recv()` |
| Enumeration | Per-call | Per-call | Session-based |
| Sorting | None | None | PrjFileNameCompare |
| Buffer Alignment | None | None | Required |
| Pre-load Data | No | No | Yes (VFSForGit pattern) |
| Cancellation | `executor.cancel_all()` | Native | `executor.cancel_all()` |

---

## Key Differences from Original Design

1. **Layered Architecture**: Clear separation into Projection → Callbacks → Virtualizer
2. **Pre-loaded Enumeration**: All data loaded in `StartDirectoryEnumerationCallback`
3. **AsyncExecutor Reuse**: Same pattern as FUSE - dedicated runtime, oneshot channels
4. **Background Tasks**: Non-critical work queued to background runner
5. **Fast/Slow Path**: Sync return for memory operations, async for I/O
6. **ActiveEnumeration**: Stateful iterator matching VFSForGit pattern

---

## Verified ProjFS Behaviors

| Topic | Verified Answer |
|-------|-----------------|
| Placeholder path | Full relative path with backing store case |
| Buffer handle lifetime | Valid until `PrjCompleteCommand` |
| Async completion | `extendedParameters = NULL` for file data |
| Tombstone + recreate | Tombstone replaced, new file becomes full |
| Case-insensitive lookup | Use `PrjFileNameCompare(a, b) == 0` |
| Concurrent callbacks | Different sessions concurrent; same session sequential |
| Pre-op error codes | Any HRESULT propagates to application |
| Partial reads | Must cover requested range; can return more |
| Directory placeholders | Never become hydrated |
| VersionInfo | Contains what we stored; may be NULL |

---

## References

- [VFSForGit Source](https://github.com/microsoft/VFSForGit)
- [ProjFS Programming Guide](https://learn.microsoft.com/en-us/windows/win32/projfs/projfs-programming-guide)
- [ProjFS API Reference](https://learn.microsoft.com/en-us/windows/win32/api/projectedfslib/)

---

## Analysis: Memory Copy Optimizations

### Issue 1: Enumeration Data Cloning

**Problem:** `get_projected_items()` returns `Vec<ProjectedFileInfo>` which clones all items:

```rust
// Current: Clones all items
pub fn get_projected_items(&self, relative_path: &str) -> Vec<ProjectedFileInfo> {
    let mut items: Vec<ProjectedFileInfo> = self.projection
        .get_projected_items(relative_path)
        .unwrap_or_default();  // Clone here
    self.apply_dirty_state(&mut items, relative_path);  // More clones
    items
}
```

**Solution:** Return references or use `Arc<[ProjectedFileInfo]>` for immutable projection data:

```rust
/// Projected file info - immutable, shareable.
#[derive(Clone)]
pub struct ProjectedFileInfo {
    /// Use Arc<str> instead of String to avoid cloning.
    pub name: Arc<str>,
    pub size: u64,
    pub is_folder: bool,
    /// Use Arc<str> for content hash.
    pub content_hash: Option<Arc<str>>,
    pub symlink_target: Option<Arc<str>>,
}

/// Get projected items - returns Arc to avoid cloning.
///
/// For directories without dirty state, returns shared reference.
/// For directories with dirty state, creates new Vec with modifications.
pub fn get_projected_items(&self, relative_path: &str) -> ProjectedItems {
    let base_items: Arc<[ProjectedFileInfo]> = self.projection
        .get_projected_items_arc(relative_path);

    // Fast path: no dirty state for this directory
    if !self.has_dirty_state_in_dir(relative_path) {
        return ProjectedItems::Shared(base_items);
    }

    // Slow path: need to apply dirty state
    let mut items: Vec<ProjectedFileInfo> = base_items.to_vec();
    self.apply_dirty_state(&mut items, relative_path);
    ProjectedItems::Owned(items)
}

/// Enumeration items - either shared or owned.
pub enum ProjectedItems {
    /// Shared reference to projection data (no copy).
    Shared(Arc<[ProjectedFileInfo]>),
    /// Owned copy with dirty state applied.
    Owned(Vec<ProjectedFileInfo>),
}
```

### Issue 2: File Data Double Copy

**Problem:** File data is copied twice - once from S3/pool, once to ProjFS aligned buffer:

```rust
// Current: Two copies
let result: Result<Vec<u8>, VfsError> = callbacks
    .fetch_file_content(&content_hash, byte_offset, length)
    .await;  // Copy 1: S3 → Vec<u8>

match write_file_data_aligned(context, &data_stream_id, &data, byte_offset) {
    // Copy 2: Vec<u8> → PrjAllocateAlignedBuffer
}
```

**Solution:** Use memory pool handles that can be written directly to aligned buffers:

```rust
/// Fetch file content directly into aligned buffer.
///
/// # Arguments
/// * `context` - ProjFS context for buffer allocation
/// * `content_hash` - Hash of content to fetch
/// * `offset` - Byte offset
/// * `length` - Bytes to read
///
/// # Returns
/// Aligned buffer ready for PrjWriteFileData.
pub async fn fetch_file_content_aligned(
    &self,
    context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    content_hash: &str,
    offset: u64,
    length: u32,
) -> Result<AlignedBuffer, VfsError> {
    // Allocate aligned buffer first
    let buffer: AlignedBuffer = unsafe {
        AlignedBuffer::new(context, length as usize)?
    };

    // Check memory pool - if hit, copy directly to aligned buffer
    if let Some(handle) = self.memory_pool.get(content_hash) {
        let src: &[u8] = &handle.data()[offset as usize..][..length as usize];
        buffer.copy_from_slice(src);
        return Ok(buffer);
    }

    // Fetch from S3 directly into aligned buffer
    self.storage.retrieve_into(
        content_hash,
        self.hash_algorithm,
        offset,
        buffer.as_mut_slice(),
    ).await?;

    Ok(buffer)
}

/// Aligned buffer wrapper for ProjFS.
pub struct AlignedBuffer {
    ptr: *mut u8,
    len: usize,
    context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
}

impl AlignedBuffer {
    /// Allocate aligned buffer.
    unsafe fn new(
        context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        len: usize,
    ) -> Result<Self, VfsError> {
        let ptr: *mut c_void = PrjAllocateAlignedBuffer(context, len);
        if ptr.is_null() {
            return Err(VfsError::OutOfMemory);
        }
        Ok(Self {
            ptr: ptr as *mut u8,
            len,
            context,
        })
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    fn copy_from_slice(&mut self, src: &[u8]) {
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr, src.len().min(self.len));
        }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe {
            PrjFreeAlignedBuffer(self.ptr as *mut c_void);
        }
    }
}
```

### Issue 3: String Conversions in Callbacks

**Problem:** Wide string conversion allocates on every callback:

```rust
// Current: Allocates String on every callback
let relative_path: String = wstr_to_string((*callback_data).FilePathName);
```

**Solution:** Use stack-allocated buffer for common path lengths:

```rust
use smallvec::SmallVec;

/// Convert wide string to UTF-8 with stack allocation for common sizes.
///
/// Paths under 256 chars use stack allocation, longer paths heap allocate.
fn wstr_to_string_fast(s: PCWSTR) -> SmallString {
    if s.is_null() {
        return SmallString::new();
    }

    unsafe {
        let len: usize = wcslen(s.as_ptr());
        if len == 0 {
            return SmallString::new();
        }

        // Stack buffer for common path lengths
        let mut buffer: SmallVec<[u8; 512]> = SmallVec::new();
        
        // Convert UTF-16 to UTF-8
        let wide_slice: &[u16] = std::slice::from_raw_parts(s.as_ptr(), len);
        for c in char::decode_utf16(wide_slice.iter().copied()) {
            match c {
                Ok(ch) => {
                    let mut buf: [u8; 4] = [0; 4];
                    let encoded: &str = ch.encode_utf8(&mut buf);
                    buffer.extend_from_slice(encoded.as_bytes());
                }
                Err(_) => buffer.push(b'?'),
            }
        }

        SmallString::from_utf8(buffer)
    }
}

/// Stack-allocated string for common sizes.
type SmallString = SmallVec<[u8; 512]>;
```

### Issue 4: Content Hash Storage

**Problem:** Content hashes stored as `String` in multiple places:

```rust
pub struct FileData {
    content_hash: ContentHash,  // Contains String
}

pub enum ContentHash {
    Single(String),           // 24 bytes + heap allocation
    Chunked(Vec<String>),     // Many heap allocations
}
```

**Solution:** Use fixed-size byte arrays for hashes:

```rust
/// Content hash - fixed size, no heap allocation.
#[derive(Clone, Copy)]
pub enum ContentHash {
    /// XXH128 hash (16 bytes).
    Xxh128([u8; 16]),
    /// SHA256 hash (32 bytes).
    Sha256([u8; 32]),
}

/// Chunked file content - hashes stored contiguously.
pub struct ChunkedContent {
    /// All chunk hashes stored contiguously.
    /// For XXH128: 16 bytes per chunk.
    hashes: Box<[u8]>,
    /// Hash algorithm (determines bytes per hash).
    algorithm: HashAlgorithm,
    /// Number of chunks.
    chunk_count: u32,
}

impl ChunkedContent {
    /// Get hash for chunk index.
    fn get_chunk_hash(&self, index: usize) -> &[u8] {
        let hash_size: usize = self.algorithm.hash_size();
        let start: usize = index * hash_size;
        &self.hashes[start..start + hash_size]
    }
}
```

---

## Threading: Reusing FUSE AsyncExecutor Pattern

The existing `AsyncExecutor` from `crates/vfs/src/executor.rs` solves all threading issues elegantly. We reuse this pattern for ProjFS.

### Why AsyncExecutor Works

```text
ProjFS Callback Thread              Executor Background Thread
──────────────────────              ──────────────────────────
        │                                      │
        │ submit(future) ─────────────────────►│
        │                                      │ spawn task on dedicated runtime
        │ oneshot::blocking_recv() ◄───────────│ send result via typed channel
        │                                      │
```

**Key insight:** The callback thread blocks on `oneshot::blocking_recv()`, which is just a condvar wait - **not** Tokio's `block_on()`. This avoids:
1. Panic from calling `block_on()` inside Tokio context
2. Deadlocks from blocking Tokio worker threads
3. Need for `Handle::current()` (which panics outside Tokio)

### Reusing AsyncExecutor for ProjFS

```rust
use vfs::AsyncExecutor;  // Reuse from crates/vfs

pub struct ProjFsVirtualizer {
    /// Async executor for bridging sync callbacks to async I/O.
    /// Same pattern as FUSE - dedicated runtime in background thread.
    executor: Arc<AsyncExecutor>,
    /// Callbacks for coordination.
    callbacks: Arc<VfsCallbacks>,
    /// Active enumeration sessions.
    active_enumerations: RwLock<HashMap<GUID, ActiveEnumeration>>,
    /// ProjFS virtualization context.
    context: RwLock<PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT>,
    /// Configuration options.
    options: ProjFsOptions,
}
```

### GetFileDataCallback with AsyncExecutor

```rust
impl ProjFsVirtualizer {
    /// Get file data callback - bridges to async via executor.
    ///
    /// Uses the same pattern as FUSE: submit async work to executor,
    /// block on typed oneshot channel (not Tokio block_on).
    unsafe extern "system" fn get_file_data_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        byte_offset: u64,
        length: u32,
    ) -> HRESULT {
        let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);

        // Extract data from callback (only valid during callback)
        let relative_path: String = wstr_to_string((*callback_data).FilePathName);
        let data_stream_id: GUID = (*callback_data).DataStreamId;
        let context = SendableContext((*callback_data).NamespaceVirtualizationContext);

        // Get content hash
        let content_hash: String = match extract_content_hash(&(*callback_data).VersionInfo) {
            Some(hash) => hash,
            None => match this.callbacks.projection.get_file_info(&relative_path) {
                Some(info) => info.content_hash.unwrap_or_default(),
                None => return HRESULT::from(ERROR_FILE_NOT_FOUND),
            },
        };

        // Clone what we need for async closure
        let callbacks: Arc<VfsCallbacks> = this.callbacks.clone();

        // Use executor.block_on - this blocks on oneshot channel, NOT Tokio
        let result: Result<(), VfsError> = this.executor.block_on(async move {
            // Fetch content (may hit memory pool or S3)
            let data: Vec<u8> = callbacks
                .fetch_file_content(&content_hash, byte_offset, length)
                .await?;

            // Write to ProjFS with aligned buffer
            write_file_data_aligned(context.0, &data_stream_id, &data, byte_offset)?;

            callbacks.on_file_hydrated(&relative_path);
            Ok(())
        });

        match result {
            Ok(Ok(())) => S_OK,
            Ok(Err(e)) => e.to_hresult(),
            Err(ExecutorError::Timeout { .. }) => HRESULT::from(ERROR_TIMEOUT),
            Err(ExecutorError::Cancelled) => HRESULT::from(ERROR_OPERATION_ABORTED),
            Err(ExecutorError::Shutdown) => HRESULT::from(ERROR_SHUTDOWN_IN_PROGRESS),
            Err(ExecutorError::TaskPanicked) => E_FAIL,
        }
    }
}
```

### Async vs Sync Decision

VFSForGit always returns `HResult.Pending` and completes commands asynchronously via `CompleteCommand()`. Our design differs:

| Scenario | Our Approach | VFSForGit Approach |
|----------|--------------|-------------------|
| Enumeration | Sync (`S_OK`) | `HResult.Pending` + async |
| Placeholder info | Sync (`S_OK`) | `HResult.Pending` + async |
| File data | Sync via executor | `HResult.Pending` + async |

**Why we chose sync returns:**
- `AsyncExecutor.block_on()` blocks on a oneshot channel (condvar wait), not Tokio
- Simpler code - no need to track `commandId` or call `PrjCompleteCommand`
- ProjFS has its own thread pool (`poolThreadCount`) that handles concurrent callbacks

**Trade-off:** We hold a ProjFS callback thread during I/O. With default `poolThreadCount` (2x CPU cores), this is acceptable for most workloads. For very high concurrency or slow networks, consider the adaptive approach below.

### When to Use Async Completion

For very large files where we want to free the ProjFS worker thread:

```rust
/// Get file data with async completion for large files.
///
/// Returns ERROR_IO_PENDING for files > threshold, sync for smaller files.
unsafe extern "system" fn get_file_data_cb_adaptive(
    callback_data: *const PRJ_CALLBACK_DATA,
    byte_offset: u64,
    length: u32,
) -> HRESULT {
    let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);
    
    // For small requests, use sync path (simpler)
    const ASYNC_THRESHOLD: u32 = 4 * 1024 * 1024; // 4MB
    if length < ASYNC_THRESHOLD {
        return Self::get_file_data_sync(callback_data, byte_offset, length);
    }

    // For large requests, use async completion
    let command_id: i32 = (*callback_data).CommandId;
    let context = SendableContext((*callback_data).NamespaceVirtualizationContext);
    // ... extract other data ...

    let callbacks: Arc<VfsCallbacks> = this.callbacks.clone();
    let executor: Arc<AsyncExecutor> = this.executor.clone();

    // Spawn on executor without blocking
    std::thread::spawn(move || {
        let result: Result<(), VfsError> = executor.block_on(async move {
            let data: Vec<u8> = callbacks
                .fetch_file_content(&content_hash, byte_offset, length)
                .await?;
            write_file_data_aligned(context.0, &data_stream_id, &data, byte_offset)?;
            Ok(())
        });

        let hresult: HRESULT = match result {
            Ok(Ok(())) => S_OK,
            Ok(Err(e)) => e.to_hresult(),
            Err(_) => E_FAIL,
        };

        unsafe {
            PrjCompleteCommand(context.0, command_id, hresult, std::ptr::null());
        }
    });

    HRESULT::from(ERROR_IO_PENDING)
}
```

### Cancellation Support

The `AsyncExecutor` has built-in cancellation:

```rust
impl ProjFsVirtualizer {
    /// Cancel command callback.
    unsafe extern "system" fn cancel_command_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
    ) {
        let this: &ProjFsVirtualizer = &*((*callback_data).InstanceContext as *const _);
        
        // Cancel all in-flight operations
        // This triggers ExecutorError::Cancelled for block_on_cancellable calls
        this.executor.cancel_all();
    }
}

// Using cancellable version for long operations:
let result = this.executor.block_on_cancellable(async move {
    callbacks.fetch_file_content(&content_hash, byte_offset, length).await
});

match result {
    Ok(data) => { /* success */ }
    Err(ExecutorError::Cancelled) => return HRESULT::from(ERROR_OPERATION_ABORTED),
    Err(e) => { /* other error */ }
}
```

### SendableContext Wrapper

```rust
/// ProjFS context wrapper that is Send.
///
/// # Safety
/// ProjFS documentation states that PrjCompleteCommand can be called
/// from any thread, so the context handle is safe to send across threads.
#[derive(Clone, Copy)]
struct SendableContext(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT);

// SAFETY: ProjFS allows PrjCompleteCommand from any thread
unsafe impl Send for SendableContext {}
unsafe impl Sync for SendableContext {}
```

### Comparison: FUSE vs ProjFS Threading

| Aspect | FUSE | ProjFS |
|--------|------|--------|
| Callback threads | libfuse thread pool | Windows thread pool |
| Async bridge | `AsyncExecutor.block_on()` | Same - reuse `AsyncExecutor` |
| Blocking mechanism | `oneshot::blocking_recv()` | Same |
| Cancellation | `executor.cancel_all()` | Same |
| Runtime | Dedicated Tokio in background | Same |

**The `AsyncExecutor` pattern is platform-agnostic and works identically for ProjFS.**

---

## Summary of Recommended Changes

### Memory Optimizations

| Issue | Current | Recommended |
|-------|---------|-------------|
| Enumeration cloning | `Vec<ProjectedFileInfo>` clone | `Arc<[ProjectedFileInfo]>` sharing |
| File data copy | S3 → Vec → aligned buffer | S3 → aligned buffer directly |
| String allocation | `String` per callback | `SmallVec<[u8; 512]>` stack alloc |
| Hash storage | `String` (heap) | `[u8; 16/32]` (inline) |

### Threading Fixes

| Issue | Solution |
|-------|----------|
| Runtime access | Reuse `AsyncExecutor` - creates dedicated runtime in background thread |
| Blocking | `executor.block_on()` blocks on oneshot channel, not Tokio |
| Cancellation | Built-in `executor.cancel_all()` and `block_on_cancellable()` |
| Context safety | `SendableContext` wrapper with `unsafe impl Send` |

### Key Benefits of Reusing AsyncExecutor

1. **Proven pattern**: Already tested and working in FUSE implementation
2. **No code duplication**: Single executor implementation for all platforms
3. **Consistent behavior**: Same timeout, cancellation, and error handling
4. **Simpler design**: No custom worker pool needed for ProjFS

