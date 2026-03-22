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

/// Tracks all pending relaxed file requests.
///
/// Uses DashMap for lock-free concurrent access. Each pending file has a
/// `Notify` that blocked readers can await.
#[derive(Debug)]
pub struct PendingFileTracker {
    /// Map of path_key → pending state.
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
    /// * `path_key` - The path key of the file.
    ///
    /// # Returns
    /// A `Notify` handle that will be signaled when the file becomes available.
    pub fn register(&self, path_key: &str) -> Arc<Notify> {
        let entry = self.pending.entry(path_key.to_string()).or_insert_with(|| {
            PendingState {
                requested_at: Instant::now(),
                poll_count: 0,
                notify: Arc::new(Notify::new()),
            }
        });
        entry.notify.clone()
    }

    /// Check if a file is already pending.
    ///
    /// # Arguments
    /// * `path_key` - The path key of the file.
    pub fn is_pending(&self, path_key: &str) -> bool {
        self.pending.contains_key(path_key)
    }

    /// Mark a file as resolved. Wakes all waiters.
    ///
    /// # Arguments
    /// * `path_key` - The path key of the resolved file.
    ///
    /// # Returns
    /// `true` if the file was pending and is now resolved.
    pub fn resolve(&self, path_key: &str) -> bool {
        if let Some((_, state)) = self.pending.remove(path_key) {
            state.notify.notify_waiters();
            true
        } else {
            false
        }
    }

    /// Increment the poll count for a pending file.
    ///
    /// # Arguments
    /// * `path_key` - The path key of the file.
    pub fn increment_poll_count(&self, path_key: &str) {
        if let Some(mut entry) = self.pending.get_mut(path_key) {
            entry.poll_count += 1;
        }
    }

    /// Get all pending path keys for batch polling.
    ///
    /// # Returns
    /// A snapshot of all currently pending path keys.
    pub fn pending_keys(&self) -> Vec<String> {
        self.pending.iter().map(|e| e.key().clone()).collect()
    }

    /// Get the number of pending requests.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get the pending state for a specific key.
    ///
    /// # Arguments
    /// * `path_key` - The path key of the file.
    pub fn get_state(&self, path_key: &str) -> Option<PendingState> {
        self.pending.get(path_key).map(|e| e.value().clone())
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
    fn test_register_and_resolve() {
        let tracker = PendingFileTracker::new();

        let notify: Arc<Notify> = tracker.register("abc123");
        assert!(tracker.is_pending("abc123"));
        assert_eq!(tracker.pending_count(), 1);

        // Registering again returns the same notify
        let notify2: Arc<Notify> = tracker.register("abc123");
        assert!(Arc::ptr_eq(&notify, &notify2));
        assert_eq!(tracker.pending_count(), 1);

        // Resolve wakes waiters
        let resolved: bool = tracker.resolve("abc123");
        assert!(resolved);
        assert!(!tracker.is_pending("abc123"));
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_resolve_nonexistent() {
        let tracker = PendingFileTracker::new();
        let resolved: bool = tracker.resolve("nonexistent");
        assert!(!resolved);
    }

    #[test]
    fn test_pending_keys() {
        let tracker = PendingFileTracker::new();
        tracker.register("key1");
        tracker.register("key2");
        tracker.register("key3");

        let mut keys: Vec<String> = tracker.pending_keys();
        keys.sort();
        assert_eq!(keys, vec!["key1", "key2", "key3"]);
    }

    #[test]
    fn test_increment_poll_count() {
        let tracker = PendingFileTracker::new();
        tracker.register("abc123");

        let state: PendingState = tracker.get_state("abc123").unwrap();
        assert_eq!(state.poll_count, 0);

        tracker.increment_poll_count("abc123");
        tracker.increment_poll_count("abc123");

        let state: PendingState = tracker.get_state("abc123").unwrap();
        assert_eq!(state.poll_count, 2);
    }

    #[tokio::test]
    async fn test_notify_wakes_waiter() {
        let tracker = Arc::new(PendingFileTracker::new());
        let notify: Arc<Notify> = tracker.register("abc123");

        let tracker_clone: Arc<PendingFileTracker> = tracker.clone();
        let handle = tokio::spawn(async move {
            // Small delay then resolve
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            tracker_clone.resolve("abc123");
        });

        // This should complete once resolve() is called
        notify.notified().await;
        handle.await.unwrap();

        assert!(!tracker.is_pending("abc123"));
    }
}
