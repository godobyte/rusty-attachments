//! Helper functions for FSKit operations.

use fskit_rs::{directory_entries, Item, ItemAttributes, ItemType};

/// Create a directory entry for enumeration.
///
/// # Arguments
/// * `name` - Entry name
/// * `item_id` - Item ID (file_id)
/// * `item_type` - Type of item (file, directory, symlink)
/// * `cookie` - Cookie for pagination
///
/// # Returns
/// Directory entry for FSKit enumeration.
pub fn make_dir_entry(
    name: &str,
    item_id: u64,
    item_type: ItemType,
    cookie: u64,
) -> directory_entries::Entry {
    directory_entries::Entry {
        item: Some(Item {
            name: name.as_bytes().to_vec(),
            attributes: Some(ItemAttributes {
                file_id: Some(item_id),
                r#type: Some(item_type as i32),
                ..Default::default()
            }),
        }),
        next_cookie: cookie,
    }
}

/// Extract filename from a path.
///
/// # Arguments
/// * `path` - Full path string
///
/// # Returns
/// Filename component of the path.
#[allow(dead_code)]
pub fn path_filename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Get parent path from a full path.
///
/// # Arguments
/// * `path` - Full path string
///
/// # Returns
/// Parent directory path, or empty string for root-level items.
#[allow(dead_code)]
pub fn path_parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[..idx],
        None => "",
    }
}

/// Join two path components.
///
/// # Arguments
/// * `parent` - Parent path
/// * `name` - Child name
///
/// # Returns
/// Combined path.
pub fn path_join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", parent, name)
    }
}
