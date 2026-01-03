//! ProjFS callback implementations.
//!
//! This module contains the actual ProjFS callback functions that are
//! invoked by the Windows ProjFS driver.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use parking_lot::RwLock;
use windows::core::{GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER,
    ERROR_OPERATION_ABORTED, ERROR_TIMEOUT, E_FAIL, S_OK,
};
use windows::Win32::Storage::ProjectedFileSystem::{
    PRJ_CALLBACK_DATA, PRJ_CALLBACKS, PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN,
    PRJ_DIR_ENTRY_BUFFER_HANDLE, PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    PRJ_NOTIFICATION, PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_DELETED,
    PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_MODIFIED, PRJ_NOTIFICATION_FILE_RENAMED,
    PRJ_NOTIFICATION_NEW_FILE_CREATED, PRJ_NOTIFICATION_PARAMETERS,
    PRJ_NOTIFICATION_PRE_DELETE, PRJ_NOTIFICATION_PRE_RENAME,
    PRJ_PLACEHOLDER_INFO,
    PrjAllocateAlignedBuffer, PrjFillDirEntryBuffer,
    PrjFreeAlignedBuffer, PrjWriteFileData, PrjWritePlaceholderInfo,
};

use rusty_attachments_vfs::{AsyncExecutor, ExecutorError};

use crate::callbacks::VfsCallbacks;
use crate::projection::types::ProjectedFileInfo;
use crate::util::wstr::{pcwstr_to_string, string_to_wide};
use crate::virtualizer::enumeration::ActiveEnumeration;
use crate::virtualizer::sendable::SendableContext;

/// Convert SystemTime to i64 FILETIME value.
///
/// FILETIME represents the number of 100-nanosecond intervals since
/// January 1, 1601 UTC.
///
/// # Arguments
/// * `time` - System time to convert
///
/// # Returns
/// i64 representation of FILETIME.
fn systemtime_to_filetime_i64(time: std::time::SystemTime) -> i64 {
    // FILETIME epoch: January 1, 1601
    // Unix epoch: January 1, 1970
    // Difference: 11644473600 seconds
    const FILETIME_UNIX_DIFF_SECS: u64 = 11644473600;
    const INTERVALS_PER_SEC: u64 = 10_000_000;

    let duration: Duration = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);

    let intervals: u64 = duration.as_secs() * INTERVALS_PER_SEC
        + duration.subsec_nanos() as u64 / 100
        + FILETIME_UNIX_DIFF_SECS * INTERVALS_PER_SEC;

    intervals as i64
}

/// Callback context passed to ProjFS callbacks.
///
/// Contains references to all components needed by callbacks.
pub struct CallbackContext {
    /// VFS callbacks for coordination.
    pub callbacks: Arc<VfsCallbacks>,
    /// Async executor for I/O operations.
    pub executor: Arc<AsyncExecutor>,
    /// Active enumeration sessions.
    pub active_enumerations: RwLock<HashMap<GUID, ActiveEnumeration>>,
}

impl CallbackContext {
    /// Create new callback context.
    ///
    /// # Arguments
    /// * `callbacks` - VFS callbacks
    /// * `executor` - Async executor
    pub fn new(callbacks: Arc<VfsCallbacks>, executor: Arc<AsyncExecutor>) -> Self {
        Self {
            callbacks,
            executor,
            active_enumerations: RwLock::new(HashMap::new()),
        }
    }
}

// ============================================================================
// Callback Implementations
// ============================================================================

/// Start directory enumeration callback.
///
/// Pre-loads ALL enumeration data (fast path - no I/O needed).
/// This ensures GetDirectoryEnumerationCallback is always fast.
pub unsafe extern "system" fn start_dir_enum_cb(
    callback_data: *const PRJ_CALLBACK_DATA,
    enumeration_id: *const GUID,
) -> HRESULT {
    let ctx: &CallbackContext = &*((*callback_data).InstanceContext as *const CallbackContext);

    let relative_path: String = match pcwstr_to_string((*callback_data).FilePathName) {
        Ok(p) => p,
        Err(_) => return E_FAIL,
    };

    tracing::debug!("StartDirectoryEnumeration: {}", relative_path);

    // Fast path: get projected items from memory
    let items: Arc<[ProjectedFileInfo]> = match ctx.callbacks.get_projected_items(&relative_path) {
        Some(items) => items,
        None => Arc::from(vec![]), // Empty directory
    };

    // Create enumeration session with pre-loaded data
    let enumeration = ActiveEnumeration::new(items);

    // Store session
    let mut enumerations = ctx.active_enumerations.write();
    if enumerations.insert(*enumeration_id, enumeration).is_some() {
        tracing::warn!("Enumeration ID collision: {:?}", *enumeration_id);
    }

    S_OK
}

/// Get directory enumeration callback.
///
/// Iterates pre-loaded data (always fast - no I/O).
pub unsafe extern "system" fn get_dir_enum_cb(
    callback_data: *const PRJ_CALLBACK_DATA,
    enumeration_id: *const GUID,
    search_expression: PCWSTR,
    dir_entry_buffer_handle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
) -> HRESULT {
    let ctx: &CallbackContext = &*((*callback_data).InstanceContext as *const CallbackContext);

    let mut enumerations = ctx.active_enumerations.write();
    let enumeration: &mut ActiveEnumeration = match enumerations.get_mut(&*enumeration_id) {
        Some(e) => e,
        None => {
            tracing::error!("Unknown enumeration ID: {:?}", *enumeration_id);
            return E_FAIL;
        }
    };

    // Handle restart scan flag
    let restart: bool = ((*callback_data).Flags.0 & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN.0) != 0;
    let filter: Option<String> = if search_expression.is_null() {
        None
    } else {
        pcwstr_to_string(search_expression).ok()
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

        let result: HRESULT = fill_dir_entry_buffer(dir_entry_buffer_handle, item);

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

/// End directory enumeration callback.
///
/// Cleans up enumeration session.
pub unsafe extern "system" fn end_dir_enum_cb(
    callback_data: *const PRJ_CALLBACK_DATA,
    enumeration_id: *const GUID,
) -> HRESULT {
    let ctx: &CallbackContext = &*((*callback_data).InstanceContext as *const CallbackContext);

    let mut enumerations = ctx.active_enumerations.write();
    enumerations.remove(&*enumeration_id);

    S_OK
}

/// Get placeholder info callback.
///
/// Fast path for manifest data, returns sync.
pub unsafe extern "system" fn get_placeholder_info_cb(
    callback_data: *const PRJ_CALLBACK_DATA,
) -> HRESULT {
    let ctx: &CallbackContext = &*((*callback_data).InstanceContext as *const CallbackContext);

    let relative_path: String = match pcwstr_to_string((*callback_data).FilePathName) {
        Ok(p) => p,
        Err(_) => return E_FAIL,
    };

    tracing::debug!("GetPlaceholderInfo: {}", relative_path);

    // Check if path is projected (fast - in memory)
    let (canonical_name, is_folder) = match ctx.callbacks.is_path_projected(&relative_path) {
        Some(result) => result,
        None => return HRESULT::from(ERROR_FILE_NOT_FOUND),
    };

    if is_folder {
        // Write folder placeholder
        return write_folder_placeholder(
            (*callback_data).NamespaceVirtualizationContext,
            &relative_path,
            &canonical_name,
        );
    }

    // Get file info for placeholder
    let file_info: ProjectedFileInfo = match ctx.callbacks.get_file_info(&relative_path) {
        Some(info) => info,
        None => return HRESULT::from(ERROR_FILE_NOT_FOUND),
    };

    // Write file placeholder
    write_file_placeholder(
        (*callback_data).NamespaceVirtualizationContext,
        &relative_path,
        &canonical_name,
        &file_info,
    )
}

/// Get file data callback.
///
/// Uses AsyncExecutor.block_on() - blocks on oneshot channel, not Tokio.
pub unsafe extern "system" fn get_file_data_cb(
    callback_data: *const PRJ_CALLBACK_DATA,
    byte_offset: u64,
    length: u32,
) -> HRESULT {
    let ctx: &CallbackContext = &*((*callback_data).InstanceContext as *const CallbackContext);

    let relative_path: String = match pcwstr_to_string((*callback_data).FilePathName) {
        Ok(p) => p,
        Err(_) => return E_FAIL,
    };

    tracing::debug!(
        "GetFileData: {} offset={} length={}",
        relative_path,
        byte_offset,
        length
    );

    let data_stream_id: GUID = (*callback_data).DataStreamId;
    let context = SendableContext::new((*callback_data).NamespaceVirtualizationContext);

    // Get content hash from file info
    let content_hash: String = match ctx.callbacks.get_file_info(&relative_path) {
        Some(info) => match info.content_hash {
            Some(ref hash) => hash.to_string(),
            None => return HRESULT::from(ERROR_FILE_NOT_FOUND),
        },
        None => return HRESULT::from(ERROR_FILE_NOT_FOUND),
    };

    // Clone for async closure
    let callbacks: Arc<VfsCallbacks> = ctx.callbacks.clone();
    let path_clone: String = relative_path.clone();

    // Use executor.block_on - blocks on oneshot channel, NOT Tokio
    let result = ctx.executor.block_on_cancellable(async move {
        // Fetch content (may hit memory pool or S3)
        let data: Vec<u8> = callbacks
            .fetch_file_content(&content_hash, byte_offset, length)
            .await?;

        // Write to ProjFS with aligned buffer
        write_file_data_aligned(context, data_stream_id, &data, byte_offset)?;

        callbacks.on_file_hydrated(&path_clone);
        Ok::<(), rusty_attachments_vfs::VfsError>(())
    });

    match result {
        Ok(Ok(())) => S_OK,
        Ok(Err(e)) => {
            tracing::error!("GetFileData error: {}", e);
            E_FAIL
        }
        Err(ExecutorError::Timeout { .. }) => HRESULT::from(ERROR_TIMEOUT),
        Err(ExecutorError::Cancelled) => HRESULT::from(ERROR_OPERATION_ABORTED),
        Err(ExecutorError::Shutdown) => E_FAIL,
        Err(ExecutorError::TaskPanicked) => E_FAIL,
    }
}

/// Query file name callback.
///
/// Checks if a path exists in the projection.
pub unsafe extern "system" fn query_file_name_cb(callback_data: *const PRJ_CALLBACK_DATA) -> HRESULT {
    let ctx: &CallbackContext = &*((*callback_data).InstanceContext as *const CallbackContext);

    let relative_path: String = match pcwstr_to_string((*callback_data).FilePathName) {
        Ok(p) => p,
        Err(_) => return E_FAIL,
    };

    match ctx.callbacks.is_path_projected(&relative_path) {
        Some(_) => S_OK,
        None => HRESULT::from(ERROR_FILE_NOT_FOUND),
    }
}

/// Notification callback.
///
/// Queues work to background - returns immediately.
pub unsafe extern "system" fn notification_cb(
    callback_data: *const PRJ_CALLBACK_DATA,
    is_directory: windows::Win32::Foundation::BOOLEAN,
    notification: PRJ_NOTIFICATION,
    destination_file_name: PCWSTR,
    _operation_parameters: *mut PRJ_NOTIFICATION_PARAMETERS,
) -> HRESULT {
    let ctx: &CallbackContext = &*((*callback_data).InstanceContext as *const CallbackContext);

    let relative_path: String = match pcwstr_to_string((*callback_data).FilePathName) {
        Ok(p) => p,
        Err(_) => return S_OK, // Don't fail on notification errors
    };

    match notification {
        PRJ_NOTIFICATION_NEW_FILE_CREATED => {
            if is_directory.as_bool() {
                tracing::debug!("Folder created: {}", relative_path);
            } else {
                ctx.callbacks.on_file_created(&relative_path);
            }
        }

        PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_MODIFIED => {
            ctx.callbacks.on_file_modified(&relative_path);
        }

        PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_DELETED => {
            if is_directory.as_bool() {
                tracing::debug!("Folder deleted: {}", relative_path);
            } else {
                ctx.callbacks.on_file_deleted(&relative_path);
            }
        }

        PRJ_NOTIFICATION_FILE_RENAMED => {
            let dest_path: String = if destination_file_name.is_null() {
                String::new()
            } else {
                pcwstr_to_string(destination_file_name).unwrap_or_default()
            };
            ctx.callbacks.on_file_renamed(&relative_path, &dest_path);
        }

        PRJ_NOTIFICATION_PRE_DELETE | PRJ_NOTIFICATION_PRE_RENAME => {
            // Pre-operation: can veto by returning error
            // For now, allow all operations
        }

        _ => {}
    }

    S_OK
}

/// Cancel command callback.
///
/// Signals cancellation via the executor's built-in cancellation.
pub unsafe extern "system" fn cancel_command_cb(callback_data: *const PRJ_CALLBACK_DATA) {
    let ctx: &CallbackContext = &*((*callback_data).InstanceContext as *const CallbackContext);

    tracing::debug!("CancelCommand received");
    ctx.executor.cancel_all();
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Fill directory entry buffer with a single item.
///
/// # Arguments
/// * `buffer_handle` - ProjFS buffer handle
/// * `item` - Item to add
///
/// # Returns
/// S_OK on success, ERROR_INSUFFICIENT_BUFFER if buffer full.
fn fill_dir_entry_buffer(
    buffer_handle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
    item: &ProjectedFileInfo,
) -> HRESULT {
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    };
    use windows::Win32::Storage::ProjectedFileSystem::PRJ_FILE_BASIC_INFO;

    let name_wide: Vec<u16> = string_to_wide(&item.name);
    let filetime: i64 = systemtime_to_filetime_i64(item.mtime);

    let file_attributes: u32 = if item.is_folder {
        FILE_ATTRIBUTE_DIRECTORY.0
    } else {
        FILE_ATTRIBUTE_NORMAL.0
    };

    let basic_info = PRJ_FILE_BASIC_INFO {
        IsDirectory: windows::Win32::Foundation::BOOLEAN(if item.is_folder { 1 } else { 0 }),
        FileSize: item.size as i64,
        CreationTime: filetime,
        LastAccessTime: filetime,
        LastWriteTime: filetime,
        ChangeTime: filetime,
        FileAttributes: file_attributes,
    };

    unsafe {
        match PrjFillDirEntryBuffer(
            PCWSTR::from_raw(name_wide.as_ptr()),
            Some(&basic_info),
            buffer_handle,
        ) {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        }
    }
}

/// Write folder placeholder info.
///
/// # Arguments
/// * `context` - ProjFS context
/// * `relative_path` - Path relative to root
/// * `_canonical_name` - Canonical folder name (unused)
fn write_folder_placeholder(
    context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    relative_path: &str,
    _canonical_name: &str,
) -> HRESULT {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;

    let path_wide: Vec<u16> = string_to_wide(relative_path);

    let placeholder_info = PRJ_PLACEHOLDER_INFO {
        FileBasicInfo: windows::Win32::Storage::ProjectedFileSystem::PRJ_FILE_BASIC_INFO {
            IsDirectory: windows::Win32::Foundation::BOOLEAN(1),
            FileSize: 0,
            CreationTime: 0,
            LastAccessTime: 0,
            LastWriteTime: 0,
            ChangeTime: 0,
            FileAttributes: FILE_ATTRIBUTE_DIRECTORY.0,
        },
        ..Default::default()
    };

    unsafe {
        match PrjWritePlaceholderInfo(
            context,
            PCWSTR::from_raw(path_wide.as_ptr()),
            &placeholder_info,
            std::mem::size_of::<PRJ_PLACEHOLDER_INFO>() as u32,
        ) {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        }
    }
}

/// Write file placeholder info.
///
/// # Arguments
/// * `context` - ProjFS context
/// * `relative_path` - Path relative to root
/// * `_canonical_name` - Canonical file name (unused)
/// * `file_info` - File information
fn write_file_placeholder(
    context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    relative_path: &str,
    _canonical_name: &str,
    file_info: &ProjectedFileInfo,
) -> HRESULT {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;

    let path_wide: Vec<u16> = string_to_wide(relative_path);
    let filetime: i64 = systemtime_to_filetime_i64(file_info.mtime);

    let placeholder_info = PRJ_PLACEHOLDER_INFO {
        FileBasicInfo: windows::Win32::Storage::ProjectedFileSystem::PRJ_FILE_BASIC_INFO {
            IsDirectory: windows::Win32::Foundation::BOOLEAN(0),
            FileSize: file_info.size as i64,
            CreationTime: filetime,
            LastAccessTime: filetime,
            LastWriteTime: filetime,
            ChangeTime: filetime,
            FileAttributes: FILE_ATTRIBUTE_NORMAL.0,
        },
        ..Default::default()
    };

    unsafe {
        match PrjWritePlaceholderInfo(
            context,
            PCWSTR::from_raw(path_wide.as_ptr()),
            &placeholder_info,
            std::mem::size_of::<PRJ_PLACEHOLDER_INFO>() as u32,
        ) {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        }
    }
}

/// Write file data with aligned buffer.
///
/// # Arguments
/// * `context` - ProjFS context (sendable wrapper)
/// * `data_stream_id` - Data stream GUID
/// * `data` - File data to write
/// * `byte_offset` - Offset in file
fn write_file_data_aligned(
    context: SendableContext,
    data_stream_id: GUID,
    data: &[u8],
    byte_offset: u64,
) -> Result<(), rusty_attachments_vfs::VfsError> {
    unsafe {
        // Allocate aligned buffer
        let aligned_buffer: *mut c_void = PrjAllocateAlignedBuffer(context.inner(), data.len());
        if aligned_buffer.is_null() {
            return Err(rusty_attachments_vfs::VfsError::MemoryPoolError(
                "PrjAllocateAlignedBuffer returned null".to_string(),
            ));
        }

        // Copy data to aligned buffer
        std::ptr::copy_nonoverlapping(data.as_ptr(), aligned_buffer as *mut u8, data.len());

        // Write to ProjFS
        let result = PrjWriteFileData(
            context.inner(),
            &data_stream_id,
            aligned_buffer,
            byte_offset,
            data.len() as u32,
        );

        // Free aligned buffer
        PrjFreeAlignedBuffer(aligned_buffer);

        match result {
            Ok(()) => Ok(()),
            Err(e) => Err(rusty_attachments_vfs::VfsError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("PrjWriteFileData failed: {:?}", e),
            ))),
        }
    }
}

/// Build ProjFS callbacks structure.
///
/// # Returns
/// PRJ_CALLBACKS structure with all callbacks set.
pub fn build_callbacks() -> PRJ_CALLBACKS {
    PRJ_CALLBACKS {
        StartDirectoryEnumerationCallback: Some(start_dir_enum_cb),
        EndDirectoryEnumerationCallback: Some(end_dir_enum_cb),
        GetDirectoryEnumerationCallback: Some(get_dir_enum_cb),
        GetPlaceholderInfoCallback: Some(get_placeholder_info_cb),
        GetFileDataCallback: Some(get_file_data_cb),
        QueryFileNameCallback: Some(query_file_name_cb),
        NotificationCallback: Some(notification_cb),
        CancelCommandCallback: Some(cancel_command_cb),
    }
}
