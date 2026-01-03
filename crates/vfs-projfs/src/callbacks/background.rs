//! Background task runner for non-critical operations.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

/// Background task types.
#[derive(Debug, Clone)]
pub enum BackgroundTask {
    /// File was created.
    FileCreated(String),
    /// File was modified.
    FileModified(String),
    /// File was deleted.
    FileDeleted(String),
    /// File was renamed.
    FileRenamed { old_path: String, new_path: String },
    /// Folder was created.
    FolderCreated(String),
    /// Folder was deleted.
    FolderDeleted(String),
}

/// Background task runner for non-critical operations.
///
/// Queues tasks to a background thread to keep ProjFS callbacks fast.
pub struct BackgroundTaskRunner {
    /// Task queue.
    queue: Arc<(Mutex<VecDeque<BackgroundTask>>, Condvar)>,
    /// Worker thread.
    thread: Option<JoinHandle<()>>,
    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,
}

impl BackgroundTaskRunner {
    /// Create new background task runner.
    ///
    /// # Arguments
    /// * `handler` - Function to handle tasks
    pub fn new<F>(handler: F) -> Self
    where
        F: Fn(BackgroundTask) + Send + 'static,
    {
        let queue = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let queue_clone = queue.clone();
        let shutdown_clone = shutdown.clone();

        let thread: JoinHandle<()> = thread::Builder::new()
            .name("projfs-background".to_string())
            .spawn(move || {
                loop {
                    let task: Option<BackgroundTask> = {
                        let (lock, cvar) = &*queue_clone;
                        let mut queue = lock.lock().unwrap();

                        while queue.is_empty() {
                            if shutdown_clone.load(Ordering::SeqCst) {
                                return;
                            }
                            queue = cvar.wait(queue).unwrap();
                        }

                        queue.pop_front()
                    };

                    if let Some(task) = task {
                        handler(task);
                    }
                }
            })
            .expect("Failed to spawn background thread");

        Self {
            queue,
            thread: Some(thread),
            shutdown,
        }
    }

    /// Enqueue a task.
    ///
    /// # Arguments
    /// * `task` - Task to enqueue
    pub fn enqueue(&self, task: BackgroundTask) {
        let (lock, cvar) = &*self.queue;
        lock.lock().unwrap().push_back(task);
        cvar.notify_one();
    }

    /// Check if queue is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        let (lock, _) = &*self.queue;
        lock.lock().unwrap().is_empty()
    }

    /// Shutdown the runner.
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let (_, cvar) = &*self.queue;
        cvar.notify_all();

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for BackgroundTaskRunner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[test]
    fn test_background_task_runner() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let runner = BackgroundTaskRunner::new(move |_task| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        runner.enqueue(BackgroundTask::FileCreated("test.txt".to_string()));
        runner.enqueue(BackgroundTask::FileModified("test.txt".to_string()));

        // Wait for tasks to process
        thread::sleep(Duration::from_millis(100));

        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_background_task_runner_shutdown() {
        let runner = BackgroundTaskRunner::new(|_task| {
            thread::sleep(Duration::from_millis(10));
        });

        runner.enqueue(BackgroundTask::FileCreated("test.txt".to_string()));

        // Shutdown should wait for pending tasks
        drop(runner);
    }
}
