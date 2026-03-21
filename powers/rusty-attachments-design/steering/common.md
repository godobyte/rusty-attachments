# Common Module Design Summary

**Full doc:** `design/common.md`  
**Status:** ✅ IMPLEMENTED in `crates/common/`

## Purpose
Shared types, constants, and utilities used across all rusty-attachments crates. No dependencies on other rusty-attachments crates.

## Constants

```rust
pub const CHUNK_SIZE_V2: u64 = 256 * 1024 * 1024;      // 256MB
pub const CHUNK_SIZE_NONE: u64 = 0;
pub const SMALL_FILE_THRESHOLD: u64 = 80 * 1024 * 1024; // 80MB
pub const DEFAULT_HASH_CACHE_TTL_DAYS: u32 = 30;
pub const DEFAULT_STAT_CACHE_CAPACITY: usize = 1024;
```

## Hash Utilities

```rust
fn hash_bytes(data: &[u8], algorithm: HashAlgorithm) -> String;
fn hash_string(s: &str, algorithm: HashAlgorithm) -> String;
fn hash_file(path: &Path, algorithm: HashAlgorithm) -> Result<String, io::Error>;

struct Xxh3Hasher {
    fn new() -> Self;
    fn update(&mut self, data: &[u8]);
    fn finish(&self) -> u128;
    fn finish_hex(&self) -> String;
}
```

## Path Utilities

```rust
fn normalize_for_manifest(path: &Path, root: &Path) -> Result<String, PathError>;
fn to_absolute(path: &Path) -> Result<PathBuf, PathError>;
fn lexical_normalize(path: &Path) -> PathBuf;
fn to_posix_path(path: &Path) -> String;
fn from_posix_path(manifest_path: &str, destination_root: &Path) -> PathBuf;
fn is_within_root(path: &Path, root: &Path) -> bool;
fn to_long_path(path: &Path) -> PathBuf;  // Windows \\?\ prefix
fn exceeds_max_path(path: &Path) -> bool;
```

## Progress Callback

```rust
pub trait ProgressCallback<T>: Send + Sync {
    fn on_progress(&self, progress: &T) -> bool;  // false = cancel
}

pub struct NoOpProgress;
pub struct FnProgress<F, T>;
pub fn progress_fn<F, T>(f: F) -> FnProgress<F, T>;
```

## Machine ID

Platform-specific unique machine identifier for cache partitioning:

```rust
pub fn get_machine_id() -> String;
```

- Linux: Reads `/etc/machine-id`
- macOS: Uses IOPlatformUUID from IOKit
- Windows: Reads MachineGuid from registry
- Fallback: Returns `"unknown-machine-id"`

Used by hash cache to partition entries so cached hashes from one machine aren't used on another.

## Error Types

```rust
pub enum PathError {
    PathOutsideRoot { path: String, root: String },
    InvalidPath { path: String },
    PathMismatch { expected: String, actual: String },
    IoError { path: String, message: String },
}

pub struct VersionNotCompatibleError {
    pub feature: &'static str,
    pub min_version: &'static str,
}
```

## When to Read Full Doc
- Adding shared utilities
- Understanding path normalization
- Progress callback patterns
- Error type definitions
- Machine ID platform behavior
