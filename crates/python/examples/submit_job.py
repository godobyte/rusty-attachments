#!/usr/bin/env python3
"""Example: Submit job attachments using rusty_attachments.

This script demonstrates how to use the Rust-based rusty_attachments library
to upload job attachments and optionally submit a job to AWS Deadline Cloud.

Usage:
    # Test attachment upload only (no job submission)
    python submit_job.py --test-upload

    # Submit with v1 manifest, default profile
    python submit_job.py

    # Submit with v2 manifest
    python submit_job.py --v2

    # Use specific AWS profile
    python submit_job.py --profile my-profile

    # Specify farm/queue IDs explicitly
    python submit_job.py --farm-id farm-xxx --queue-id queue-xxx

    # Use a job bundle directory
    python submit_job.py --job-bundle /path/to/bundle
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
from pathlib import Path
from typing import TYPE_CHECKING, Any, Optional

from rusty_attachments import (
    AssetReferences,
    BundleSubmitOptions,
    BundleSubmitResult,
    ManifestLocation,
    S3Location,
    submit_bundle_attachments_py,
)

if TYPE_CHECKING:
    from mypy_boto3_deadline import DeadlineCloudClient

# Manifest version constants
MANIFEST_VERSION_V1 = "v2023-03-03"
MANIFEST_VERSION_V2 = "v2025-12-04-beta"


def parse_args() -> argparse.Namespace:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        description="Submit job attachments using rusty_attachments"
    )
    parser.add_argument(
        "--v2",
        action="store_true",
        help="Use v2 manifest format (v2025-12-04-beta). Default is v1 (v2023-03-03)",
    )
    parser.add_argument(
        "--profile",
        type=str,
        default=None,
        help="AWS profile to use (default: use environment/default)",
    )
    parser.add_argument(
        "--region",
        type=str,
        default="us-west-2",
        help="AWS region (default: us-west-2)",
    )
    parser.add_argument(
        "--farm-id",
        type=str,
        help="Deadline Cloud farm ID (or set DEADLINE_FARM_ID env var)",
    )
    parser.add_argument(
        "--queue-id",
        type=str,
        help="Deadline Cloud queue ID (or set DEADLINE_QUEUE_ID env var)",
    )
    parser.add_argument(
        "--job-bundle",
        type=str,
        help="Path to job bundle directory containing template.yaml and asset_references.yaml",
    )
    parser.add_argument(
        "--input-files",
        type=str,
        nargs="+",
        help="Input files/directories to upload (if not using --job-bundle)",
    )
    parser.add_argument(
        "--output-dirs",
        type=str,
        nargs="+",
        default=[],
        help="Output directories to track",
    )
    parser.add_argument(
        "--test-upload",
        action="store_true",
        help="Test attachment upload only, do not submit job",
    )
    parser.add_argument(
        "--submit-job",
        action="store_true",
        help="Submit job to Deadline Cloud after uploading attachments",
    )
    parser.add_argument(
        "--job-name",
        type=str,
        default="RustyAttachmentsTestJob",
        help="Job name for submission (default: RustyAttachmentsTestJob)",
    )
    parser.add_argument(
        "--priority",
        type=int,
        default=50,
        help="Job priority (default: 50)",
    )
    return parser.parse_args()


def progress_callback(progress: dict[str, Any]) -> bool:
    """Print progress and return True to continue."""
    phase: str = progress.get("phase", "Unknown")
    files: int = progress.get("files_processed", 0)
    total: Optional[int] = progress.get("total_files")
    current: str = progress.get("current_path", "")

    if total:
        print(f"[{phase}] {files}/{total} - {current}")
    else:
        print(f"[{phase}] {files} files - {current}")

    return True  # Return False to cancel


def load_job_bundle(bundle_dir: str) -> tuple[dict[str, Any], AssetReferences]:
    """Load job template and asset references from a job bundle directory.

    Args:
        bundle_dir: Path to the job bundle directory.

    Returns:
        Tuple of (job_template_dict, asset_references).
    """
    bundle_path: Path = Path(bundle_dir)

    # Load job template (template.yaml or template.json)
    template_path: Optional[Path] = None
    for name in ["template.yaml", "template.yml", "template.json"]:
        candidate: Path = bundle_path / name
        if candidate.exists():
            template_path = candidate
            break

    if template_path is None:
        raise FileNotFoundError(f"No template file found in {bundle_dir}")

    if template_path.suffix in (".yaml", ".yml"):
        import yaml
        with open(template_path) as f:
            job_template: dict[str, Any] = yaml.safe_load(f)
    else:
        with open(template_path) as f:
            job_template = json.load(f)

    # Load asset references (asset_references.yaml or asset_references.json)
    asset_refs_path: Optional[Path] = None
    for name in ["asset_references.yaml", "asset_references.yml", "asset_references.json"]:
        candidate = bundle_path / name
        if candidate.exists():
            asset_refs_path = candidate
            break

    input_filenames: list[str] = []
    output_directories: list[str] = []
    referenced_paths: list[str] = []

    if asset_refs_path is not None:
        if asset_refs_path.suffix in (".yaml", ".yml"):
            import yaml
            with open(asset_refs_path) as f:
                refs: dict[str, Any] = yaml.safe_load(f) or {}
        else:
            with open(asset_refs_path) as f:
                refs = json.load(f)

        # Handle both list and dict formats
        raw_filenames: list[str] = refs.get("inputs", {}).get("filenames", [])
        raw_dirs: list[str] = refs.get("inputs", {}).get("directories", [])
        raw_outputs: list[str] = refs.get("outputs", {}).get("directories", [])
        raw_refs: list[str] = refs.get("referencedPaths", [])

        # Resolve relative paths against the bundle directory
        for f in raw_filenames:
            resolved: Path = (bundle_path / f).resolve() if not Path(f).is_absolute() else Path(f)
            input_filenames.append(str(resolved))
        for d in raw_dirs:
            resolved = (bundle_path / d).resolve() if not Path(d).is_absolute() else Path(d)
            input_filenames.append(str(resolved))
        for d in raw_outputs:
            resolved = (bundle_path / d).resolve() if not Path(d).is_absolute() else Path(d)
            output_directories.append(str(resolved))
        for r in raw_refs:
            resolved = (bundle_path / r).resolve() if not Path(r).is_absolute() else Path(r)
            referenced_paths.append(str(resolved))

    asset_references = AssetReferences(
        input_filenames=input_filenames,
        output_directories=output_directories,
        referenced_paths=referenced_paths,
    )

    return job_template, asset_references


def get_queue_attachment_settings(
    deadline_client: "DeadlineCloudClient",
    farm_id: str,
    queue_id: str,
) -> tuple[S3Location, ManifestLocation]:
    """Get S3 location settings from queue configuration.

    Args:
        deadline_client: Boto3 Deadline Cloud client.
        farm_id: Farm ID.
        queue_id: Queue ID.

    Returns:
        Tuple of (S3Location, ManifestLocation).
    """
    queue: dict[str, Any] = deadline_client.get_queue(farmId=farm_id, queueId=queue_id)

    if "jobAttachmentSettings" not in queue:
        raise ValueError(f"Queue {queue_id} does not have job attachment settings configured")

    settings: dict[str, str] = queue["jobAttachmentSettings"]
    bucket: str = settings["s3BucketName"]
    root_prefix: str = settings["rootPrefix"]

    s3_location = S3Location(
        bucket=bucket,
        root_prefix=root_prefix,
        cas_prefix="Data",
        manifest_prefix="Manifests",
    )

    manifest_location = ManifestLocation(
        bucket=bucket,
        root_prefix=root_prefix,
        farm_id=farm_id,
        queue_id=queue_id,
    )

    return s3_location, manifest_location


def create_deadline_job(
    deadline_client: "DeadlineCloudClient",
    farm_id: str,
    queue_id: str,
    job_template: dict[str, Any],
    attachments: dict[str, Any],
    job_name: str,
    priority: int,
) -> str:
    """Create a job in Deadline Cloud.

    Args:
        deadline_client: Boto3 Deadline Cloud client.
        farm_id: Farm ID.
        queue_id: Queue ID.
        job_template: Job template dictionary.
        attachments: Attachments dictionary from rusty_attachments.
        job_name: Name for the job.
        priority: Job priority.

    Returns:
        Job ID of the created job.
    """
    # Override job name if provided
    job_template["name"] = job_name

    # Determine template type
    template_type: str = "JSON"

    response: dict[str, Any] = deadline_client.create_job(
        farmId=farm_id,
        queueId=queue_id,
        template=json.dumps(job_template),
        templateType=template_type,
        attachments=attachments,
        priority=priority,
    )

    job_id: str = response["jobId"]
    return job_id


async def run_upload(
    args: argparse.Namespace,
    s3_location: S3Location,
    manifest_location: ManifestLocation,
    asset_references: AssetReferences,
    storage_profile: Any,
) -> BundleSubmitResult:
    """Run the attachment upload using rusty_attachments.

    Args:
        args: Parsed command line arguments.
        s3_location: S3 location configuration.
        manifest_location: Manifest location configuration.
        asset_references: Asset references to upload.
        storage_profile: Optional storage profile (StorageProfile or None).

    Returns:
        BundleSubmitResult from the upload operation.
    """
    manifest_version: str = MANIFEST_VERSION_V2 if args.v2 else MANIFEST_VERSION_V1
    print(f"Using manifest version: {manifest_version}")

    options = BundleSubmitOptions(
        require_paths_exist=False,
        file_system_mode="COPIED",
        manifest_version=manifest_version,
        exclude_patterns=["**/*.tmp", "**/__pycache__/**", "**/.git/**"],
    )

    print("Submitting job attachments via rusty_attachments...")
    result: BundleSubmitResult = await submit_bundle_attachments_py(
        region=args.region,
        s3_location=s3_location,
        manifest_location=manifest_location,
        asset_references=asset_references,
        storage_profile=storage_profile,
        options=options,
        progress_callback=progress_callback,
    )

    return result


async def main() -> None:
    """Main entry point."""
    args: argparse.Namespace = parse_args()

    # Set AWS profile if specified
    if args.profile:
        os.environ["AWS_PROFILE"] = args.profile
        print(f"Using AWS profile: {args.profile}")

    # Get farm and queue IDs
    farm_id: str = args.farm_id or os.environ.get("DEADLINE_FARM_ID", "")
    queue_id: str = args.queue_id or os.environ.get("DEADLINE_QUEUE_ID", "")

    if not farm_id or not queue_id:
        print("Error: --farm-id and --queue-id are required (or set DEADLINE_FARM_ID/DEADLINE_QUEUE_ID)")
        sys.exit(1)

    print(f"Farm ID: {farm_id}")
    print(f"Queue ID: {queue_id}")

    # Initialize boto3 client for Deadline
    import boto3
    deadline_client: "DeadlineCloudClient" = boto3.client("deadline", region_name=args.region)

    # Get S3 location settings from queue
    print("Fetching queue attachment settings...")
    s3_location, manifest_location = get_queue_attachment_settings(
        deadline_client, farm_id, queue_id
    )
    print(f"S3 Bucket: {s3_location.bucket}")
    print(f"Root Prefix: {s3_location.root_prefix}")

    # Load job bundle or use provided inputs
    job_template: Optional[dict[str, Any]] = None
    if args.job_bundle:
        print(f"Loading job bundle from: {args.job_bundle}")
        job_template, asset_references = load_job_bundle(args.job_bundle)
    elif args.input_files:
        asset_references = AssetReferences(
            input_filenames=args.input_files,
            output_directories=args.output_dirs,
            referenced_paths=[],
        )
    else:
        print("Error: Either --job-bundle or --input-files is required")
        sys.exit(1)

    print(f"Input files: {asset_references.input_filenames}")
    print(f"Output directories: {asset_references.output_directories}")

    # Optional: Configure a storage profile
    # To use a storage profile, import FileSystemLocation and StorageProfile:
    # from rusty_attachments import FileSystemLocation, StorageProfile
    # storage_profile = StorageProfile(
    #     locations=[
    #         FileSystemLocation(name="ProjectFiles", path="/projects", location_type="LOCAL"),
    #         FileSystemLocation(name="SharedAssets", path="/mnt/shared", location_type="SHARED"),
    #     ]
    # )
    storage_profile = None

    # Run the upload
    result: BundleSubmitResult = await run_upload(
        args,
        s3_location,
        manifest_location,
        asset_references,
        storage_profile,
    )

    # Print results
    print("\n=== Upload Results ===")
    print(f"Hashing: {result.hashing_stats.processed_files} files, "
          f"{result.hashing_stats.processed_bytes:,} bytes")
    print(f"Upload: {result.upload_stats.files_transferred} transferred, "
          f"{result.upload_stats.files_skipped} skipped")
    print(f"Bytes transferred: {result.upload_stats.bytes_transferred:,}")

    # Get the attachments for CreateJob API
    attachments_dict: dict[str, Any] = result.attachments_dict()
    print("\n=== Attachments JSON ===")
    print(json.dumps(attachments_dict, indent=2))

    # Test upload only mode
    if args.test_upload:
        print("\n=== Test Upload Complete ===")
        print("Attachments uploaded successfully. Use --submit-job to create a job.")
        return

    # Submit job if requested
    if args.submit_job:
        if job_template is None:
            print("Error: --job-bundle is required for job submission")
            sys.exit(1)

        print("\n=== Submitting Job ===")
        job_id: str = create_deadline_job(
            deadline_client,
            farm_id,
            queue_id,
            job_template,
            attachments_dict,
            args.job_name,
            args.priority,
        )
        print(f"Job submitted successfully!")
        print(f"Job ID: {job_id}")
    else:
        print("\n=== Upload Complete ===")
        print("Use --submit-job to create a job with these attachments.")


if __name__ == "__main__":
    asyncio.run(main())
