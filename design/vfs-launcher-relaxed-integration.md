# VFS Launcher: Relaxed Consistency Integration

**Status:** DRAFT
**Date:** 2026-03-21
**Extends:** async-upload-relaxed-consistency.md

---

## Current State

Three platform-specific VFS launchers exist as example binaries:

| Platform | Crate | Example | Backend |
|----------|-------|---------|---------|
| Linux | `vfs` (FUSE) | `mount_vfs.rs` | fuser |
| macOS | `vfs-fskit` | `mount_fskit.rs` | FSKit |
| Windows | `vfs-projfs` | `mount_projfs.rs` | ProjFS |

All three share the same pattern:
1. Parse CLI args (manual, no clap)
2. Load manifest JSON
3. Create `StorageClientAdapter` wrapping `CrtStorageClient`
4. Build VFS with `VfsOptions::default()`
5. Mount and wait for Ctrl+C

None have any concept of relaxed consistency or configurable consistency modes.

---

## Design: `--relaxed-roots` CLI Option

### CLI Extension (All Platforms)

```
mount_vfs <manifest.json> <mountpoint> [options]

Existing options:
  --bucket <name>        S3 bucket
  --root-prefix <pfx>    S3 root prefix
  --region <region>      AWS region
  --stats                Live stats dashboard
  --writable             COW write support
  --mock                 Mock file store

New options:
  --relaxed-roots <path> JSON file declaring relaxed consistency roots
```

Strong consistency is the default. Relaxed consistency is opt-in via `--relaxed-roots`.

### Relaxed Roots JSON File

```json
{
  "roots": [
    {
      "rootId": "a1b2c3d4e5f6a7b8c9d0",
      "sourcePath": "/mnt/shared/assets",
      "mountPath": "assets"
    }
  ],
  "sqsRegion": "us-west-2",
  "farmId": "farm-abc123",
  "queueId": "queue-def456",
  "pollIntervalSecs": 30,
  "maxWaitTimeoutSecs": 1800
}
```

### Why a Separate JSON File?

- Relaxed roots config can have many entries — too verbose for CLI flags
- The same config file can be shared between VFS and upload agent
- The Deadline service can generate this file alongside the manifest
- Keeps the CLI simple: one flag to enable, one file for config

### Integration Flow

```
1. Parse --relaxed-roots <path>
2. Load and parse the JSON config
3. Build RelaxedConsistencyOptions from config
4. Pass to VfsOptions::with_relaxed(...)
5. INodeManager creates relaxed root directories at mount time
6. FUSE/FSKit/ProjFS lookup auto-vivifies under relaxed roots
```

### Platform-Agnostic: Shared Config Parsing

The config parsing lives in the VFS crate (not in each example), so all
three platforms reuse the same logic:

```rust
// In crates/vfs/src/relaxed/config.rs
pub fn load_relaxed_config(path: &Path) -> Result<RelaxedLaunchConfig, VfsError>;
```

---

## Implementation Scope

1. Add `RelaxedLaunchConfig` struct + `load_relaxed_config()` to `crates/vfs/src/relaxed/`
2. Add `--relaxed-roots` to `mount_vfs.rs` (FUSE launcher)
3. Wire `RelaxedConsistencyOptions` into VFS construction
4. Register relaxed root directories in `INodeManager` at mount time
5. Unit tests for config loading

FSKit and ProjFS launchers are deferred — same pattern, just copy the flag.
