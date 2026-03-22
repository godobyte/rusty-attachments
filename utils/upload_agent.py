#!/usr/bin/env python3
"""
On-premises upload agent for relaxed consistency file fetching.

This agent polls SQS queues for file upload requests from the VFS,
reads files from local network storage, hashes them with XXH128,
uploads to S3 CAS, and writes completion markers.

Usage:
    python3 upload_agent.py --config agent_config.json

Config file format:
    {
        "region": "us-west-2",
        "bucket": "my-deadline-bucket",
        "rootPrefix": "DeadlineCloud",
        "farmId": "farm-abc123",
        "queueId": "queue-def456",
        "rootMappings": {
            "a1b2c3d4e5f6a7b8c9d0": "/mnt/shared/assets",
            "f0e1d2c3b4a5f6e7d8c9": "/mnt/shared/textures"
        },
        "maxConcurrentUploads": 8,
        "highPriorityPollIntervalSecs": 1,
        "asyncPollIntervalSecs": 5
    }
"""

from __future__ import annotations

import argparse
import hashlib
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


@dataclass
class AgentConfig:
    """Configuration for the upload agent."""

    region: str
    bucket: str
    root_prefix: str
    farm_id: str
    queue_id: str
    root_mappings: dict[str, str] = field(default_factory=dict)
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
        return cls(
            region=data["region"],
            bucket=data["bucket"],
            root_prefix=data["rootPrefix"],
            farm_id=data["farmId"],
            queue_id=data["queueId"],
            root_mappings=data.get("rootMappings", {}),
            max_concurrent_uploads=data.get("maxConcurrentUploads", 8),
            high_priority_poll_interval_secs=data.get(
                "highPriorityPollIntervalSecs", 1.0
            ),
            async_poll_interval_secs=data.get("asyncPollIntervalSecs", 5.0),
        )


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
    marker: dict = {
        "status": "completed",
        "contentHash": content_hash,
        "hashAlgorithm": "xxh128",
        "size": size,
        "uploadedAt": time.time(),
        "relativePath": relative_path,
    }
    body: str = json.dumps(marker)
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
    marker: dict = {
        "status": "failed",
        "reason": reason,
        "failedAt": time.time(),
        "relativePath": relative_path,
    }
    body: str = json.dumps(marker)
    s3_client.put_object(
        Bucket=bucket,
        Key=marker_s3_key,
        Body=body.encode("utf-8"),
        ContentType="application/json",
    )
    log.warning("Wrote failure marker: %s reason=%s", marker_s3_key, reason)


def process_message(
    s3_client: "S3Client",
    config: AgentConfig,
    message_body: str,
) -> bool:
    """Process a single SQS file upload request message.

    Args:
        s3_client: Boto3 S3 client.
        config: Agent configuration.
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

    # Resolve local path
    local_root: str | None = config.root_mappings.get(root_id)
    if local_root is None:
        # Try using source_root_path from the message
        local_root = req.get("sourceRootPath")

    if local_root is None:
        log.error("No root mapping for root_id=%s", root_id)
        write_failure_marker(
            s3_client, bucket, mk,
            f"No root mapping for root_id={root_id}",
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

    log.info("Upload agent started")
    log.info("  Farm: %s", config.farm_id)
    log.info("  Queue: %s", config.queue_id)
    log.info("  Bucket: %s", config.bucket)
    log.info("  Root mappings: %s", config.root_mappings)
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
