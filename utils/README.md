# Rusty Attachments Utilities

Standalone utility scripts for working with rusty-attachments.

## Prerequisites

1. Build and install the `rusty_attachments` Python package:

```bash
cd rusty-attachments/crates/python
python3 -m venv .venv
source .venv/bin/activate
pip install maturin boto3
maturin develop
```

2. Configure AWS credentials (via profile, environment variables, or IAM role).

## Scripts

### create_and_upload_manifest.py

Create a V2 manifest from a local folder and upload files to S3 CAS.

#### Usage

```bash
python create_and_upload_manifest.py <folder_path> [options]
```

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `folder` | (required) | Path to folder to create manifest from |
| `--bucket` | `adeadlineja` | S3 bucket name |
| `--root-prefix` | `DeadlineCloud` | Root prefix in S3 |
| `--farm-id` | `farm-test` | Farm ID for manifest location |
| `--queue-id` | `queue-test` | Queue ID for manifest location |
| `--profile` | None | AWS profile to use |
| `--region` | `us-west-2` | AWS region |
| `--output-manifest` | None | Path to save manifest JSON |
| `--dry-run` | False | Only scan files, don't upload |

#### Examples

**Basic upload:**
```bash
python create_and_upload_manifest.py ./my_project_files
```

**Upload with custom S3 settings:**
```bash
python create_and_upload_manifest.py ./my_project_files \
    --bucket my-bucket \
    --root-prefix MyPrefix \
    --region us-east-1
```

**Upload with AWS profile and save manifest:**
```bash
python create_and_upload_manifest.py ./my_project_files \
    --profile my-aws-profile \
    --output-manifest /tmp/manifest.json
```

**Dry run (scan only, no upload):**
```bash
python create_and_upload_manifest.py ./my_project_files --dry-run
```

#### Output

The script outputs:
- Progress during hashing and upload
- Summary of files processed and bytes transferred
- Attachments JSON with manifest location in S3

Example output:
```
Scanning folder: /path/to/my_project_files
Found 100 files
  - file1.txt
  - subdir/file2.png
  ... and 98 more

Uploading to s3://adeadlineja/DeadlineCloud/
Using manifest version: v2025-12-04-beta
  [Hashing] 50/100 (50.0%) - file50.dat
  [Complete] 100/100 (100.0%) - None

=== Results ===
Hashing:
  Files processed: 100
  Bytes processed: 1,234,567,890
Upload:
  Files transferred: 45
  Files skipped: 55
  Bytes transferred: 567,890,123

=== Attachments JSON ===
{
  "manifests": [
    {
      "rootPath": "/path/to/my_project_files",
      "rootPathFormat": "posix",
      "inputManifestPath": "farm-test/queue-test/Inputs/.../manifest_input",
      "inputManifestHash": "abc123..."
    }
  ],
  "fileSystem": "COPIED"
}

✅ Upload complete!
```

#### S3 Structure

Files are uploaded to:
- **CAS data:** `s3://<bucket>/<root-prefix>/Data/<hash>.<algorithm>`
- **Manifest:** `s3://<bucket>/<root-prefix>/Manifests/<farm-id>/<queue-id>/Inputs/<session-id>/<manifest-hash>_input`

---

## Mounting with VFS

After uploading, you can mount the manifest as a FUSE filesystem using the VFS example.

### Prerequisites

1. Build the VFS crate with FUSE support:
```bash
cd rusty-attachments
cargo build -p rusty-attachments-vfs --features fuse --example mount_vfs --release
```

2. Download the manifest from S3 (use the path from the upload output):
```bash
aws s3 cp s3://adeadlineja/DeadlineCloud/Manifests/farm-test/queue-test/Inputs/<session-id>/<hash>_input /tmp/manifest.json
```

### Mount the VFS

```bash
# Basic mount with S3 backend
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs --release -- \
    /tmp/manifest.json ~/vfs \
    --bucket adeadlineja --root-prefix DeadlineCloud

# Mount with live stats dashboard
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs --release -- \
    /tmp/manifest.json ~/vfs --stats \
    --bucket adeadlineja --root-prefix DeadlineCloud

# Mount with custom region
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs --release -- \
    /tmp/manifest.json ~/vfs --stats \
    --bucket my-bucket --root-prefix MyPrefix --region us-east-1
```

### CLI Options

| Option | Default | Description |
|--------|---------|-------------|
| `--stats` | off | Show live statistics dashboard |
| `--mock` | off | Use mock file store (no S3) |
| `--bucket` | `adeadlineja` | S3 bucket name |
| `--root-prefix` | `DeadlineCloud` | S3 root prefix |
| `--region` | `us-west-2` | AWS region |

### Example Workflow

```bash
# 1. Upload a folder to S3 CAS
python utils/create_and_upload_manifest.py ./my_project \
    --bucket adeadlineja \
    --root-prefix DeadlineCloud \
    --output-manifest /tmp/my_project_manifest.json

# 2. Mount the uploaded content as a filesystem
cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs --release -- \
    /tmp/my_project_manifest.json ~/vfs --stats \
    --bucket adeadlineja --root-prefix DeadlineCloud

# 3. Access files (in another terminal)
ls ~/vfs
cat ~/vfs/some_file.txt

# 4. Unmount with Ctrl+C
```
