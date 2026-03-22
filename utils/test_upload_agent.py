#!/usr/bin/env python3
"""Unit tests for the upload agent."""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

# Add utils to path
sys.path.insert(0, os.path.dirname(__file__))

import upload_agent


class TestStorageProfileConfig(unittest.TestCase):
    """Tests for StorageProfileConfig parsing."""

    def test_from_dict(self) -> None:
        """Test parsing storage profile from dict."""
        data: dict = {
            "fileSystemLocations": [
                {"name": "Assets", "path": "/mnt/assets", "type": "LOCAL"},
                {"name": "Shared", "path": "/mnt/shared", "type": "SHARED"},
            ]
        }
        profile = upload_agent.StorageProfileConfig.from_dict(data)
        assert len(profile.file_system_locations) == 2
        assert profile.file_system_locations[0].name == "Assets"
        assert profile.file_system_locations[0].location_type == "LOCAL"
        assert profile.file_system_locations[1].location_type == "SHARED"

    def test_local_locations(self) -> None:
        """Test filtering LOCAL locations."""
        data: dict = {
            "fileSystemLocations": [
                {"name": "A", "path": "/a", "type": "LOCAL"},
                {"name": "B", "path": "/b", "type": "SHARED"},
                {"name": "C", "path": "/c", "type": "LOCAL"},
            ]
        }
        profile = upload_agent.StorageProfileConfig.from_dict(data)
        local: list = profile.local_locations()
        assert len(local) == 2
        assert local[0].name == "A"
        assert local[1].name == "C"

    def test_find_location_by_name(self) -> None:
        """Test finding a location by name."""
        data: dict = {
            "fileSystemLocations": [
                {"name": "Assets", "path": "/mnt/assets", "type": "LOCAL"},
            ]
        }
        profile = upload_agent.StorageProfileConfig.from_dict(data)
        loc = profile.find_location_by_name("Assets")
        assert loc is not None
        assert loc.path == "/mnt/assets"
        assert profile.find_location_by_name("Unknown") is None


class TestAgentConfig(unittest.TestCase):
    """Tests for AgentConfig loading."""

    def test_from_json_with_storage_profile(self) -> None:
        """Test loading config with storage profile."""
        config_data: dict = {
            "farmId": "farm-123",
            "queueId": "queue-456",
            "region": "us-west-2",
            "bucket": "test-bucket",
            "rootPrefix": "DeadlineCloud",
            "storageProfile": {
                "fileSystemLocations": [
                    {"name": "StudioAssets", "path": "/mnt/shared/assets", "type": "LOCAL"},
                    {"name": "TextureLib", "path": "/mnt/shared/textures", "type": "LOCAL"},
                ]
            },
            "maxConcurrentUploads": 4,
        }

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".json", delete=False
        ) as f:
            json.dump(config_data, f)
            config_path: str = f.name

        try:
            config = upload_agent.AgentConfig.from_json(config_path)
            assert config.farm_id == "farm-123"
            assert config.bucket == "test-bucket"
            assert len(config.storage_profile.file_system_locations) == 2
            assert config.storage_profile.file_system_locations[0].name == "StudioAssets"
            assert config.max_concurrent_uploads == 4
        finally:
            os.unlink(config_path)

    def test_from_json_defaults(self) -> None:
        """Test that missing optional fields use defaults."""
        config_data: dict = {
            "farmId": "farm-1",
            "queueId": "queue-1",
            "bucket": "bucket",
        }

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".json", delete=False
        ) as f:
            json.dump(config_data, f)
            config_path: str = f.name

        try:
            config = upload_agent.AgentConfig.from_json(config_path)
            assert config.region == "us-west-2"
            assert config.root_prefix == "DeadlineCloud"
            assert config.max_concurrent_uploads == 8
            assert len(config.storage_profile.file_system_locations) == 0
        finally:
            os.unlink(config_path)


class TestResolveSourcePath(unittest.TestCase):
    """Tests for storage-profile-based path resolution."""

    def _make_config(self, locations: list[dict]) -> upload_agent.AgentConfig:
        """Create a config with given storage profile locations."""
        profile = upload_agent.StorageProfileConfig.from_dict(
            {"fileSystemLocations": locations}
        )
        return upload_agent.AgentConfig(
            farm_id="farm-1",
            queue_id="queue-1",
            region="us-west-2",
            bucket="bucket",
            root_prefix="DeadlineCloud",
            storage_profile=profile,
        )

    def test_resolve_exact_match(self) -> None:
        """Test resolving when source_root_path matches a LOCAL location exactly."""
        config = self._make_config([
            {"name": "Assets", "path": "/mnt/shared/assets", "type": "LOCAL"},
        ])
        result: str | None = config.resolve_source_path("root1", "/mnt/shared/assets")
        assert result == "/mnt/shared/assets"

    def test_resolve_subpath_of_local(self) -> None:
        """Test resolving when source_root_path is under a LOCAL location."""
        config = self._make_config([
            {"name": "Assets", "path": "/mnt/shared", "type": "LOCAL"},
        ])
        result: str | None = config.resolve_source_path("root1", "/mnt/shared/assets")
        assert result == "/mnt/shared/assets"

    def test_resolve_rejects_unknown_path(self) -> None:
        """Test that paths outside all LOCAL locations are rejected."""
        config = self._make_config([
            {"name": "Assets", "path": "/mnt/shared/assets", "type": "LOCAL"},
        ])
        # /mnt/other is not under any LOCAL location and doesn't exist on disk
        result: str | None = config.resolve_source_path("root1", "/mnt/other/stuff")
        assert result is None

    def test_resolve_ignores_shared_locations(self) -> None:
        """Test that SHARED locations are not used for resolution."""
        config = self._make_config([
            {"name": "SharedLib", "path": "/mnt/shared", "type": "SHARED"},
        ])
        result: str | None = config.resolve_source_path("root1", "/mnt/shared/file")
        # SHARED locations are filtered out by local_locations()
        # but resolve_source_path checks all locations for prefix match
        # The fallback checks if the directory exists
        assert result is None  # /mnt/shared/file doesn't exist on disk


class TestKeyGeneration(unittest.TestCase):
    """Tests for S3 key and queue name generation."""

    def test_cas_key(self) -> None:
        key: str = upload_agent.cas_key("DeadlineCloud", "abc123def456")
        assert key == "DeadlineCloud/Data/abc123def456.xxh128"

    def test_marker_key(self) -> None:
        key: str = upload_agent.marker_key("DeadlineCloud", "root1", "pathkey1")
        assert key == "DeadlineCloud/PendingUploads/root1/pathkey1.xxh128"

    def test_queue_name_high(self) -> None:
        name: str = upload_agent.queue_name("farm-abc", "queue-def", "high")
        assert name == "deadline-farm-abc-queue-def-file-requests-high"

    def test_queue_name_async(self) -> None:
        name: str = upload_agent.queue_name("farm-abc", "queue-def", "async")
        assert name == "deadline-farm-abc-queue-def-file-requests-async"


class TestXxh128(unittest.TestCase):
    """Tests for XXH128 hashing."""

    def test_xxh128_hex_deterministic(self) -> None:
        data: bytes = b"hello world"
        hash1: str = upload_agent.xxh128_hex(data)
        hash2: str = upload_agent.xxh128_hex(data)
        assert hash1 == hash2
        assert len(hash1) == 32

    def test_xxh128_hex_different_inputs(self) -> None:
        hash1: str = upload_agent.xxh128_hex(b"hello")
        hash2: str = upload_agent.xxh128_hex(b"world")
        assert hash1 != hash2

    def test_xxh128_file(self) -> None:
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(b"test file content")
            file_path: str = f.name
        try:
            file_hash: str = upload_agent.xxh128_file(Path(file_path))
            mem_hash: str = upload_agent.xxh128_hex(b"test file content")
            assert file_hash == mem_hash
        finally:
            os.unlink(file_path)


class TestProcessMessage(unittest.TestCase):
    """Tests for message processing with storage profile resolution."""

    def _make_config_with_profile(
        self, tmpdir: str
    ) -> upload_agent.AgentConfig:
        """Create a config with a storage profile pointing at tmpdir."""
        profile = upload_agent.StorageProfileConfig.from_dict({
            "fileSystemLocations": [
                {"name": "StudioAssets", "path": tmpdir, "type": "LOCAL"},
            ]
        })
        return upload_agent.AgentConfig(
            farm_id="farm-1",
            queue_id="queue-1",
            region="us-west-2",
            bucket="test-bucket",
            root_prefix="DeadlineCloud",
            storage_profile=profile,
        )

    def test_invalid_json(self) -> None:
        s3_mock = MagicMock()
        config = self._make_config_with_profile("/tmp")
        result: bool = upload_agent.process_message(s3_mock, config, "not json")
        assert not result

    def test_missing_fields(self) -> None:
        s3_mock = MagicMock()
        config = self._make_config_with_profile("/tmp")
        msg: str = json.dumps({"rootId": "root1"})
        result: bool = upload_agent.process_message(s3_mock, config, msg)
        assert not result

    def test_marker_already_exists(self) -> None:
        s3_mock = MagicMock()
        s3_mock.head_object.return_value = {}
        config = self._make_config_with_profile("/tmp")

        msg: str = json.dumps({
            "rootId": "root1",
            "relativePath": "file.txt",
            "pathKey": "abc123",
            "sourceRootPath": "/tmp",
        })
        result: bool = upload_agent.process_message(s3_mock, config, msg)
        assert result
        s3_mock.upload_file.assert_not_called()

    def test_source_path_not_in_profile_writes_failure(self) -> None:
        """Test that requests for paths outside the storage profile are rejected."""
        s3_mock = MagicMock()
        config = self._make_config_with_profile("/mnt/studio/assets")

        msg: str = json.dumps({
            "rootId": "root1",
            "relativePath": "file.txt",
            "pathKey": "abc123",
            "sourceRootPath": "/mnt/other/unknown",
        })

        with patch.object(upload_agent, "check_marker_exists", return_value=False):
            result: bool = upload_agent.process_message(s3_mock, config, msg)
            assert result  # Processed (failed)
            s3_mock.put_object.assert_called_once()
            call_kwargs: dict = s3_mock.put_object.call_args[1]
            body: dict = json.loads(call_kwargs["Body"])
            assert body["status"] == "failed"
            assert "storage profile" in body["reason"]

    def test_file_not_found_writes_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            s3_mock = MagicMock()
            config = self._make_config_with_profile(tmpdir)

            msg: str = json.dumps({
                "rootId": "root1",
                "relativePath": "missing.txt",
                "pathKey": "abc123",
                "sourceRootPath": tmpdir,
            })

            with patch.object(upload_agent, "check_marker_exists", return_value=False):
                result: bool = upload_agent.process_message(s3_mock, config, msg)
                assert result
                s3_mock.put_object.assert_called_once()
                call_kwargs: dict = s3_mock.put_object.call_args[1]
                body: dict = json.loads(call_kwargs["Body"])
                assert body["status"] == "failed"

    def test_successful_upload_with_storage_profile(self) -> None:
        """Test the full upload flow using storage profile path resolution."""
        with tempfile.TemporaryDirectory() as tmpdir:
            s3_mock = MagicMock()
            config = self._make_config_with_profile(tmpdir)

            # Create a test file under the storage profile root
            test_file = Path(tmpdir) / "project" / "texture.png"
            test_file.parent.mkdir(parents=True)
            test_file.write_bytes(b"fake png data")

            msg: str = json.dumps({
                "rootId": "root1",
                "relativePath": "project/texture.png",
                "pathKey": "abc123",
                "sourceRootPath": tmpdir,
            })

            with patch.object(
                upload_agent, "check_marker_exists", return_value=False
            ):
                result: bool = upload_agent.process_message(s3_mock, config, msg)
                assert result
                s3_mock.upload_file.assert_called_once()
                s3_mock.put_object.assert_called_once()
                call_kwargs: dict = s3_mock.put_object.call_args[1]
                body: dict = json.loads(call_kwargs["Body"])
                assert body["status"] == "completed"
                assert body["relativePath"] == "project/texture.png"
                assert body["size"] == 13
                assert len(body["contentHash"]) == 32

    def test_path_mapping_scenario(self) -> None:
        """Test the storage profile path mapping scenario end-to-end.

        Submitter has /mnt/shared/assets (mapped via storage profile).
        Worker VFS mounts at /mnt/worker/assets.
        SQS message carries sourceRootPath=/mnt/shared/assets.
        Agent's storage profile has LOCAL at /mnt/shared (parent of source).
        Agent resolves /mnt/shared/assets/textures/diffuse.png.
        """
        with tempfile.TemporaryDirectory() as tmpdir:
            # Simulate: agent's LOCAL location is the parent dir
            assets_dir = Path(tmpdir) / "assets"
            assets_dir.mkdir()
            tex_file = assets_dir / "textures" / "diffuse.png"
            tex_file.parent.mkdir(parents=True)
            tex_file.write_bytes(b"texture data")

            # Storage profile LOCAL covers the parent tmpdir
            profile = upload_agent.StorageProfileConfig.from_dict({
                "fileSystemLocations": [
                    {"name": "StudioRoot", "path": tmpdir, "type": "LOCAL"},
                ]
            })
            config = upload_agent.AgentConfig(
                farm_id="farm-1",
                queue_id="queue-1",
                region="us-west-2",
                bucket="bucket",
                root_prefix="DC",
                storage_profile=profile,
            )

            # SQS message from VFS: sourceRootPath is the submitter's assets dir
            # (which is a subpath of the agent's LOCAL location)
            msg: str = json.dumps({
                "rootId": "root_mapped",
                "relativePath": "textures/diffuse.png",
                "pathKey": "abc123",
                "sourceRootPath": str(assets_dir),
            })

            s3_mock = MagicMock()
            with patch.object(
                upload_agent, "check_marker_exists", return_value=False
            ):
                result: bool = upload_agent.process_message(s3_mock, config, msg)
                assert result
                # Verify the file was uploaded
                s3_mock.upload_file.assert_called_once()
                upload_call_args = s3_mock.upload_file.call_args[0]
                # First arg is the local file path
                assert upload_call_args[0] == str(tex_file)


if __name__ == "__main__":
    unittest.main()
