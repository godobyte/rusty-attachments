//! Example: Mount a manifest using ProjFS.
//!
//! This example demonstrates how to mount a job attachments manifest
//! as a virtual filesystem on Windows using ProjFS.
//!
//! Usage:
//!   cargo run --example mount_projfs -p rusty-attachments-vfs-projfs -- <manifest.json> <mount_point> [options]
//!
//! Options:
//!   --cache-dir <path>   Directory for write cache (default: ./vfs-cache)
//!   --stats              Show live statistics dashboard
//!   --bucket <name>      S3 bucket name (or set S3_BUCKET env var)
//!   --root-prefix <pfx>  S3 root prefix (or set S3_ROOT_PREFIX env var)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rusty_attachments_model::Manifest;
use rusty_attachments_storage::{S3Location, StorageSettings};
use rusty_attachments_storage_crt::CrtStorageClient;
use rusty_attachments_vfs::StorageClientAdapter;
use rusty_attachments_vfs_projfs::{ProjFsOptions, ProjFsWriteOptions, WritableProjFs};

/// Parsed command line arguments.
struct Args {
    manifest_path: PathBuf,
    mount_point: PathBuf,
    cache_dir: PathBuf,
    show_stats: bool,
    cleanup: bool,
    bucket: String,
    root_prefix: String,
}

/// Parse command line arguments.
///
/// # Returns
/// Parsed arguments or exits with usage message.
fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let manifest_path = PathBuf::from(&args[1]);
    let mount_point = PathBuf::from(&args[2]);

    // Defaults
    let mut cache_dir = PathBuf::from("vfs-cache");
    let mut show_stats: bool = false;
    let mut cleanup: bool = false;
    let mut bucket: String = std::env::var("S3_BUCKET").unwrap_or_else(|_| "adeadlineja".to_string());
    let mut root_prefix: String = std::env::var("S3_ROOT_PREFIX").unwrap_or_else(|_| "DeadlineCloud".to_string());

    // Parse optional arguments
    let mut i: usize = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--cache-dir" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --cache-dir requires a path");
                    std::process::exit(1);
                }
                cache_dir = PathBuf::from(&args[i]);
            }
            "--stats" => {
                show_stats = true;
            }
            "--cleanup" => {
                cleanup = true;
            }
            "--bucket" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --bucket requires a name");
                    std::process::exit(1);
                }
                bucket = args[i].clone();
            }
            "--root-prefix" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --root-prefix requires a value");
                    std::process::exit(1);
                }
                root_prefix = args[i].clone();
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                print_usage(&args[0]);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    Args {
        manifest_path,
        mount_point,
        cache_dir,
        show_stats,
        cleanup,
        bucket,
        root_prefix,
    }
}

/// Print usage message.
///
/// # Arguments
/// * `program` - Program name for usage message
fn print_usage(program: &str) {
    eprintln!("Usage: {} <manifest.json> <mount_point> [options]", program);
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --cache-dir <path>   Directory for write cache (default: ./vfs-cache)");
    eprintln!("  --stats              Show live statistics dashboard");
    eprintln!("  --cleanup            Delete mount directory after unmounting");
    eprintln!("  --bucket <name>      S3 bucket name (default: $S3_BUCKET or adeadlineja)");
    eprintln!("  --root-prefix <pfx>  S3 root prefix (default: $S3_ROOT_PREFIX or DeadlineCloud)");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let args: Args = parse_args();

    // Load manifest
    println!("Loading manifest from {:?}...", args.manifest_path);
    let manifest_json: String = std::fs::read_to_string(&args.manifest_path)?;
    let manifest: Manifest = Manifest::decode(&manifest_json)?;

    println!(
        "Manifest loaded: {} files, {} total bytes",
        manifest.file_count(),
        manifest.total_size()
    );

    // Create S3 location configuration
    let cas_prefix: String = std::env::var("S3_CAS_PREFIX").unwrap_or_else(|_| "Data".to_string());
    let manifest_prefix: String = std::env::var("S3_MANIFEST_PREFIX").unwrap_or_else(|_| "Manifests".to_string());

    let s3_location = S3Location::new(
        args.bucket.clone(),
        args.root_prefix.clone(),
        cas_prefix,
        manifest_prefix,
    );

    // Create storage client
    let storage_settings = StorageSettings::default();

    println!("Connecting to S3: {}/{}", s3_location.bucket, args.root_prefix);
    let crt_client: CrtStorageClient = CrtStorageClient::new(storage_settings).await?;
    let storage = Arc::new(StorageClientAdapter::new(crt_client, s3_location));

    // Create ProjFS options
    let options = ProjFsOptions::new(args.mount_point.clone())
        .with_worker_threads(4)
        .with_notifications(rusty_attachments_vfs_projfs::NotificationMask::for_writable());

    let write_options = ProjFsWriteOptions::default()
        .with_cache_dir(args.cache_dir.clone())
        .with_disk_cache(true);

    // Create and start virtualizer
    println!("Creating ProjFS virtualizer at {:?}...", args.mount_point);
    println!("Write cache: {:?}", args.cache_dir);
    let vfs = WritableProjFs::new(&manifest, storage, options, write_options)?;

    println!("Starting virtualization...");
    vfs.start()?;

    println!("\n✓ Virtual filesystem mounted at {:?}", args.mount_point);
    println!("Press Ctrl+C to unmount and exit...\n");

    // Start stats display if requested
    if args.show_stats {
        let stats_collector = vfs.stats_collector();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                // Clear screen and print stats
                print!("\x1B[2J\x1B[1;1H");
                println!("{}", stats_collector.collect().display_grid());
            }
        });
    }

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;

    println!("\nUnmounting...");
    vfs.stop()?;

    println!("✓ Unmounted successfully");

    // Cleanup mount directory if requested
    if args.cleanup {
        println!("Cleaning up mount directory...");
        std::fs::remove_dir_all(&args.mount_point)?;
        println!("✓ Mount directory removed");
    }

    Ok(())
}
