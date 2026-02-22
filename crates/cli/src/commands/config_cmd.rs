//! `ra config` — Configuration management and benchmarking.

use std::path::PathBuf;
use std::time::Instant;

use clap::Subcommand;

use rusty_attachments_common::hash_file;
use rusty_attachments_common::ProgressCallback;
use rusty_attachments_filesystem::{FileSystemScanner, GlobFilter, ScanProgress, SnapshotOptions};
use rusty_attachments_model::{HashAlgorithm, ManifestVersion};

use crate::config::CliConfig;
use crate::output;

use super::CliError;

/// Config subcommands.
#[derive(Subcommand)]
pub enum ConfigAction {
    /// Display all configuration settings.
    Show {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Get a specific configuration value.
    Get {
        /// Dotted key (e.g. "defaults.farm_id").
        key: String,
    },
    /// Set a configuration value.
    Set {
        /// Dotted key (e.g. "defaults.farm_id").
        key: String,
        /// Value to set.
        value: String,
    },
    /// Run a performance benchmark.
    Benchmark(BenchmarkArgs),
}

/// Arguments for the benchmark subcommand.
#[derive(clap::Args)]
pub struct BenchmarkArgs {
    /// Operation to benchmark.
    #[arg(long, value_enum)]
    pub operation: BenchmarkOperation,

    /// Root directory for snapshot benchmarks.
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// Number of synthetic files to generate (if --root not provided).
    #[arg(long, default_value = "100")]
    pub file_count: usize,

    /// Size of each synthetic file in bytes.
    #[arg(long, default_value = "1048576")]
    pub file_size: u64,

    /// Number of benchmark iterations.
    #[arg(long, default_value = "3")]
    pub iterations: usize,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Benchmark operation types.
#[derive(clap::ValueEnum, Clone, Debug)]
pub enum BenchmarkOperation {
    /// Benchmark directory scanning + hashing.
    Snapshot,
    /// Benchmark pure xxh128 hashing throughput.
    Hash,
}

impl ConfigAction {
    /// Execute the config action.
    pub fn run(self) -> Result<(), CliError> {
        match self {
            ConfigAction::Show { json } => run_show(json),
            ConfigAction::Get { key } => run_get(&key),
            ConfigAction::Set { key, value } => run_set(&key, &value),
            ConfigAction::Benchmark(args) => run_benchmark(args),
        }
    }
}

/// Display all settings.
fn run_show(json: bool) -> Result<(), CliError> {
    let cfg: CliConfig = CliConfig::load()?;
    let settings: Vec<(String, String)> = cfg.all_settings();

    if json {
        let map: serde_json::Map<String, serde_json::Value> = settings
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&map).unwrap_or_default()
        );
    } else if settings.is_empty() {
        println!("No configuration settings found.");
        println!("Config file: {}", cfg_path_display());
    } else {
        for (key, value) in &settings {
            println!("{} = {}", key, value);
        }
    }
    Ok(())
}

/// Get a single setting.
fn run_get(key: &str) -> Result<(), CliError> {
    let cfg: CliConfig = CliConfig::load()?;
    match cfg.get(key) {
        Some(value) => {
            println!("{}", value);
            Ok(())
        }
        None => {
            eprintln!("Key '{}' not found", key);
            Ok(())
        }
    }
}

/// Set a single setting.
fn run_set(key: &str, value: &str) -> Result<(), CliError> {
    let mut cfg: CliConfig = CliConfig::load()?;
    cfg.set(key, value)?;
    println!("Set {} = {}", key, value);
    Ok(())
}

/// Run a benchmark.
fn run_benchmark(args: BenchmarkArgs) -> Result<(), CliError> {
    match args.operation {
        BenchmarkOperation::Snapshot => benchmark_snapshot(&args),
        BenchmarkOperation::Hash => benchmark_hash(&args),
    }
}

/// Benchmark directory scanning + hashing.
fn benchmark_snapshot(args: &BenchmarkArgs) -> Result<(), CliError> {
    let root: PathBuf = match &args.root {
        Some(r) => {
            if !r.is_dir() {
                return Err(CliError::Validation(format!(
                    "Root directory does not exist: {}",
                    r.display()
                )));
            }
            r.clone()
        }
        None => {
            println!(
                "Generating {} synthetic files ({} bytes each)...",
                args.file_count, args.file_size
            );
            let dir: tempfile::TempDir = generate_synthetic_workload(args.file_count, args.file_size)?;
            // Leak the TempDir so it persists through the benchmark
            let path: PathBuf = dir.path().to_path_buf();
            std::mem::forget(dir);
            path
        }
    };

    println!("Benchmarking snapshot on: {}", root.display());
    println!("Iterations: {}\n", args.iterations);

    let mut durations: Vec<f64> = Vec::with_capacity(args.iterations);
    let mut file_count: usize = 0;
    let mut total_bytes: u64 = 0;

    for i in 0..args.iterations {
        let options: SnapshotOptions = SnapshotOptions {
            root: root.clone(),
            input_files: None,
            version: ManifestVersion::V2025_12,
            filter: GlobFilter::default(),
            hash_algorithm: HashAlgorithm::Xxh128,
            follow_symlinks: false,
            include_empty_dirs: true,
        };

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let start: Instant = Instant::now();
        let manifest = scanner
            .snapshot(&options, None::<&dyn ProgressCallback<ScanProgress>>)
            .map_err(|e| CliError::Filesystem(e.to_string()))?;
        let elapsed: f64 = start.elapsed().as_secs_f64();

        file_count = manifest.file_count();
        total_bytes = manifest.total_size();
        durations.push(elapsed);

        println!(
            "  Iteration {}: {:.3}s ({} files, {})",
            i + 1,
            elapsed,
            file_count,
            output::human_readable_size(total_bytes)
        );
    }

    let mean: f64 = durations.iter().sum::<f64>() / durations.len() as f64;
    let stddev: f64 = if durations.len() > 1 {
        let variance: f64 =
            durations.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (durations.len() - 1) as f64;
        variance.sqrt()
    } else {
        0.0
    };
    let throughput: f64 = if mean > 0.0 {
        total_bytes as f64 / mean / 1_000_000.0
    } else {
        0.0
    };

    println!("\nResults:");
    println!("  Files: {}", file_count);
    println!("  Total size: {}", output::human_readable_size(total_bytes));
    println!("  Mean: {:.3}s ± {:.3}s", mean, stddev);
    println!("  Throughput: {:.1} MB/s", throughput);

    if args.json {
        let result = serde_json::json!({
            "operation": "snapshot",
            "files": file_count,
            "total_bytes": total_bytes,
            "iterations": args.iterations,
            "mean_seconds": mean,
            "stddev_seconds": stddev,
            "throughput_mbps": throughput,
        });
        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    }

    Ok(())
}

/// Benchmark pure xxh128 hashing throughput.
fn benchmark_hash(args: &BenchmarkArgs) -> Result<(), CliError> {
    println!(
        "Generating {} files ({} bytes each) for hash benchmark...",
        args.file_count, args.file_size
    );
    let dir: tempfile::TempDir = generate_synthetic_workload(args.file_count, args.file_size)?;
    let total_bytes: u64 = args.file_count as u64 * args.file_size;

    // Collect file paths
    let files: Vec<PathBuf> = walkdir(dir.path())?;
    println!("Hashing {} files ({})\n", files.len(), output::human_readable_size(total_bytes));

    let mut durations: Vec<f64> = Vec::with_capacity(args.iterations);

    for i in 0..args.iterations {
        let start: Instant = Instant::now();
        for file in &files {
            hash_file(file).map_err(|e| CliError::Io(e))?;
        }
        let elapsed: f64 = start.elapsed().as_secs_f64();
        durations.push(elapsed);

        let throughput: f64 = total_bytes as f64 / elapsed / 1_000_000.0;
        println!("  Iteration {}: {:.3}s ({:.1} MB/s)", i + 1, elapsed, throughput);
    }

    let mean: f64 = durations.iter().sum::<f64>() / durations.len() as f64;
    let throughput: f64 = if mean > 0.0 {
        total_bytes as f64 / mean / 1_000_000.0
    } else {
        0.0
    };

    println!("\nResults:");
    println!("  Mean: {:.3}s", mean);
    println!("  Throughput: {:.1} MB/s", throughput);

    Ok(())
}

/// Generate a temporary directory with synthetic files for benchmarking.
fn generate_synthetic_workload(
    file_count: usize,
    file_size: u64,
) -> Result<tempfile::TempDir, CliError> {
    use std::io::Write;

    let dir: tempfile::TempDir = tempfile::tempdir()?;
    let buf: Vec<u8> = vec![0xABu8; std::cmp::min(file_size, 64 * 1024) as usize];

    for i in 0..file_count {
        let depth: usize = i % 5;
        let subdir: PathBuf = (0..depth).fold(dir.path().to_path_buf(), |p, d| {
            p.join(format!("d{}", d))
        });
        std::fs::create_dir_all(&subdir)?;

        let file_path: PathBuf = subdir.join(format!("file_{:06}.bin", i));
        let mut file: std::fs::File = std::fs::File::create(&file_path)?;
        let mut remaining: u64 = file_size;
        while remaining > 0 {
            let chunk: usize = std::cmp::min(remaining, buf.len() as u64) as usize;
            file.write_all(&buf[..chunk])?;
            remaining -= chunk as u64;
        }
    }
    Ok(dir)
}

/// Walk a directory and collect all file paths.
fn walkdir(root: &std::path::Path) -> Result<Vec<PathBuf>, CliError> {
    let mut files: Vec<PathBuf> = Vec::new();
    walk_recursive(root, &mut files)?;
    Ok(files)
}

/// Recursive directory walker.
fn walk_recursive(dir: &std::path::Path, files: &mut Vec<PathBuf>) -> Result<(), CliError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path: PathBuf = entry.path();
        if path.is_dir() {
            walk_recursive(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// Display path for config file.
fn cfg_path_display() -> String {
    dirs::home_dir()
        .map(|h| h.join(".deadline").join("config").display().to_string())
        .unwrap_or_else(|| "~/.deadline/config".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_synthetic_workload() {
        let dir: tempfile::TempDir = generate_synthetic_workload(10, 1024).unwrap();
        let files: Vec<PathBuf> = walkdir(dir.path()).unwrap();
        assert_eq!(files.len(), 10);

        // Verify file sizes
        for file in &files {
            let meta = std::fs::metadata(file).unwrap();
            assert_eq!(meta.len(), 1024);
        }
    }

    #[test]
    fn test_generate_workload_creates_subdirectories() {
        let dir: tempfile::TempDir = generate_synthetic_workload(10, 64).unwrap();
        // Files at depth 0..4 should create nested directories
        assert!(dir.path().join("d0").exists());
        assert!(dir.path().join("d0").join("d1").exists());
    }

    #[test]
    fn test_walkdir_empty() {
        let dir: tempfile::TempDir = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = walkdir(dir.path()).unwrap();
        assert!(files.is_empty());
    }
}
