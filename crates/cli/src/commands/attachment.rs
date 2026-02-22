//! `ra attachment` — Attachment download and upload commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::Subcommand;

use rusty_attachments_model::Manifest;
use rusty_attachments_storage::{
    ConflictResolution, DownloadOrchestrator, ManifestLocation, S3CheckCache, S3Location,
    SqliteS3CheckCache, StorageSettings, TransferStatistics, UploadOrchestrator,
    manifest_storage::{ManifestUploadResult, upload_input_manifest},
};
use rusty_attachments_storage_crt::DefaultClient;

use crate::config::CliConfig;
use crate::output;

use super::CliError;

/// Attachment subcommands.
#[derive(Subcommand)]
pub enum AttachmentAction {
    /// Download CAS file contents from S3 using local manifest files.
    Download(DownloadArgs),
    /// Upload CAS file contents to S3 using local manifest files.
    Upload(UploadArgs),
}

/// Arguments for `ra attachment download`.
#[derive(clap::Args)]
pub struct DownloadArgs {
    /// Path(s) to manifest file(s).
    #[arg(long, short = 'm', required = true)]
    pub manifests: Vec<String>,

    /// S3 root URI (e.g. "s3://bucket/DeadlineCloud").
    #[arg(long)]
    pub s3_root_uri: Option<String>,

    /// Conflict resolution: SKIP, OVERWRITE, or CREATE_COPY.
    #[arg(long, default_value = "CREATE_COPY")]
    pub conflict_resolution: String,

    /// AWS region.
    #[arg(long)]
    pub region: Option<String>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `ra attachment upload`.
#[derive(clap::Args)]
pub struct UploadArgs {
    /// Path(s) to manifest file(s).
    #[arg(long, short = 'm', required = true)]
    pub manifests: Vec<String>,

    /// Root directory(ies) holding the actual files.
    #[arg(long, short = 'r', required = true)]
    pub root_dirs: Vec<String>,

    /// S3 root URI (e.g. "s3://bucket/DeadlineCloud").
    #[arg(long)]
    pub s3_root_uri: Option<String>,

    /// AWS region.
    #[arg(long)]
    pub region: Option<String>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

impl AttachmentAction {
    /// Execute the attachment action.
    pub async fn run(self) -> Result<(), CliError> {
        match self {
            AttachmentAction::Download(args) => run_download(args).await,
            AttachmentAction::Upload(args) => run_upload(args).await,
        }
    }
}

/// Parse an S3 root URI into (bucket, root_prefix).
fn parse_s3_root_uri(uri: &str) -> Result<(String, String), CliError> {
    let stripped: &str = uri
        .strip_prefix("s3://")
        .ok_or_else(|| CliError::Validation("S3 URI must start with s3://".to_string()))?;
    let slash_pos: usize = stripped
        .find('/')
        .ok_or_else(|| CliError::Validation("S3 URI must contain bucket/prefix".to_string()))?;
    let bucket: String = stripped[..slash_pos].to_string();
    let prefix: String = stripped[slash_pos + 1..].to_string();
    Ok((bucket, prefix))
}

/// Parse a conflict resolution string.
fn parse_conflict_resolution(s: &str) -> Result<ConflictResolution, CliError> {
    match s.to_uppercase().as_str() {
        "SKIP" => Ok(ConflictResolution::Skip),
        "OVERWRITE" => Ok(ConflictResolution::Overwrite),
        "CREATE_COPY" => Ok(ConflictResolution::CreateCopy),
        _ => Err(CliError::Validation(format!(
            "Invalid conflict resolution: {}. Use SKIP, OVERWRITE, or CREATE_COPY",
            s
        ))),
    }
}

/// Resolve the AWS region from args, env, or default.
fn resolve_region(arg: Option<&str>) -> String {
    arg.map(String::from)
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
        .unwrap_or_else(|| "us-west-2".to_string())
}

/// Create a storage client for the given region.
async fn create_client(region: &str) -> Result<DefaultClient, CliError> {
    let settings: StorageSettings = StorageSettings {
        region: region.to_string(),
        ..Default::default()
    };
    DefaultClient::new(settings)
        .await
        .map_err(|e| CliError::Storage(format!("Failed to create client: {}", e)))
}

/// Read and decode a manifest from a local file.
fn read_manifest(path: &str) -> Result<Manifest, CliError> {
    let json: String = std::fs::read_to_string(path)
        .map_err(|e| CliError::Io(e))?;
    Manifest::decode(&json).map_err(|e| CliError::Manifest(e.to_string()))
}

// ── Download ────────────────────────────────────────────────────────

/// Execute `ra attachment download`.
async fn run_download(args: DownloadArgs) -> Result<(), CliError> {
    let s3_uri: &str = args
        .s3_root_uri
        .as_deref()
        .ok_or_else(|| CliError::Validation("--s3-root-uri is required".to_string()))?;
    let (bucket, root_prefix) = parse_s3_root_uri(s3_uri)?;
    let conflict: ConflictResolution = parse_conflict_resolution(&args.conflict_resolution)?;
    let region: String = resolve_region(args.region.as_deref());

    let s3_loc: S3Location = S3Location::new(&bucket, &root_prefix, "Data", "Manifests");
    let client: DefaultClient = create_client(&region).await?;
    let orchestrator: DownloadOrchestrator<'_, DefaultClient> =
        DownloadOrchestrator::new(&client, s3_loc);

    let mut total_stats: TransferStatistics = TransferStatistics::default();
    let mut file_counts: HashMap<String, u64> = HashMap::new();

    for manifest_path in &args.manifests {
        let manifest: Manifest = read_manifest(manifest_path)?;

        let dest_root: String = Path::new(manifest_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(".")
            .to_string();

        println!("Downloading files from {} to {}", manifest_path, dest_root);

        let stats: TransferStatistics = orchestrator
            .download_manifest_contents(&manifest, &dest_root, conflict, None)
            .await
            .map_err(|e| CliError::Storage(format!("Download failed: {}", e)))?;

        *file_counts.entry(dest_root).or_insert(0) += stats.files_processed;
        total_stats.merge(stats);
    }

    println!(
        "\nDownloaded {} files ({}), skipped {} files",
        total_stats.files_transferred,
        output::human_readable_size(total_stats.bytes_transferred),
        total_stats.files_skipped,
    );

    if args.json {
        let out = serde_json::json!({
            "total_files": total_stats.files_processed,
            "downloaded_files": total_stats.files_transferred,
            "downloaded_bytes": total_stats.bytes_transferred,
            "skipped_files": total_stats.files_skipped,
            "skipped_bytes": total_stats.bytes_skipped,
            "file_counts_by_root": file_counts,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    }
    Ok(())
}

// ── Upload ──────────────────────────────────────────────────────────

/// Execute `ra attachment upload`.
async fn run_upload(args: UploadArgs) -> Result<(), CliError> {
    let s3_uri: &str = args
        .s3_root_uri
        .as_deref()
        .ok_or_else(|| CliError::Validation("--s3-root-uri is required".to_string()))?;
    let (bucket, root_prefix) = parse_s3_root_uri(s3_uri)?;
    let region: String = resolve_region(args.region.as_deref());

    if args.manifests.len() != args.root_dirs.len() {
        return Err(CliError::Validation(
            "Number of --manifests must match number of --root-dirs".to_string(),
        ));
    }

    let s3_loc: S3Location = S3Location::new(&bucket, &root_prefix, "Data", "Manifests");
    let manifest_loc: ManifestLocation = ManifestLocation::new(&bucket, &root_prefix, "", "");
    let client: DefaultClient = create_client(&region).await?;

    // Open S3 check cache
    let cfg: CliConfig = CliConfig::load()?;
    let cache_dir: PathBuf = cfg.cache_dir();
    std::fs::create_dir_all(&cache_dir)?;
    let cache_path: PathBuf = cache_dir.join("s3_check_cache.db");
    let s3_cache: Option<S3CheckCache> = SqliteS3CheckCache::open(&cache_path)
        .ok()
        .map(S3CheckCache::new);

    let mut orchestrator: UploadOrchestrator<'_, DefaultClient> =
        UploadOrchestrator::new(&client, s3_loc);
    if let Some(cache) = s3_cache {
        orchestrator = orchestrator.with_s3_check_cache(cache);
    }

    let mut results: Vec<serde_json::Value> = Vec::new();

    for (manifest_path, root_dir) in args.manifests.iter().zip(args.root_dirs.iter()) {
        let manifest: Manifest = read_manifest(manifest_path)?;

        println!("Uploading files from {} (root: {})", manifest_path, root_dir);

        let stats: TransferStatistics = orchestrator
            .upload_manifest_contents(&manifest, root_dir, None)
            .await
            .map_err(|e| CliError::Storage(format!("Upload failed: {}", e)))?;

        // Upload the manifest itself
        let upload_result: ManifestUploadResult = upload_input_manifest(
            &client,
            &manifest_loc,
            &manifest,
            root_dir,
            None,
        )
        .await
        .map_err(|e| CliError::Storage(format!("Manifest upload failed: {}", e)))?;

        println!(
            "  Uploaded {} files ({}), skipped {}. Manifest: {}",
            stats.files_transferred,
            output::human_readable_size(stats.bytes_transferred),
            stats.files_skipped,
            upload_result.s3_key,
        );

        results.push(serde_json::json!({
            "output_manifest_path": upload_result.s3_key,
            "output_manifest_hash": upload_result.manifest_hash,
            "source_path": root_dir,
        }));
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results).unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_s3_root_uri_valid() {
        let (bucket, prefix) = parse_s3_root_uri("s3://my-bucket/DeadlineCloud").unwrap();
        assert_eq!(bucket, "my-bucket");
        assert_eq!(prefix, "DeadlineCloud");
    }

    #[test]
    fn test_parse_s3_root_uri_nested_prefix() {
        let (bucket, prefix) = parse_s3_root_uri("s3://bucket/a/b/c").unwrap();
        assert_eq!(bucket, "bucket");
        assert_eq!(prefix, "a/b/c");
    }

    #[test]
    fn test_parse_s3_root_uri_no_scheme() {
        let result = parse_s3_root_uri("my-bucket/prefix");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_s3_root_uri_no_prefix() {
        let result = parse_s3_root_uri("s3://bucket-only");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_conflict_resolution() {
        assert!(matches!(parse_conflict_resolution("SKIP").unwrap(), ConflictResolution::Skip));
        assert!(matches!(parse_conflict_resolution("overwrite").unwrap(), ConflictResolution::Overwrite));
        assert!(matches!(parse_conflict_resolution("Create_Copy").unwrap(), ConflictResolution::CreateCopy));
        assert!(parse_conflict_resolution("invalid").is_err());
    }

    #[test]
    fn test_resolve_region_from_arg() {
        assert_eq!(resolve_region(Some("eu-west-1")), "eu-west-1");
    }

    #[test]
    fn test_resolve_region_default() {
        // Clear env var for test isolation
        std::env::remove_var("AWS_DEFAULT_REGION");
        assert_eq!(resolve_region(None), "us-west-2");
    }
}
