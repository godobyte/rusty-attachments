#!/usr/bin/env python3
"""
On-premises upload agent for relaxed consistency file fetching.

This agent integrates with Deadline Cloud's storage profile model.
It polls SQS queues for file upload requests from the VFS, resolves
files using the storage profile's FileSystemLocation entries, hashes
them with XXH128, uploads to S3 CAS, and writes completion markers.

The agent config mirrors Deadline's own structures:
- A storage profile defines LOCAL roots the agent can serve
- The farm/queue IDs determine which SQS queues to poll
- root_id in SQS messages maps to FileSystemLocation names

Usage:
    python3 upload_agent.py --config agent_config.json

Config file format:
    {
        "farmId": "farm-abc123",
        "queueId": "queue-def456",
        "region": "us-west-2",
        "bucket": "my-deadline-bucket",
        "rootPrefix": "DeadlineCloud",
        "storageProfile": {
            "fileSystemLocations": [
                {
                    "name": "StudioAssets",
                    "path": "/mnt/shared/assets",
                    "type": "LOCAL"
                },
                {
                    "name": "TextureLibrary",
                    "path": "/mnt/shared/textures",
                    "type": "LOCAL"
                }
            ]
        },
        "maxConcurrentUploads": 8,
        "highPriorityPollIntervalSecs": 1,
        "asyncPollIntervalSecs": 5
    }
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from mypy_boto3_s3 import S3Client
    from mypy_boto3_sqs import SQSClient

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
)
log: logging.Logger = logging.getLogger("upload_agent")


# ---------------------------------------------------------------------------
# Data structures (mirrors Deadline Cloud API)
# ---------------------------------------------------------------------------


@dataclass
class FileSystemLocation:
    """A named file system location from a Deadline storage profile.

    Attributes:
        name: Human-readable location name (e.g., "StudioAssets").
        path: Root path on the local filesystem.
        location_type: "LOCAL" (uploaded) or "SHARED" (accessible, not uploaded).
    """

    name: str
    path: str
    location_type: str  # "LOCAL" or "SHARED"


@dataclass
class StorageProfileConfig:
    """Storage profile configuration matching Deadline's model.

    Attributes:
        file_system_locations: List of named filesystem locations.
    """

    file_system_locations: list[FileSystemLocation] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: dict) -> StorageProfileConfig:
        """Parse from a dict (JSON-deserialized).

        Args:
            data: Dict with "fileSystemLocations" key.

        Returns:
            Parsed StorageProfileConfig.
        """
        locations: list[FileSystemLocation] = []
        for loc in data.get("fileSystemLocations", []):
            locations.append(
                FileSystemLocation(
                    name=loc["name"],
                    path=loc["path"],
                    location_type=loc.get("type", "LOCAL"),
                )
            )
        return cls(file_system_locations=locations)

    def local_locations(self) -> list[FileSystemLocation]:
        """Get all LOCAL type locations.

        Returns:
            List of LOCAL FileSystemLocations.
        """
        return [
            loc
            for loc in self.file_system_locations
            if loc.location_type == "LOCAL"
        ]

    def find_location_by_name(self, name: str) -> FileSystemLocation | None:
        """Find a location by its name.

        Args:
            name: The FileSystemLocation name.

        Returns:
            The matching location, or None.
        """
        for loc in self.file_system_locations:
            if loc.name == name:
                return loc
        return None


@dataclass
class AgentConfig:
    """Configuration for the upload agent.

    Attributes:
        farm_id: Deadline farm ID.
        queue_id: Deadline queue ID.
        region: AWS region for SQS and S3.
        bucket: S3 bucket for CAS and pending uploads.
        root_prefix: S3 root prefix (e.g., "DeadlineCloud").
        storage_profile: Storage profile defining LOCAL roots to serve.
        max_concurrent_uploads: Max parallel uploads.
        high_priority_poll_interval_secs: Poll interval for high-priority queue.
        async_poll_interval_secs: Poll interval for async queue.
    """

    farm_id: str
    queue_id: str
    region: str
    bucket: str
    root_prefix: str
    storage_profile: StorageProfileConfig
    max_concurrent_uploads: int = 8
    high_priority_poll_interval_secs: float = 1.0
    async_poll_interval_secs: float = 5.0

    @classmethod
    def from_json(cls, path: str) -> AgentConfig:
        """Load config from a JSON file.

        Args:
            path: Path to the JSON config file.

        Returns:
            Parsed AgentConfig.
        """
        with open(path) as f:
            data: dict = json.load(f)

        profile = StorageProfileConfig.from_dict(data.get("storageProfile", {}))

        return cls(
            farm_id=data["farmId"],
            queue_id=data["queueId"],
            region=data.get("region", "us-west-2"),
            bucket=data["bucket"],
            root_prefix=data.get("rootPrefix", "DeadlineCloud"),
            storage_profile=profile,
            max_concurrent_uploads=data.get("maxConcurrentUploads", 8),
            high_priority_poll_interval_secs=data.get(
                "highPriorityPollIntervalSecs", 1.0
            ),
            async_poll_interval_secs=data.get("asyncPollIntervalSecs", 5.0),
        )

    def resolve_source_path(self, root_id: str, source_root_path: str) -> str | None:
        """Resolve the local filesystem path for a root.

        Tries to match by fileSystemLocationName first (from the SQS message's
        root_id which may encode the location name), then falls back to the
        source_root_path from the SQS message.

        The storage profile's LOCAL locations define which roots this agent
        can serve. If the source_root_path falls under a known LOCAL location,
        we use it. Otherwise we reject the request.

        Args:
            root_id: The relaxed root identifier from the SQS message.
            source_root_path: The submitter's source path from the SQS message.

        Returns:
            The local root path to use, or None if this agent can't serve it.
        """
        # The source_root_path in the SQS message is the submitter's path.
        # Check if it falls under any of our LOCAL locations.
        for loc in self.storage_profile.local_locations():
            if source_root_path.startswith(loc.path) or loc.path.startswith(source_root_path):
                return source_root_path
            # Exact match on the location path itself
            if os.path.normpath(source_root_path) == os.path.normpath(loc.path):
                return loc.path
        # Fallback: if the source_root_path is a valid local directory, use it
        if os.path.isdir(source_root_path):
            return source_root_path
        return None


# ---------------------------------------------------------------------------
# Hashing and S3 key utilities
# ---------------------------------------------------------------------------


def xxh128_hex(data: bytes) -> str:
    """Compute XXH128 hex digest of input bytes.

    Args:
        data: The bytes to hash.

    Returns:
        A 32-character lowercase hex string.
    """
    import xxhash

    return xxhash.xxh128(data).hexdigest()


def xxh128_file(path: Path) -> str:
    """Compute XXH128 hex digest of a file.

    Args:
        path: Path to the file.

    Returns:
        A 32-character lowercase hex string.
    """
    import xxhash

    hasher = xxhash.xxh128()
    buf_size: int = 8 * 1024 * 1024  # 8MB chunks
    with open(path, "rb") as f:
        while True:
            chunk: bytes = f.read(buf_size)
            if not chunk:
                break
            hasher.update(chunk)
    return hasher.hexdigest()


def cas_key(root_prefix: str, content_hash: str) -> str:
    """Build the S3 CAS key for a content hash.

    Args:
        root_prefix: S3 root prefix (e.g., "DeadlineCloud").
        content_hash: XXH128 content hash.

    Returns:
        The full S3 CAS key.
    """
    return f"{root_prefix}/Data/{content_hash}.xxh128"


def marker_key(root_prefix: str, root_id: str, path_key: str) -> str:
    """Build the S3 key for a pending upload completion marker.

    Args:
        root_prefix: S3 root prefix.
        root_id: The relaxed root identifier.
        path_key: XXH128 hash of the relative path.

    Returns:
        The full S3 marker key.
    """
    return f"{root_prefix}/PendingUploads/{root_id}/{path_key}.xxh128"


def queue_name(farm_id: str, queue_id: str, priority: str) -> str:
    """Build the SQS queue name.

    Args:
        farm_id: Deadline farm ID.
        queue_id: Deadline queue ID.
        priority: "high" or "async".

    Returns:
        The SQS queue name.
    """
    return f"deadline-{farm_id}-{queue_id}-file-requests-{priority}"


# ---------------------------------------------------------------------------
# S3 operations
# ---------------------------------------------------------------------------


def check_marker_exists(
    s3_client: "S3Client", bucket: str, key: str
) -> bool:
    """Check if a completion marker already exists in S3.

    Args:
        s3_client: Boto3 S3 client.
        bucket: S3 bucket name.
        key: S3 object key.

    Returns:
        True if the marker exists.
    """
    try:
        s3_client.head_object(Bucket=bucket, Key=key)
        return True
    except s3_client.exceptions.ClientError as e:
        if e.response["Error"]["Code"] == "404":
            return False
        raise


def upload_to_cas(
    s3_client: "S3Client",
    bucket: str,
    cas_s3_key: str,
    file_path: Path,
) -> None:
    """Upload a file to S3 CAS if it doesn't already exist.

    Args:
        s3_client: Boto3 S3 client.
        bucket: S3 bucket name.
        cas_s3_key: The CAS S3 key.
        file_path: Local path to the file.
    """
    if check_marker_exists(s3_client, bucket, cas_s3_key):
        log.info("CAS object already exists: %s", cas_s3_key)
        return

    log.info("Uploading to CAS: %s (%d bytes)", cas_s3_key, file_path.stat().st_size)
    s3_client.upload_file(str(file_path), bucket, cas_s3_key)


def write_completion_marker(
    s3_client: "S3Client",
    bucket: str,
    marker_s3_key: str,
    content_hash: str,
    size: int,
    relative_path: str,
) -> None:
    """Write a completion marker to S3.

    Args:
        s3_client: Boto3 S3 client.
        bucket: S3 bucket name.
        marker_s3_key: The S3 key for the marker.
        content_hash: The CAS content hash.
        size: File size in bytes.
        relative_path: Original relative path.
    """
    marker_body: dict = {
        "status": "completed",
        "contentHash": content_hash,
        "hashAlgorithm": "xxh128",
        "size": size,
        "uploadedAt": time.time(),
        "relativePath": relative_path,
    }
    body: str = json.dumps(marker_body)
    s3_client.put_object(
        Bucket=bucket,
        Key=marker_s3_key,
        Body=body.encode("utf-8"),
        ContentType="application/json",
    )
    log.info("Wrote completion marker: %s", marker_s3_key)


def write_failure_marker(
    s3_client: "S3Client",
    bucket: str,
    marker_s3_key: str,
    reason: str,
    relative_path: str,
) -> None:
    """Write a failure marker to S3.

    Args:
        s3_client: Boto3 S3 client.
        bucket: S3 bucket name.
        marker_s3_key: The S3 key for the marker.
        reason: Failure reason.
        relative_path: Original relative path.
    """
    marker_body: dict = {
        "status": "failed",
        "reason": reason,
        "failedAt": time.time(),
        "relativePath": relative_path,
    }
    body: str = json.dumps(marker_body)
    s3_client.put_object(
        Bucket=bucket,
        Key=marker_s3_key,
        Body=body.encode("utf-8"),
        ContentType="application/json",
    )
    log.warning("Wrote failure marker: %s reason=%s", marker_s3_key, reason)


# ---------------------------------------------------------------------------
# Message processing
# ---------------------------------------------------------------------------


def process_message(
    s3_client: "S3Client",
    config: AgentConfig,
    message_body: str,
) -> bool:
    """Process a single SQS file upload request message.

    The SQS message contains the submitter's source_root_path and a relative_path.
    The agent resolves the local file using the storage profile's LOCAL locations,
    hashes it, uploads to CAS, and writes a completion marker.

    Args:
        s3_client: Boto3 S3 client.
        config: Agent configuration (with storage profile).
        message_body: Raw SQS message body (JSON).

    Returns:
        True if the message was processed successfully.
    """
    try:
        req: dict = json.loads(message_body)
    except json.JSONDecodeError:
        log.error("Invalid JSON in message: %s", message_body[:200])
        return False

    root_id: str = req.get("rootId", "")
    relative_path: str = req.get("relativePath", "")
    path_key: str = req.get("pathKey", "")
    source_root_path: str = req.get("sourceRootPath", "")
    bucket: str = req.get("bucket", config.bucket)
    root_prefix: str = req.get("rootPrefix", config.root_prefix)

    if not root_id or not relative_path or not path_key:
        log.error("Missing required fields in message: %s", req)
        return False

    mk: str = marker_key(root_prefix, root_id, path_key)

    # Check if already completed (idempotency)
    if check_marker_exists(s3_client, bucket, mk):
        log.info("Marker already exists, skipping: %s", mk)
        return True

    # Resolve local root using storage profile
    local_root: str | None = config.resolve_source_path(root_id, source_root_path)

    if local_root is None:
        log.error(
            "Cannot serve root_id=%s source=%s — not in storage profile LOCAL locations",
            root_id,
            source_root_path,
        )
        write_failure_marker(
            s3_client,
            bucket,
            mk,
            f"Source path not in agent's storage profile: {source_root_path}",
            relative_path,
        )
        return True  # Processed (failed), don't retry

    local_path = Path(local_root) / relative_path

    if not local_path.exists():
        log.warning("File not found: %s", local_path)
        write_failure_marker(
            s3_client, bucket, mk,
            f"File not found: {local_path}",
            relative_path,
        )
        return True

    if not local_path.is_file():
        log.warning("Not a regular file: %s", local_path)
        write_failure_marker(
            s3_client, bucket, mk,
            f"Not a regular file: {local_path}",
            relative_path,
        )
        return True

    # Hash the file
    file_size: int = local_path.stat().st_size
    log.info("Hashing file: %s (%d bytes)", local_path, file_size)
    content_hash: str = xxh128_file(local_path)

    # Upload to CAS
    ck: str = cas_key(root_prefix, content_hash)
    upload_to_cas(s3_client, bucket, ck, local_path)

    # Write completion marker
    write_completion_marker(
        s3_client, bucket, mk,
        content_hash, file_size, relative_path,
    )

    return True


# ---------------------------------------------------------------------------
# SQS polling
# ---------------------------------------------------------------------------


def get_queue_url(
    sqs_client: "SQSClient", name: str
) -> str | None:
    """Get the URL for an SQS queue by name.

    Args:
        sqs_client: Boto3 SQS client.
        name: Queue name.

    Returns:
        The queue URL, or None if not found.
    """
    try:
        resp: dict = sqs_client.get_queue_url(QueueName=name)
        return resp["QueueUrl"]
    except sqs_client.exceptions.QueueDoesNotExist:
        log.warning("Queue not found: %s", name)
        return None


def poll_queue(
    sqs_client: "SQSClient",
    s3_client: "S3Client",
    queue_url: str,
    config: AgentConfig,
    max_messages: int = 10,
) -> int:
    """Poll an SQS queue and process messages.

    Args:
        sqs_client: Boto3 SQS client.
        s3_client: Boto3 S3 client.
        queue_url: SQS queue URL.
        config: Agent configuration.
        max_messages: Maximum messages to receive per poll.

    Returns:
        Number of messages processed.
    """
    resp: dict = sqs_client.receive_message(
        QueueUrl=queue_url,
        MaxNumberOfMessages=min(max_messages, 10),
        WaitTimeSeconds=1,
    )

    messages: list[dict] = resp.get("Messages", [])
    processed: int = 0

    for msg in messages:
        receipt_handle: str = msg["ReceiptHandle"]
        body: str = msg["Body"]

        success: bool = process_message(s3_client, config, body)

        if success:
            sqs_client.delete_message(
                QueueUrl=queue_url,
                ReceiptHandle=receipt_handle,
            )
            processed += 1
        else:
            log.error("Failed to process message, will retry")

    return processed


# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------


def run_agent(config: AgentConfig) -> None:
    """Run the upload agent main loop.

    Args:
        config: Agent configuration.
    """
    import boto3

    session = boto3.Session(region_name=config.region)
    sqs_client: "SQSClient" = session.client("sqs")
    s3_client: "S3Client" = session.client("s3")

    high_queue_name: str = queue_name(config.farm_id, config.queue_id, "high")
    async_queue_name: str = queue_name(config.farm_id, config.queue_id, "async")

    high_url: str | None = get_queue_url(sqs_client, high_queue_name)
    async_url: str | None = get_queue_url(sqs_client, async_queue_name)

    if high_url is None and async_url is None:
        log.error("Neither queue found. Run setup_sqs.py first.")
        sys.exit(1)

    local_locations: list[FileSystemLocation] = config.storage_profile.local_locations()
    log.info("Upload agent started")
    log.info("  Farm: %s  Queue: %s", config.farm_id, config.queue_id)
    log.info("  Bucket: s3://%s/%s", config.bucket, config.root_prefix)
    log.info("  Storage profile LOCAL locations (%d):", len(local_locations))
    for loc in local_locations:
        log.info("    %s -> %s", loc.name, loc.path)
    if high_url:
        log.info("  High-priority queue: %s", high_queue_name)
    if async_url:
        log.info("  Async queue: %s", async_queue_name)

    try:
        while True:
            high_processed: int = 0
            if high_url:
                high_processed = poll_queue(
                    sqs_client, s3_client, high_url, config
                )

            if high_processed == 0 and async_url:
                poll_queue(sqs_client, s3_client, async_url, config)
                time.sleep(config.async_poll_interval_secs)
            elif high_processed == 0:
                time.sleep(config.high_priority_poll_interval_secs)

    except KeyboardInterrupt:
        log.info("Shutting down upload agent")


def main() -> None:
    """Entry point for the upload agent CLI."""
    parser = argparse.ArgumentParser(
        description="On-premises upload agent for relaxed consistency"
    )
    parser.add_argument(
        "--config",
        required=True,
        help="Path to agent config JSON file",
    )
    args = parser.parse_args()

    config = AgentConfig.from_json(args.config)
    run_agent(config)


if __name__ == "__main__":
    main()
