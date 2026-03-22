#!/usr/bin/env python3
"""
Setup SQS queues for relaxed consistency file upload requests.

Creates two standard SQS queues per farm/queue combination:
  - deadline-{farm_id}-{queue_id}-file-requests-high
  - deadline-{farm_id}-{queue_id}-file-requests-async

Usage:
    python3 setup_sqs.py --farm-id farm-abc123 --queue-id queue-def456 --region us-west-2

To delete the queues:
    python3 setup_sqs.py --farm-id farm-abc123 --queue-id queue-def456 --region us-west-2 --delete
"""

from __future__ import annotations

import argparse
import json
import sys
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from mypy_boto3_sqs import SQSClient


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


def create_queue(
    sqs_client: "SQSClient",
    name: str,
    visibility_timeout: int = 300,
    retention_period: int = 86400,
) -> str:
    """Create an SQS standard queue.

    Args:
        sqs_client: Boto3 SQS client.
        name: Queue name.
        visibility_timeout: Message visibility timeout in seconds.
        retention_period: Message retention period in seconds.

    Returns:
        The queue URL.
    """
    resp: dict = sqs_client.create_queue(
        QueueName=name,
        Attributes={
            "VisibilityTimeout": str(visibility_timeout),
            "MessageRetentionPeriod": str(retention_period),
            "ReceiveMessageWaitTimeSeconds": "5",
        },
        tags={
            "Purpose": "relaxed-consistency-file-requests",
            "ManagedBy": "deadline-upload-agent",
        },
    )
    return resp["QueueUrl"]


def delete_queue(sqs_client: "SQSClient", name: str) -> bool:
    """Delete an SQS queue by name.

    Args:
        sqs_client: Boto3 SQS client.
        name: Queue name.

    Returns:
        True if the queue was deleted, False if not found.
    """
    try:
        resp: dict = sqs_client.get_queue_url(QueueName=name)
        url: str = resp["QueueUrl"]
        sqs_client.delete_queue(QueueUrl=url)
        return True
    except sqs_client.exceptions.QueueDoesNotExist:
        return False


def main() -> None:
    """Entry point for the SQS setup script."""
    parser = argparse.ArgumentParser(
        description="Setup SQS queues for relaxed consistency file requests"
    )
    parser.add_argument("--farm-id", required=True, help="Deadline farm ID")
    parser.add_argument("--queue-id", required=True, help="Deadline queue ID")
    parser.add_argument("--region", default="us-west-2", help="AWS region")
    parser.add_argument(
        "--delete",
        action="store_true",
        help="Delete the queues instead of creating them",
    )
    parser.add_argument(
        "--visibility-timeout",
        type=int,
        default=300,
        help="Message visibility timeout in seconds (default: 300)",
    )
    parser.add_argument(
        "--retention-period",
        type=int,
        default=86400,
        help="Message retention period in seconds (default: 86400 = 1 day)",
    )
    args = parser.parse_args()

    import boto3

    session = boto3.Session(region_name=args.region)
    sqs_client: "SQSClient" = session.client("sqs")

    high_name: str = queue_name(args.farm_id, args.queue_id, "high")
    async_name: str = queue_name(args.farm_id, args.queue_id, "async")

    if args.delete:
        print(f"Deleting queues for farm={args.farm_id} queue={args.queue_id}...")
        deleted_high: bool = delete_queue(sqs_client, high_name)
        deleted_async: bool = delete_queue(sqs_client, async_name)
        if deleted_high:
            print(f"  Deleted: {high_name}")
        else:
            print(f"  Not found: {high_name}")
        if deleted_async:
            print(f"  Deleted: {async_name}")
        else:
            print(f"  Not found: {async_name}")
    else:
        print(f"Creating queues for farm={args.farm_id} queue={args.queue_id}...")
        high_url: str = create_queue(
            sqs_client,
            high_name,
            visibility_timeout=args.visibility_timeout,
            retention_period=args.retention_period,
        )
        async_url: str = create_queue(
            sqs_client,
            async_name,
            visibility_timeout=args.visibility_timeout,
            retention_period=args.retention_period,
        )
        print(f"  High-priority queue: {high_name}")
        print(f"    URL: {high_url}")
        print(f"  Async queue: {async_name}")
        print(f"    URL: {async_url}")

        # Print sample agent config
        sample_config: dict = {
            "region": args.region,
            "bucket": "<YOUR_BUCKET>",
            "rootPrefix": "DeadlineCloud",
            "farmId": args.farm_id,
            "queueId": args.queue_id,
            "rootMappings": {
                "<ROOT_ID>": "/mnt/shared/assets",
            },
            "maxConcurrentUploads": 8,
            "highPriorityPollIntervalSecs": 1,
            "asyncPollIntervalSecs": 5,
        }
        print("\nSample agent config (save as agent_config.json):")
        print(json.dumps(sample_config, indent=2))


if __name__ == "__main__":
    main()
