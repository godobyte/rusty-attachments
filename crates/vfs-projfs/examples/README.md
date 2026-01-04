# ProjFS Examples

## mount_projfs

Mount a job attachments manifest as a virtual filesystem using Windows ProjFS.

### Prerequisites

1. Windows 10 1809+ or Windows Server 2019+
2. ProjFS feature enabled:
   ```powershell
   Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS
   ```
3. AWS credentials configured (via environment or credentials file)

### Usage

```powershell
cargo run --example mount_projfs -p rusty-attachments-vfs-projfs -- <manifest.json> <mount_point> [options]
```

### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--cache-dir <path>` | `./vfs-cache` | Directory for write cache |
| `--stats` | off | Show live statistics dashboard |
| `--cleanup` | off | Delete mount directory after unmounting |
| `--bucket <name>` | `$S3_BUCKET` or `adeadlineja` | S3 bucket name |
| `--root-prefix <pfx>` | `$S3_ROOT_PREFIX` or `DeadlineCloud` | S3 root prefix |

### Environment Variables

| Variable | Description |
|----------|-------------|
| `S3_BUCKET` | S3 bucket name |
| `S3_ROOT_PREFIX` | S3 root prefix (e.g., `DeadlineCloud`) |
| `S3_CAS_PREFIX` | CAS prefix (default: `Data`) |
| `S3_MANIFEST_PREFIX` | Manifest prefix (default: `Manifests`) |

### Examples

Basic mount:
```powershell
cargo run --example mount_projfs -p rusty-attachments-vfs-projfs -- manifest.json vfs
```

With custom cache directory:
```powershell
cargo run --example mount_projfs -p rusty-attachments-vfs-projfs -- manifest.json vfs --cache-dir vfs-cache
```

With live stats dashboard:
```powershell
cargo run --example mount_projfs -p rusty-attachments-vfs-projfs -- manifest.json vfs --cache-dir vfs-cache --stats
```

With cleanup on exit:
```powershell
cargo run --example mount_projfs -p rusty-attachments-vfs-projfs -- manifest.json vfs --cleanup
```

With credentials script:
```powershell
. .\creds.ps1; cargo run --example mount_projfs -p rusty-attachments-vfs-projfs -- manifestv2.json vfs --cache-dir vfs-cache --cleanup
```

---

## ProjFS vs FUSE: Key Differences

### Persistence Model

| Aspect | FUSE (Linux/macOS) | ProjFS (Windows) |
|--------|-------------------|------------------|
| **Mount/Unmount** | Files disappear when unmounted | Placeholders persist after stop |
| **Hydrated Files** | Virtual until unmount | Become real files on disk |
| **Interrupted Jobs** | Must re-download everything | Hydrated files preserved |
| **Cleanup** | Automatic on unmount | Manual deletion required |

### Why ProjFS Works This Way

ProjFS uses a "placeholder" model where:
1. Files start as lightweight placeholders (just metadata)
2. When accessed, content is fetched and the file becomes "hydrated"
3. Hydrated files are real files that persist even after virtualization stops

This is actually beneficial for job attachments:
- If a job is interrupted, hydrated files don't need to be re-downloaded
- Multiple jobs can share the same virtualization root
- Files can be inspected after the job completes

### Cleanup Options

**Option 1: Use `--cleanup` flag**
```powershell
cargo run --example mount_projfs -- manifest.json vfs --cleanup
```
This deletes the mount directory when the virtualizer stops.

**Option 2: Manual cleanup**
```powershell
Remove-Item -Recurse -Force vfs
```

**Option 3: Keep for inspection**
Leave the directory intact to inspect hydrated files after the job.

### Notes

- Press `Ctrl+C` to stop virtualization
- Without `--cleanup`, the mount directory and any hydrated files remain
- The `--cleanup` flag removes the entire mount directory, including any user-created files
