# Rusty Attachments: Manifest Storage Design

## Overview

This document proposes a set of functions for uploading and downloading manifest files to/from S3. These operations are distinct from CAS data transfers - manifests are stored with specific naming conventions, metadata, and content types.

---

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── storage/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── manifest_storage.rs   # NEW - Manifest upload/download
│   │       └── ...
```

---

## S3 Key Structure

### Input Manifests
```
s3://{bucket}/{root_prefix}/Manifests/{farm_id}/{queue_id}/Inputs/{guid}/{manifest_hash}_input
```

### Output Manifests (Task-Level)

Used when a session action is associated with a specific task:
```
s3://{bucket}/{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/{timestamp}_{session_action_id}/{manifest_hash}_output
```

### Output Manifests (Step-Level)

Used when a session action is associated with a step but not a specific task (e.g., step-level environment actions):
```
s3://{bucket}/{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{timestamp}_{session_action_id}/{manifest_hash}_output
```

The timestamp format is ISO 8601: `YYYY-MM-DDTHH:MM:SS.ffffffZ` (e.g., `2025-05-22T22:17:03.409012Z`)

---

## S3 Metadata

Manifests are uploaded with S3 user metadata to track the asset root and optional file system location.

### Metadata Fields

| Field | Required | Description |
|-------|----------|-------------|
| `asset-root` | Yes* | The root path the manifest was created from (ASCII paths only) |
| `asset-root-json` | Yes* | JSON-encoded root path (for non-ASCII paths, or as fallback) |
| `file-system-location-name` | No | File system location name for storage profile mapping |

*One of `asset-root` or `asset-root-json` is required. For backward compatibility, when the path contains non-ASCII characters, both fields are populated with the JSON-encoded value.

### Metadata Structure

```rust
/// S3 metadata attached to manifest uploads
#[derive(Debug, Clone, Default)]
pub struct ManifestS3Metadata {
    /// Root path the manifest was created from
    pub asset_root: String,
    
    /// Optional file system location name (for storage profile mapping)
    pub file_system_location_name: Option<String>,
}

impl ManifestS3Metadata {
    /// Build S3 metadata headers for upload
    pub fn to_s3_metadata(&self) -> HashMap<String, String> {
        let mut metadata = HashMap::new();
        
        // Handle non-ASCII paths with JSON encoding
        if self.asset_root.is_ascii() {
            metadata.insert("asset-root".into(), self.asset_root.clone());
        } else {
            // For non-ASCII paths, populate both fields for backward compatibility
            let json_encoded = serde_json::to_string(&self.asset_root).unwrap();
            metadata.insert("asset-root".into(), json_encoded.clone());
            metadata.insert("asset-root-json".into(), json_encoded);
        }
        
        if let Some(ref location) = self.file_system_location_name {
            metadata.insert("file-system-location-name".into(), location.clone());
        }
        
        metadata
    }
    
    /// Parse S3 metadata from download response
    pub fn from_s3_metadata(metadata: &HashMap<String, String>) -> Option<Self> {
        // Prefer asset-root-json for non-ASCII paths
        let asset_root = metadata.get("asset-root-json")
            .and_then(|v| serde_json::from_str(v).ok())
            .or_else(|| metadata.get("asset-root").cloned())?;
        
        let file_system_location_name = metadata.get("file-system-location-name").cloned();
        
        Some(Self { asset_root, file_system_location_name })
    }
}
```

---

## Data Structures

```rust
/// Location for storing/retrieving manifests
#[derive(Debug, Clone)]
pub struct ManifestLocation {
    pub bucket: String,
    pub root_prefix: String,
    pub farm_id: String,
    pub queue_id: String,
}

/// Identifiers for output manifest path (task-level)
#[derive(Debug, Clone)]
pub struct TaskOutputManifestPath {
    pub job_id: String,
    pub step_id: String,
    pub task_id: String,
    pub session_action_id: String,
    pub timestamp: f64,  // Seconds since epoch, converted to ISO 8601
}

/// Identifiers for output manifest path (step-level, no task)
#[derive(Debug, Clone)]
pub struct StepOutputManifestPath {
    pub job_id: String,
    pub step_id: String,
    pub session_action_id: String,
    pub timestamp: f64,  // Seconds since epoch, converted to ISO 8601
}

/// Convert a float timestamp (seconds since epoch) to ISO 8601 format.
/// 
/// Format: `YYYY-MM-DDTHH:MM:SS.ffffffZ`
/// Example: `2025-05-22T22:17:03.409012Z`
/// 
/// # Arguments
/// * `timestamp` - Seconds since Unix epoch as f64
/// 
/// # Returns
/// ISO 8601 formatted string with microsecond precision
pub fn float_to_iso_datetime_string(timestamp: f64) -> String {
    let seconds = timestamp.trunc() as i64;
    let microseconds = ((timestamp.fract()) * 1_000_000.0) as u32;
    
    // Create datetime from timestamp
    let dt = chrono::DateTime::from_timestamp(seconds, microseconds * 1000)
        .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH);
    
    dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

/// Metadata returned when downloading a manifest
#[derive(Debug, Clone)]
pub struct ManifestDownloadMetadata {
    /// Root path the manifest was created from
    pub asset_root: String,
    /// Optional file system location name
    pub file_system_location_name: Option<String>,
    /// Content type from S3
    pub content_type: Option<String>,
    /// S3 LastModified timestamp (seconds since epoch)
    /// Used for chronological manifest merging
    pub last_modified: Option<i64>,
}

/// Result of uploading a manifest
#[derive(Debug, Clone)]
pub struct ManifestUploadResult {
    pub s3_key: String,
    pub manifest_hash: String,
}
```

---

## Functions

### Upload Functions

```rust
/// Upload an input manifest to S3
///
/// Computes manifest hash, generates S3 key, sets content-type and metadata.
/// Key format: {root_prefix}/Manifests/{farm_id}/{queue_id}/Inputs/{guid}/{hash}_input
pub async fn upload_input_manifest(
    client: &impl StorageClient,
    location: &ManifestLocation,
    manifest: &Manifest,
    asset_root: &str,
    file_system_location_name: Option<&str>,
) -> Result<ManifestUploadResult, StorageError>;

/// Upload an output manifest to S3 (task-level)
///
/// Key format: {root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/{timestamp}_{session_action_id}/{hash}_output
pub async fn upload_task_output_manifest(
    client: &impl StorageClient,
    location: &ManifestLocation,
    output_path: &TaskOutputManifestPath,
    manifest: &Manifest,
    asset_root: &str,
    file_system_location_name: Option<&str>,
) -> Result<ManifestUploadResult, StorageError>;

/// Upload an output manifest to S3 (step-level, no task)
///
/// Key format: {root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{timestamp}_{session_action_id}/{hash}_output
pub async fn upload_step_output_manifest(
    client: &impl StorageClient,
    location: &ManifestLocation,
    output_path: &StepOutputManifestPath,
    manifest: &Manifest,
    asset_root: &str,
    file_system_location_name: Option<&str>,
) -> Result<ManifestUploadResult, StorageError>;
```

### Download Functions

```rust
/// Download a manifest by its S3 key
pub async fn download_manifest(
    client: &impl StorageClient,
    bucket: &str,
    s3_key: &str,
) -> Result<(Manifest, ManifestDownloadMetadata), StorageError>;

/// Download a manifest by its hash (input manifest)
pub async fn download_input_manifest(
    client: &impl StorageClient,
    location: &ManifestLocation,
    manifest_hash: &str,
) -> Result<(Manifest, ManifestDownloadMetadata), StorageError>;
```

---

## Output Manifest Discovery

Output manifests are stored in S3 with a hierarchical structure. To download job outputs, we need to discover which manifests exist. This section describes the primitives and composed functions for output manifest discovery.

### Primitive: List S3 Objects

```rust
/// Information about an S3 object from list operations
#[derive(Debug, Clone)]
pub struct S3ObjectInfo {
    pub key: String,
    pub size: u64,
    pub last_modified: i64,  // Unix timestamp (seconds)
}

/// List all objects under a prefix with pagination.
/// This is a primitive operation that handles S3 pagination internally.
pub async fn list_objects_with_prefix(
    client: &impl StorageClient,
    bucket: &str,
    prefix: &str,
) -> Result<Vec<S3ObjectInfo>, StorageError>;
```

### Primitive: Build Output Manifest Prefix

```rust
/// Scope for output manifest discovery
#[derive(Debug, Clone)]
pub enum OutputManifestScope {
    /// All outputs for a job
    Job { job_id: String },
    /// All outputs for a step (includes all tasks)
    Step { job_id: String, step_id: String },
    /// All outputs for a specific task
    Task { job_id: String, step_id: String, task_id: String },
}

/// Build the S3 prefix for listing output manifests.
/// 
/// Returns prefix like:
/// - Job: `{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/`
/// - Step: `{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/`
/// - Task: `{root_prefix}/Manifests/{farm_id}/{queue_id}/{job_id}/{step_id}/{task_id}/`
pub fn build_output_manifest_prefix(
    location: &ManifestLocation,
    scope: &OutputManifestScope,
) -> String;
```

### Primitive: Filter Manifest Keys

```rust
/// Filter S3 keys to find output manifest files.
/// 
/// Output manifests match the pattern: `*_output` or `*_output.json`
/// This filters out non-manifest files that may exist in the prefix.
pub fn filter_output_manifest_keys(keys: &[S3ObjectInfo]) -> Vec<&S3ObjectInfo>;
```

### Primitive: Group Manifests by Task

```rust
/// Manifest key with parsed components
#[derive(Debug, Clone)]
pub struct ParsedManifestKey {
    pub key: String,
    pub last_modified: i64,
    /// Task ID if this is a task-level manifest, None for step-level (chunked)
    pub task_id: Option<String>,
    /// The timestamp_sessionaction folder name (e.g., "2025-05-22T12:00:00.000000Z_sessionaction-xxx")
    pub session_folder: String,
}

/// Parse manifest keys to extract task IDs and session folders.
/// 
/// Handles both:
/// - Task-based: `.../step_id/task_id/timestamp_sessionaction/hash_output`
/// - Chunked (step-level): `.../step_id/timestamp_sessionaction/hash_output`
pub fn parse_manifest_keys(keys: &[S3ObjectInfo]) -> Vec<ParsedManifestKey>;

/// Group parsed manifest keys by task ID.
/// 
/// Returns a map where:
/// - Key is `Some(task_id)` for task-based manifests
/// - Key is `None` for step-level (chunked) manifests
pub fn group_manifests_by_task(
    parsed_keys: &[ParsedManifestKey],
) -> HashMap<Option<String>, Vec<&ParsedManifestKey>>;
```

### Primitive: Select Latest Per Task

```rust
/// Select the latest session action's manifests for each task.
/// 
/// For each task, keeps only manifests from the session folder with the
/// latest `last_modified` timestamp. Step-level manifests (chunked steps)
/// are always included.
/// 
/// # Why LastModified instead of folder name?
/// The timestamp in the folder name comes from WorkerAgent and shouldn't be
/// relied upon. S3's LastModified is authoritative.
pub fn select_latest_manifests_per_task(
    grouped: &HashMap<Option<String>, Vec<&ParsedManifestKey>>,
) -> Vec<String>;  // Returns S3 keys
```

### Composed: Discover Output Manifest Keys

```rust
/// Options for output manifest discovery
#[derive(Debug, Clone)]
pub struct OutputManifestDiscoveryOptions {
    /// Scope of discovery (job, step, or task)
    pub scope: OutputManifestScope,
    /// If true, select only the latest session action per task.
    /// If false, return all manifests.
    pub select_latest_per_task: bool,
}

/// Discover output manifest keys from S3.
/// 
/// This composed function:
/// 1. Builds the S3 prefix for the given scope
/// 2. Lists all objects under that prefix
/// 3. Filters to only manifest files
/// 4. Parses keys to extract task IDs and session folders
/// 5. Optionally selects only the latest per task
/// 
/// # Example
/// ```rust
/// let options = OutputManifestDiscoveryOptions {
///     scope: OutputManifestScope::Step {
///         job_id: "job-123".into(),
///         step_id: "step-456".into(),
///     },
///     select_latest_per_task: true,
/// };
/// 
/// let manifest_keys = discover_output_manifest_keys(&client, &location, &options).await?;
/// ```
pub async fn discover_output_manifest_keys(
    client: &impl StorageClient,
    location: &ManifestLocation,
    options: &OutputManifestDiscoveryOptions,
) -> Result<Vec<String>, StorageError> {
    // 1. Build prefix
    let prefix = build_output_manifest_prefix(location, &options.scope);
    
    // 2. List objects
    let objects = list_objects_with_prefix(client, &location.bucket, &prefix).await?;
    
    // 3. Filter to manifests
    let manifest_objects = filter_output_manifest_keys(&objects);
    
    // 4. Parse keys
    let parsed = parse_manifest_keys(manifest_objects);
    
    // 5. Select latest if requested
    if options.select_latest_per_task {
        let grouped = group_manifests_by_task(&parsed);
        Ok(select_latest_manifests_per_task(&grouped))
    } else {
        Ok(parsed.iter().map(|p| p.key.clone()).collect())
    }
}
```

### Composed: Download Output Manifests by Asset Root

```rust
/// Downloaded manifest with metadata for merging
#[derive(Debug, Clone)]
pub struct DownloadedManifest {
    pub manifest: Manifest,
    pub asset_root: String,
    pub last_modified: i64,
}

/// Download output manifests and group by asset root.
/// 
/// This composed function:
/// 1. Discovers manifest keys using the given options
/// 2. Downloads all manifests in parallel
/// 3. Groups manifests by their asset root (from S3 metadata)
/// 4. Merges manifests chronologically within each asset root
/// 
/// # Returns
/// Map of asset_root -> merged manifest
pub async fn download_output_manifests_by_asset_root(
    client: &impl StorageClient,
    location: &ManifestLocation,
    options: &OutputManifestDiscoveryOptions,
) -> Result<HashMap<String, Manifest>, StorageError> {
    // 1. Discover manifest keys
    let keys = discover_output_manifest_keys(client, location, options).await?;
    
    // 2. Download all manifests in parallel
    let downloaded: Vec<DownloadedManifest> = download_manifests_parallel(client, &location.bucket, &keys).await?;
    
    // 3. Group by asset root
    let mut by_root: HashMap<String, Vec<DownloadedManifest>> = HashMap::new();
    for dm in downloaded {
        by_root.entry(dm.asset_root.clone()).or_default().push(dm);
    }
    
    // 4. Merge chronologically within each root
    let mut result = HashMap::new();
    for (root, manifests) in by_root {
        let merged = merge_manifests_chronologically(manifests)?;
        if let Some(m) = merged {
            result.insert(root, m);
        }
    }
    
    Ok(result)
}

/// Download multiple manifests in parallel.
async fn download_manifests_parallel(
    client: &impl StorageClient,
    bucket: &str,
    keys: &[String],
) -> Result<Vec<DownloadedManifest>, StorageError>;
```

### Matching Manifests to Job Attachment Roots

When discovering output manifests, each manifest S3 key contains a hash that identifies which job attachment root it belongs to. This is necessary because a job can have multiple asset roots, and each session action produces one manifest per root.

```rust
use rusty_attachments_common::hash_data;
use crate::hash::HashAlgorithm;

/// Compute the hash used to match manifest S3 keys to job attachment roots.
/// 
/// The hash is computed from the concatenation of the file system location name
/// (if any) and the root path. This hash appears in the manifest S3 key filename.
/// 
/// # Arguments
/// * `file_system_location_name` - Optional file system location name from storage profile
/// * `root_path` - The root path of the job attachment
/// 
/// # Returns
/// The xxh128 hash as a hex string
/// 
/// # Example
/// ```rust
/// use rusty_attachments_storage::manifest_storage::compute_root_path_hash;
/// 
/// // Without file system location
/// let hash = compute_root_path_hash(None, "/mnt/projects/job1");
/// 
/// // With file system location
/// let hash = compute_root_path_hash(Some("ProjectShare"), "Z:\\Projects\\Job1");
/// 
/// // Check if a manifest key belongs to this root
/// let manifest_key = ".../{hash}_output";
/// assert!(manifest_key.contains(&hash));
/// ```
pub fn compute_root_path_hash(
    file_system_location_name: Option<&str>,
    root_path: &str,
) -> String {
    let data = format!(
        "{}{}",
        file_system_location_name.unwrap_or(""),
        root_path
    );
    hash_data(data.as_bytes(), HashAlgorithm::Xxh128)
}

/// Match manifest S3 keys to job attachment roots.
/// 
/// Given a list of manifest keys and job attachment roots, returns a mapping
/// from session action ID to manifest keys indexed by root position.
/// 
/// # Arguments
/// * `manifest_keys` - S3 keys of discovered manifests
/// * `job_attachment_roots` - Job attachment roots from job.attachments.manifests
/// 
/// # Returns
/// Map of session_action_id -> Vec<Option<String>> where the Vec is indexed
/// by root position (same order as job_attachment_roots)
/// 
/// # Example
/// ```rust
/// let roots = vec![
///     JobAttachmentRoot {
///         root_path: "/mnt/assets".into(),
///         file_system_location_name: None,
///     },
///     JobAttachmentRoot {
///         root_path: "/mnt/textures".into(),
///         file_system_location_name: Some("TextureShare".into()),
///     },
/// ];
/// 
/// let manifest_keys = discover_output_manifest_keys(&client, &location, &options).await?;
/// let matched = match_manifests_to_roots(&manifest_keys, &roots)?;
/// 
/// // matched["sessionaction-abc-0"] = [Some("...key1..."), Some("...key2...")]
/// ```
pub fn match_manifests_to_roots(
    manifest_keys: &[String],
    job_attachment_roots: &[JobAttachmentRoot],
) -> Result<HashMap<String, Vec<Option<String>>>, ManifestError> {
    use regex::Regex;
    
    // Regex to extract session action ID from manifest key
    let session_action_re = Regex::new(r"(sessionaction-[^/-]+-[^/-]+)/")
        .expect("valid regex");
    
    // Compute hash for each root
    let root_hashes: Vec<String> = job_attachment_roots
        .iter()
        .map(|root| compute_root_path_hash(
            root.file_system_location_name.as_deref(),
            &root.root_path,
        ))
        .collect();
    
    let mut result: HashMap<String, Vec<Option<String>>> = HashMap::new();
    
    for key in manifest_keys {
        // Extract session action ID
        let session_action_id = session_action_re
            .captures(key)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| ManifestError::InvalidKey {
                key: key.clone(),
                reason: "missing session action ID".into(),
            })?;
        
        // Find which root this manifest belongs to
        let root_index = root_hashes
            .iter()
            .position(|hash| key.contains(hash))
            .ok_or_else(|| ManifestError::InvalidKey {
                key: key.clone(),
                reason: "no matching root path hash".into(),
            })?;
        
        // Initialize entry for this session action if needed
        let entry = result
            .entry(session_action_id)
            .or_insert_with(|| vec![None; job_attachment_roots.len()]);
        
        entry[root_index] = Some(key.clone());
    }
    
    Ok(result)
}

/// Job attachment root information needed for manifest matching.
#[derive(Debug, Clone)]
pub struct JobAttachmentRoot {
    /// The root path of the job attachment
    pub root_path: String,
    /// Optional file system location name from storage profile
    pub file_system_location_name: Option<String>,
}
```

### Session Action ID Lookup

```rust
/// Find manifests for a specific session action ID.
/// 
/// When the session action ID is known (e.g., from GetTask API), we can
/// search for manifests containing that ID in their path. This is more
/// reliable than timestamp-based selection.
/// 
/// # Search Strategy
/// 1. First searches in task folder (`.../step_id/task_id/`)
/// 2. Falls back to step folder (`.../step_id/`) for chunked steps
pub async fn find_manifests_by_session_action_id(
    client: &impl StorageClient,
    location: &ManifestLocation,
    job_id: &str,
    step_id: &str,
    task_id: &str,
    session_action_id: &str,
) -> Result<Vec<String>, StorageError>;
```

---

## Content Types

| Manifest Type | Version | Content-Type |
|---------------|---------|--------------|
| Snapshot | v2023-03-03 | `application/x-deadline-manifest-2023-03-03` |
| Snapshot | v2025-12-04-beta | `application/x-deadline-manifest-2025-12-04-beta` |
| Diff | v2025-12-04-beta | `application/x-deadline-manifest-diff-2025-12-04-beta` |

```rust
fn get_content_type(manifest: &Manifest) -> &'static str {
    match manifest {
        Manifest::V2023_03_03(_) => "application/x-deadline-manifest-2023-03-03",
        Manifest::V2025_12_04_beta(m) => {
            if m.parent_manifest_hash.is_some() {
                "application/x-deadline-manifest-diff-2025-12-04-beta"
            } else {
                "application/x-deadline-manifest-2025-12-04-beta"
            }
        }
    }
}
```

---

## S3 Metadata Handling

See the [S3 Metadata](#s3-metadata) section above for the `ManifestS3Metadata` struct that handles building and parsing S3 metadata.

---

## Usage Example

```rust
use rusty_attachments_storage::{
    upload_input_manifest, download_manifest,
    ManifestLocation, CrtStorageClient,
};

let client = CrtStorageClient::new(settings)?;
let location = ManifestLocation {
    bucket: "my-bucket".into(),
    root_prefix: "DeadlineCloud".into(),
    farm_id: "farm-123".into(),
    queue_id: "queue-456".into(),
};

// Upload input manifest
let result = upload_input_manifest(
    &client,
    &location,
    &manifest,
    "/project/assets",
    None,  // file_system_location_name
).await?;
println!("Uploaded: {} (hash: {})", result.s3_key, result.manifest_hash);

// Download manifest
let (manifest, metadata) = download_manifest(&client, &location.bucket, &result.s3_key).await?;
println!("Asset root: {}", metadata.asset_root);
```

---

## Related Documents

- [storage-design.md](storage-design.md) - StorageClient trait and CAS operations
- [model-design.md](model-design.md) - Manifest data structures
- [file_system.md](file_system.md) - Creating manifests from directories

---

## Reference: deadline-cloud Python Implementation

The following files in the [deadline-cloud](https://github.com/aws-deadline/deadline-cloud) library implement the equivalent functionality:

### Path Construction
- `src/deadline/job_attachments/models.py`
  - `JobAttachmentS3Settings.full_output_prefix()` - Builds task-level output prefix
  - `JobAttachmentS3Settings.partial_session_action_manifest_prefix()` - Task-level partial path
  - `JobAttachmentS3Settings.partial_session_action_manifest_prefix_without_task()` - Step-level partial path
  - `JobAttachmentS3Settings.partial_manifest_prefix()` - Input manifest partial path with GUID

### Upload Operations
- `src/deadline/job_attachments/asset_sync.py`
  - `AssetSync._upload_output_manifest_to_s3()` - Uploads output manifest with metadata

### Download Operations
- `src/deadline/job_attachments/download.py`
  - `get_manifest_from_s3()` - Downloads and decodes a manifest by S3 key
  - `get_output_manifests_by_asset_root()` - Lists and downloads output manifests
  - `_get_output_manifest_prefix()` - Builds prefix for listing output manifests

### Metadata Handling
- `src/deadline/job_attachments/download.py`
  - `_get_asset_root_from_metadata()` - Extracts asset root from S3 metadata (handles both ASCII and JSON-encoded non-ASCII paths)
