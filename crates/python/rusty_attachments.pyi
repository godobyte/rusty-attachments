"""Type stubs for rusty_attachments Python bindings."""

from typing import Callable, Optional

class S3Location:
    """S3 bucket and prefix configuration for CAS storage."""

    def __init__(
        self,
        bucket: str,
        root_prefix: str,
        cas_prefix: str,
        manifest_prefix: str,
    ) -> None: ...
    @property
    def bucket(self) -> str: ...
    @property
    def root_prefix(self) -> str: ...
    @property
    def cas_prefix(self) -> str: ...
    @property
    def manifest_prefix(self) -> str: ...

class ManifestLocation:
    """Location for storing/retrieving manifests."""

    def __init__(
        self,
        bucket: str,
        root_prefix: str,
        farm_id: str,
        queue_id: str,
    ) -> None: ...
    @property
    def bucket(self) -> str: ...
    @property
    def root_prefix(self) -> str: ...
    @property
    def farm_id(self) -> str: ...
    @property
    def queue_id(self) -> str: ...

class AssetReferences:
    """Asset references for job submission."""

    def __init__(
        self,
        input_filenames: list[str],
        output_directories: list[str],
        referenced_paths: Optional[list[str]] = None,
    ) -> None: ...
    @property
    def input_filenames(self) -> list[str]: ...
    @property
    def output_directories(self) -> list[str]: ...
    @property
    def referenced_paths(self) -> list[str]: ...

class BundleSubmitOptions:
    """Options for bundle submit operation."""

    def __init__(
        self,
        require_paths_exist: bool = False,
        file_system_mode: str = "COPIED",
        manifest_version: str = "v2025-12-04-beta",
        exclude_patterns: Optional[list[str]] = None,
    ) -> None: ...
    @property
    def require_paths_exist(self) -> bool: ...
    @property
    def file_system_mode(self) -> str: ...

class SummaryStatistics:
    """Summary statistics for an operation phase."""

    @property
    def processed_files(self) -> int: ...
    @property
    def processed_bytes(self) -> int: ...
    @property
    def files_transferred(self) -> int: ...
    @property
    def bytes_transferred(self) -> int: ...
    @property
    def files_skipped(self) -> int: ...
    @property
    def bytes_skipped(self) -> int: ...

class BundleSubmitResult:
    """Result of bundle submit operation."""

    @property
    def attachments_json(self) -> str:
        """Get the attachments JSON string for CreateJob API."""
        ...

    def attachments_dict(self) -> dict:
        """Get the attachments as a Python dict."""
        ...

    @property
    def hashing_stats(self) -> SummaryStatistics:
        """Get hashing phase statistics."""
        ...

    @property
    def upload_stats(self) -> SummaryStatistics:
        """Get upload phase statistics."""
        ...

class FileSystemLocation:
    """A file system location in a storage profile."""

    def __init__(
        self,
        name: str,
        path: str,
        location_type: str,
    ) -> None:
        """
        Create a new file system location.

        Args:
            name: Location name
            path: File system path
            location_type: "LOCAL" or "SHARED"
        """
        ...

    @property
    def name(self) -> str: ...
    @property
    def path(self) -> str: ...
    @property
    def location_type(self) -> str: ...

class StorageProfile:
    """Storage profile with file system locations."""

    def __init__(self, locations: list[FileSystemLocation]) -> None: ...

class Manifest:
    """Job attachment manifest."""

    def encode(self) -> str:
        """Encode the manifest to canonical JSON string."""
        ...

    @property
    def version(self) -> str: ...
    @property
    def hash_alg(self) -> str: ...
    @property
    def total_size(self) -> int: ...
    @property
    def file_count(self) -> int: ...
    def is_v2023(self) -> bool: ...
    def is_v2025(self) -> bool: ...

class AttachmentError(Exception):
    """Base exception for attachment errors."""

    ...

class StorageError(AttachmentError):
    """Storage operation error."""

    ...

class ValidationError(AttachmentError):
    """Validation error."""

    ...

class ProgressInfo:
    """Progress information passed to callbacks."""

    phase: str
    current_path: Optional[str]
    files_processed: int
    total_files: Optional[int]
    bytes_processed: int
    total_bytes: Optional[int]

async def submit_bundle_attachments_py(
    region: str,
    s3_location: S3Location,
    manifest_location: ManifestLocation,
    asset_references: AssetReferences,
    storage_profile: Optional[StorageProfile] = None,
    options: Optional[BundleSubmitOptions] = None,
    progress_callback: Optional[Callable[[ProgressInfo], bool]] = None,
) -> BundleSubmitResult:
    """
    Submit a job bundle with attachments.

    This function uploads input files to S3 CAS and returns the attachments
    JSON payload for the Deadline Cloud CreateJob API.

    Args:
        region: AWS region (e.g., "us-west-2")
        s3_location: S3Location configuration
        manifest_location: ManifestLocation configuration
        asset_references: AssetReferences with input files and output directories
        storage_profile: Optional StorageProfile for path classification
        options: Optional BundleSubmitOptions
        progress_callback: Optional callback function receiving progress dict.
            Return True to continue, False to cancel.

    Returns:
        BundleSubmitResult with attachments_json and statistics

    Raises:
        StorageError: If S3 operations fail
        ValidationError: If input validation fails

    Example:
        ```python
        result = await submit_bundle_attachments_py(
            region="us-west-2",
            s3_location=S3Location("bucket", "DeadlineCloud", "Data", "Manifests"),
            manifest_location=ManifestLocation("bucket", "DeadlineCloud", "farm-xxx", "queue-xxx"),
            asset_references=AssetReferences(["/path/to/files"], ["/path/to/outputs"]),
        )
        print(result.attachments_json)
        ```
    """
    ...

def decode_manifest(json: str) -> Manifest:
    """
    Decode a manifest from JSON string.

    Args:
        json: JSON string containing the manifest

    Returns:
        Manifest object

    Raises:
        ValueError: If JSON is invalid or not a valid manifest
    """
    ...
