# Pure-Rust CLI: `ra submit`

## Command

```
ra submit <job_bundle_dir> \
    [--parameter <key>=<value>]... \
    [--name <job_name>] \
    [--priority <int>] \
    [--max-failed-tasks-count <int>] \
    [--max-retries-per-task <int>] \
    [--farm-id <id>] \
    [--queue-id <id>] \
    [--storage-profile-id <id>] \
    [--profile <aws_profile>] \
    [--require-paths-exist] \
    [--file-system-mode COPIED|VIRTUAL] \
    [--manifest-version v2023-03-03|v2025-12] \
    [--exclude <glob>]... \
    [--yes] \
    [--save-debug-snapshot <dir>] \
    [--json]
```

## Implementation

This is the most complex command. It mirrors `deadline bundle submit` which
orchestrates template resolution, parameter merging, file scanning, hashing,
uploading, and the CreateJob API call.

```
main()
  → parse_args()
  → load_config(farm_id, queue_id, profile)
  │
  ├─ 1. TEMPLATE RESOLUTION (Rust-native)
  │    → read_job_template(job_bundle_dir)        // YAML or JSON
  │    → read_asset_references(job_bundle_dir)
  │    → read_job_parameters(job_bundle_dir)
  │
  ├─ 2. DEADLINE API SETUP
  │    → deadline_api::get_queue(farm_id, queue_id)
  │    → deadline_api::get_storage_profile(farm_id, queue_id, storage_profile_id)
  │    → deadline_api::get_queue_parameter_definitions(farm_id, queue_id)
  │    → merge_parameters(template_params, queue_params, cli_params)
  │    → resolve_path_parameters()
  │
  ├─ 3. JOB ATTACHMENTS (existing Rust pipeline)
  │    │
  │    ├─ Phase 1: Scan & Classify
  │    │    → FileSystemScanner::snapshot()
  │    │    → group_asset_paths(storage_profile)
  │    │    → compute file counts + sizes for confirmation
  │    │
  │    ├─ [interactive] confirmation prompt (unless --yes)
  │    │    → display file counts, sizes, path warnings
  │    │    → prompt: "Do you want to proceed? [y/N]"
  │    │
  │    └─ Phase 2: Hash + Upload
  │         → submit_bundle_attachments()          // crates/ja-deadline-utils
  │         → returns AttachmentSettings JSON
  │
  ├─ 4. CREATE JOB
  │    → deadline_api::create_job(template, parameters, attachments)
  │    → deadline_api::get_job() — poll until READY or FAILED
  │
  └─ 5. OUTPUT
       → format_output(job_id, stats, json)
```

## Template Resolution

Python uses `pyyaml` for YAML parsing and custom parameter substitution.
Rust uses `serde_yaml` and implements the same substitution logic:

```rust
/// Read a job template from YAML or JSON.
fn read_job_template(bundle_dir: &Path) -> Result<serde_json::Value, CliError> {
    let template_path: PathBuf = find_template_file(bundle_dir)?;
    let content: String = std::fs::read_to_string(&template_path)?;

    if template_path.extension() == Some("yaml".as_ref())
        || template_path.extension() == Some("yml".as_ref())
    {
        Ok(serde_yaml::from_str(&content)?)
    } else {
        Ok(serde_json::from_str(&content)?)
    }
}
```

## Confirmation Prompt

The two-phase design matches the Python CLI's interactive confirmation:

```rust
fn display_confirmation(scan_result: &AssetScanResult, auto_accept: bool) -> Result<bool, CliError> {
    println!("Files to upload: {} ({} bytes)", scan_result.total_input_files, scan_result.total_input_bytes);

    if !scan_result.paths_outside_profile.is_empty() {
        eprintln!("WARNING: {} paths are outside the storage profile", scan_result.paths_outside_profile.len());
    }

    if auto_accept {
        return Ok(true);
    }

    print!("Do you want to proceed? [y/N] ");
    std::io::stdout().flush()?;
    let mut input: String = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}
```

## Deadline API Client

The submit command requires the most Deadline API calls. The `deadline_api`
module provides a thin async wrapper:

```rust
pub struct DeadlineClient {
    inner: aws_sdk_deadline::Client,
}

impl DeadlineClient {
    pub async fn get_queue(&self, farm_id: &str, queue_id: &str) -> Result<Queue, ApiError>;
    pub async fn get_job(&self, farm_id: &str, queue_id: &str, job_id: &str) -> Result<Job, ApiError>;
    pub async fn create_job(&self, args: CreateJobArgs) -> Result<String, ApiError>;
    pub async fn get_queue_parameter_definitions(&self, farm_id: &str, queue_id: &str) -> Result<Vec<ParamDef>, ApiError>;
    pub async fn get_storage_profile(&self, farm_id: &str, queue_id: &str, profile_id: &str) -> Result<StorageProfile, ApiError>;
}
```

## Improvements over Python

1. **Single-binary deployment** — No virtualenv, no pip, no dependency
   conflicts. The `ra` binary includes everything.

2. **Parallel scan + hash** — `FileSystemScanner` uses rayon for parallel
   directory walking and hashing. Python is single-threaded for both.

3. **Pipelined upload** — CRT uploads files as they're hashed, not after
   all hashing completes. This overlaps CPU (hashing) with I/O (upload).

4. **Native YAML parsing** — `serde_yaml` is faster than Python's `pyyaml`
   for large job templates.

5. **No GIL** — All phases (scan, hash, upload, API calls) run with true
   parallelism on the tokio runtime.

## Dependencies

```toml
[dependencies]
clap = { version = "4", features = ["derive"] }
serde_yaml = "0.9"
dialoguer = "0.11"          # interactive prompts
indicatif = "0.17"          # progress bars
aws-sdk-deadline = "1"      # Deadline API
aws-sdk-sts = "1"           # AssumeRole for queue credentials
```
