# rusty_attachments

Python bindings for the rusty-attachments job attachment library.

## Installation

```bash
pip install rusty_attachments
```

## Usage

```python
import asyncio
from rusty_attachments import (
    S3Location,
    ManifestLocation,
    AssetReferences,
    BundleSubmitOptions,
    submit_bundle_attachments_py,
)

async def main():
    # Configure S3 locations
    s3_location = S3Location(
        bucket="my-bucket",
        root_prefix="DeadlineCloud",
        cas_prefix="Data",
        manifest_prefix="Manifests",
    )

    manifest_location = ManifestLocation(
        bucket="my-bucket",
        root_prefix="DeadlineCloud",
        farm_id="farm-xxx",
        queue_id="queue-xxx",
    )

    # Define assets
    asset_references = AssetReferences(
        input_filenames=["/path/to/files"],
        output_directories=["/path/to/outputs"],
    )

    # Submit
    result = await submit_bundle_attachments_py(
        region="us-west-2",
        s3_location=s3_location,
        manifest_location=manifest_location,
        asset_references=asset_references,
    )

    # Use result.attachments_json in CreateJob API
    print(result.attachments_json)

asyncio.run(main())
```

## Development

```bash
# Create virtual environment
python3 -m venv .venv
source .venv/bin/activate

# Install dev dependencies
pip install maturin pytest pytest-asyncio

# Build and install in development mode
maturin develop

# Run tests
pytest
```
