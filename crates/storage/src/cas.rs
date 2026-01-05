//! CAS (Content-Addressable Storage) utilities.
//!
//! This module contains pure logic for chunking decisions and CAS key generation.
//! No I/O operations - just decision making.

/// Information about a single chunk of a large file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkInfo {
    /// Zero-based chunk index.
    pub index: usize,
    /// Byte offset within the file.
    pub offset: u64,
    /// Length of this chunk in bytes.
    pub length: u64,
}

/// Determine if a file needs chunking based on size.
///
/// Files larger than `chunk_size` should be split into chunks.
/// If `chunk_size` is 0, chunking is disabled.
pub fn needs_chunking(size: u64, chunk_size: u64) -> bool {
    chunk_size > 0 && size > chunk_size
}

/// Generate chunk information for a large file.
///
/// Splits a file of `size` bytes into chunks of `chunk_size` bytes.
/// The last chunk may be smaller than `chunk_size`.
///
/// # Arguments
/// * `size` - Total file size in bytes
/// * `chunk_size` - Size of each chunk (use CHUNK_SIZE_V2 for v2 manifests)
///
/// # Returns
/// Vector of ChunkInfo describing each chunk's offset and length.
/// Returns a single chunk for files smaller than chunk_size.
pub fn generate_chunks(size: u64, chunk_size: u64) -> Vec<ChunkInfo> {
    if chunk_size == 0 || size == 0 {
        return vec![ChunkInfo {
            index: 0,
            offset: 0,
            length: size,
        }];
    }

    let mut chunks = Vec::new();
    let mut offset = 0u64;
    let mut index = 0usize;

    while offset < size {
        let length = std::cmp::min(chunk_size, size - offset);
        chunks.push(ChunkInfo {
            index,
            offset,
            length,
        });
        offset += length;
        index += 1;
    }

    chunks
}

/// Calculate the expected number of chunks for a file.
///
/// # Arguments
/// * `size` - Total file size in bytes
/// * `chunk_size` - Size of each chunk in bytes (0 disables chunking)
///
/// # Returns
/// Number of chunks needed, minimum 1.
pub fn expected_chunk_count(size: u64, chunk_size: u64) -> usize {
    if chunk_size == 0 || size == 0 {
        return 1;
    }
    size.div_ceil(chunk_size) as usize
}

/// Upload strategy based on file size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadStrategy {
    /// Upload as single S3 object (file hash is the key).
    SingleObject,
    /// Upload as multiple CAS objects (chunk hashes are the keys).
    ChunkedCas,
}

/// Determine upload strategy based on file size.
pub fn upload_strategy(size: u64, chunk_size: u64) -> UploadStrategy {
    if needs_chunking(size, chunk_size) {
        UploadStrategy::ChunkedCas
    } else {
        UploadStrategy::SingleObject
    }
}

/// Download strategy based on manifest entry type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStrategy {
    /// Download single CAS object.
    SingleObject,
    /// Download multiple chunks and reassemble.
    ChunkedCas,
    /// Create local symlink (no download).
    Symlink,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CHUNK_SIZE_V2;

    #[test]
    fn test_needs_chunking() {
        // File smaller than chunk size
        assert!(!needs_chunking(100, CHUNK_SIZE_V2));

        // File exactly at chunk size
        assert!(!needs_chunking(CHUNK_SIZE_V2, CHUNK_SIZE_V2));

        // File larger than chunk size
        assert!(needs_chunking(CHUNK_SIZE_V2 + 1, CHUNK_SIZE_V2));

        // Chunking disabled
        assert!(!needs_chunking(1_000_000_000, 0));
    }

    #[test]
    fn test_generate_chunks_small_file() {
        let chunks = generate_chunks(1000, CHUNK_SIZE_V2);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].index, 0);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].length, 1000);
    }

    #[test]
    fn test_generate_chunks_exact_multiple() {
        let chunk_size = 100u64;
        let chunks = generate_chunks(300, chunk_size);
        assert_eq!(chunks.len(), 3);

        assert_eq!(chunks[0], ChunkInfo { index: 0, offset: 0, length: 100 });
        assert_eq!(chunks[1], ChunkInfo { index: 1, offset: 100, length: 100 });
        assert_eq!(chunks[2], ChunkInfo { index: 2, offset: 200, length: 100 });
    }

    #[test]
    fn test_generate_chunks_with_remainder() {
        let chunk_size = 100u64;
        let chunks = generate_chunks(250, chunk_size);
        assert_eq!(chunks.len(), 3);

        assert_eq!(chunks[0], ChunkInfo { index: 0, offset: 0, length: 100 });
        assert_eq!(chunks[1], ChunkInfo { index: 1, offset: 100, length: 100 });
        assert_eq!(chunks[2], ChunkInfo { index: 2, offset: 200, length: 50 });
    }

    #[test]
    fn test_generate_chunks_large_file() {
        // 500MB file with 256MB chunks = 2 chunks
        let size = 500 * 1024 * 1024;
        let chunks = generate_chunks(size, CHUNK_SIZE_V2);
        assert_eq!(chunks.len(), 2);

        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].length, CHUNK_SIZE_V2);

        assert_eq!(chunks[1].offset, CHUNK_SIZE_V2);
        assert_eq!(chunks[1].length, size - CHUNK_SIZE_V2);
    }

    #[test]
    fn test_expected_chunk_count() {
        assert_eq!(expected_chunk_count(100, CHUNK_SIZE_V2), 1);
        assert_eq!(expected_chunk_count(CHUNK_SIZE_V2, CHUNK_SIZE_V2), 1);
        assert_eq!(expected_chunk_count(CHUNK_SIZE_V2 + 1, CHUNK_SIZE_V2), 2);
        assert_eq!(expected_chunk_count(CHUNK_SIZE_V2 * 2, CHUNK_SIZE_V2), 2);
        assert_eq!(expected_chunk_count(CHUNK_SIZE_V2 * 2 + 1, CHUNK_SIZE_V2), 3);
    }

    #[test]
    fn test_upload_strategy() {
        assert_eq!(upload_strategy(100, CHUNK_SIZE_V2), UploadStrategy::SingleObject);
        assert_eq!(upload_strategy(CHUNK_SIZE_V2, CHUNK_SIZE_V2), UploadStrategy::SingleObject);
        assert_eq!(upload_strategy(CHUNK_SIZE_V2 + 1, CHUNK_SIZE_V2), UploadStrategy::ChunkedCas);
    }
}
