"""Tests for rusty_attachments Python bindings."""

import pytest


def test_import():
    """Test that the module can be imported."""
    import rusty_attachments

    assert hasattr(rusty_attachments, "S3Location")
    assert hasattr(rusty_attachments, "ManifestLocation")
    assert hasattr(rusty_attachments, "AssetReferences")
    assert hasattr(rusty_attachments, "BundleSubmitOptions")
    assert hasattr(rusty_attachments, "submit_bundle_attachments_py")


def test_s3_location():
    """Test S3Location class."""
    from rusty_attachments import S3Location

    loc = S3Location(
        bucket="my-bucket",
        root_prefix="DeadlineCloud",
        cas_prefix="Data",
        manifest_prefix="Manifests",
    )

    assert loc.bucket == "my-bucket"
    assert loc.root_prefix == "DeadlineCloud"
    assert loc.cas_prefix == "Data"
    assert loc.manifest_prefix == "Manifests"
    assert "S3Location" in repr(loc)


def test_manifest_location():
    """Test ManifestLocation class."""
    from rusty_attachments import ManifestLocation

    loc = ManifestLocation(
        bucket="my-bucket",
        root_prefix="DeadlineCloud",
        farm_id="farm-123",
        queue_id="queue-456",
    )

    assert loc.bucket == "my-bucket"
    assert loc.root_prefix == "DeadlineCloud"
    assert loc.farm_id == "farm-123"
    assert loc.queue_id == "queue-456"
    assert "ManifestLocation" in repr(loc)


def test_asset_references():
    """Test AssetReferences class."""
    from rusty_attachments import AssetReferences

    refs = AssetReferences(
        input_filenames=["/path/to/file1.txt", "/path/to/file2.txt"],
        output_directories=["/path/to/output"],
        referenced_paths=["/path/to/ref"],
    )

    assert refs.input_filenames == ["/path/to/file1.txt", "/path/to/file2.txt"]
    assert refs.output_directories == ["/path/to/output"]
    assert refs.referenced_paths == ["/path/to/ref"]


def test_asset_references_default_referenced():
    """Test AssetReferences with default referenced_paths."""
    from rusty_attachments import AssetReferences

    refs = AssetReferences(
        input_filenames=["/path/to/file.txt"],
        output_directories=[],
    )

    assert refs.referenced_paths == []


def test_bundle_submit_options_defaults():
    """Test BundleSubmitOptions with defaults."""
    from rusty_attachments import BundleSubmitOptions

    opts = BundleSubmitOptions()

    assert opts.require_paths_exist is False
    assert opts.file_system_mode == "COPIED"


def test_bundle_submit_options_custom():
    """Test BundleSubmitOptions with custom values."""
    from rusty_attachments import BundleSubmitOptions

    opts = BundleSubmitOptions(
        require_paths_exist=True,
        file_system_mode="VIRTUAL",
        manifest_version="v2023-03-03",
        exclude_patterns=["**/*.tmp"],
    )

    assert opts.require_paths_exist is True
    assert opts.file_system_mode == "VIRTUAL"


def test_bundle_submit_options_invalid_version():
    """Test BundleSubmitOptions with invalid manifest version."""
    from rusty_attachments import BundleSubmitOptions

    with pytest.raises(ValueError, match="Invalid manifest version"):
        BundleSubmitOptions(manifest_version="invalid")


def test_file_system_location():
    """Test FileSystemLocation class."""
    from rusty_attachments import FileSystemLocation

    loc = FileSystemLocation(
        name="ProjectFiles",
        path="/projects",
        location_type="LOCAL",
    )

    assert loc.name == "ProjectFiles"
    assert loc.path == "/projects"
    assert loc.location_type == "LOCAL"


def test_file_system_location_shared():
    """Test FileSystemLocation with SHARED type."""
    from rusty_attachments import FileSystemLocation

    loc = FileSystemLocation(
        name="SharedAssets",
        path="/mnt/shared",
        location_type="SHARED",
    )

    assert loc.location_type == "SHARED"


def test_file_system_location_invalid_type():
    """Test FileSystemLocation with invalid type."""
    from rusty_attachments import FileSystemLocation

    with pytest.raises(ValueError, match="Invalid location type"):
        FileSystemLocation(name="Test", path="/test", location_type="INVALID")


def test_storage_profile():
    """Test StorageProfile class."""
    from rusty_attachments import FileSystemLocation, StorageProfile

    profile = StorageProfile(
        locations=[
            FileSystemLocation("Local", "/local", "LOCAL"),
            FileSystemLocation("Shared", "/shared", "SHARED"),
        ]
    )

    assert "StorageProfile" in repr(profile)


def test_exceptions():
    """Test that exceptions are defined."""
    from rusty_attachments import AttachmentError, StorageError, ValidationError

    assert issubclass(StorageError, AttachmentError)
    assert issubclass(ValidationError, AttachmentError)
    assert issubclass(AttachmentError, Exception)


def test_decode_manifest_invalid():
    """Test decode_manifest with invalid JSON."""
    from rusty_attachments import decode_manifest

    with pytest.raises(ValueError):
        decode_manifest("not valid json")


def test_decode_manifest_valid_v2023():
    """Test decode_manifest with valid v2023 manifest."""
    from rusty_attachments import decode_manifest

    manifest_json = """{
        "hashAlg": "xxh128",
        "manifestVersion": "2023-03-03",
        "paths": [
            {"path": "file.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
        ],
        "totalSize": 100
    }"""

    manifest = decode_manifest(manifest_json)
    assert manifest.version == "2023-03-03"
    assert manifest.hash_alg == "xxh128"
    assert manifest.total_size == 100
    assert manifest.file_count == 1
    assert manifest.is_v2023() is True
    assert manifest.is_v2025() is False


def test_manifest_encode():
    """Test manifest encode round-trip."""
    from rusty_attachments import decode_manifest

    manifest_json = """{
        "hashAlg": "xxh128",
        "manifestVersion": "2023-03-03",
        "paths": [
            {"path": "file.txt", "hash": "abc123", "size": 100, "mtime": 1234567890}
        ],
        "totalSize": 100
    }"""

    manifest = decode_manifest(manifest_json)
    encoded = manifest.encode()

    # Re-decode to verify
    manifest2 = decode_manifest(encoded)
    assert manifest2.version == manifest.version
    assert manifest2.file_count == manifest.file_count
