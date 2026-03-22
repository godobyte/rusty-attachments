//! Utility functions for relaxed consistency path resolution.

use super::types::RelaxedFileKey;

/// Generate a path-based key for a relaxed consistency file.
///
/// # Arguments
/// * `root_id` - The relaxed root identifier.
/// * `relative_path` - File path relative to the root (posix-normalized).
///
/// # Returns
/// A `RelaxedFileKey` with an XXH128 hash of the relative path.
pub fn relaxed_file_key(root_id: &str, relative_path: &str) -> RelaxedFileKey {
    let normalized: String = normalize_path(relative_path);
    let path_key: String = xxh128_hex(normalized.as_bytes());
    RelaxedFileKey {
        root_id: root_id.to_string(),
        relative_path: normalized,
        path_key,
    }
}

/// Compute the relative path of a VFS path within its relaxed root.
///
/// # Arguments
/// * `vfs_path` - The full VFS path of the inode (e.g., "shared/project/file.png").
/// * `root_mount_path` - The VFS mount path of the relaxed root (e.g., "shared").
///
/// # Returns
/// The relative path (e.g., "project/file.png").
pub fn relaxed_relative_path(vfs_path: &str, root_mount_path: &str) -> String {
    let relative: &str = vfs_path
        .strip_prefix(root_mount_path)
        .unwrap_or(vfs_path)
        .trim_start_matches('/');
    normalize_path(relative)
}

/// Build the S3 key for a pending upload completion marker.
///
/// # Arguments
/// * `root_prefix` - S3 root prefix (e.g., "DeadlineCloud").
/// * `root_id` - The relaxed root identifier.
/// * `path_key` - The XXH128 hash of the relative path.
///
/// # Returns
/// The full S3 key (e.g., "DeadlineCloud/PendingUploads/a1b2c3/abc123.xxh128").
pub fn s3_marker_key(root_prefix: &str, root_id: &str, path_key: &str) -> String {
    format!(
        "{}/PendingUploads/{}/{}.xxh128",
        root_prefix, root_id, path_key
    )
}

/// Normalize a path for consistent hashing.
/// Removes leading/trailing slashes, collapses double slashes.
///
/// # Arguments
/// * `path` - The path to normalize.
///
/// # Returns
/// The normalized path string.
fn normalize_path(path: &str) -> String {
    let trimmed: &str = path.trim_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    parts.join("/")
}

/// Compute XXH128 hex digest of input bytes.
///
/// # Arguments
/// * `data` - The bytes to hash.
///
/// # Returns
/// A 32-character lowercase hex string.
fn xxh128_hex(data: &[u8]) -> String {
    // Use the xxhash_rust crate which is already a transitive dependency
    // via rusty-attachments-common. For now, use a simple implementation
    // that matches the existing codebase's hash format.
    let hash: u128 = xxhash_rust::xxh3::xxh3_128(data);
    format!("{:032x}", hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relaxed_file_key_basic() {
        let key: RelaxedFileKey = relaxed_file_key("root1", "project/file.png");
        assert_eq!(key.root_id, "root1");
        assert_eq!(key.relative_path, "project/file.png");
        assert_eq!(key.path_key.len(), 32); // XXH128 = 128 bits = 32 hex chars
    }

    #[test]
    fn test_relaxed_file_key_normalizes_path() {
        let key1: RelaxedFileKey = relaxed_file_key("root1", "/project/file.png");
        let key2: RelaxedFileKey = relaxed_file_key("root1", "project/file.png");
        let key3: RelaxedFileKey = relaxed_file_key("root1", "project//file.png");

        assert_eq!(key1.path_key, key2.path_key);
        assert_eq!(key2.path_key, key3.path_key);
        assert_eq!(key1.relative_path, "project/file.png");
    }

    #[test]
    fn test_relaxed_file_key_deterministic() {
        let key1: RelaxedFileKey = relaxed_file_key("root1", "project/file.png");
        let key2: RelaxedFileKey = relaxed_file_key("root1", "project/file.png");
        assert_eq!(key1.path_key, key2.path_key);
    }

    #[test]
    fn test_relaxed_file_key_different_paths_differ() {
        let key1: RelaxedFileKey = relaxed_file_key("root1", "project/file1.png");
        let key2: RelaxedFileKey = relaxed_file_key("root1", "project/file2.png");
        assert_ne!(key1.path_key, key2.path_key);
    }

    #[test]
    fn test_relaxed_relative_path() {
        let rel: String = relaxed_relative_path("shared/project/file.png", "shared");
        assert_eq!(rel, "project/file.png");
    }

    #[test]
    fn test_relaxed_relative_path_with_trailing_slash() {
        let rel: String = relaxed_relative_path("shared/project/file.png", "shared/");
        assert_eq!(rel, "project/file.png");
    }

    #[test]
    fn test_relaxed_relative_path_nested() {
        let rel: String =
            relaxed_relative_path("assets/textures/diffuse/color.exr", "assets/textures");
        assert_eq!(rel, "diffuse/color.exr");
    }

    #[test]
    fn test_s3_marker_key() {
        let key: String = s3_marker_key("DeadlineCloud", "a1b2c3", "def456");
        assert_eq!(key, "DeadlineCloud/PendingUploads/a1b2c3/def456.xxh128");
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("/foo/bar/"), "foo/bar");
        assert_eq!(normalize_path("foo//bar"), "foo/bar");
        assert_eq!(normalize_path("///foo///bar///"), "foo/bar");
        assert_eq!(normalize_path("simple"), "simple");
        assert_eq!(normalize_path(""), "");
    }
}
