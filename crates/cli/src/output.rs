//! Output formatting for JSON and human-readable modes.

use std::fmt;

/// Print an error message to stderr.
pub fn print_error(err: &dyn fmt::Display) {
    eprintln!("Error: {}", err);
}

/// Format a value as JSON or human-readable text.
///
/// # Arguments
/// * `value` - Serializable value
/// * `json` - If true, output JSON; otherwise human-readable
#[allow(dead_code)]
pub fn format_result<T: serde::Serialize + fmt::Display>(value: &T, json: bool) {
    if json {
        match serde_json::to_string_pretty(value) {
            Ok(s) => println!("{}", s),
            Err(e) => eprintln!("JSON serialization error: {}", e),
        }
    } else {
        println!("{}", value);
    }
}

/// Format a byte count as a human-readable string.
///
/// # Arguments
/// * `bytes` - Number of bytes
///
/// # Returns
/// Formatted string like "1.23 GB", "456 MB", "789 KB", or "123 B".
pub fn human_readable_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_readable_size() {
        assert_eq!(human_readable_size(0), "0 B");
        assert_eq!(human_readable_size(512), "512 B");
        assert_eq!(human_readable_size(1024), "1.00 KB");
        assert_eq!(human_readable_size(1536), "1.50 KB");
        assert_eq!(human_readable_size(1048576), "1.00 MB");
        assert_eq!(human_readable_size(1073741824), "1.00 GB");
        assert_eq!(human_readable_size(1099511627776), "1.00 TB");
    }
}
