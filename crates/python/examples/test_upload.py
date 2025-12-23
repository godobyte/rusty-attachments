#!/usr/bin/env python3
"""Quick test script for rusty_attachments upload functionality.

This script creates temporary test files and uploads them using rusty_attachments
to verify the Rust implementation works correctly.

Usage:
    # Basic test with default settings
    python test_upload.py --farm-id farm-xxx --queue-id queue-xxx

    # Test with v2 manifest
    python test_upload.py --farm-id farm-xxx --queue-id queue-xxx --v2

    # Test with specific file count and size
    python test_upload.py --farm-id farm-xxx --queue-id queue-xxx --num-files 10 --file-size 1024
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import tempfile
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

MANIFEST_VERSION_V1 = "v2023-03-03"
MANIFEST_VERSION_V2 = "v2025-12-04-beta"


def parse_args() -> argparse.Namespace:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        description="Test rusty_attachments upload functionality"
    )
    parser.add_argument(
        "--v2",
        action="store_true",
        help="Use v2 manifest format",
    )
    parser.add_argument(
        "--profile",
        type=str,
        default=None,
        help="AWS profile to use",
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
        required=True,
        help="Deadline Cloud farm ID",
    )
    parser.add_argument(
        "--queue-id",
        type=str,
        required=True,
        help="Deadline Cloud queue ID",
    )
    parser.add_argument(
        "--num-files",
        type=int,
        default=5,
        help="Number of test files to create (default: 5)",
    )
    parser.add_argument(
        "--file-size",
        type=int,
        default=1024,
        help="Size of each test file in bytes (default: 1024)",
    )
    return parser.parse_args()


def create_test_files(temp_dir: Path, num_files: int, file_size: int) -> list[str]:
    """Create temporary test files.

    Args:
        temp_dir: Directory to create files in.
        num_files: Number of files to create.
        file_size: Size of each file in bytes.

    Returns:
        List of file paths.
    """
    files: list[str] = []
    for i in range(num_files):
        file_path: Path = temp_dir / f"test_file_{i:03d}.dat"
        # Create file with pseudo-random content based on index
        content: bytes = bytes([(i + j) % 256 for j in range(file_size)])
        file_path.write_bytes(content)
        files.append(str(file_path))
        print(f"  Created: {file_path} ({file_size} bytes)")
    return files


def progress_callback(progress: dict[str, Any]) -> bool:
    """Print progress and return True to continue."""
    phase: str = progress.get("phase", "Unknown")
    files: int = progress.get("files_processed", 0)
    total: Optional[int] = progress.get("total_files")
    current: str = progress.get("current_path", "")

    if total:
        pct: float = (files / total) * 100 if total > 0 else 0
        print(f"  [{phase}] {files}/{total} ({pct:.1f}%) - {Path(current).name if current else ''}")
    else:
        print(f"  [{phase}] {files} files")

    return True


def get_queue_attachment_settings(
    deadline_client: "DeadlineCloudClient",
    farm_id: str,
    queue_id: str,
) -> tuple[S3Location, ManifestLocation]:
    """Get S3 location settings from queue configuration."""
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


async def run_test(args: argparse.Namespace) -> bool:
    """Run the upload test.

    Args:
        args: Parsed command line arguments.

    Returns:
        True if test passed, False otherwise.
    """
    # Set AWS profile if specified
    if args.profile:
        os.environ["AWS_PROFILE"] = args.profile
        print(f"Using AWS profile: {args.profile}")

    print(f"Region: {args.region}")
    print(f"Farm ID: {args.farm_id}")
    print(f"Queue ID: {args.queue_id}")

    # Initialize boto3 client
    import boto3
    deadline_client: "DeadlineCloudClient" = boto3.client("deadline", region_name=args.region)

    # Get queue settings
    print("\nFetching queue attachment settings...")
    s3_location, manifest_location = get_queue_attachment_settings(
        deadline_client, args.farm_id, args.queue_id
    )
    print(f"  Bucket: {s3_location.bucket}")
    print(f"  Root Prefix: {s3_location.root_prefix}")

    # Create temporary test files
    with tempfile.TemporaryDirectory(prefix="rusty_attachments_test_") as temp_dir:
        temp_path: Path = Path(temp_dir)
        print(f"\nCreating {args.num_files} test files ({args.file_size} bytes each)...")
        test_files: list[str] = create_test_files(temp_path, args.num_files, args.file_size)

        # Create output directory
        output_dir: Path = temp_path / "outputs"
        output_dir.mkdir()

        asset_references = AssetReferences(
            input_filenames=test_files,
            output_directories=[str(output_dir)],
            referenced_paths=[],
        )

        manifest_version: str = MANIFEST_VERSION_V2 if args.v2 else MANIFEST_VERSION_V1
        print(f"\nUsing manifest version: {manifest_version}")

        options = BundleSubmitOptions(
            require_paths_exist=True,
            file_system_mode="COPIED",
            manifest_version=manifest_version,
            exclude_patterns=[],
        )

        print("\nUploading attachments...")
        result: BundleSubmitResult = await submit_bundle_attachments_py(
            region=args.region,
            s3_location=s3_location,
            manifest_location=manifest_location,
            asset_references=asset_references,
            storage_profile=None,
            options=options,
            progress_callback=progress_callback,
        )

        # Verify results
        print("\n=== Results ===")
        print(f"Hashing:")
        print(f"  Files processed: {result.hashing_stats.processed_files}")
        print(f"  Bytes processed: {result.hashing_stats.processed_bytes:,}")

        print(f"Upload:")
        print(f"  Files transferred: {result.upload_stats.files_transferred}")
        print(f"  Files skipped: {result.upload_stats.files_skipped}")
        print(f"  Bytes transferred: {result.upload_stats.bytes_transferred:,}")

        # Validate
        expected_files: int = args.num_files
        expected_bytes: int = args.num_files * args.file_size

        success: bool = True
        if result.hashing_stats.processed_files != expected_files:
            print(f"\n❌ FAIL: Expected {expected_files} files hashed, got {result.hashing_stats.processed_files}")
            success = False
        if result.hashing_stats.processed_bytes != expected_bytes:
            print(f"\n❌ FAIL: Expected {expected_bytes} bytes hashed, got {result.hashing_stats.processed_bytes}")
            success = False

        # Check attachments JSON
        attachments: dict[str, Any] = result.attachments_dict()
        print("\n=== Attachments JSON ===")
        print(json.dumps(attachments, indent=2))

        if "manifests" not in attachments:
            print("\n❌ FAIL: No manifests in attachments")
            success = False
        elif len(attachments["manifests"]) == 0:
            print("\n❌ FAIL: Empty manifests list")
            success = False

        if success:
            print("\n✅ TEST PASSED")
        else:
            print("\n❌ TEST FAILED")

        return success


async def main() -> None:
    """Main entry point."""
    args: argparse.Namespace = parse_args()

    try:
        success: bool = await run_test(args)
        sys.exit(0 if success else 1)
    except Exception as e:
        print(f"\n❌ ERROR: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())
