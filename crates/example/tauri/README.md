# Job Attachment Manager - Tauri Example

A desktop application for managing Deadline Cloud job attachments, built with Tauri and the Cyborg Bootstrap theme.

## Features

### Two-Tab Interface

1. **Upload Files** - Browse local directories, preview snapshots, upload to S3 CAS
2. **Browse Manifests** - Navigate the S3 Manifests folder, view manifest contents

### Manifest Viewer

When selecting a manifest file (`*_input` or `*_output`), displays:
- Version, file count, total size badges
- Asset root from S3 metadata
- File tree with: path, size, mtime, executable flag, symlink targets, chunk counts

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     Tauri Application                            │
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
├── Cargo.toml
├── build.rs
├── tauri.conf.json
├── icons/
│   └── icon.png
├── ui/
│   └── index.html       # Cyborg Bootstrap UI
└── src/
    ├── main.rs          # Tauri app entry point
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
List S3 objects under a prefix with folder simulation.

### `fetch_manifest(config: JobAttachmentConfig, s3_key: String) -> ParsedManifest`
Download and parse a manifest, returning structured file entries.

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

## Running

```bash
cd crates/example/tauri
cargo tauri dev
```

## Building

```bash
cargo tauri build
```
