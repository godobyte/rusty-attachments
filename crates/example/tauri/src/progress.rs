//! Progress reporting for Tauri events.

use serde::Serialize;
use tauri::{Emitter, Window};

use rusty_attachments_common::ProgressCallback;
use rusty_attachments_filesystem::ScanProgress;

/// Progress update sent to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressUpdate {
    /// Current phase: "scanning", "hashing", "uploading", "complete".
    pub phase: String,
    /// Current file being processed.
    pub current_path: Option<String>,
    /// Files processed so far.
    pub files_processed: u64,
    /// Total files (if known).
    pub total_files: Option<u64>,
    /// Bytes processed so far.
    pub bytes_processed: u64,
    /// Total bytes (if known).
    pub total_bytes: Option<u64>,
    /// Percentage complete (0-100).
    pub percent: Option<f32>,
}

/// Adapter that emits Tauri events for scan progress.
pub struct TauriProgressCallback {
    window: Window,
    phase: String,
}

impl TauriProgressCallback {
    pub fn new(window: Window, phase: impl Into<String>) -> Self {
        Self {
            window,
            phase: phase.into(),
        }
    }
}

impl ProgressCallback<ScanProgress> for TauriProgressCallback {
    fn on_progress(&self, progress: &ScanProgress) -> bool {
        let percent: Option<f32> = match (progress.bytes_processed, progress.total_bytes) {
            (processed, Some(total)) if total > 0 => {
                Some((processed as f32 / total as f32) * 100.0)
            }
            _ => match (progress.files_processed, progress.total_files) {
                (processed, Some(total)) if total > 0 => {
                    Some((processed as f32 / total as f32) * 100.0)
                }
                _ => None,
            },
        };

        let update = ProgressUpdate {
            phase: self.phase.clone(),
            current_path: progress.current_path.clone(),
            files_processed: progress.files_processed,
            total_files: progress.total_files,
            bytes_processed: progress.bytes_processed,
            total_bytes: progress.total_bytes,
            percent,
        };

        // Emit event, ignore errors to avoid blocking
        let _ = self.window.emit("upload-progress", &update);

        true // Continue processing
    }
}
