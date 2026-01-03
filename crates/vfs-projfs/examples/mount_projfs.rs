//! Example: Mount a manifest using ProjFS.
//!
//! This example demonstrates how to mount a job attachments manifest
//! as a virtual filesystem on Windows using ProjFS.
//!
//! Usage:
//!   cargo run --example mount_projfs -- <manifest.json> <mount_point>

use std::path::PathBuf;
use std::sync::Arc;

use rusty_attachments_model::Manifest;
use rusty_attachments_storage::{S3Location, StorageSettings};
use rusty_attachments_storage_crt::CrtStorageClient;
use rusty_attachments_vfs::StorageClientAdapter;
use rusty_attachments_vfs_projfs::{ProjFsOptions, ProjFsWriteOptions, WritableProjFs};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <manifest.json> <mount_point>", args[0]);
        std::process::exit(1);
    }

    let manifest_path = PathBuf::from(&args[1]);
    let mount_point = PathBuf::from(&args[2]);

    // Load manifest
    println!("Loading manifest from {:?}...", manifest_path);
    let manifest_json: String = std::fs::read_to_string(&manifest_path)?;
    let manifest: Manifest = Manifest::decode(&manifest_json)?;

    println!(
        "Manifest loaded: {} files, {} total bytes",
        manifest.file_count(),
        manifest.total_size()
    );

    // Create S3 location configuration
    let s3_location = S3Location::new(
        std::env::var("S3_BUCKET").unwrap_or_else(|_| "my-bucket".to_string()),
        std::env::var("S3_ROOT_PREFIX").unwrap_or_else(|_| "DeadlineCloud".to_string()),
        std::env::var("S3_CAS_PREFIX").unwrap_or_else(|_| "Data".to_string()),
        std::env::var("S3_MANIFEST_PREFIX").unwrap_or_else(|_| "Manifests".to_string()),
    );

    // Create storage client
    let storage_settings = StorageSettings::default();

    println!("Connecting to S3: {}", s3_location.bucket);
    let crt_client: CrtStorageClient = CrtStorageClient::new(storage_settings).await?;
    let storage = Arc::new(StorageClientAdapter::new(crt_client, s3_location));

    // Create ProjFS options
    let options = ProjFsOptions::new(mount_point.clone())
        .with_worker_threads(4)
        .with_notifications(rusty_attachments_vfs_projfs::NotificationMask::for_writable());

    let write_options = ProjFsWriteOptions::default()
        .with_cache_dir(PathBuf::from("C:\\Temp\\vfs-write-cache"))
        .with_disk_cache(true);

    // Create and start virtualizer
    println!("Creating ProjFS virtualizer at {:?}...", mount_point);
    let vfs = WritableProjFs::new(&manifest, storage, options, write_options)?;

    println!("Starting virtualization...");
    vfs.start()?;

    println!("\n✓ Virtual filesystem mounted at {:?}", mount_point);
    println!("Press Ctrl+C to unmount and exit...\n");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;

    println!("\nUnmounting...");
    vfs.stop()?;

    println!("✓ Unmounted successfully");

    Ok(())
}
