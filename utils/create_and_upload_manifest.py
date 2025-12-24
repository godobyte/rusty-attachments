#!/usr/bin/env python3
"""Create a V2 manifest from a folder and upload to S3 CAS.

This script scans a directory, creates a V2 manifest (v2025-12-04-beta),
and uploads the files to S3 CAS storage.

Usage:
    python create_and_upload_manifest.py <folder_path> [options]

Example:
    python create_and_upload_manifest.py ./my_folder \
        --bucket adeadlineja \
        --root-prefix DeadlineCloud \
        --profile my-aws-profile
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
from pathlib import Path
from typing import TYPE_CHECKING

from rusty_attachments import (
    AssetReferences,
    BundleSubmitOptions,
    BundleSubmitResult,
    ManifestLocation,
    S3Location,
    submit_bundle_attachments_py,
)

if TYPE_CHECKING:
    pass

MANIFEST_VERSION_V2 = "v2025-12-04-beta"


def parse_args() -> argparse.Namespace:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        description="Create V2 manifest from folder and upload to S3 CAS"
    )
    parser.add_argument(
        "folder",
        type=str,
        help="Path to folder to create manifest from",
    )
    parser.add_argument(
        "--bucket",
        type=str,
        default="adeadlineja",
        help="S3 bucket name (default: adeadlineja)",
    )
    parser.add_argument(
        "--root-prefix",
        type=str,
        default="DeadlineCloud",
        help="Root prefix in S3 (default: DeadlineCloud)",
    )
    parser.add_argument(
        "--farm-id",
        type=str,
        default="farm-test",
        help="Farm ID for manifest location (default: farm-test)",
    )
    parser.add_argument(
        "--queue-id",
        type=str,
        default="queue-test",
        help="Queue ID for manifest location (default: queue-test)",
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
        "--output-manifest",
        type=str,
        default=None,
        help="Path to save manifest JSON (optional)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Only scan and create manifest, don't upload",
    )
    return parser.parse_args()


def collect_files(folder: Path) -> list[str]:
    """Recursively collect all files in a folder.

    Args:
        folder: Root folder to scan.

    Returns:
        List of absolute file paths.
    """
    files: list[str] = []
    for path in folder.rglob("*"):
        if path.is_file():
            files.append(str(path.resolve()))
    return sorted(files)


def progress_callback(progress: dict[str, object]) -> bool:
    """Print progress updates."""
    phase: str = str(progress.get("phase", "Unknown"))
    files: int = int(progress.get("files_processed", 0))
    total: int | None = progress.get("total_files")  # type: ignore[assignment]
    current: str = str(progress.get("current_path", ""))

    if total:
        pct: float = (files / total) * 100 if total > 0 else 0
        name: str = Path(current).name if current else ""
        print(f"  [{phase}] {files}/{total} ({pct:.1f}%) - {name}")
    else:
        print(f"  [{phase}] {files} files")

    return True


async def run_upload(args: argparse.Namespace) -> bool:
    """Run the manifest creation and upload.

    Args:
        args: Parsed command line arguments.

    Returns:
        True if successful, False otherwise.
    """
    import os

    # Set AWS profile if specified
    if args.profile:
        os.environ["AWS_PROFILE"] = args.profile
        print(f"Using AWS profile: {args.profile}")

    folder: Path = Path(args.folder).resolve()
    if not folder.exists():
        print(f"Error: Folder does not exist: {folder}")
        return False

    if not folder.is_dir():
        print(f"Error: Path is not a directory: {folder}")
        return False

    print(f"Scanning folder: {folder}")
    files: list[str] = collect_files(folder)
    print(f"Found {len(files)} files")

    if not files:
        print("Error: No files found in folder")
        return False

    # Show first few files
    for f in files[:5]:
        print(f"  - {Path(f).relative_to(folder)}")
    if len(files) > 5:
        print(f"  ... and {len(files) - 5} more")

    if args.dry_run:
        print("\n[DRY RUN] Would upload to:")
        print(f"  Bucket: {args.bucket}")
        print(f"  Root Prefix: {args.root_prefix}")
        return True

    # Configure S3 locations
    s3_location = S3Location(
        bucket=args.bucket,
        root_prefix=args.root_prefix,
        cas_prefix="Data",
        manifest_prefix="Manifests",
    )

    manifest_location = ManifestLocation(
        bucket=args.bucket,
        root_prefix=args.root_prefix,
        farm_id=args.farm_id,
        queue_id=args.queue_id,
    )

    asset_references = AssetReferences(
        input_filenames=files,
        output_directories=[],
        referenced_paths=[],
    )

    options = BundleSubmitOptions(
        require_paths_exist=True,
        file_system_mode="COPIED",
        manifest_version=MANIFEST_VERSION_V2,
        exclude_patterns=[],
    )

    print(f"\nUploading to s3://{args.bucket}/{args.root_prefix}/")
    print(f"Using manifest version: {MANIFEST_VERSION_V2}")

    result: BundleSubmitResult = await submit_bundle_attachments_py(
        region=args.region,
        s3_location=s3_location,
        manifest_location=manifest_location,
        asset_references=asset_references,
        storage_profile=None,
        options=options,
        progress_callback=progress_callback,
    )

    # Print results
    print("\n=== Results ===")
    print(f"Hashing:")
    print(f"  Files processed: {result.hashing_stats.processed_files}")
    print(f"  Bytes processed: {result.hashing_stats.processed_bytes:,}")

    print(f"Upload:")
    print(f"  Files transferred: {result.upload_stats.files_transferred}")
    print(f"  Files skipped: {result.upload_stats.files_skipped}")
    print(f"  Bytes transferred: {result.upload_stats.bytes_transferred:,}")

    # Get attachments info
    attachments: dict[str, object] = result.attachments_dict()
    print("\n=== Attachments JSON ===")
    print(json.dumps(attachments, indent=2))

    # Save manifest if requested
    if args.output_manifest:
        output_path: Path = Path(args.output_manifest)
        output_path.write_text(json.dumps(attachments, indent=2))
        print(f"\nManifest saved to: {output_path}")

    print("\n✅ Upload complete!")
    return True


async def main() -> None:
    """Main entry point."""
    args: argparse.Namespace = parse_args()

    try:
        success: bool = await run_upload(args)
        sys.exit(0 if success else 1)
    except Exception as e:
        print(f"\n❌ ERROR: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())
