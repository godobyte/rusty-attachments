//! ProjFS file name comparison utilities.

use std::cmp::Ordering;

#[cfg(target_os = "windows")]
use windows::Win32::Storage::ProjectedFileSystem::PrjFileNameCompare;

/// Compare two file names using ProjFS collation order.
///
/// This is the same ordering used by ProjFS for directory enumeration.
///
/// # Arguments
/// * `a` - First file name
/// * `b` - Second file name
///
/// # Returns
/// Ordering result.
#[cfg(target_os = "windows")]
pub fn prj_file_name_compare(a: &str, b: &str) -> Ordering {
    use crate::util::wstr::string_to_wide;

    let a_wide: Vec<u16> = string_to_wide(a);
    let b_wide: Vec<u16> = string_to_wide(b);

    unsafe {
        let result: i32 = PrjFileNameCompare(
            windows::core::PCWSTR::from_raw(a_wide.as_ptr()),
            windows::core::PCWSTR::from_raw(b_wide.as_ptr()),
        );

        match result {
            r if r < 0 => Ordering::Less,
            r if r > 0 => Ordering::Greater,
            _ => Ordering::Equal,
        }
    }
}

/// Compare two file names using ProjFS collation order (non-Windows fallback).
///
/// Uses case-insensitive comparison as a fallback.
///
/// # Arguments
/// * `a` - First file name
/// * `b` - Second file name
///
/// # Returns
/// Ordering result.
#[cfg(not(target_os = "windows"))]
pub fn prj_file_name_compare(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_equal() {
        assert_eq!(prj_file_name_compare("test", "test"), Ordering::Equal);
    }

    #[test]
    fn test_compare_case_insensitive() {
        assert_eq!(prj_file_name_compare("Test", "test"), Ordering::Equal);
        assert_eq!(prj_file_name_compare("TEST", "test"), Ordering::Equal);
    }

    #[test]
    fn test_compare_ordering() {
        assert_eq!(prj_file_name_compare("a", "b"), Ordering::Less);
        assert_eq!(prj_file_name_compare("b", "a"), Ordering::Greater);
    }

    #[test]
    fn test_compare_numbers() {
        // ProjFS uses natural sort order
        let result: Ordering = prj_file_name_compare("file1", "file10");
        // On Windows, this should be Less (natural sort)
        // On non-Windows, it depends on string comparison
        assert!(result != Ordering::Equal);
    }
}
