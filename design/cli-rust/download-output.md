# Pure-Rust CLI: `ra download-output`

## Command

```
ra download-output \
    --job-id <id> \
    [--step-id <id>] \
    [--task-id <id>] \
    [--farm-id <id>] \
    [--queue-id <id>] \
    [--profile <aws_profile>] \
    [--conflict-resolution SKIP|OVERWRITE|CREATE_COPY] \
    [--output <dir>] \
    [--yes] \
    [--json]
```

## Overview

Downloads output files produced by a completed job, step, or task. This is
the Rust equivalent of `deadline job download-output`. It's a two-phase
interactive command: discover manifests → user selects roots → download files.

## Implementation

```
main()
  → parse_args()
  → load_config(farm_id, queue_id, profile)
  → resolve_credentials(profile)
  │
  ├─ 1. DEADLINE API RESOLUTION
  │    → deadline_api::get_job(farm_id, queue_id, job_id)
  │    → deadline_api::get_step(farm_id, queue_id, job_id, step_id)  // if step_id
  │    → deadline_api::get_task(...)                                  // if task_id
  │    → deadline_api::get_queue(farm_id, queue_id)
  │    → assume_queue_role(queue)                                     // STS AssumeRole
  │    → extract root_path_format_mapping from job.attachments
  │
  ├─ 2. PHASE 1: DISCOVER MANIFESTS
  │    → determine OutputManifestScope (Job | Step | Task)
  │    → discover_output_manifest_keys(client, manifest_loc, scope)
  │    → download_manifest() for each key (concurrent)
  │    → group by asset_root
  │    → merge_manifests() per root
  │    → collect outputs_by_root: HashMap<root, Vec<file_path>>
  │
  ├─ 3. INTERACTIVE ROOT SELECTION
  │    → display roots with file counts and OS format warnings
  │    → if root_path_format != current_os:
  │    │    warn "Root {root} is {format}, current OS is {os}"
  │    → prompt user to confirm or override each root path
  │    → if --yes: accept all defaults
  │    │
  │    → detect conflicting filenames across roots
  │    → if conflicts exist:
  │         prompt conflict resolution selection
  │
  ├─ 4. PHASE 2: DOWNLOAD FILES
  │    → for each (root, manifest) in merged_manifests:
  │    │    → DownloadOrchestrator::download_manifest_contents(
  │    │          manifest, dest_root, conflict_resolution, progress
  │    │      )
  │    │    → accumulate TransferStatistics
  │    │
  │    → display download summary
  │
  └─ 5. OUTPUT
       → format_output(stats, json)
```

## Interactive Prompts

The Python CLI has several interactive prompts that must be replicated:

### Root Path Selection

```
Output roots found:
  1. /projects/renders (Linux) - 42 files
  2. C:\projects\textures (Windows) - 15 files

WARNING: Root "C:\projects\textures" is Windows format but current OS is Linux.

Enter download directory for root 1 [/projects/renders]:
Enter download directory for root 2 [C:\projects\textures]: /tmp/textures
```

### Conflict Resolution

```
The following files already exist locally:
  - /projects/renders/frame_001.exr
  - /projects/renders/frame_002.exr

How should conflicts be handled?
  1. CREATE_COPY - Download with (1) suffix
  2. SKIP - Don't download existing files
  3. OVERWRITE - Replace existing files
Select [1]:
```

Implementation uses `dialoguer`:

```rust
use dialoguer::{Confirm, Input, Select};

fn prompt_root_overrides(
    outputs_by_root: &HashMap<String, Vec<String>>,
    root_formats: &HashMap<String, String>,
    auto_accept: bool,
) -> Result<HashMap<String, String>, CliError> {
    let mut overrides: HashMap<String, String> = HashMap::new();

    for (root, files) in outputs_by_root {
        let format_warning: String = check_os_mismatch(root, root_formats.get(root));
        if !format_warning.is_empty() {
            eprintln!("WARNING: {}", format_warning);
        }

        let dest: String = if auto_accept {
            root.clone()
        } else {
            Input::new()
                .with_prompt(format!("Download dir for {} ({} files)", root, files.len()))
                .default(root.clone())
                .interact_text()?
        };
        overrides.insert(root.clone(), dest);
    }
    Ok(overrides)
}
```

## Improvements over Python

1. **Concurrent manifest discovery** — All output manifests are downloaded
   concurrently via `FuturesUnordered`. Python's `OutputDownloader.__init__`
   downloads them sequentially.

2. **No OutputDownloader class** — Python wraps state in a class with
   mutable fields. Rust uses a linear pipeline with explicit data flow.

3. **Streaming progress** — `indicatif::MultiProgress` shows per-root
   progress bars simultaneously. Python shows a single aggregate bar.

4. **CRT downloads** — True parallel S3 GET with CRT connection pooling.
   Python uses `ThreadPoolExecutor` with GIL contention.

5. **Memory-efficient merge** — Manifests are merged as they arrive using
   `merge_manifests()`. Python buffers all manifests in
   `ManifestPathGroup` objects before merging.

## Error Handling

```rust
/// Errors specific to download-output.
#[derive(Debug, thiserror::Error)]
enum DownloadOutputError {
    #[error("Job {job_id} has no attachments")]
    NoAttachments { job_id: String },

    #[error("No output manifests found for scope {scope}")]
    NoOutputManifests { scope: String },

    #[error("Root path {root} exceeds Windows MAX_PATH ({len} chars)")]
    WindowsLongPath { root: String, len: usize },
}
```
