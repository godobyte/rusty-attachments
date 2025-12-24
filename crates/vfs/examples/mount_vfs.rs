//! Example: Mount a manifest as a FUSE filesystem.
//!
//! Usage:
//!   cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- <manifest.json> <mountpoint> [options]
//!
//! Options:
//!   --stats              Show live statistics dashboard
//!   --bucket <name>      S3 bucket name (default: adeadlineja)
//!   --root-prefix <pfx>  S3 root prefix (default: DeadlineCloud)
//!   --region <region>    AWS region (default: us-west-2)
//!   --mock               Use mock file store instead of S3
//!
//! Example:
//!   cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
//!       /tmp/manifest.json ~/vfs --stats --bucket adeadlineja --root-prefix DeadlineCloud

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use rusty_attachments_model::{HashAlgorithm, Manifest};
use rusty_attachments_storage::{S3Location, StorageClient, StorageSettings};
use rusty_attachments_storage_crt::CrtStorageClient;
use rusty_attachments_vfs::{DeadlineVfs, FileStore, VfsError, VfsOptions, VfsStatsCollector};

/// Adapter that wraps a StorageClient to implement FileStore.
///
/// This bridges the storage crate's `StorageClient` trait with the VFS
/// crate's `FileStore` trait, following the pyramid architecture pattern.
struct StorageClientAdapter<C: StorageClient> {
    /// The underlying storage client.
    client: C,
    /// S3 location configuration.
    location: S3Location,
}

impl<C: StorageClient> StorageClientAdapter<C> {
    /// Create a new adapter wrapping a storage client.
    ///
    /// # Arguments
    /// * `client` - Storage client implementing S3 operations
    /// * `location` - S3 location with bucket and root prefix
    fn new(client: C, location: S3Location) -> Self {
        Self { client, location }
    }
}

#[async_trait]
impl<C: StorageClient + 'static> FileStore for StorageClientAdapter<C> {
    /// Retrieve the entire content for a hash from S3 CAS.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm used
    ///
    /// # Returns
    /// The raw bytes of the content.
    async fn retrieve(&self, hash: &str, algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError> {
        let key: String = self.location.cas_key(hash, algorithm);

        self.client
            .get_object(&self.location.bucket, &key)
            .await
            .map_err(|e| VfsError::ContentRetrievalFailed {
                hash: hash.to_string(),
                source: format!("StorageClient get_object failed: {}", e).into(),
            })
    }

    /// Retrieve a range of content for a hash from S3 CAS.
    ///
    /// Note: Currently fetches full object then slices. Could be optimized
    /// with S3 range requests if StorageClient adds get_object_range.
    ///
    /// # Arguments
    /// * `hash` - Content hash
    /// * `algorithm` - Hash algorithm used
    /// * `offset` - Start offset in bytes
    /// * `size` - Number of bytes to retrieve
    ///
    /// # Returns
    /// The raw bytes of the requested range.
    async fn retrieve_range(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, VfsError> {
        // TODO: Add get_object_range to StorageClient trait for efficiency
        let data: Vec<u8> = self.retrieve(hash, algorithm).await?;
        let start: usize = offset as usize;
        let end: usize = (offset + size).min(data.len() as u64) as usize;
        Ok(data[start..end].to_vec())
    }
}

/// Mock file store that returns placeholder content for testing.
struct MockFileStore;

#[async_trait]
impl FileStore for MockFileStore {
    async fn retrieve(&self, hash: &str, _algorithm: HashAlgorithm) -> Result<Vec<u8>, VfsError> {
        let placeholder: String = format!("[Mock content for hash: {}]", hash);
        Ok(placeholder.into_bytes())
    }

    async fn retrieve_range(
        &self,
        hash: &str,
        algorithm: HashAlgorithm,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, VfsError> {
        let data: Vec<u8> = self.retrieve(hash, algorithm).await?;
        let start: usize = offset as usize;
        let end: usize = (offset + size).min(data.len() as u64) as usize;
        Ok(data[start..end].to_vec())
    }
}

/// CLI arguments for the mount_vfs example.
struct CliArgs {
    manifest_path: PathBuf,
    mountpoint: PathBuf,
    show_stats: bool,
    use_mock: bool,
    bucket: String,
    root_prefix: String,
    region: String,
}

impl CliArgs {
    /// Parse CLI arguments.
    ///
    /// # Returns
    /// Parsed CLI arguments or None if help was requested or args invalid.
    fn parse() -> Option<Self> {
        let args: Vec<String> = std::env::args().collect();

        if args.len() < 3 || args.iter().any(|a| a == "--help" || a == "-h") {
            Self::print_usage(&args[0]);
            return None;
        }

        let mut manifest_path: Option<PathBuf> = None;
        let mut mountpoint: Option<PathBuf> = None;
        let mut show_stats: bool = false;
        let mut use_mock: bool = false;
        let mut bucket: String = "adeadlineja".to_string();
        let mut root_prefix: String = "DeadlineCloud".to_string();
        let mut region: String = "us-west-2".to_string();

        let mut i: usize = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--stats" => show_stats = true,
                "--mock" => use_mock = true,
                "--bucket" => {
                    i += 1;
                    bucket = args.get(i)?.clone();
                }
                "--root-prefix" => {
                    i += 1;
                    root_prefix = args.get(i)?.clone();
                }
                "--region" => {
                    i += 1;
                    region = args.get(i)?.clone();
                }
                arg if !arg.starts_with('-') => {
                    if manifest_path.is_none() {
                        manifest_path = Some(PathBuf::from(arg));
                    } else if mountpoint.is_none() {
                        mountpoint = Some(PathBuf::from(arg));
                    }
                }
                _ => {
                    eprintln!("Unknown option: {}", args[i]);
                    Self::print_usage(&args[0]);
                    return None;
                }
            }
            i += 1;
        }

        Some(Self {
            manifest_path: manifest_path?,
            mountpoint: mountpoint?,
            show_stats,
            use_mock,
            bucket,
            root_prefix,
            region,
        })
    }

    /// Print usage information.
    ///
    /// # Arguments
    /// * `program` - Program name for usage message
    fn print_usage(program: &str) {
        eprintln!("Usage: {} <manifest.json> <mountpoint> [options]", program);
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --stats              Show live statistics dashboard (updates every 2s)");
        eprintln!("  --mock               Use mock file store instead of S3");
        eprintln!("  --bucket <name>      S3 bucket name (default: adeadlineja)");
        eprintln!("  --root-prefix <pfx>  S3 root prefix (default: DeadlineCloud)");
        eprintln!("  --region <region>    AWS region (default: us-west-2)");
        eprintln!();
        eprintln!("Example:");
        eprintln!(
            "  {} /tmp/manifest.json ~/vfs --stats --bucket adeadlineja --root-prefix DeadlineCloud",
            program
        );
    }
}

/// Format bytes as human-readable string.
///
/// # Arguments
/// * `bytes` - Number of bytes
///
/// # Returns
/// Human-readable string (e.g., "1.23 MB").
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

/// Print VFS statistics dashboard.
///
/// # Arguments
/// * `collector` - Stats collector to query
fn print_stats(collector: &VfsStatsCollector) {
    let stats = collector.collect();

    // Clear screen and move cursor to top
    print!("\x1B[2J\x1B[H");

    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║                    VFS Statistics Dashboard                       ║");
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ Uptime: {:>5}s                                                   ║", stats.uptime_secs);
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ FILESYSTEM                                                        ║");
    println!("║   Inodes: {:>10}                                              ║", stats.inode_count);
    println!("║   Open files: {:>6}                                              ║", stats.open_files);
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ MEMORY POOL                                                       ║");
    println!("║   Blocks: {:>6} total, {:>6} in use                            ║",
             stats.pool_stats.total_blocks, stats.pool_stats.in_use_blocks);
    println!("║   Memory: {:>12} / {:>12} ({:.1}%)                   ║",
             format_bytes(stats.pool_stats.current_size),
             format_bytes(stats.pool_stats.max_size),
             stats.pool_stats.utilization());
    println!("║   Pending fetches: {:>4}                                          ║", stats.pool_stats.pending_fetches);
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ CACHE                                                             ║");
    println!("║   Hits: {:>10}  Allocations: {:>10}                       ║",
             stats.cache_hits, stats.cache_allocations);
    println!("║   Hit rate: {:>6.2}%                                              ║", stats.cache_hit_rate);
    println!("╠══════════════════════════════════════════════════════════════════╣");

    if stats.open_files > 0 {
        println!("║ OPEN FILES                                                        ║");
        for (i, file) in stats.open_file_list.iter().take(10).enumerate() {
            let path_display: String = if file.path.len() > 50 {
                format!("...{}", &file.path[file.path.len() - 47..])
            } else {
                file.path.clone()
            };
            println!("║   {:>2}. {:50} {:>8} ║",
                     i + 1, path_display, format_bytes(file.size));
        }
        if stats.open_files > 10 {
            println!("║   ... and {} more                                               ║",
                     stats.open_files - 10);
        }
    } else {
        println!("║ OPEN FILES: (none)                                                ║");
    }

    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!("\nPress Ctrl+C to unmount and exit.");
}

/// Spawn a background thread that prints stats periodically.
///
/// # Arguments
/// * `collector` - Stats collector to query
/// * `running` - Atomic flag to control thread lifetime
/// * `interval_secs` - Interval between stats updates
///
/// # Returns
/// Handle to the spawned thread.
fn spawn_stats_thread(
    collector: VfsStatsCollector,
    running: Arc<AtomicBool>,
    interval_secs: u64,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while running.load(Ordering::SeqCst) {
            print_stats(&collector);
            thread::sleep(Duration::from_secs(interval_secs));
        }
    })
}

/// Expand tilde in path to home directory.
///
/// # Arguments
/// * `path` - Path that may start with ~
///
/// # Returns
/// Expanded path.
fn expand_tilde(path: PathBuf) -> PathBuf {
    if path.starts_with("~") {
        let home: PathBuf = dirs::home_dir().expect("Could not determine home directory");
        home.join(path.strip_prefix("~").unwrap())
    } else {
        path
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: CliArgs = match CliArgs::parse() {
        Some(a) => a,
        None => std::process::exit(1),
    };

    let mountpoint: PathBuf = expand_tilde(args.mountpoint);

    println!("Loading manifest from: {}", args.manifest_path.display());
    let json: String = std::fs::read_to_string(&args.manifest_path)?;
    let manifest: Manifest = Manifest::decode(&json)?;

    println!(
        "Manifest version: {:?}, files: {}, total size: {} bytes",
        manifest.version(),
        manifest.file_count(),
        manifest.total_size()
    );

    if !mountpoint.exists() {
        std::fs::create_dir_all(&mountpoint)?;
    }

    let runtime: tokio::runtime::Runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();

    // Create file store (S3 via CrtStorageClient or mock)
    let store: Arc<dyn FileStore> = if args.use_mock {
        println!("Using mock file store");
        Arc::new(MockFileStore)
    } else {
        println!(
            "Using S3 file store: s3://{}/{}/Data/",
            args.bucket, args.root_prefix
        );

        let settings = StorageSettings {
            region: args.region,
            ..Default::default()
        };

        let client: CrtStorageClient = runtime.block_on(CrtStorageClient::new(settings))?;

        let location = S3Location::new(args.bucket, args.root_prefix, "Data", "Manifests");

        Arc::new(StorageClientAdapter::new(client, location))
    };

    let vfs: DeadlineVfs = DeadlineVfs::new(&manifest, store, VfsOptions::default())?;

    // Get stats collector before moving vfs
    let stats_collector: VfsStatsCollector = vfs.stats_collector();

    let running: Arc<AtomicBool> = Arc::new(AtomicBool::new(true));
    let r: Arc<AtomicBool> = running.clone();
    ctrlc::set_handler(move || {
        println!("\nReceived SIGINT, unmounting...");
        r.store(false, Ordering::SeqCst);
    })?;

    println!("Mounting VFS at: {}", mountpoint.display());

    let session = rusty_attachments_vfs::spawn_mount(vfs, &mountpoint)?;

    // Spawn stats thread if requested
    let stats_handle: Option<thread::JoinHandle<()>> = if args.show_stats {
        Some(spawn_stats_thread(stats_collector, running.clone(), 2))
    } else {
        println!("Press Ctrl+C to unmount and exit.");
        None
    };

    while running.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }

    drop(session);

    if let Some(handle) = stats_handle {
        let _ = handle.join();
    }

    println!("Unmounted successfully.");

    Ok(())
}
