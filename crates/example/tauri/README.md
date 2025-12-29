# Job Attachment Manager - Tauri v2 Example

A desktop application for managing Deadline Cloud job attachments, built with Tauri v2 and the Cyborg Bootstrap theme.

## Features

### Two-Tab Interface

1. **Upload Files** - Browse local directories, preview snapshots, upload to S3 CAS
2. **Browse Manifests** - Navigate the S3 Manifests folder, view manifest contents

### Manifest Viewer

When selecting a manifest file (`*_input` or `*_output`), displays:
- Version, file count, total size badges
- Asset root from S3 metadata
- File tree with: path, size, mtime, executable flag (⚡), symlink targets (🔗), chunk counts, deleted markers (❌)

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     Tauri v2 Application                         │
│  UI (Cyborg Bootstrap)  ←→  Tauri Commands  ←→  Rust Backend    │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Tauri Commands (commands.rs)                   │
│  browse_directory, browse_s3_prefix, fetch_manifest,            │
│  create_snapshot, submit_bundle, cancel_operation               │
└─────────────────────────────────────────────────────────────────┘
                                │
            ┌───────────────────┼───────────────────┐
            ▼                   ▼                   ▼
┌───────────────────┐ ┌───────────────────┐ ┌───────────────────┐
│ ja-deadline-utils │ │     storage       │ │      model        │
│ - submit_bundle   │ │ - CrtStorageClient│ │ - Manifest        │
│ - AssetReferences │ │ - list_objects    │ │ - v2023/v2025     │
└───────────────────┘ └───────────────────┘ └───────────────────┘
```

## Project Structure

```
crates/example/tauri/
├── Cargo.toml           # Tauri v2.9 with lib + bin
├── build.rs
├── tauri.conf.json      # Tauri v2 config with withGlobalTauri
├── capabilities/
│   └── default.json     # Permissions for core and dialog
├── icons/
│   └── icon.png         # App icon (rusty.png)
├── ui/
│   └── index.html       # Cyborg Bootstrap UI with ES modules
└── src/
    ├── lib.rs           # Shared library entry point
    ├── main.rs          # Desktop binary entry point
    ├── commands.rs      # Tauri command handlers
    ├── types.rs         # Serializable IPC types
    ├── progress.rs      # Progress event emitter
    └── error.rs         # Error types
```

## Deadline Cloud S3 Structure

```
s3://{bucket}/{root_prefix}/
├── Data/                                    # CAS objects
│   └── {hash}.xxh128
└── Manifests/
    └── {farm_id}/
        └── {queue_id}/
            ├── Inputs/                      # Input manifests
            │   └── {guid}/
            │       └── {root_hash}_input
            └── {job_id}/                    # Output manifests
                └── {step_id}/
                    └── {task_id}/
                        └── {timestamp}_{session_action_id}/
                            └── {root_hash}_output
```

## Tauri Commands

### `browse_directory(path: String) -> Vec<DirectoryEntry>`
List local directory contents for file browser.

### `browse_s3_prefix(config: JobAttachmentConfig, prefix: String) -> Vec<S3ObjectEntry>`
List S3 objects under a prefix with folder simulation (client-side hierarchical grouping).

### `fetch_manifest(config: JobAttachmentConfig, s3_key: String) -> ParsedManifest`
Download and parse a manifest, returning structured file entries with all v2 attributes.

### `create_snapshot(config: SnapshotConfig, window: Window) -> SnapshotResult`
Create a manifest from a directory without uploading (preview mode).

### `submit_bundle(ja_config, snapshot_config, output_directories, window) -> SubmitResult`
Full upload: hash files, upload to CAS, upload manifest, return attachments JSON.

## IPC Types

### JobAttachmentConfig
```rust
pub struct JobAttachmentConfig {
    pub region: String,
    pub bucket: String,
    pub root_prefix: String,
    pub farm_id: String,
    pub queue_id: String,
}
```

### SnapshotConfig
```rust
pub struct SnapshotConfig {
    pub root_path: String,
    pub input_files: Vec<String>,
    pub include_patterns: Vec<String>,  // e.g., ["**/*.exr"]
    pub exclude_patterns: Vec<String>,  // e.g., ["**/*.tmp"]
    pub manifest_version: String,       // "v2023-03-03" or "v2025-12-04-beta"
}
```

### ManifestFileEntry
```rust
pub struct ManifestFileEntry {
    pub path: String,
    pub size: Option<u64>,
    pub mtime: Option<i64>,           // microseconds since epoch
    pub hash: Option<String>,
    pub executable: bool,
    pub entry_type: String,           // "file", "symlink", "deleted"
    pub symlink_target: Option<String>,
    pub chunk_count: Option<usize>,   // for files >256MB
}
```

## Setup

### Prerequisites

1. Install Rust: https://rustup.rs/
2. Install Tauri CLI v2:
   ```bash
   cargo install tauri-cli --version "^2.0" --locked
   ```

### Running

```bash
cd crates/example/tauri
cargo tauri dev
```

The app will:
- Compile the Rust backend
- Launch a window with the UI
- Use your current AWS credentials from the environment

### Building

```bash
cargo tauri build
```

## Tauri v2 Migration Notes

This app uses Tauri v2.9 with the following key changes from v1:

1. **Library Structure**: Uses `lib.rs` + `main.rs` pattern for mobile support
2. **Config Format**: Updated to Tauri v2 schema with `withGlobalTauri: true`
3. **Permissions**: Uses capabilities system (`capabilities/default.json`)
4. **JavaScript API**: Uses `window.__TAURI__.core.invoke()` and `window.__TAURI__.event.listen()`
5. **Plugins**: Uses `tauri-plugin-dialog` for file picker

## Default Configuration

The app defaults to:
- **Bucket**: `adeadlineja`
- **Root Prefix**: `DeadlineCloud`
- **Farm ID**: `farm-fd8e9a84d9c04142848c6ea56c9d7568`
- **Queue ID**: `queue-2eb8ef58ce5d48d1bbaf3e2f65ea2c38`
- **Region**: `us-west-2`
- **Manifest Version**: `v2025-12-04-beta`

## Troubleshooting

### "window.__TAURI__ is undefined"
Make sure `withGlobalTauri: true` is set in `tauri.conf.json` under the `app` section.

### Icon errors
Ensure `icons/icon.png` is a valid RGBA PNG file. Copy from `rusty.png` if needed:
```bash
cp rusty.png icons/icon.png
```

### AWS credentials
The app uses the default AWS credential chain. Set credentials via:
- Environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`)
- AWS config files (`~/.aws/credentials`)
- IAM role (if running on EC2)
