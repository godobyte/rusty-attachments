//! `ra manifest` — Manifest snapshot, diff, upload, and download commands.

use std::path::PathBuf;

use clap::Subcommand;

use rusty_attachments_common::ProgressCallback;
use rusty_attachments_filesystem::{
    DiffEngine, DiffMode, DiffOptions, DiffResult, FileSystemScanner, GlobFilter, ScanProgress,
    SnapshotOptions,
};
use rusty_attachments_model::{HashAlgorithm, Manifest, ManifestVersion};
use rusty_attachments_storage::manifest_storage;

use crate::progress::ScanProgressBar;

use super::CliError;

/// Manifest subcommands.
#[derive(Subcommand)]
pub enum ManifestAction {
    /// Create a manifest snapshot of a directory.
    Snapshot(SnapshotArgs),
    /// Diff a directory against an existing manifest.
    Diff(DiffArgs),
}

// ── Snapshot ────────────────────────────────────────────────────────

/// Arguments for `ra manifest snapshot`.
#[derive(clap::Args)]
pub struct SnapshotArgs {
    /// Root directory to snapshot.
    #[arg(long)]
    pub root: String,

    /// Destination directory for the manifest file.
    #[arg(long, short = 'd')]
    pub destination: Option<String>,

    /// Manifest name (defaults to sanitized root path).
    #[arg(long, short = 'n')]
    pub name: Option<String>,

    /// Glob include patterns.
    #[arg(long, short = 'i')]
    pub include: Vec<String>,

    /// Glob exclude patterns.
    #[arg(long, short = 'e')]
    pub exclude: Vec<String>,

    /// Path to JSON config with include/exclude patterns.
    #[arg(long)]
    pub include_exclude_config: Option<String>,

    /// Path to existing manifest for diff mode.
    #[arg(long)]
    pub diff: Option<String>,

    /// Rehash all files (use hash comparison instead of mtime/size).
    #[arg(long)]
    pub force_rehash: bool,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

// ── Diff ────────────────────────────────────────────────────────────

/// Arguments for `ra manifest diff`.
#[derive(clap::Args)]
pub struct DiffArgs {
    /// Root directory to compare.
    #[arg(long)]
    pub root: String,

    /// Path to the reference manifest file.
    #[arg(long)]
    pub manifest: String,

    /// Glob include patterns.
    #[arg(long, short = 'i')]
    pub include: Vec<String>,

    /// Glob exclude patterns.
    #[arg(long, short = 'e')]
    pub exclude: Vec<String>,

    /// Path to JSON config with include/exclude patterns.
    #[arg(long)]
    pub include_exclude_config: Option<String>,

    /// Rehash all files (use hash comparison instead of mtime/size).
    #[arg(long)]
    pub force_rehash: bool,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

impl ManifestAction {
    /// Execute the manifest action.
    pub async fn run(self) -> Result<(), CliError> {
        match self {
            ManifestAction::Snapshot(args) => run_snapshot(args),
            ManifestAction::Diff(args) => run_diff(args),
        }
    }
}

// ── Snapshot implementation ─────────────────────────────────────────

/// Execute `ra manifest snapshot`.
fn run_snapshot(args: SnapshotArgs) -> Result<(), CliError> {
    let root: &str = &args.root;
    if !PathBuf::from(root).is_dir() {
        return Err(CliError::Validation(format!(
            "Root directory does not exist: {}",
            root
        )));
    }

    let destination: String = args.destination.unwrap_or_else(|| root.to_string());
    if !PathBuf::from(&destination).is_dir() {
        return Err(CliError::Validation(format!(
            "Destination directory does not exist: {}",
            destination
        )));
    }

    let filter: GlobFilter = build_glob_filter(&args.include, &args.exclude, args.include_exclude_config.as_deref())?;
    let progress: ScanProgressBar = ScanProgressBar::new();

    // Diff mode
    if let Some(diff_path) = &args.diff {
        let result: Option<(String, String)> =
            snapshot_diff(root, &destination, args.name.as_deref(), diff_path, args.force_rehash, &filter, &progress)?;
        progress.finish();

        match result {
            Some((manifest_root, manifest_path)) => {
                println!("Manifest generated at {}", manifest_path);
                if args.json {
                    let out = serde_json::json!({"root": manifest_root, "manifest": manifest_path});
                    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                }
            }
            None => println!("No changes detected."),
        }
        return Ok(());
    }

    // Full snapshot mode
    let result: Option<(String, String)> =
        snapshot_full(root, &destination, args.name.as_deref(), &filter, &progress)?;
    progress.finish();

    match result {
        Some((manifest_root, manifest_path)) => {
            println!("Manifest generated at {}", manifest_path);
            if args.json {
                let out = serde_json::json!({"root": manifest_root, "manifest": manifest_path});
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            }
        }
        None => println!("No files found in root directory."),
    }
    Ok(())
}

/// Full snapshot: scan directory, hash files, write manifest.
///
/// # Returns
/// `Some((root, manifest_path))` on success, `None` if no files found.
fn snapshot_full(
    root: &str,
    destination: &str,
    name: Option<&str>,
    filter: &GlobFilter,
    progress: &ScanProgressBar,
) -> Result<Option<(String, String)>, CliError> {
    let options: SnapshotOptions = SnapshotOptions {
        root: PathBuf::from(root),
        input_files: None,
        version: ManifestVersion::V2025_12,
        filter: filter.clone(),
        hash_algorithm: HashAlgorithm::Xxh128,
        follow_symlinks: false,
        include_empty_dirs: true,
    };

    let scanner: FileSystemScanner = FileSystemScanner::new();
    let manifest: Manifest = scanner
        .snapshot(&options, Some(progress as &dyn ProgressCallback<ScanProgress>))
        .map_err(|e| CliError::Filesystem(e.to_string()))?;

    if manifest.file_count() == 0 {
        return Ok(None);
    }

    let path: String = write_manifest(&manifest, root, destination, name)?;
    Ok(Some((root.to_string(), path)))
}

/// Diff snapshot: compare directory against existing manifest, write diff manifest.
///
/// # Returns
/// `Some((root, manifest_path))` on success, `None` if no changes.
fn snapshot_diff(
    root: &str,
    destination: &str,
    name: Option<&str>,
    diff_path: &str,
    force_rehash: bool,
    filter: &GlobFilter,
    progress: &ScanProgressBar,
) -> Result<Option<(String, String)>, CliError> {
    let manifest_bytes: Vec<u8> =
        std::fs::read(diff_path).map_err(|e| CliError::Io(e))?;
    let json: String = String::from_utf8(manifest_bytes.clone())
        .map_err(|e| CliError::Manifest(format!("Invalid UTF-8: {}", e)))?;
    let reference: Manifest =
        Manifest::decode(&json).map_err(|e| CliError::Manifest(e.to_string()))?;

    let mode: DiffMode = if force_rehash { DiffMode::Hash } else { DiffMode::Fast };
    let diff_opts: DiffOptions = DiffOptions {
        root: PathBuf::from(root),
        filter: filter.clone(),
        mode,
        parallelism: 0,
    };

    let engine: DiffEngine = DiffEngine::new();
    let result: DiffResult = engine
        .diff(
            &reference,
            &diff_opts,
            Some(progress as &dyn ProgressCallback<ScanProgress>),
        )
        .map_err(|e| CliError::Filesystem(e.to_string()))?;

    if result.added.is_empty() && result.modified.is_empty() && result.deleted.is_empty() {
        return Ok(None);
    }

    let diff_manifest: Manifest = engine
        .create_diff_manifest(&reference, &manifest_bytes, &result, &diff_opts)
        .map_err(|e| CliError::Filesystem(e.to_string()))?;

    let path: String = write_manifest(&diff_manifest, root, destination, name)?;
    Ok(Some((root.to_string(), path)))
}

// ── Diff implementation ─────────────────────────────────────────────

/// Execute `ra manifest diff`.
fn run_diff(args: DiffArgs) -> Result<(), CliError> {
    if !PathBuf::from(&args.root).is_dir() {
        return Err(CliError::Validation(format!(
            "Root directory does not exist: {}",
            args.root
        )));
    }
    if !PathBuf::from(&args.manifest).is_file() {
        return Err(CliError::Validation(format!(
            "Manifest file does not exist: {}",
            args.manifest
        )));
    }

    let json_str: String =
        std::fs::read_to_string(&args.manifest).map_err(|e| CliError::Io(e))?;
    let reference: Manifest =
        Manifest::decode(&json_str).map_err(|e| CliError::Manifest(e.to_string()))?;

    let filter: GlobFilter = build_glob_filter(&args.include, &args.exclude, args.include_exclude_config.as_deref())?;
    let mode: DiffMode = if args.force_rehash { DiffMode::Hash } else { DiffMode::Fast };

    let diff_opts: DiffOptions = DiffOptions {
        root: PathBuf::from(&args.root),
        filter,
        mode,
        parallelism: 0,
    };

    let engine: DiffEngine = DiffEngine::new();
    let progress: ScanProgressBar = ScanProgressBar::new();
    let result: DiffResult = engine
        .diff(
            &reference,
            &diff_opts,
            Some(&progress as &dyn ProgressCallback<ScanProgress>),
        )
        .map_err(|e| CliError::Filesystem(e.to_string()))?;
    progress.finish();

    if args.json {
        let out = serde_json::json!({
            "new": result.added.iter().map(|f| &f.path).collect::<Vec<_>>(),
            "modified": result.modified.iter().map(|f| &f.path).collect::<Vec<_>>(),
            "deleted": &result.deleted,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        println!("New files ({}):", result.added.len());
        for f in &result.added {
            println!("  + {}", f.path);
        }
        println!("Modified files ({}):", result.modified.len());
        for f in &result.modified {
            println!("  ~ {}", f.path);
        }
        println!("Deleted files ({}):", result.deleted.len());
        for f in &result.deleted {
            println!("  - {}", f);
        }
    }
    Ok(())
}

// ── Shared helpers ──────────────────────────────────────────────────

/// Build a GlobFilter from CLI arguments.
///
/// Exclude patterns take priority over include patterns.
pub fn build_glob_filter(
    include: &[String],
    exclude: &[String],
    config_path: Option<&str>,
) -> Result<GlobFilter, CliError> {
    if let Some(path) = config_path {
        let json: String = std::fs::read_to_string(path)
            .map_err(|e| CliError::Validation(format!("Cannot read glob config: {}", e)))?;
        let config: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| CliError::Validation(format!("Invalid glob config: {}", e)))?;

        let exc: Vec<String> = config
            .get("exclude")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if !exc.is_empty() {
            return GlobFilter::exclude(exc)
                .map_err(|e| CliError::Validation(format!("Invalid exclude pattern: {}", e)));
        }

        let inc: Vec<String> = config
            .get("include")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if !inc.is_empty() {
            return GlobFilter::include(inc)
                .map_err(|e| CliError::Validation(format!("Invalid include pattern: {}", e)));
        }
    }

    if !exclude.is_empty() {
        return GlobFilter::exclude(exclude.to_vec())
            .map_err(|e| CliError::Validation(format!("Invalid exclude pattern: {}", e)));
    }
    if !include.is_empty() {
        return GlobFilter::include(include.to_vec())
            .map_err(|e| CliError::Validation(format!("Invalid include pattern: {}", e)));
    }

    Ok(GlobFilter::default())
}

/// Write a manifest to a destination directory.
///
/// # Returns
/// The full path to the written manifest file.
pub fn write_manifest(
    manifest: &Manifest,
    root: &str,
    destination: &str,
    name: Option<&str>,
) -> Result<String, CliError> {
    let encoded: String = manifest
        .encode()
        .map_err(|e| CliError::Manifest(format!("Encode failed: {}", e)))?;

    let file_name: String = if let Some(n) = name {
        format!("{}.manifest", n)
    } else {
        let root_hash: String = manifest_storage::compute_manifest_name_hash(root);
        format!("{}.manifest", root_hash)
    };

    let manifest_path: PathBuf = PathBuf::from(destination).join(&file_name);
    std::fs::create_dir_all(destination)?;
    std::fs::write(&manifest_path, &encoded)?;

    Ok(manifest_path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a temp directory with some test files.
    fn create_test_dir() -> TempDir {
        let dir: TempDir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file1.txt"), "hello world").unwrap();
        std::fs::write(dir.path().join("file2.txt"), "goodbye world").unwrap();
        std::fs::create_dir_all(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("subdir").join("file3.txt"), "nested").unwrap();
        dir
    }

    #[test]
    fn test_snapshot_full_creates_manifest() {
        let dir: TempDir = create_test_dir();
        let dest: TempDir = TempDir::new().unwrap();
        let progress: ScanProgressBar = ScanProgressBar::new();

        let result = snapshot_full(
            dir.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            Some("test"),
            &GlobFilter::default(),
            &progress,
        )
        .unwrap();

        assert!(result.is_some());
        let (root, path) = result.unwrap();
        assert_eq!(root, dir.path().to_str().unwrap());
        assert!(PathBuf::from(&path).exists());
        assert!(path.ends_with("test.manifest"));
    }

    #[test]
    fn test_snapshot_full_empty_dir() {
        let dir: TempDir = TempDir::new().unwrap();
        let progress: ScanProgressBar = ScanProgressBar::new();

        let result = snapshot_full(
            dir.path().to_str().unwrap(),
            dir.path().to_str().unwrap(),
            None,
            &GlobFilter::default(),
            &progress,
        )
        .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_snapshot_diff_no_changes() {
        let dir: TempDir = create_test_dir();
        let dest: TempDir = TempDir::new().unwrap();
        let progress: ScanProgressBar = ScanProgressBar::new();

        // First create a snapshot
        let (_, manifest_path) = snapshot_full(
            dir.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            Some("base"),
            &GlobFilter::default(),
            &progress,
        )
        .unwrap()
        .unwrap();

        // Diff against the same directory — should find no changes
        let diff_result = snapshot_diff(
            dir.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            Some("diff"),
            &manifest_path,
            false,
            &GlobFilter::default(),
            &progress,
        )
        .unwrap();

        assert!(diff_result.is_none());
    }

    #[test]
    fn test_snapshot_diff_detects_new_file() {
        let dir: TempDir = create_test_dir();
        let dest: TempDir = TempDir::new().unwrap();
        let progress: ScanProgressBar = ScanProgressBar::new();

        // Create base snapshot
        let (_, manifest_path) = snapshot_full(
            dir.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            Some("base"),
            &GlobFilter::default(),
            &progress,
        )
        .unwrap()
        .unwrap();

        // Add a new file
        std::fs::write(dir.path().join("new_file.txt"), "new content").unwrap();

        // Diff should detect the new file
        let diff_result = snapshot_diff(
            dir.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            Some("diff"),
            &manifest_path,
            false,
            &GlobFilter::default(),
            &progress,
        )
        .unwrap();

        assert!(diff_result.is_some());
    }

    #[test]
    fn test_diff_detects_changes() {
        let dir: TempDir = create_test_dir();
        let dest: TempDir = TempDir::new().unwrap();
        let progress: ScanProgressBar = ScanProgressBar::new();

        // Create base snapshot
        let (_, manifest_path) = snapshot_full(
            dir.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            Some("base"),
            &GlobFilter::default(),
            &progress,
        )
        .unwrap()
        .unwrap();

        // Add a file, delete a file
        std::fs::write(dir.path().join("added.txt"), "new").unwrap();
        std::fs::remove_file(dir.path().join("file1.txt")).unwrap();

        // Run diff
        let json_str: String = std::fs::read_to_string(&manifest_path).unwrap();
        let reference: Manifest = Manifest::decode(&json_str).unwrap();
        let engine: DiffEngine = DiffEngine::new();
        let result: DiffResult = engine
            .diff(
                &reference,
                &DiffOptions {
                    root: dir.path().to_path_buf(),
                    filter: GlobFilter::default(),
                    mode: DiffMode::Fast,
                    parallelism: 0,
                },
                None::<&dyn ProgressCallback<ScanProgress>>,
            )
            .unwrap();

        assert!(!result.added.is_empty(), "Should detect added file");
        assert!(!result.deleted.is_empty(), "Should detect deleted file");
    }

    #[test]
    fn test_build_glob_filter_default() {
        let _filter = build_glob_filter(&[], &[], None).unwrap();
        // Default filter should match everything — just verify it doesn't error
        assert!(true);
    }

    #[test]
    fn test_build_glob_filter_exclude() {
        let filter = build_glob_filter(&[], &["*.tmp".to_string()], None);
        assert!(filter.is_ok());
    }

    #[test]
    fn test_write_manifest_with_name() {
        let dir: TempDir = create_test_dir();
        let dest: TempDir = TempDir::new().unwrap();
        let progress: ScanProgressBar = ScanProgressBar::new();

        let (_, path) = snapshot_full(
            dir.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            Some("my_manifest"),
            &GlobFilter::default(),
            &progress,
        )
        .unwrap()
        .unwrap();

        assert!(path.contains("my_manifest.manifest"));
    }

    #[test]
    fn test_write_manifest_without_name() {
        let dir: TempDir = create_test_dir();
        let dest: TempDir = TempDir::new().unwrap();
        let progress: ScanProgressBar = ScanProgressBar::new();

        let (_, path) = snapshot_full(
            dir.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            None,
            &GlobFilter::default(),
            &progress,
        )
        .unwrap()
        .unwrap();

        // Should use hash-based name
        assert!(path.ends_with(".manifest"));
        assert!(!path.contains("my_manifest"));
    }
}
