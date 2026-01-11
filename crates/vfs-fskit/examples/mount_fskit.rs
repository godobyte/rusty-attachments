//! Example: Mount a manifest using FSKit.
//!
//! This example demonstrates how to mount a Deadline Cloud job attachment
//! manifest as a virtual filesystem using FSKit on macOS 15.4+.
//!
//! # Usage
//!
//! ```bash
//! cargo run --example mount_fskit -- \
//!     --manifest manifest.json \
//!     --mount-point /tmp/deadline-assets \
//!     --cache-dir /tmp/vfs-cache \
//!     --bucket my-bucket \
//!     --root-prefix my-root \
//!     --stats
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(target_os = "macos")]
use rusty_attachments_model::Manifest;
#[cfg(target_os = "macos")]
use rusty_attachments_vfs::{
    CrtStorageClient, FileStore, S3Location, StorageClientAdapter, StorageSettings,
    WritableVfsStats, WritableVfsStatsCollector,
};
#[cfg(target_os = "macos")]
use rusty_attachments_vfs_fskit::{
    FsKitMountOptions, FsKitVfsOptions, FsKitWriteOptions, WritableFsKit,
};

/// Command-line arguments for the mount example.
struct Args {
    /// Path to manifest JSON file.
    manifest: PathBuf,
    /// Mount point directory.
    mount_point: PathBuf,
    /// Cache directory for writes.
    cache_dir: PathBuf,
    /// S3 bucket name.
    bucket: String,
    /// S3 root prefix.
    root_prefix: String,
    /// Volume name displayed in Finder.
    volume_name: String,
    /// Show live statistics dashboard.
    show_stats: bool,
}

impl Args {
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut manifest: Option<PathBuf> = None;
        let mut mount_point = PathBuf::from("/tmp/deadline-assets");
        let mut cache_dir = PathBuf::from("/tmp/vfs-fskit-cache");
        let mut bucket = String::new();
        let mut root_prefix = String::new();
        let mut volume_name = String::from("Deadline Assets");
        let mut show_stats: bool = false;

        let mut i: usize = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--manifest" | "-m" => {
                    i += 1;
                    manifest = Some(PathBuf::from(&args[i]));
                }
                "--mount-point" | "-p" => {
                    i += 1;
                    mount_point = PathBuf::from(&args[i]);
                }
                "--cache-dir" | "-c" => {
                    i += 1;
                    cache_dir = PathBuf::from(&args[i]);
                }
                "--bucket" | "-b" => {
                    i += 1;
                    bucket = args[i].clone();
                }
                "--root-prefix" | "-r" => {
                    i += 1;
                    root_prefix = args[i].clone();
                }
                "--volume-name" | "-n" => {
                    i += 1;
                    volume_name = args[i].clone();
                }
                "--stats" | "-s" => {
                    show_stats = true;
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                _ => {
                    eprintln!("Unknown argument: {}", args[i]);
                    print_usage();
                    std::process::exit(1);
                }
            }
            i += 1;
        }

        let manifest: PathBuf = manifest.unwrap_or_else(|| {
            eprintln!("Error: --manifest is required");
            print_usage();
            std::process::exit(1);
        });

        if bucket.is_empty() {
            eprintln!("Error: --bucket is required");
            print_usage();
            std::process::exit(1);
        }

        Self {
            manifest,
            mount_point,
            cache_dir,
            bucket,
            root_prefix,
            volume_name,
            show_stats,
        }
    }
}

fn print_usage() {
    eprintln!(
        r#"
Usage: mount_fskit [OPTIONS]

Options:
    -m, --manifest <PATH>       Path to manifest JSON file (required)
    -p, --mount-point <PATH>    Mount point directory [default: /tmp/deadline-assets]
    -c, --cache-dir <PATH>      Cache directory for writes [default: /tmp/vfs-fskit-cache]
    -b, --bucket <NAME>         S3 bucket name (required)
    -r, --root-prefix <PREFIX>  S3 root prefix [default: ""]
    -n, --volume-name <NAME>    Volume name in Finder [default: "Deadline Assets"]
    -s, --stats                 Show live statistics dashboard (updates every 2s)
    -h, --help                  Print this help message

Example:
    cargo run --example mount_fskit -- \
        --manifest manifest.json \
        --mount-point /tmp/deadline-assets \
        --bucket my-bucket \
        --root-prefix my-root \
        --stats
"#
    );
}

/// Format bytes as human-readable string.
///
/// # Arguments
/// * `bytes` - Number of bytes
///
/// # Returns
/// Human-readable string (e.g., "1.23 MB").
#[cfg(target_os = "macos")]
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Truncate a path for display.
///
/// # Arguments
/// * `path` - Path to truncate
/// * `max_len` - Maximum length
///
/// # Returns
/// Truncated path string.
#[cfg(target_os = "macos")]
fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else {
        format!("...{}", &path[path.len() - (max_len - 3)..])
    }
}

/// Print FSKit VFS statistics dashboard.
///
/// # Arguments
/// * `stats` - Collected writable VFS stats
/// * `mount_point` - Mount point path for display
#[cfg(target_os = "macos")]
fn print_stats(stats: &WritableVfsStats, mount_point: &str) {
    // Clear screen and move cursor to top
    print!("\x1B[2J\x1B[H");

    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║               FSKit VFS Statistics Dashboard                      ║");
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ Mode: READ-WRITE (COW) via FSKit                                  ║");
    println!("║ Mount: {:58} ║", truncate_path(mount_point, 58));
    println!(
        "║ Uptime: {:>5}s                                                   ║",
        stats.uptime_secs
    );
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ FILESYSTEM                                                        ║");
    println!(
        "║   Inodes: {:>10}                                              ║",
        stats.inode_count
    );
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ MEMORY POOL                                                       ║");
    println!(
        "║   Blocks: {:>6} total, {:>6} in use                            ║",
        stats.pool_stats.total_blocks, stats.pool_stats.in_use_blocks
    );
    println!(
        "║   Memory: {:>12} / {:>12} ({:.1}%)                   ║",
        format_bytes(stats.pool_stats.current_size),
        format_bytes(stats.pool_stats.max_size),
        stats.pool_stats.utilization()
    );
    println!(
        "║   Pending fetches: {:>4}                                          ║",
        stats.pool_stats.pending_fetches
    );
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ CACHE                                                             ║");
    println!(
        "║   Hits: {:>10}  Allocations: {:>10}                       ║",
        stats.cache_hits, stats.cache_allocations
    );
    println!(
        "║   Hit rate: {:>6.2}%                                              ║",
        stats.cache_hit_rate
    );
    println!("╠══════════════════════════════════════════════════════════════════╣");

    // Dirty files section
    let summary = &stats.dirty_summary;
    println!("║ COW DIRTY STATE                                                   ║");
    println!(
        "║   Files:   Modified: {:>3}    New: {:>3}    Deleted: {:>3}           ║",
        summary.modified_count, summary.new_count, summary.deleted_count
    );
    println!(
        "║   Dirs:    New: {:>3}         Deleted: {:>3}                        ║",
        summary.new_dir_count, summary.deleted_dir_count
    );

    if !stats.modified_files.is_empty() || !stats.new_files.is_empty() {
        println!("║                                                                   ║");
        println!("║   Dirty files:                                                    ║");

        // Combine modified and new files for display
        let mut display_count: usize = 0;
        for file in stats.modified_files.iter().take(5) {
            let path_display: String = truncate_path(&file.path, 42);
            println!(
                "║   {:>2}. {:42} {:>8}      ║",
                display_count + 1,
                path_display,
                format_bytes(file.size)
            );
            display_count += 1;
        }
        for file in stats.new_files.iter().take(5 - display_count.min(5)) {
            let path_display: String = truncate_path(&file.path, 42);
            println!(
                "║   {:>2}. {:42} {:>8} [NEW]║",
                display_count + 1,
                path_display,
                format_bytes(file.size)
            );
            display_count += 1;
        }

        let total_dirty: usize = stats.modified_files.len() + stats.new_files.len();
        if total_dirty > 5 {
            println!(
                "║   ... and {} more                                               ║",
                total_dirty - 5
            );
        }
    }

    if !stats.deleted_files.is_empty() {
        println!("║                                                                   ║");
        println!("║   Deleted files:                                                  ║");
        for (i, path) in stats.deleted_files.iter().take(3).enumerate() {
            let path_display: String = truncate_path(path, 50);
            println!("║   {:>2}. {:50}        ║", i + 1, path_display);
        }
        if stats.deleted_files.len() > 3 {
            println!(
                "║   ... and {} more                                               ║",
                stats.deleted_files.len() - 3
            );
        }
    }

    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!("\nPress Ctrl+C to unmount and exit.");
}

/// Spawn a background thread that prints stats periodically.
///
/// # Arguments
/// * `collector` - Stats collector to query
/// * `running` - Atomic flag to control thread lifetime
/// * `mount_point` - Mount point path for display
/// * `interval_secs` - Interval between stats updates
///
/// # Returns
/// Handle to the spawned thread.
#[cfg(target_os = "macos")]
fn spawn_stats_thread(
    collector: WritableVfsStatsCollector,
    running: Arc<AtomicBool>,
    mount_point: String,
    interval_secs: u64,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while running.load(Ordering::SeqCst) {
            let stats: WritableVfsStats = collector.collect();
            print_stats(&stats, &mount_point);
            std::thread::sleep(std::time::Duration::from_secs(interval_secs));
        }
    })
}

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing (only if not showing stats dashboard)
    let args = Args::parse();
    if !args.show_stats {
        tracing_subscriber::fmt::init();
    }

    // Check FSKit availability
    if !rusty_attachments_vfs_fskit::fskit_available() {
        eprintln!("Error: FSKit requires macOS 15.4 or later");
        std::process::exit(1);
    }

    // Load manifest
    if !args.show_stats {
        println!("Loading manifest from {:?}...", args.manifest);
    }
    let manifest_json: String = std::fs::read_to_string(&args.manifest)?;
    let manifest: Manifest = Manifest::decode(&manifest_json)?;
    if !args.show_stats {
        println!("Loaded manifest ({} version)", manifest.version());
    }

    // Create storage client
    if !args.show_stats {
        println!("Connecting to S3 bucket: {}", args.bucket);
    }
    let settings = StorageSettings::default();
    let client = CrtStorageClient::new(settings).await?;
    let location = S3Location::new(&args.bucket, &args.root_prefix, "Data", "Manifests");
    let store: Arc<dyn FileStore> = Arc::new(StorageClientAdapter::new(client, location));

    // Create VFS options
    let options = FsKitVfsOptions::default().with_volume_name(&args.volume_name);
    let write_options = FsKitWriteOptions::default().with_cache_dir(&args.cache_dir);

    // Create writable FSKit VFS
    if !args.show_stats {
        println!("Creating FSKit VFS...");
    }
    let vfs = WritableFsKit::new(&manifest, store, options, write_options)?;

    // Get stats collector for monitoring
    let stats_collector = vfs.stats_collector();

    // Create mount point if it doesn't exist
    if !args.mount_point.exists() {
        std::fs::create_dir_all(&args.mount_point)?;
    }

    // Mount options
    let mount_opts = FsKitMountOptions::default().with_mount_point(&args.mount_point);

    // Mount via FSKit
    if !args.show_stats {
        println!("Mounting at {:?}...", args.mount_point);
    }
    let session = fskit_rs::mount(vfs.clone(), mount_opts.into()).await?;

    if !args.show_stats {
        println!("Mounted successfully!");
        println!("  Mount point: {:?}", args.mount_point);
        println!("  Cache dir: {:?}", args.cache_dir);
        println!();
        println!("Press Ctrl+C to unmount.");
    }

    // Set up signal handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Spawn stats thread
    let stats_handle: Option<std::thread::JoinHandle<()>> = if args.show_stats {
        let mount_point_str: String = args.mount_point.to_string_lossy().to_string();
        Some(spawn_stats_thread(
            stats_collector,
            running.clone(),
            mount_point_str,
            2, // Update every 2 seconds
        ))
    } else {
        None
    };

    // Wait for Ctrl+C
    while running.load(Ordering::SeqCst) {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    if !args.show_stats {
        println!();
        println!("Unmounting...");
    }

    // Print dirty summary before unmount
    let summary = vfs.dirty_summary();
    if summary.new_count > 0
        || summary.modified_count > 0
        || summary.deleted_count > 0
        || summary.new_dir_count > 0
        || summary.deleted_dir_count > 0
    {
        // Clear screen if we were showing stats
        if args.show_stats {
            print!("\x1B[2J\x1B[H");
        }
        println!("Unmounting with pending changes:");
        println!(
            "  Files: {} new, {} modified, {} deleted",
            summary.new_count, summary.modified_count, summary.deleted_count
        );
        println!(
            "  Dirs:  {} new, {} deleted",
            summary.new_dir_count, summary.deleted_dir_count
        );
    }

    // Stop stats thread
    if let Some(handle) = stats_handle {
        let _ = handle.join();
    }

    // Unmount
    drop(session);

    println!("Unmounted successfully.");

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Error: FSKit is only available on macOS 15.4+");
    std::process::exit(1);
}
