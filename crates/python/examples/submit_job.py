#!/usr/bin/env python3
"""Example: Submit job attachments using rusty_attachments."""

import asyncio

from rusty_attachments import (
    AssetReferences,
    BundleSubmitOptions,
    ManifestLocation,
    S3Location,
    StorageProfile,
    FileSystemLocation,
    submit_bundle_attachments_py,
)


def progress_callback(progress: dict) -> bool:
    """Print progress and return True to continue."""
    phase = progress.get("phase", "Unknown")
    files = progress.get("files_processed", 0)
    total = progress.get("total_files")
    current = progress.get("current_path", "")

    if total:
        print(f"[{phase}] {files}/{total} - {current}")
    else:
        print(f"[{phase}] {files} files - {current}")

    return True  # Return False to cancel


async def main() -> None:
    # Configure S3 locations (from your Deadline queue settings)
    s3_location = S3Location(
        bucket="my-deadline-bucket",
        root_prefix="DeadlineCloud",
        cas_prefix="Data",
        manifest_prefix="Manifests",
    )

    manifest_location = ManifestLocation(
        bucket="my-deadline-bucket",
        root_prefix="DeadlineCloud",
        farm_id="farm-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        queue_id="queue-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    )

    # Define your job's assets
    asset_references = AssetReferences(
        input_filenames=[
            "/projects/my-job/scene.blend",
            "/projects/my-job/textures",  # Directories are expanded automatically
        ],
        output_directories=[
            "/projects/my-job/renders",
        ],
        referenced_paths=[],  # Paths that may not exist
    )

    # Optional: Configure a storage profile to skip shared storage
    storage_profile = StorageProfile(
        locations=[
            FileSystemLocation(
                name="ProjectFiles",
                path="/projects",
                location_type="LOCAL",
            ),
            FileSystemLocation(
                name="SharedAssets",
                path="/mnt/shared",
                location_type="SHARED",  # Files here are skipped
            ),
        ]
    )

    # Optional: Configure submit options
    options = BundleSubmitOptions(
        require_paths_exist=False,  # Treat missing files as references
        file_system_mode="COPIED",  # or "VIRTUAL"
        manifest_version="v2025-12-04-beta",
        exclude_patterns=["**/*.tmp", "**/__pycache__/**", "**/.git/**"],
    )

    # Submit the bundle
    print("Submitting job attachments...")
    result = await submit_bundle_attachments_py(
        region="us-west-2",
        s3_location=s3_location,
        manifest_location=manifest_location,
        asset_references=asset_references,
        storage_profile=storage_profile,
        options=options,
        progress_callback=progress_callback,
    )

    # Print results
    print("\n=== Results ===")
    print(f"Hashing: {result.hashing_stats.processed_files} files, "
          f"{result.hashing_stats.processed_bytes:,} bytes")
    print(f"Upload: {result.upload_stats.files_transferred} transferred, "
          f"{result.upload_stats.files_skipped} skipped")
    print(f"Bytes transferred: {result.upload_stats.bytes_transferred:,}")

    # Get the attachments JSON for CreateJob API
    print("\n=== Attachments JSON ===")
    attachments_dict = result.attachments_dict()
    import json
    print(json.dumps(attachments_dict, indent=2))

    # Or get as raw JSON string
    # attachments_json = result.attachments_json

    # Now use attachments_json in your CreateJob API call:
    # deadline_client.create_job(
    #     farmId="farm-xxx",
    #     queueId="queue-xxx",
    #     template=job_template,
    #     attachments=attachments_json,
    # )


if __name__ == "__main__":
    asyncio.run(main())
