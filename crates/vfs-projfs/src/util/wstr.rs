//! Wide string conversion utilities.

use smallvec::SmallVec;
use windows::core::PCWSTR;

use crate::error::ProjFsError;

/// Convert PCWSTR to Rust String with stack allocation for common sizes.
///
/// Paths under 256 chars use stack allocation, longer paths heap allocate.
///
/// # Arguments
/// * `s` - Wide string pointer
///
/// # Returns
/// UTF-8 string.
pub fn pcwstr_to_string(s: PCWSTR) -> Result<String, ProjFsError> {
    if s.is_null() {
        return Ok(String::new());
    }

    unsafe {
        // Find length
        let mut len: usize = 0;
        let mut ptr: *const u16 = s.as_ptr();
        while *ptr != 0 {
            len += 1;
            ptr = ptr.add(1);
        }

        if len == 0 {
            return Ok(String::new());
        }

        // Stack buffer for common path lengths (512 bytes = 256 UTF-16 chars)
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

        String::from_utf8(buffer.to_vec()).map_err(|e| ProjFsError::PathConversion(e.to_string()))
    }
}

/// Convert wide string slice to Rust String.
///
/// # Arguments
/// * `wide` - Wide string slice
///
/// # Returns
/// UTF-8 string.
#[allow(dead_code)] // Used in tests
pub fn wide_to_string(wide: &[u16]) -> Result<String, ProjFsError> {
    String::from_utf16(wide).map_err(|e| ProjFsError::PathConversion(e.to_string()))
}

/// Convert Rust String to wide string (null-terminated).
///
/// # Arguments
/// * `s` - UTF-8 string
///
/// # Returns
/// Null-terminated wide string.
pub fn string_to_wide(s: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.push(0); // Null terminator
    wide
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wide_to_string() {
        let wide: Vec<u16> = vec![0x0048, 0x0065, 0x006C, 0x006C, 0x006F]; // "Hello"
        let result: String = wide_to_string(&wide).unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_string_to_wide() {
        let s: &str = "Hello";
        let wide: Vec<u16> = string_to_wide(s);
        assert_eq!(wide, vec![0x0048, 0x0065, 0x006C, 0x006C, 0x006F, 0x0000]);
    }

    #[test]
    fn test_string_to_wide_unicode() {
        let s: &str = "Hello 世界";
        let wide: Vec<u16> = string_to_wide(s);
        // Should end with null terminator
        assert_eq!(wide.last(), Some(&0));
        // Should be able to convert back
        let back: String = wide_to_string(&wide[..wide.len() - 1]).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn test_empty_string() {
        let wide: Vec<u16> = string_to_wide("");
        assert_eq!(wide, vec![0x0000]);

        let back: String = wide_to_string(&[]).unwrap();
        assert_eq!(back, "");
    }
}
