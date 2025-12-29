#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod commands;
mod error;
mod progress;
mod types;

use commands::{
    browse_directory, browse_s3_prefix, cancel_operation, create_snapshot, fetch_manifest,
    submit_bundle,
};

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            browse_directory,
            browse_s3_prefix,
            fetch_manifest,
            create_snapshot,
            submit_bundle,
            cancel_operation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
