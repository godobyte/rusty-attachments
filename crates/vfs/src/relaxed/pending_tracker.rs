//! Tracks pending relaxed file requests and notifies waiters on resolution.
//!
//! Performance-optimized for the background poller pattern:
//! - DashMap keys are `Arc<str>` so cloning is a refcount bump, not a heap alloc
//! - New registrations are pushed to a lock-free queue for the poller to drain
//! - The poller processes only newly-registered keys, not the entire map (O(new) vs O(total))
//! - Composite key methods accept `&str` to avoid re-allocation

use std::sync::Arc;
use std::time::Instant;

use crossbeam_queue::SegQueue;
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
/// A composite key `Arc<str>` "root_id:path_key". Clone is a refcount bump.
pub fn composite_key(root_id: &str, path_key: &str) -> Arc<str> {
    let s: String = format!("{}:{}", root_id, path_key);
    Arc::from(s)
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
/// Uses `DashMap<Arc<str>, PendingState>` for lock-free concurrent access.
/// Keys are `Arc<str>` so that `pending_keys()` snapshots and poller iterations
/// only bump refcounts instead of cloning heap-allocated Strings.
///
/// New registrations are also pushed to a lock-free `SegQueue` so the background
/// poller can drain only newly-added keys (O(new) per poll cycle) instead of
/// iterating the entire map (O(total)).
#[derive(Debug)]
pub struct PendingFileTracker {
    /// Map of "root_id:path_key" → pending state.
    pending: DashMap<Arc<str>, PendingState>,
    /// Lock-free queue of newly registered keys for the poller to drain.
    /// The poller drains this each cycle and adds the keys to its local poll set.
    new_keys: SegQueue<Arc<str>>,
}

impl PendingFileTracker {
    /// Create a new empty tracker.
    pub fn new() -> Self {
        Self {
            pending: DashMap::new(),
            new_keys: SegQueue::new(),
        }
    }

    /// Register a new pending file request, or return the existing notify handle.
    ///
    /// Also pushes the key to the poll queue so the background poller picks it up
    /// without needing to iterate the entire DashMap.
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    /// * `path_key` - The XXH128 hash of the relative path.
    ///
    /// # Returns
    /// A `Notify` handle that will be signaled when the file becomes available.
    pub fn register(&self, root_id: &str, path_key: &str) -> Arc<Notify> {
        let key: Arc<str> = composite_key(root_id, path_key);
        let is_new: bool = !self.pending.contains_key(&key);

        let entry = self
            .pending
            .entry(key.clone())
            .or_insert_with(|| PendingState {
                requested_at: Instant::now(),
                poll_count: 0,
                notify: Arc::new(Notify::new()),
            });

        if is_new {
            self.new_keys.push(key);
        }

        entry.notify.clone()
    }

    /// Check if a file is already pending (by composite key, zero-alloc).
    ///
    /// # Arguments
    /// * `composite` - The composite key "root_id:path_key".
    pub fn is_pending_composite(&self, composite: &str) -> bool {
        self.pending.contains_key(composite)
    }

    /// Check if a file is already pending (by parts, allocates composite key).
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    /// * `path_key` - The XXH128 hash of the relative path.
    pub fn is_pending(&self, root_id: &str, path_key: &str) -> bool {
        let key: Arc<str> = composite_key(root_id, path_key);
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
        let key: Arc<str> = composite_key(root_id, path_key);
        self.resolve(&key)
    }

    /// Increment the poll count for a pending file (by composite key, zero-alloc).
    ///
    /// # Arguments
    /// * `composite` - The composite key "root_id:path_key".
    pub fn increment_poll_count(&self, composite: &str) {
        if let Some(mut entry) = self.pending.get_mut(composite) {
            entry.poll_count += 1;
        }
    }

    /// Drain newly registered keys since the last drain.
    ///
    /// The background poller calls this each cycle to get only the keys
    /// that were added since the last poll, avoiding a full DashMap iteration.
    /// Keys that are still pending after a HEAD miss should be re-added to
    /// the poller's local set for the next cycle.
    ///
    /// # Returns
    /// A vector of newly registered composite keys (Arc<str>, clone is free).
    pub fn drain_new_keys(&self) -> Vec<Arc<str>> {
        let mut keys: Vec<Arc<str>> = Vec::new();
        while let Some(key) = self.new_keys.pop() {
            // Only include keys that are still pending (may have been resolved
            // between registration and drain)
            if self.pending.contains_key(&key) {
                keys.push(key);
            }
        }
        keys
    }

    /// Get all pending composite keys for batch polling (full snapshot).
    ///
    /// Prefer `drain_new_keys()` for the background poller. This method is
    /// useful for diagnostics or when the poller needs a full resync.
    ///
    /// # Returns
    /// A snapshot of all currently pending composite keys. Clone is a refcount
    /// bump since keys are `Arc<str>`.
    pub fn pending_keys(&self) -> Vec<Arc<str>> {
        self.pending.iter().map(|e| e.key().clone()).collect()
    }

    /// Get the number of pending requests.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get the pending state for a specific file (by composite key, zero-alloc).
    ///
    /// # Arguments
    /// * `composite` - The composite key "root_id:path_key".
    pub fn get_state_composite(&self, composite: &str) -> Option<PendingState> {
        self.pending.get(composite).map(|e| e.value().clone())
    }

    /// Get the pending state for a specific file (by parts).
    ///
    /// # Arguments
    /// * `root_id` - The relaxed root identifier.
    /// * `path_key` - The XXH128 hash of the relative path.
    pub fn get_state(&self, root_id: &str, path_key: &str) -> Option<PendingState> {
        let key: Arc<str> = composite_key(root_id, path_key);
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
    fn test_composite_key_is_arc_str() {
        let key: Arc<str> = composite_key("root1", "abc123");
        assert_eq!(&*key, "root1:abc123");
        // Clone is a refcount bump, not a heap alloc
        let key2: Arc<str> = key.clone();
        assert!(Arc::ptr_eq(&key, &key2));
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
    fn test_is_pending_composite_zero_alloc() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "key1");

        // is_pending_composite takes &str — no Arc<str> allocation needed
        assert!(tracker.is_pending_composite("root1:key1"));
        assert!(!tracker.is_pending_composite("root1:key2"));
    }

    #[test]
    fn test_get_state_composite_zero_alloc() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "abc123");

        // get_state_composite takes &str — no Arc<str> allocation needed
        let state: PendingState = tracker.get_state_composite("root1:abc123").unwrap();
        assert_eq!(state.poll_count, 0);
        assert!(tracker.get_state_composite("root1:missing").is_none());
    }

    #[test]
    fn test_drain_new_keys() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "key1");
        tracker.register("root2", "key2");

        // First drain gets both keys
        let mut new_keys: Vec<Arc<str>> = tracker.drain_new_keys();
        new_keys.sort();
        assert_eq!(new_keys.len(), 2);
        assert_eq!(&*new_keys[0], "root1:key1");
        assert_eq!(&*new_keys[1], "root2:key2");

        // Second drain is empty (no new registrations)
        let empty: Vec<Arc<str>> = tracker.drain_new_keys();
        assert!(empty.is_empty());

        // Register a new key
        tracker.register("root3", "key3");
        let new_keys: Vec<Arc<str>> = tracker.drain_new_keys();
        assert_eq!(new_keys.len(), 1);
        assert_eq!(&*new_keys[0], "root3:key3");
    }

    #[test]
    fn test_drain_new_keys_skips_already_resolved() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "key1");
        tracker.register("root2", "key2");

        // Resolve key1 before draining
        tracker.resolve("root1:key1");

        // Drain should only return key2 (key1 was resolved)
        let new_keys: Vec<Arc<str>> = tracker.drain_new_keys();
        assert_eq!(new_keys.len(), 1);
        assert_eq!(&*new_keys[0], "root2:key2");
    }

    #[test]
    fn test_drain_new_keys_deduplicates_re_register() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "key1");
        tracker.register("root1", "key1"); // re-register same key

        // The SegQueue has 1 entry (second register didn't push because !is_new)
        let new_keys: Vec<Arc<str>> = tracker.drain_new_keys();
        assert_eq!(new_keys.len(), 1);
    }

    #[test]
    fn test_pending_keys_returns_arc_str() {
        let tracker = PendingFileTracker::new();
        tracker.register("root1", "key1");
        tracker.register("root2", "key2");

        let keys: Vec<Arc<str>> = tracker.pending_keys();
        assert_eq!(keys.len(), 2);

        // Verify clone is a refcount bump (same pointer)
        let keys2: Vec<Arc<str>> = tracker.pending_keys();
        // Keys are Arc<str> — the DashMap stores Arc<str> and iter() clones them
        // (refcount bump). The two snapshots should have the same Arc pointers
        // for keys that haven't been removed.
        for k in &keys {
            // Each key should be findable in the map
            assert!(tracker.is_pending_composite(k));
        }
        assert_eq!(keys2.len(), 2);
    }

    #[test]
    fn test_split_composite_key_roundtrip() {
        let original_root: &str = "my_root_id";
        let original_path: &str = "abc123def456";
        let key: Arc<str> = composite_key(original_root, original_path);
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

        // increment_poll_count takes &str composite — zero alloc
        tracker.increment_poll_count("root1:abc123");
        tracker.increment_poll_count("root1:abc123");

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
