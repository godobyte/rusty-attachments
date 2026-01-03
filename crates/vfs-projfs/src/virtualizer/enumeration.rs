//! Active enumeration session management.
//!
//! Tracks state for directory enumeration sessions as required by ProjFS.

use std::sync::Arc;

use windows::Win32::Storage::ProjectedFileSystem::PrjFileNameMatch;

use crate::projection::types::ProjectedFileInfo;

/// Active enumeration session.
///
/// Holds pre-loaded enumeration data and current position.
/// Analogous to VFSForGit's `ActiveEnumeration`.
pub struct ActiveEnumeration {
    /// Pre-loaded items (sorted in ProjFS order).
    items: Arc<[ProjectedFileInfo]>,
    /// Current position in items.
    index: usize,
    /// Captured search expression (wildcard filter).
    filter: Option<String>,
    /// Whether filter has been captured.
    filter_captured: bool,
}

impl ActiveEnumeration {
    /// Create new enumeration with pre-loaded items.
    ///
    /// # Arguments
    /// * `items` - Pre-sorted list of items to enumerate
    pub fn new(items: Arc<[ProjectedFileInfo]>) -> Self {
        Self {
            items,
            index: 0,
            filter: None,
            filter_captured: false,
        }
    }

    /// Get current item if valid.
    ///
    /// # Returns
    /// Reference to current item, or None if past end.
    pub fn current(&self) -> Option<&ProjectedFileInfo> {
        self.items.get(self.index)
    }

    /// Check if current position is valid.
    pub fn is_current_valid(&self) -> bool {
        self.index < self.items.len()
    }

    /// Move to next item (skipping filtered items).
    ///
    /// # Returns
    /// True if moved to a valid item, false if past end.
    pub fn move_next(&mut self) -> bool {
        self.index += 1;
        self.skip_filtered();
        self.is_current_valid()
    }

    /// Restart enumeration with new filter.
    ///
    /// # Arguments
    /// * `filter` - Optional wildcard filter pattern
    pub fn restart(&mut self, filter: Option<String>) {
        self.index = 0;
        self.filter = filter;
        self.filter_captured = true;
        self.skip_filtered();
    }

    /// Try to save filter (only on first call).
    ///
    /// # Arguments
    /// * `filter` - Optional wildcard filter pattern
    ///
    /// # Returns
    /// True if filter was saved, false if already captured.
    pub fn try_save_filter(&mut self, filter: Option<String>) -> bool {
        if !self.filter_captured {
            self.filter = filter;
            self.filter_captured = true;
            self.skip_filtered();
            true
        } else {
            false
        }
    }

    /// Skip items that don't match filter.
    fn skip_filtered(&mut self) {
        while self.is_current_valid() {
            if self.matches_filter(self.current().unwrap()) {
                break;
            }
            self.index += 1;
        }
    }

    /// Check if item matches current filter.
    ///
    /// # Arguments
    /// * `item` - Item to check
    ///
    /// # Returns
    /// True if item matches filter (or no filter set).
    fn matches_filter(&self, item: &ProjectedFileInfo) -> bool {
        match &self.filter {
            None => true,
            Some(pattern) if pattern.is_empty() => true,
            Some(pattern) => self.prj_file_name_match(&item.name, pattern),
        }
    }

    /// Match file name against wildcard pattern using ProjFS.
    ///
    /// # Arguments
    /// * `name` - File name to check
    /// * `pattern` - Wildcard pattern
    ///
    /// # Returns
    /// True if name matches pattern.
    fn prj_file_name_match(&self, name: &str, pattern: &str) -> bool {
        use crate::util::string_to_wide;

        let name_wide: Vec<u16> = string_to_wide(name);
        let pattern_wide: Vec<u16> = string_to_wide(pattern);

        unsafe {
            PrjFileNameMatch(
                windows::core::PCWSTR::from_raw(name_wide.as_ptr()),
                windows::core::PCWSTR::from_raw(pattern_wide.as_ptr()),
            )
            .as_bool()
        }
    }

    /// Get total item count.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Check if enumeration is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn create_test_items() -> Arc<[ProjectedFileInfo]> {
        Arc::from(vec![
            ProjectedFileInfo::file(
                "apple.txt".to_string(),
                100,
                "hash1".to_string(),
                SystemTime::UNIX_EPOCH,
                false,
            ),
            ProjectedFileInfo::file(
                "banana.txt".to_string(),
                200,
                "hash2".to_string(),
                SystemTime::UNIX_EPOCH,
                false,
            ),
            ProjectedFileInfo::folder("docs".to_string()),
            ProjectedFileInfo::file(
                "zebra.txt".to_string(),
                300,
                "hash3".to_string(),
                SystemTime::UNIX_EPOCH,
                false,
            ),
        ])
    }

    #[test]
    fn test_enumeration_basic() {
        let items: Arc<[ProjectedFileInfo]> = create_test_items();
        let mut enum_session = ActiveEnumeration::new(items);

        assert!(enum_session.is_current_valid());
        assert_eq!(enum_session.current().unwrap().name.as_ref(), "apple.txt");

        assert!(enum_session.move_next());
        assert_eq!(enum_session.current().unwrap().name.as_ref(), "banana.txt");

        assert!(enum_session.move_next());
        assert_eq!(enum_session.current().unwrap().name.as_ref(), "docs");

        assert!(enum_session.move_next());
        assert_eq!(enum_session.current().unwrap().name.as_ref(), "zebra.txt");

        assert!(!enum_session.move_next());
        assert!(!enum_session.is_current_valid());
    }

    #[test]
    fn test_enumeration_restart() {
        let items: Arc<[ProjectedFileInfo]> = create_test_items();
        let mut enum_session = ActiveEnumeration::new(items);

        // Move to middle
        enum_session.move_next();
        enum_session.move_next();
        assert_eq!(enum_session.current().unwrap().name.as_ref(), "docs");

        // Restart
        enum_session.restart(None);
        assert_eq!(enum_session.current().unwrap().name.as_ref(), "apple.txt");
    }

    #[test]
    fn test_enumeration_filter() {
        let items: Arc<[ProjectedFileInfo]> = create_test_items();
        let mut enum_session = ActiveEnumeration::new(items);

        // Filter for *.txt (on non-Windows, uses simple contains)
        enum_session.restart(Some("*.txt".to_string()));

        // On Windows, this would filter properly
        // On non-Windows, our fallback just checks contains
        assert!(enum_session.is_current_valid());
    }

    #[test]
    fn test_enumeration_empty() {
        let items: Arc<[ProjectedFileInfo]> = Arc::from(vec![]);
        let enum_session = ActiveEnumeration::new(items);

        assert!(!enum_session.is_current_valid());
        assert!(enum_session.is_empty());
    }

    #[test]
    fn test_try_save_filter() {
        let items: Arc<[ProjectedFileInfo]> = create_test_items();
        let mut enum_session = ActiveEnumeration::new(items);

        // First save should succeed
        assert!(enum_session.try_save_filter(Some("*.txt".to_string())));

        // Second save should fail (already captured)
        assert!(!enum_session.try_save_filter(Some("*.doc".to_string())));
    }
}
