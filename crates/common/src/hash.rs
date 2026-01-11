//! Hash computation utilities.

use std::io::Read;
use std::path::Path;

use xxhash_rust::xxh3::Xxh3;

/// Compute XXH128 hash of a byte slice.
///
/// # Arguments
/// * `data` - Bytes to hash
///
/// # Returns
/// 32-character lowercase hex string (128 bits).
pub fn hash_bytes(data: &[u8]) -> String {
    let hash: u128 = xxhash_rust::xxh3::xxh3_128(data);
    format!("{:032x}", hash)
}

/// Compute XXH128 hash of a string (convenience wrapper).
///
/// # Arguments
/// * `s` - String to hash
///
/// # Returns
/// 32-character lowercase hex string (128 bits).
pub fn hash_string(s: &str) -> String {
    hash_bytes(s.as_bytes())
}

/// Compute XXH128 hash of a file.
///
/// Reads the file in chunks to avoid loading entire file into memory.
///
/// # Arguments
/// * `path` - Path to the file to hash
///
/// # Returns
/// 32-character lowercase hex string (128 bits).
///
/// # Errors
/// Returns error if file cannot be read.
pub fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file: std::fs::File = std::fs::File::open(path)?;
    let mut hasher: Xxh3Hasher = Xxh3Hasher::new();
    let mut buffer: Vec<u8> = vec![0u8; 64 * 1024]; // 64KB buffer

    loop {
        let bytes_read: usize = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hasher.finish_hex())
}

/// Streaming hasher for incremental XXH128 hashing.
///
/// Use this when you need to hash data incrementally, such as
/// when reading from a stream or computing hash while uploading.
pub struct Xxh3Hasher {
    inner: Xxh3,
}

impl Xxh3Hasher {
    /// Create a new streaming hasher.
    pub fn new() -> Self {
        Self { inner: Xxh3::new() }
    }

    /// Update the hasher with additional data.
    ///
    /// # Arguments
    /// * `data` - Bytes to add to the hash computation
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalize and return the hash as u128.
    pub fn finish(&self) -> u128 {
        self.inner.digest128()
    }

    /// Finalize and return the hash as 32-char hex string.
    pub fn finish_hex(&self) -> String {
        format!("{:032x}", self.finish())
    }
}

impl Default for Xxh3Hasher {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Hash Folding for Memory Efficiency
// ============================================================================

/// Fold a 128-bit XXH3 hash into a 64-bit integer for compact storage.
///
/// Uses XOR folding to preserve hash distribution while reducing size.
/// This is suitable for hash tables and memory-efficient storage where
/// the full 128-bit hash isn't required.
///
/// # Arguments
/// * `hash_hex` - 32-character hex string (128-bit hash)
///
/// # Returns
/// 64-bit folded hash value
///
/// # Panics
/// Panics if hash_hex is not exactly 32 hex characters
pub fn fold_hash_to_u64(hash_hex: &str) -> u64 {
    assert_eq!(hash_hex.len(), 32, "Hash must be 32 hex characters");

    // Parse as u128, then fold upper and lower 64 bits
    let full_hash: u128 = u128::from_str_radix(hash_hex, 16).expect("Invalid hex hash");

    let upper: u64 = (full_hash >> 64) as u64;
    let lower: u64 = full_hash as u64;

    // XOR fold preserves distribution
    upper ^ lower
}

/// Compute XXH128 hash and return folded 64-bit value.
///
/// # Arguments
/// * `data` - Bytes to hash
///
/// # Returns
/// 64-bit folded hash value
pub fn hash_bytes_folded(data: &[u8]) -> u64 {
    let hash_hex: String = hash_bytes(data);
    fold_hash_to_u64(&hash_hex)
}

impl Xxh3Hasher {
    /// Finalize and return folded 64-bit hash.
    ///
    /// # Returns
    /// 64-bit folded hash value
    pub fn finish_folded(&self) -> u64 {
        let full_hash: u128 = self.finish();
        let upper: u64 = (full_hash >> 64) as u64;
        let lower: u64 = full_hash as u64;
        upper ^ lower
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_hash_bytes_empty() {
        let hash: String = hash_bytes(b"");
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn test_hash_bytes_hello() {
        let hash: String = hash_bytes(b"hello world");
        assert_eq!(hash.len(), 32);
        // Verify deterministic
        assert_eq!(hash, hash_bytes(b"hello world"));
    }

    #[test]
    fn test_hash_bytes_different_inputs() {
        let hash1: String = hash_bytes(b"hello");
        let hash2: String = hash_bytes(b"world");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_string() {
        let hash: String = hash_string("hello world");
        assert_eq!(hash.len(), 32);
        // Should be identical to hash_bytes with same content
        assert_eq!(hash, hash_bytes(b"hello world"));
    }

    #[test]
    fn test_hash_string_empty() {
        let hash: String = hash_string("");
        assert_eq!(hash.len(), 32);
        assert_eq!(hash, hash_bytes(b""));
    }

    #[test]
    fn test_xxh3_hasher_incremental() {
        let mut hasher: Xxh3Hasher = Xxh3Hasher::new();
        hasher.update(b"hello ");
        hasher.update(b"world");
        let incremental: String = hasher.finish_hex();

        let direct: String = hash_bytes(b"hello world");
        assert_eq!(incremental, direct);
    }

    #[test]
    fn test_hash_file() {
        let dir: tempfile::TempDir = tempfile::tempdir().unwrap();
        let file_path: std::path::PathBuf = dir.path().join("test.txt");

        let mut file: std::fs::File = std::fs::File::create(&file_path).unwrap();
        file.write_all(b"hello world").unwrap();
        drop(file);

        let file_hash: String = hash_file(&file_path).unwrap();
        let direct_hash: String = hash_bytes(b"hello world");
        assert_eq!(file_hash, direct_hash);
    }

    #[test]
    fn test_hash_file_not_found() {
        let result: Result<String, std::io::Error> = hash_file(Path::new("/nonexistent/file.txt"));
        assert!(result.is_err());
    }

    // ========================================================================
    // Hash Folding Tests
    // ========================================================================

    #[test]
    fn test_hash_folding_deterministic() {
        let hash: String = hash_bytes(b"test data");
        let folded1: u64 = fold_hash_to_u64(&hash);
        let folded2: u64 = fold_hash_to_u64(&hash);
        assert_eq!(folded1, folded2);
    }

    #[test]
    fn test_hash_folding_different_inputs() {
        // Use actual hashes from real data
        let hash1: String = hash_bytes(b"hello");
        let hash2: String = hash_bytes(b"world");
        let folded1: u64 = fold_hash_to_u64(&hash1);
        let folded2: u64 = fold_hash_to_u64(&hash2);
        assert_ne!(folded1, folded2);
    }

    #[test]
    fn test_hash_bytes_folded() {
        let data: &[u8] = b"hello world";
        let folded1: u64 = hash_bytes_folded(data);
        let folded2: u64 = hash_bytes_folded(data);
        assert_eq!(folded1, folded2);

        let different_data: &[u8] = b"goodbye world";
        let folded3: u64 = hash_bytes_folded(different_data);
        assert_ne!(folded1, folded3);
    }

    #[test]
    fn test_xxh3_hasher_finish_folded() {
        let mut hasher: Xxh3Hasher = Xxh3Hasher::new();
        hasher.update(b"hello ");
        hasher.update(b"world");
        let folded: u64 = hasher.finish_folded();

        let direct_folded: u64 = hash_bytes_folded(b"hello world");
        assert_eq!(folded, direct_folded);
    }

    #[test]
    #[should_panic(expected = "Hash must be 32 hex characters")]
    fn test_fold_hash_invalid_length() {
        fold_hash_to_u64("abc123");
    }

    #[test]
    #[should_panic(expected = "Invalid hex hash")]
    fn test_fold_hash_invalid_hex() {
        fold_hash_to_u64("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz");
    }
}
