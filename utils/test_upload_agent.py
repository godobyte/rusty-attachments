#!/usr/bin/env python3
"""Unit tests for the upload agent pure functions."""

from __future__ import annotations

import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

# Add utils to path
sys.path.insert(0, os.path.dirname(__file__))

import upload_agent


class TestAgentConfig(unittest.TestCase):
    """Tests for AgentConfig loading."""

    def test_from_json(self) -> None:
        """Test loading config from a JSON file."""
        config_data: dict = {
            "region": "us-west-2",
            "bucket": "test-bucket",
            "rootPrefix": "DeadlineCloud",
            "farmId": "farm-123",
            "queueId": "queue-456",
            "rootMappings": {"root1": "/mnt/shared"},
            "maxConcurrentUploads": 4,
            "highPriorityPollIntervalSecs": 2.0,
            "asyncPollIntervalSecs": 10.0,
        }

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".json", delete=False
        ) as f:
            json.dump(config_data, f)
            config_path: str = f.name

        try:
            config = upload_agent.AgentConfig.from_json(config_path)
            self.assertEqual(config.region, "us-west-2")
            self.assertEqual(config.bucket, "test-bucket")
            self.assertEqual(config.root_prefix, "DeadlineCloud")
            self.assertEqual(config.farm_id, "farm-123")
            self.assertEqual(config.queue_id, "queue-456")
            self.assertEqual(config.root_mappings, {"root1": "/mnt/shared"})
            self.assertEqual(config.max_concurrent_uploads, 4)
            self.assertAlmostEqual(config.high_priority_poll_interval_secs, 2.0)
            self.assertAlmostEqual(config.async_poll_interval_secs, 10.0)
        finally:
            os.unlink(config_path)

    def test_from_json_defaults(self) -> None:
        """Test that missing optional fields use defaults."""
        config_data: dict = {
            "region": "us-east-1",
            "bucket": "bucket",
            "rootPrefix": "Prefix",
            "farmId": "farm-1",
            "queueId": "queue-1",
        }

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".json", delete=False
        ) as f:
            json.dump(config_data, f)
            config_path: str = f.name

        try:
            config = upload_agent.AgentConfig.from_json(config_path)
            self.assertEqual(config.root_mappings, {})
            self.assertEqual(config.max_concurrent_uploads, 8)
            self.assertAlmostEqual(config.high_priority_poll_interval_secs, 1.0)
            self.assertAlmostEqual(config.async_poll_interval_secs, 5.0)
        finally:
            os.unlink(config_path)


class TestKeyGeneration(unittest.TestCase):
    """Tests for S3 key and queue name generation."""

    def test_cas_key(self) -> None:
        """Test CAS key generation."""
        key: str = upload_agent.cas_key("DeadlineCloud", "abc123def456")
        self.assertEqual(key, "DeadlineCloud/Data/abc123def456.xxh128")

    def test_marker_key(self) -> None:
        """Test marker key generation."""
        key: str = upload_agent.marker_key("DeadlineCloud", "root1", "pathkey1")
        self.assertEqual(
            key, "DeadlineCloud/PendingUploads/root1/pathkey1.xxh128"
        )

    def test_queue_name_high(self) -> None:
        """Test high-priority queue name."""
        name: str = upload_agent.queue_name("farm-abc", "queue-def", "high")
        self.assertEqual(name, "deadline-farm-abc-queue-def-file-requests-high")

    def test_queue_name_async(self) -> None:
        """Test async queue name."""
        name: str = upload_agent.queue_name("farm-abc", "queue-def", "async")
        self.assertEqual(name, "deadline-farm-abc-queue-def-file-requests-async")


class TestXxh128(unittest.TestCase):
    """Tests for XXH128 hashing."""

    def test_xxh128_hex_deterministic(self) -> None:
        """Test that XXH128 is deterministic."""
        data: bytes = b"hello world"
        hash1: str = upload_agent.xxh128_hex(data)
        hash2: str = upload_agent.xxh128_hex(data)
        self.assertEqual(hash1, hash2)
        self.assertEqual(len(hash1), 32)  # 128 bits = 32 hex chars

    def test_xxh128_hex_different_inputs(self) -> None:
        """Test that different inputs produce different hashes."""
        hash1: str = upload_agent.xxh128_hex(b"hello")
        hash2: str = upload_agent.xxh128_hex(b"world")
        self.assertNotEqual(hash1, hash2)

    def test_xxh128_file(self) -> None:
        """Test file hashing."""
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(b"test file content")
            file_path: str = f.name

        try:
            file_hash: str = upload_agent.xxh128_file(Path(file_path))
            mem_hash: str = upload_agent.xxh128_hex(b"test file content")
            self.assertEqual(file_hash, mem_hash)
        finally:
            os.unlink(file_path)


class TestProcessMessage(unittest.TestCase):
    """Tests for message processing logic."""

    def _make_config(self) -> upload_agent.AgentConfig:
        """Create a test config."""
        return upload_agent.AgentConfig(
            region="us-west-2",
            bucket="test-bucket",
            root_prefix="DeadlineCloud",
            farm_id="farm-1",
            queue_id="queue-1",
            root_mappings={},
        )

    def test_invalid_json(self) -> None:
        """Test handling of invalid JSON."""
        s3_mock = MagicMock()
        config = self._make_config()
        result: bool = upload_agent.process_message(s3_mock, config, "not json")
        self.assertFalse(result)

    def test_missing_fields(self) -> None:
        """Test handling of missing required fields."""
        s3_mock = MagicMock()
        config = self._make_config()
        msg: str = json.dumps({"rootId": "root1"})  # Missing relativePath, pathKey
        result: bool = upload_agent.process_message(s3_mock, config, msg)
        self.assertFalse(result)

    def test_marker_already_exists(self) -> None:
        """Test idempotency — skip if marker exists."""
        s3_mock = MagicMock()
        s3_mock.head_object.return_value = {}  # Marker exists
        config = self._make_config()

        msg: str = json.dumps({
            "rootId": "root1",
            "relativePath": "file.txt",
            "pathKey": "abc123",
        })
        result: bool = upload_agent.process_message(s3_mock, config, msg)
        self.assertTrue(result)
        # Should not have called upload_file
        s3_mock.upload_file.assert_not_called()

    def test_file_not_found_writes_failure(self) -> None:
        """Test that missing files produce a failure marker."""
        s3_mock = MagicMock()
        # head_object raises 404 (marker doesn't exist)
        error_response: dict = {"Error": {"Code": "404"}}
        s3_mock.head_object.side_effect = s3_mock.exceptions.ClientError(
            error_response, "HeadObject"
        )
        s3_mock.exceptions.ClientError = type(
            "ClientError", (Exception,), {"response": property(lambda self: error_response)}
        )

        config = self._make_config()
        config.root_mappings = {"root1": "/nonexistent/path"}

        msg: str = json.dumps({
            "rootId": "root1",
            "relativePath": "missing.txt",
            "pathKey": "abc123",
            "sourceRootPath": "/nonexistent/path",
        })

        # Use a simpler approach — mock check_marker_exists directly
        with patch.object(upload_agent, "check_marker_exists", return_value=False):
            result: bool = upload_agent.process_message(s3_mock, config, msg)
            self.assertTrue(result)
            # Should have written a failure marker
            s3_mock.put_object.assert_called_once()
            call_kwargs: dict = s3_mock.put_object.call_args[1]
            body: dict = json.loads(call_kwargs["Body"])
            self.assertEqual(body["status"], "failed")

    def test_successful_upload(self) -> None:
        """Test successful file upload flow."""
        s3_mock = MagicMock()
        config = self._make_config()

        with tempfile.TemporaryDirectory() as tmpdir:
            # Create a test file
            test_file = Path(tmpdir) / "project" / "texture.png"
            test_file.parent.mkdir(parents=True)
            test_file.write_bytes(b"fake png data")

            config.root_mappings = {"root1": tmpdir}

            msg: str = json.dumps({
                "rootId": "root1",
                "relativePath": "project/texture.png",
                "pathKey": "abc123",
            })

            with patch.object(
                upload_agent, "check_marker_exists", return_value=False
            ):
                result: bool = upload_agent.process_message(s3_mock, config, msg)
                self.assertTrue(result)
                # Should have uploaded to CAS
                s3_mock.upload_file.assert_called_once()
                # Should have written completion marker
                s3_mock.put_object.assert_called_once()
                call_kwargs: dict = s3_mock.put_object.call_args[1]
                body: dict = json.loads(call_kwargs["Body"])
                self.assertEqual(body["status"], "completed")
                self.assertEqual(body["relativePath"], "project/texture.png")
                self.assertEqual(body["size"], 13)  # len(b"fake png data")
                self.assertEqual(len(body["contentHash"]), 32)

    def test_uses_source_root_path_fallback(self) -> None:
        """Test that sourceRootPath from message is used when no root mapping."""
        s3_mock = MagicMock()
        config = self._make_config()
        config.root_mappings = {}  # No mappings

        with tempfile.TemporaryDirectory() as tmpdir:
            test_file = Path(tmpdir) / "data.bin"
            test_file.write_bytes(b"binary data")

            msg: str = json.dumps({
                "rootId": "unknown_root",
                "relativePath": "data.bin",
                "pathKey": "def456",
                "sourceRootPath": tmpdir,
            })

            with patch.object(
                upload_agent, "check_marker_exists", return_value=False
            ):
                result: bool = upload_agent.process_message(s3_mock, config, msg)
                self.assertTrue(result)
                s3_mock.upload_file.assert_called_once()


if __name__ == "__main__":
    unittest.main()
