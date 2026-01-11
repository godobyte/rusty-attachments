//! Conversion utilities between VFS types and FSKit types.

use std::time::SystemTime;

use fskit_rs::{Item, ItemAttributes, ItemType};
use prost_types::Timestamp;
use rusty_attachments_vfs::inode::{INode, INodeType};

/// Root item ID (matches VFS ROOT_INODE).
#[allow(dead_code)]
pub const ROOT_ITEM_ID: u64 = 1;

/// Convert a SystemTime to a protobuf Timestamp.
///
/// # Arguments
/// * `time` - System time to convert
///
/// # Returns
/// Protobuf timestamp representation.
pub fn to_proto_timestamp(time: SystemTime) -> Timestamp {
    let duration: std::time::Duration = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: duration.as_secs() as i64,
        nanos: duration.subsec_nanos() as i32,
    }
}

/// Convert VFS INodeType to FSKit ItemType.
///
/// # Arguments
/// * `inode_type` - VFS inode type
///
/// # Returns
/// Corresponding FSKit item type.
pub fn to_item_type(inode_type: INodeType) -> ItemType {
    match inode_type {
        INodeType::File => ItemType::File,
        INodeType::Directory => ItemType::Directory,
        INodeType::Symlink => ItemType::Symlink,
    }
}

/// Convert VFS INode to FSKit ItemAttributes.
///
/// # Arguments
/// * `inode` - VFS inode to convert
///
/// # Returns
/// FSKit item attributes.
pub fn to_item_attributes(inode: &dyn INode) -> ItemAttributes {
    let uid: u32 = unsafe { libc::getuid() };
    let gid: u32 = unsafe { libc::getgid() };
    let mtime: Timestamp = to_proto_timestamp(inode.mtime());
    let link_count: u32 = if inode.inode_type() == INodeType::Directory {
        2
    } else {
        1
    };

    ItemAttributes {
        file_id: Some(inode.id()),
        parent_id: Some(inode.parent_id()),
        r#type: Some(to_item_type(inode.inode_type()) as i32),
        mode: Some(inode.permissions() as u32),
        link_count: Some(link_count),
        uid: Some(uid),
        gid: Some(gid),
        size: Some(inode.size()),
        alloc_size: Some(inode.size()),
        modify_time: Some(mtime),
        access_time: Some(mtime),
        change_time: Some(mtime),
        birth_time: None,
        backup_time: None,
        added_time: None,
        flags: None,
        supports_limited_xattrs: None,
        inhibit_kernel_offloaded_io: None,
    }
}

/// Convert VFS INode to FSKit Item.
///
/// # Arguments
/// * `inode` - VFS inode to convert
/// * `name` - Item name (filename)
///
/// # Returns
/// FSKit item with attributes.
pub fn to_item(inode: &dyn INode, name: &str) -> Item {
    Item {
        attributes: Some(to_item_attributes(inode)),
        name: name.as_bytes().to_vec(),
    }
}

/// Create ItemAttributes for a new file.
///
/// # Arguments
/// * `item_id` - Item ID (file_id)
/// * `parent_id` - Parent directory ID
/// * `size` - File size
/// * `mtime` - Modification time
///
/// # Returns
/// FSKit item attributes for a new file.
pub fn new_file_attributes(
    item_id: u64,
    parent_id: u64,
    size: u64,
    mtime: SystemTime,
) -> ItemAttributes {
    let uid: u32 = unsafe { libc::getuid() };
    let gid: u32 = unsafe { libc::getgid() };
    let timestamp: Timestamp = to_proto_timestamp(mtime);

    ItemAttributes {
        file_id: Some(item_id),
        parent_id: Some(parent_id),
        r#type: Some(ItemType::File as i32),
        mode: Some(0o644),
        link_count: Some(1),
        uid: Some(uid),
        gid: Some(gid),
        size: Some(size),
        alloc_size: Some(size),
        modify_time: Some(timestamp),
        access_time: Some(timestamp),
        change_time: Some(timestamp),
        birth_time: None,
        backup_time: None,
        added_time: None,
        flags: None,
        supports_limited_xattrs: None,
        inhibit_kernel_offloaded_io: None,
    }
}

/// Create ItemAttributes for a new directory.
///
/// # Arguments
/// * `item_id` - Item ID (file_id)
/// * `parent_id` - Parent directory ID
///
/// # Returns
/// FSKit item attributes for a new directory.
pub fn new_dir_attributes(item_id: u64, parent_id: u64) -> ItemAttributes {
    let uid: u32 = unsafe { libc::getuid() };
    let gid: u32 = unsafe { libc::getgid() };
    let now: Timestamp = to_proto_timestamp(SystemTime::now());

    ItemAttributes {
        file_id: Some(item_id),
        parent_id: Some(parent_id),
        r#type: Some(ItemType::Directory as i32),
        mode: Some(0o755),
        link_count: Some(2),
        uid: Some(uid),
        gid: Some(gid),
        size: Some(0),
        alloc_size: Some(0),
        modify_time: Some(now),
        access_time: Some(now),
        change_time: Some(now),
        birth_time: None,
        backup_time: None,
        added_time: None,
        flags: None,
        supports_limited_xattrs: None,
        inhibit_kernel_offloaded_io: None,
    }
}
