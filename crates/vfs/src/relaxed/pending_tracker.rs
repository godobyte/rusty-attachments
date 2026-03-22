//! Tracks pending relaxed file requests and notifies waiters on resolution.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tokio::sync::Notify;

/// State of a single pending file request.
#[derive(Debug, Clone)]
pub struct PendingState {
    /// When the request was first made.
    pub requested_at: Instant,
    /// Number of poll attempts so far.
    pub poll_count: u32,
    /// Notify handle — signaled when the file becomes available.
    pub notify: Arc<Notify>,
}

/// Build a composite key from root_id and path_key.
///
/// # Arguments
/// * `root_id` - The relaxed root identifier.
/// * `path_key` - The XXH128 hash of the relative path.
///
/// # Returns
/// A composite key string "root_id:path_key".
pub fn composite_key(root_id: &str, path_key: &str) -> String {
    format!("{}:{}", root_id, path_key)
}

/// Split a composite key back into (root_id, path_key).
///
/// # Arguments
/// * `key` - The composite key "root_id:path_key".
///
/// # Returns
/// A tuple of (root_id, path_key), or ("", key) if no separator found.
pub fn split_composite_key(key: &str) -> (&str, &str) {
    match key.find(':') {
        Some(pos) => (&key[..pos], &key[pos + 1..]),
        None => ("", key),
    }
}

/// Tracks all pending relaxed file requests.
///
/// Uses DashMap for lock-free concurrent access. Each pending file has a
/// `Notify` that blocked readers can await.
///
/// Keys are composite "root_id:path_key" strings so the background poller
/// can reconstruct the S3 marker key for head_object calls.
#[derive(Debug)]
pub struct PendingFileTracker {
    /// Map of "root_id:path_key" → pending state.
    pending: DashMap<String, PendingState>,
}

impl PendingFileTracker {
    /// Create a new empty tracker.
    pub fn new() -> Self {
        Self {
            pending: DashMap::new(),
        }
    }

    /// Register a new pending file request, or return the existing notify handle.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    /// * `path_key` - The XXH128 hash of the relative path.
    ///
    /// # Returns
    /// A `Notify` handle that will be signaled when the file becomes available.
    pub fn register(&self, root_id: &str, path_key: &str) -> Arc<Notify> {
        let key: String = composite_key(root_id, path_key);
        let entry = self.pending.entry(key).or_insert_with(|| PendingState {
            requested_at: Instant::now(),
            poll_count: 0,
            notify: Arc::new(Notify::new()),
        });
        entry.notify.clone()
    }

    /// Check if a file is already pending.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    /// * `path_key` - The XXH128 hash of the relative path.
    pub fn is_pending(&self, root_id: &str, path_key: &str) -> bool {
        let key: String = composite_key(root_id, path_key);
        self.pending.contains_key(&key)
    }

    /// Mark a file as resolved by composite key. Wakes all waiters.
    ///
    /// # Arguments
    /// * `composite` - The composite key "root_id:path_key".
    ///
    /// # Returns
    /// `true` if the file was pending and is now resolved.
    pub fn resolve(&self, composite: &str) -> bool {
        if let Some((_, state)) = self.pending.remove(composite) {
            state.notify.notify_waiters();
            true
        } else {
            false
        }
    }

    /// Mark a file as resolved by root_id and path_key. Wakes all waiters.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    /// * `path_key` - The XXH128 hash of the relative path.
    ///
    /// # Returns
    /// `true` if the file was pending and is now resolved.
    pub fn resolve_by_parts(&self, root_id: &str, path_key: &str) -> bool {
        let key: String = composite_key(root_id, path_key);
        self.resolve(&key)
    }

    /// Increment the poll count for a pending file.
    ///
    /// # Arguments
    /// * `composite` - The composite key "root_id:path_key".
    pub fn increment_poll_count(&self, composite: &str) {
        if let Some(mut entry) = self.pending.get_mut(composite) {
            entry.poll_count += 1;
        }
    }

    /// Get all pending composite keys for batch polling.
    ///
    /// # Returns
    /// A snapshot of all currently pending composite keys ("root_id:path_key").
    pub fn pending_keys(&self) -> Vec<String> {
        self.pending.iter().map(|e| e.key().clone()).collect()
    }

    /// Get the number of pending requests.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get the pending state for a specific file.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    /// * `path_key` - The XXH128 hash of the relative path.
    pub fn get_state(&self, root_id: &str, path_key: &str) -> Option<PendingState> {
        let key: String = composite_key(root_id, path_key);
        self.pending.get(&key).map(|e| e.value().clone())
    }
}

impl Default for PendingFileTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_composite_key() {
        let key: String = composite_key("root1", "abc123");
        assert_eq!(key, "root1:abc123");
    }

    #[test]
    fn test_split_composite_key() {
        let (root_id, path_key) = split_composite_key("root1:abc123");
        assert_eq!(root_id, "root1");
        assert_eq!(path_key, "abc123");
    }

    #[test]
    fn test_split_composite_key_no_separator() {
        let (root_id, path_key) = split_composite_key("nocolon");
        assert_eq!(root_id, "");
        assert_eq!(path_key, "nocolon");
    }

    #[test]
    fn test_register_and_resolve() {
        let tracker = PendingFileTracker::new();

        let notify: Arc<Notify> = tracker.register("root1", "abc123");
        assert!(tracker.is_pending("root1", "abc123"));
        assert_eq!(tracker.pending_count(), 1);

        // Registering again returns the same notify
        let notify2: Arc<Notify> = tracker.register("root1", "abc123");
        assert!(Arc::ptr_eq(&notify, &notify2));
        assert_eq!(tracker.pending_count(), 1);

        // Resolve by parts
        let resolved: bool = tracker.resolve_by_parts("root1", "abc123");
        assert!(resolved);
        assert!(!tracker.is_pending("root1", "abc123"));
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_resolve_by_composite() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "abc123");

        let resolved: bool = tracker.resolve("root1:abc123");
        assert!(resolved);
        assert!(!tracker.is_pending("root1", "abc123"));
    }

    #[test]
    fn test_resolve_nonexistent() {
        let tracker = PendingFileTracker::new();
        let resolved: bool = tracker.resolve("root1:nonexistent");
        assert!(!resolved);
    }

    #[test]
    fn test_pending_keys_contain_root_id() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "key1");
        tracker.register("root2", "key2");
        tracker.register("root1", "key3");

        let mut keys: Vec<String> = tracker.pending_keys();
        keys.sort();
        assert_eq!(keys, vec!["root1:key1", "root1:key3", "root2:key2"]);
    }

    #[test]
    fn test_split_composite_key_roundtrip() {
        let original_root: &str = "my_root_id";
        let original_path: &str = "abc123def456";
        let key: String = composite_key(original_root, original_path);
        let (root_id, path_key) = split_composite_key(&key);
        assert_eq!(root_id, original_root);
        assert_eq!(path_key, original_path);
    }

    #[test]
    fn test_increment_poll_count() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "abc123");

        let state: PendingState = tracker.get_state("root1", "abc123").unwrap();
        assert_eq!(state.poll_count, 0);

        let key: String = composite_key("root1", "abc123");
        tracker.increment_poll_count(&key);
        tracker.increment_poll_count(&key);

        let state: PendingState = tracker.get_state("root1", "abc123").unwrap();
        assert_eq!(state.poll_count, 2);
    }

    #[tokio::test]
    async fn test_notify_wakes_waiter() {
        let tracker = Arc::new(PendingFileTracker::new());
        let notify: Arc<Notify> = tracker.register("root1", "abc123");

        let tracker_clone: Arc<PendingFileTracker> = tracker.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            tracker_clone.resolve_by_parts("root1", "abc123");
        });

        notify.notified().await;
        handle.await.unwrap();

        assert!(!tracker.is_pending("root1", "abc123"));
    }

    #[test]
    fn test_different_roots_same_path_key() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "same_key");
        tracker.register("root2", "same_key");

        assert_eq!(tracker.pending_count(), 2);
        assert!(tracker.is_pending("root1", "same_key"));
        assert!(tracker.is_pending("root2", "same_key"));

        tracker.resolve_by_parts("root1", "same_key");
        assert!(!tracker.is_pending("root1", "same_key"));
        assert!(tracker.is_pending("root2", "same_key"));
    }
}
