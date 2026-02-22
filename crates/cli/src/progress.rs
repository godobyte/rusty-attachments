//! Terminal progress bar rendering using `indicatif`.

use indicatif::{ProgressBar, ProgressStyle};
use rusty_attachments_common::ProgressCallback;
use rusty_attachments_filesystem::ScanProgress;
use rusty_attachments_storage::TransferProgress;

/// Progress bar wrapper for scan operations (snapshot, diff).
pub struct ScanProgressBar {
    bar: ProgressBar,
}

impl ScanProgressBar {
    /// Create a new scan progress bar.
    pub fn new() -> Self {
        let bar: ProgressBar = ProgressBar::new(0);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} files ({msg})")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=>-"),
        );
        Self { bar }
    }

    /// Finish the progress bar.
    pub fn finish(&self) {
        self.bar.finish_with_message("done");
    }
}

impl ProgressCallback<ScanProgress> for ScanProgressBar {
    fn on_progress(&self, progress: &ScanProgress) -> bool {
        if let Some(total) = progress.total_files {
            self.bar.set_length(total);
        }
        self.bar.set_position(progress.files_processed);
        self.bar
            .set_message(format!("{:?}", progress.phase));
        true
    }
}

/// Progress bar wrapper for transfer operations (upload, download).
#[allow(dead_code)]
pub struct TransferProgressBar {
    bar: ProgressBar,
}

#[allow(dead_code)]
impl TransferProgressBar {
    /// Create a new transfer progress bar.
    pub fn new() -> Self {
        let bar: ProgressBar = ProgressBar::new(0);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({msg})")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=>-"),
        );
        Self { bar }
    }

    /// Set the total bytes for the progress bar.
    pub fn set_total(&self, total: u64) {
        self.bar.set_length(total);
    }

    /// Finish the progress bar.
    pub fn finish(&self) {
        self.bar.finish_with_message("done");
    }
}

impl rusty_attachments_storage::ProgressCallback for TransferProgressBar {
    fn on_progress(&self, progress: &TransferProgress) -> bool {
        self.bar.set_position(progress.current_bytes);
        self.bar.set_message(format!("{:?}", progress.operation));
        true
    }
}
