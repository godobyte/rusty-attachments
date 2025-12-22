# Rusty Attachments: Common Module Design

**Status: ✅ IMPLEMENTED** - See `crates/common/`

## Overview

This document defines shared types, constants, and utilities used across all rusty-attachments crates. The `common` crate has no dependencies on other rusty-attachments crates, making it safe to import from anywhere.

---

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── common/                   # NEW - Shared types and utilities
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── constants.rs      # Shared constants
│   │       ├── hash.rs           # Hash utilities
│   │       ├── path_utils.rs     # Path normalization
│   │       ├── progress.rs       # Generic progress callback
│   │       └── error.rs          # Shared error types
│   │
│   ├── model/                    # Depends on: common
│   ├── storage/                  # Depends on: common, model
│   ├── filesystem/               # Depends on: common, model
│   └── ...
```

---

## Constants

```rust
// constants.rs

/// Chunk size for manifest v2 format (256MB).
/// Files larger than this are split into chunks with individual hashes.
/// Used by both filesystem (for hashing) and storage (for upload/download).
pub const CHUNK_SIZE_V2: u64 = 256 * 1024 * 1024; // 268435456 bytes

/// No chunking (for v1 format or when chunking is disabled).
pub const CHUNK_SIZE_NONE: u64 = 0;

/// Default threshold for small vs large file classification (80MB).
/// Small files benefit from parallel object uploads.
/// Large files benefit from serial processing with parallel multipart.
pub const SMALL_FILE_THRESHOLD: u64 = 8 * 1024 * 1024 * 10; // 80MB

/// Windows MAX_PATH limit.
#[cfg(windows)]
pub const WINDOWS_MAX_PATH: usize = 260;

/// Default hash cache TTL in days.
pub const DEFAULT_HASH_CACHE_TTL_DAYS: u32 = 30;

/// Default stat cache capacity (number of entries).
pub const DEFAULT_STAT_CACHE_CAPACITY: usize = 1024;
```

---

## Hash Utilities

Consolidated hash functions used across all crates.

```rust
// hash.rs

use crate::HashAlgorithm;

/// Compute hash of a byte slice.
/// 
/// # Example
/// ```
/// use rusty_attachments_common::{hash_bytes, HashAlgorithm};
/// 
/// let hash = hash_bytes(b"hello world", HashAlgorithm::Xxh128);
/// assert_eq!(hash.len(), 32); // 128 bits = 32 hex chars
/// ```
pub fn hash_bytes(data: &[u8], algorithm: HashAlgorithm) -> String {
    match algorithm {
        HashAlgorithm::Xxh128 => {
            use xxhash_rust::xxh3::xxh3_128;
            format!("{:032x}", xxh3_128(data))
        }
    }
}

/// Compute hash of a string (convenience wrapper).
pub fn hash_string(s: &str, algorithm: HashAlgorithm) -> String {
    hash_bytes(s.as_bytes(), algorithm)
}

/// Compute hash of a file.
/// 
/// For large files, reads in chunks to avoid loading entire file into memory.
/// 
/// # Errors
/// Returns error if file cannot be read.
pub fn hash_file(path: &Path, algorithm: HashAlgorithm) -> Result<String, std::io::Error> {
    use std::io::Read;
    
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Xxh3Hasher::new();
    let mut buffer = vec![0u8; 64 * 1024]; // 64KB buffer
    
    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    
    Ok(format!("{:032x}", hasher.finish()))
}

/// Streaming hasher for incremental hashing.
pub struct Xxh3Hasher {
    inner: xxhash_rust::xxh3::Xxh3,
}

impl Xxh3Hasher {
    pub fn new() -> Self {
        Self { inner: xxhash_rust::xxh3::Xxh3::new() }
    }
    
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }
    
    pub fn finish(&self) -> u128 {
        self.inner.digest128()
    }
    
    pub fn finish_hex(&self) -> String {
        format!("{:032x}", self.finish())
    }
}

impl Default for Xxh3Hasher {
    fn default() -> Self {
        Self::new()
    }
}
```

---

## Path Utilities

Consolidated path normalization used across filesystem, storage, and path-mapping.

```rust
// path_utils.rs

use std::path::{Path, PathBuf, Component};

/// Normalize a path for storage in manifests.
/// 
/// This function:
/// 1. Converts to absolute path WITHOUT resolving symlinks (preserves symlink structure)
/// 2. Removes `.` and `..` components via lexical normalization
/// 3. Converts Windows backslashes to forward slashes
/// 4. Returns path relative to the asset root
/// 
/// # Errors
/// Returns error if path is outside the root directory.
pub fn normalize_for_manifest(path: &Path, root: &Path) -> Result<String, PathError> {
    let abs_path = to_absolute(path)?;
    let normalized = lexical_normalize(&abs_path);
    
    let abs_root = to_absolute(root)?;
    let normalized_root = lexical_normalize(&abs_root);
    
    let relative = normalized.strip_prefix(&normalized_root)
        .map_err(|_| PathError::PathOutsideRoot { 
            path: normalized.display().to_string(),
            root: normalized_root.display().to_string(),
        })?;
    
    Ok(to_posix_path(relative))
}

/// Convert a path to absolute without resolving symlinks.
pub fn to_absolute(path: &Path) -> Result<PathBuf, PathError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|e| PathError::IoError { 
                path: path.display().to_string(), 
                message: e.to_string() 
            })
    }
}

/// Lexical path normalization without filesystem access.
/// Removes `.` components and resolves `..` components lexically.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => { /* skip . */ }
            Component::ParentDir => {
                // Pop if we can and it's not a ParentDir
                if !components.is_empty() 
                    && !matches!(components.last(), Some(Component::ParentDir)) 
                {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }
    components.iter().collect()
}

/// Convert a path to POSIX-style string (forward slashes).
/// Used for manifest paths which are always POSIX format.
pub fn to_posix_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Convert a manifest path (POSIX format) to the host OS path format.
pub fn from_posix_path(manifest_path: &str, destination_root: &Path) -> PathBuf {
    let components: Vec<&str> = manifest_path.split('/').collect();
    let mut result = destination_root.to_path_buf();
    for component in components {
        if !component.is_empty() {
            result.push(component);
        }
    }
    result
}

/// Check if a path is within a root directory (security validation).
/// Uses lexical comparison, does not access filesystem.
pub fn is_within_root(path: &Path, root: &Path) -> bool {
    let norm_path = lexical_normalize(path);
    let norm_root = lexical_normalize(root);
    norm_path.starts_with(&norm_root)
}

/// Convert a path to Windows long path format if needed.
/// 
/// On Windows, paths longer than MAX_PATH (260 chars) need the `\\?\` prefix.
/// No-op on non-Windows platforms.
#[cfg(windows)]
pub fn to_long_path(path: &Path) -> PathBuf {
    use crate::constants::WINDOWS_MAX_PATH;
    
    let path_str = path.to_string_lossy();
    
    if path_str.starts_with(r"\\?\") {
        return path.to_path_buf();
    }
    
    if path_str.len() < WINDOWS_MAX_PATH {
        return path.to_path_buf();
    }
    
    let abs_path = to_absolute(path).unwrap_or_else(|_| path.to_path_buf());
    PathBuf::from(format!(r"\\?\{}", abs_path.display()))
}

#[cfg(not(windows))]
pub fn to_long_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Check if a path would exceed Windows MAX_PATH limit.
/// Always returns false on non-Windows platforms.
pub fn exceeds_max_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use crate::constants::WINDOWS_MAX_PATH;
        path.to_string_lossy().len() >= WINDOWS_MAX_PATH
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}
```

---

## Generic Progress Callback

A generic progress callback trait that works across all operations.

```rust
// progress.rs

/// Generic progress callback trait.
/// 
/// Type parameter `T` is the progress data type, allowing different
/// operations to report different progress information while sharing
/// the same callback pattern.
/// 
/// # Example
/// ```
/// use rusty_attachments_common::ProgressCallback;
/// 
/// struct MyProgress {
///     files_done: u64,
///     total_files: u64,
/// }
/// 
/// struct ConsoleReporter;
/// 
/// impl ProgressCallback<MyProgress> for ConsoleReporter {
///     fn on_progress(&self, progress: &MyProgress) -> bool {
///         println!("{}/{} files", progress.files_done, progress.total_files);
///         true // continue
///     }
/// }
/// ```
pub trait ProgressCallback<T>: Send + Sync {
    /// Called with progress updates.
    /// 
    /// # Returns
    /// - `true` to continue the operation
    /// - `false` to cancel the operation
    fn on_progress(&self, progress: &T) -> bool;
}

/// A no-op progress callback that always continues.
pub struct NoOpProgress;

impl<T> ProgressCallback<T> for NoOpProgress {
    fn on_progress(&self, _progress: &T) -> bool {
        true
    }
}

/// A progress callback that wraps a closure.
pub struct FnProgress<F, T> {
    callback: F,
    _marker: std::marker::PhantomData<T>,
}

impl<F, T> FnProgress<F, T>
where
    F: Fn(&T) -> bool + Send + Sync,
{
    pub fn new(callback: F) -> Self {
        Self { 
            callback, 
            _marker: std::marker::PhantomData,
        }
    }
}

impl<F, T> ProgressCallback<T> for FnProgress<F, T>
where
    F: Fn(&T) -> bool + Send + Sync,
{
    fn on_progress(&self, progress: &T) -> bool {
        (self.callback)(progress)
    }
}

/// Helper to create a progress callback from a closure.
pub fn progress_fn<F, T>(f: F) -> FnProgress<F, T>
where
    F: Fn(&T) -> bool + Send + Sync,
{
    FnProgress::new(f)
}
```

---

## Shared Error Types

Common error types used across multiple crates.

```rust
// error.rs

use std::path::PathBuf;
use thiserror::Error;

/// Path-related errors shared across crates.
#[derive(Debug, Error, Clone)]
pub enum PathError {
    #[error("Path is outside root: {path} not in {root}")]
    PathOutsideRoot { path: String, root: String },
    
    #[error("Invalid path: {path}")]
    InvalidPath { path: String },
    
    #[error("Path mismatch - expected {expected}, got {actual}")]
    PathMismatch { expected: String, actual: String },
    
    #[error("IO error at {path}: {message}")]
    IoError { path: String, message: String },
}

impl PathError {
    /// Create an IoError from std::io::Error.
    pub fn from_io(path: impl Into<String>, err: std::io::Error) -> Self {
        Self::IoError {
            path: path.into(),
            message: err.to_string(),
        }
    }
}

/// Version compatibility error for features not supported in older manifest versions.
/// 
/// Per steering guidelines, unsupported versions should fail explicitly
/// rather than silently degrading.
#[derive(Debug, Error, Clone)]
#[error("{feature} requires manifest version {min_version} or later")]
pub struct VersionNotCompatibleError {
    /// The feature that was requested.
    pub feature: &'static str,
    /// Minimum version required for this feature.
    pub min_version: &'static str,
}

impl VersionNotCompatibleError {
    pub fn new(feature: &'static str, min_version: &'static str) -> Self {
        Self { feature, min_version }
    }
}
```

---

## lib.rs Exports

```rust
// lib.rs

pub mod constants;
pub mod error;
pub mod hash;
pub mod path_utils;
pub mod progress;

// Re-export commonly used items at crate root
pub use constants::*;
pub use error::{PathError, VersionNotCompatibleError};
pub use hash::{hash_bytes, hash_string, hash_file, Xxh3Hasher};
pub use path_utils::{
    normalize_for_manifest, to_absolute, lexical_normalize,
    to_posix_path, from_posix_path, is_within_root, to_long_path,
    exceeds_max_path,
};
pub use progress::{ProgressCallback, NoOpProgress, FnProgress, progress_fn};

// Re-export HashAlgorithm from model crate for convenience
// Note: This creates a dependency on model. If circular dependency is a concern,
// move HashAlgorithm to common crate instead.
// pub use rusty_attachments_model::HashAlgorithm;
```

---

## Cargo.toml

```toml
[package]
name = "rusty-attachments-common"
version = "0.1.0"
edition = "2021"

[dependencies]
thiserror = "1.0"
xxhash-rust = { version = "0.8", features = ["xxh3"] }

[target.'cfg(windows)'.dependencies]
# No additional Windows dependencies needed for path_utils
```

---

## Usage Examples

### Using Hash Utilities

```rust
use rusty_attachments_common::{hash_bytes, hash_file, hash_string, HashAlgorithm};

// Hash bytes
let hash = hash_bytes(b"hello world", HashAlgorithm::Xxh128);

// Hash a string (manifest content)
let manifest_hash = hash_string(&manifest_json, HashAlgorithm::Xxh128);

// Hash a file
let file_hash = hash_file(Path::new("/path/to/file"), HashAlgorithm::Xxh128)?;
```

### Using Path Utilities

```rust
use rusty_attachments_common::{normalize_for_manifest, from_posix_path, is_within_root};

// Normalize for manifest storage
let manifest_path = normalize_for_manifest(
    Path::new("/project/assets/texture.png"),
    Path::new("/project"),
)?;
assert_eq!(manifest_path, "assets/texture.png");

// Convert back for download
let local_path = from_posix_path("assets/texture.png", Path::new("/downloads"));
assert_eq!(local_path, PathBuf::from("/downloads/assets/texture.png"));

// Security check
assert!(is_within_root(Path::new("/project/assets/file.txt"), Path::new("/project")));
assert!(!is_within_root(Path::new("/etc/passwd"), Path::new("/project")));
```

### Using Generic Progress Callback

```rust
use rusty_attachments_common::{ProgressCallback, progress_fn};

// Define operation-specific progress types
struct ScanProgress {
    files_scanned: u64,
    current_file: String,
}

struct TransferProgress {
    bytes_transferred: u64,
    total_bytes: u64,
}

// Use with closure
let callback = progress_fn(|p: &ScanProgress| {
    println!("Scanning: {}", p.current_file);
    true
});

// Or implement trait directly
struct ProgressBar;
impl ProgressCallback<TransferProgress> for ProgressBar {
    fn on_progress(&self, p: &TransferProgress) -> bool {
        let pct = (p.bytes_transferred * 100) / p.total_bytes;
        print!("\r[{:3}%] ", pct);
        true
    }
}
```

---

## Migration Notes

When updating existing design documents to use common:

1. **Remove duplicate definitions** of path utilities, hash functions, constants
2. **Import from common** instead of defining locally
3. **Update error types** to use `PathError` from common where applicable
4. **Update progress callbacks** to use `ProgressCallback<T>` generic

---

## Related Documents

- [model-design.md](model-design.md) - Manifest data structures (depends on common)
- [storage-design.md](storage-design.md) - Storage abstraction (depends on common)
- [file_system.md](file_system.md) - File system operations (depends on common)
- [path-mapping.md](path-mapping.md) - Path transformation (depends on common)
