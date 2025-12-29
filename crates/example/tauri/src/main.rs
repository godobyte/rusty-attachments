#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use serde::{Deserialize, Serialize};

/// Response from the greet command.
#[derive(Debug, Serialize, Deserialize)]
struct GreetResponse {
    message: String,
    timestamp: u64,
}

/// Greet the user by name.
///
/// # Arguments
/// * `name` - The name to greet
///
/// # Returns
/// A greeting message with timestamp.
#[tauri::command]
fn greet(name: &str) -> GreetResponse {
    let timestamp: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    GreetResponse {
        message: format!("Hello, {}! Welcome to Tauri.", name),
        timestamp,
    }
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
