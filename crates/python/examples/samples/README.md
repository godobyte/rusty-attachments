# Sample Job Bundles for rusty_attachments

This directory contains sample job bundles for testing the rusty_attachments Python bindings.

## Setup

### 1. Create and activate a virtual environment

```bash
cd rusty-attachments/crates/python
python3 -m venv .venv
source .venv/bin/activate
```

### 2. Install dependencies

```bash
pip install maturin boto3 pyyaml deadline
```

### 3. Build and install rusty_attachments

```bash
maturin develop
```

## Usage

### Test upload only (no job submission)

```bash
python examples/submit_job.py --test-upload \
    --farm-id farm-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --queue-id queue-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --job-bundle examples/samples/BealineTestJobBundles/simple_job
```

### Submit a job

```bash
python examples/submit_job.py --submit-job \
    --farm-id farm-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --queue-id queue-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --job-bundle examples/samples/BealineTestJobBundles/simple_job \
    --job-name "MyTestJob"
```

### Upload ad-hoc files (without a job bundle)

```bash
python examples/submit_job.py --test-upload \
    --farm-id farm-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --queue-id queue-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --input-files /path/to/file1.txt /path/to/directory
```

### Use v2 manifest format

```bash
python examples/submit_job.py --test-upload --v2 \
    --farm-id farm-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --queue-id queue-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --job-bundle examples/samples/BealineTestJobBundles/simple_job
```

### Quick upload test with generated files

```bash
python examples/test_upload.py \
    --farm-id farm-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --queue-id queue-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    --num-files 10 \
    --file-size 4096
```

## CLI Options

| Option | Description |
|--------|-------------|
| `--farm-id` | Deadline Cloud farm ID (or set `DEADLINE_FARM_ID` env var) |
| `--queue-id` | Deadline Cloud queue ID (or set `DEADLINE_QUEUE_ID` env var) |
| `--job-bundle` | Path to job bundle directory |
| `--input-files` | Input files/directories to upload |
| `--output-dirs` | Output directories to track |
| `--test-upload` | Test upload only, don't submit job |
| `--submit-job` | Submit job after uploading attachments |
| `--v2` | Use v2 manifest format (v2025-12-04-beta) |
| `--profile` | AWS profile to use |
| `--region` | AWS region (default: us-west-2) |
| `--job-name` | Job name for submission |
| `--priority` | Job priority (default: 50) |

## Sample Job Bundles

### simple_job

A minimal job bundle for testing:
- `template.yaml` - Simple echo command
- `asset_references.yaml` - Single input file
- `input_data/test_file.txt` - Test input file
