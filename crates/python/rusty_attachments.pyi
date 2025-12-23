"""
rusty_attachments - Python bindings for job attachment operations.

This module provides high-performance Python bindings for the Rust job attachments
library, enabling efficient file hashing, S3 uploads, and manifest management for
AWS Deadline Cloud job submissions.

Example:
    Basic job submission workflow::

        import asyncio
        from rusty_attachments import (
            S3Location,
            ManifestLocation,
            AssetReferences,
            submit_bundle_attachments_py,
        )

        async def submit():
            result = await submit_bundle_attachments_py(
                region="us-west-2",
                s3_location=S3Location("bucket", "DeadlineCloud", "Data", "Manifests"),
                manifest_location=ManifestLocation("bucket", "DeadlineCloud", "farm-xxx", "queue-xxx"),
                asset_references=AssetReferences(["/path/to/files"], ["/path/to/outputs"]),
            )
            print(result.attachments_json)

        asyncio.run(submit())
"""

from typing import Callable, Optional


class S3Location:
    """
    S3 bucket and prefix configuration for Content-Addressable Storage (CAS).

    This class configures where job attachment data and manifests are stored in S3.
    The structure follows the Deadline Cloud conventions.

    Attributes:
        bucket: S3 bucket name.
        root_prefix: Root prefix for all operations (e.g., "DeadlineCloud").
        cas_prefix: Prefix for CAS data objects (e.g., "Data").
        manifest_prefix: Prefix for manifest files (e.g., "Manifests").

    Example:
        >>> loc = S3Location(
        ...     bucket="my-deadline-bucket",
        ...     root_prefix="DeadlineCloud",
        ...     cas_prefix="Data",
        ...     manifest_prefix="Manifests",
        ... )
        >>> loc.bucket
        'my-deadline-bucket'
    """

    def __init__(
        self,
        bucket: str,
        root_prefix: str,
        cas_prefix: str,
        manifest_prefix: str,
    ) -> None:
        """
        Create a new S3 location configuration.

        Args:
            bucket: S3 bucket name.
            root_prefix: Root prefix for all operations (e.g., "DeadlineCloud").
            cas_prefix: CAS data prefix (e.g., "Data").
            manifest_prefix: Manifest prefix (e.g., "Manifests").
        """
        ...

    @property
    def bucket(self) -> str:
        """S3 bucket name."""
        ...

    @property
    def root_prefix(self) -> str:
        """Root prefix for all operations."""
        ...

    @property
    def cas_prefix(self) -> str:
        """CAS data prefix."""
        ...

    @property
    def manifest_prefix(self) -> str:
        """Manifest prefix."""
        ...


class ManifestLocation:
    """
    Location for storing and retrieving job attachment manifests.

    This class identifies where manifests are stored within the S3 bucket,
    scoped to a specific farm and queue.

    Attributes:
        bucket: S3 bucket name.
        root_prefix: Root prefix for all operations.
        farm_id: Deadline Cloud farm ID.
        queue_id: Deadline Cloud queue ID.

    Example:
        >>> loc = ManifestLocation(
        ...     bucket="my-bucket",
        ...     root_prefix="DeadlineCloud",
        ...     farm_id="farm-01234567890123456789012345678901",
        ...     queue_id="queue-01234567890123456789012345678901",
        ... )
    """

    def __init__(
        self,
        bucket: str,
        root_prefix: str,
        farm_id: str,
        queue_id: str,
    ) -> None:
        """
        Create a new manifest location.

        Args:
            bucket: S3 bucket name.
            root_prefix: Root prefix for all operations.
            farm_id: Deadline Cloud farm ID.
            queue_id: Deadline Cloud queue ID.
        """
        ...

    @property
    def bucket(self) -> str:
        """S3 bucket name."""
        ...

    @property
    def root_prefix(self) -> str:
        """Root prefix for all operations."""
        ...

    @property
    def farm_id(self) -> str:
        """Deadline Cloud farm ID."""
        ...

    @property
    def queue_id(self) -> str:
        """Deadline Cloud queue ID."""
        ...


class AssetReferences:
    """
    Asset references for job submission.

    This class defines the input files to upload, output directories to track,
    and referenced paths that may not exist on the submitting machine.

    Attributes:
        input_filenames: List of input file/directory paths to upload.
        output_directories: List of output directory paths to track.
        referenced_paths: List of paths that may not exist.

    Example:
        >>> refs = AssetReferences(
        ...     input_filenames=["/projects/job/scene.blend", "/projects/job/textures"],
        ...     output_directories=["/projects/job/renders"],
        ... )

    Note:
        Directories in ``input_filenames`` are automatically expanded to include
        all files within them recursively.
    """

    def __init__(
        self,
        input_filenames: list[str],
        output_directories: list[str],
        referenced_paths: Optional[list[str]] = None,
    ) -> None:
        """
        Create new asset references.

        Args:
            input_filenames: List of input file/directory paths to upload.
                Directories are expanded recursively.
            output_directories: List of output directory paths to track.
                These are not uploaded but recorded in the manifest.
            referenced_paths: List of paths that may not exist on the
                submitting machine. Defaults to empty list.
        """
        ...

    @property
    def input_filenames(self) -> list[str]:
        """List of input file/directory paths."""
        ...

    @property
    def output_directories(self) -> list[str]:
        """List of output directory paths."""
        ...

    @property
    def referenced_paths(self) -> list[str]:
        """List of referenced paths that may not exist."""
        ...


class BundleSubmitOptions:
    """
    Options for bundle submit operation.

    This class configures how the bundle submission behaves, including
    validation strictness, file system mode, and file filtering.

    Attributes:
        require_paths_exist: Whether to error on missing input files.
        file_system_mode: "COPIED" or "VIRTUAL".

    Example:
        >>> opts = BundleSubmitOptions(
        ...     require_paths_exist=True,
        ...     file_system_mode="COPIED",
        ...     exclude_patterns=["**/*.tmp", "**/__pycache__/**"],
        ... )
    """

    def __init__(
        self,
        require_paths_exist: bool = False,
        file_system_mode: str = "COPIED",
        manifest_version: str = "v2025-12-04-beta",
        exclude_patterns: Optional[list[str]] = None,
    ) -> None:
        """
        Create new bundle submit options.

        Args:
            require_paths_exist: If True, raise an error when input files
                don't exist. If False, treat missing files as references.
                Defaults to False.
            file_system_mode: How workers access files. Either "COPIED"
                (files are copied to worker) or "VIRTUAL" (files accessed
                via virtual file system). Defaults to "COPIED".
            manifest_version: Manifest format version. Either "v2023-03-03"
                or "v2025-12-04-beta". Defaults to "v2025-12-04-beta".
            exclude_patterns: Glob patterns for files to exclude from upload.
                Example: ``["**/*.tmp", "**/__pycache__/**"]``.

        Raises:
            ValueError: If manifest_version is invalid.
            ValueError: If exclude_patterns contains invalid glob syntax.
        """
        ...

    @property
    def require_paths_exist(self) -> bool:
        """Whether to error on missing input files."""
        ...

    @property
    def file_system_mode(self) -> str:
        """File system mode: "COPIED" or "VIRTUAL"."""
        ...


class SummaryStatistics:
    """
    Summary statistics for an operation phase.

    This class provides metrics about file processing during hashing
    or upload operations.

    Attributes:
        processed_files: Total number of files processed.
        processed_bytes: Total bytes processed.
        files_transferred: Number of files actually transferred.
        bytes_transferred: Bytes actually transferred.
        files_skipped: Number of files skipped (already existed).
        bytes_skipped: Bytes skipped.
    """

    @property
    def processed_files(self) -> int:
        """Total number of files processed."""
        ...

    @property
    def processed_bytes(self) -> int:
        """Total bytes processed."""
        ...

    @property
    def files_transferred(self) -> int:
        """Number of files actually transferred."""
        ...

    @property
    def bytes_transferred(self) -> int:
        """Bytes actually transferred."""
        ...

    @property
    def files_skipped(self) -> int:
        """Number of files skipped (already existed in CAS)."""
        ...

    @property
    def bytes_skipped(self) -> int:
        """Bytes skipped."""
        ...


class BundleSubmitResult:
    """
    Result of bundle submit operation.

    This class contains the attachments JSON payload for the CreateJob API
    and statistics about the hashing and upload operations.

    Attributes:
        attachments_json: JSON string for CreateJob API attachments field.
        hashing_stats: Statistics from the file hashing phase.
        upload_stats: Statistics from the S3 upload phase.

    Example:
        >>> result = await submit_bundle_attachments_py(...)
        >>> print(result.attachments_json)
        {"manifests": [...], "fileSystem": "COPIED"}
        >>> print(f"Uploaded {result.upload_stats.files_transferred} files")
    """

    @property
    def attachments_json(self) -> str:
        """
        Get the attachments JSON string for CreateJob API.

        This JSON string should be passed to the ``attachments`` parameter
        of the Deadline Cloud CreateJob API.

        Returns:
            JSON string containing the attachments payload.
        """
        ...

    def attachments_dict(self) -> dict:
        """
        Get the attachments as a Python dictionary.

        Returns:
            Dictionary representation of the attachments payload.
        """
        ...

    @property
    def hashing_stats(self) -> SummaryStatistics:
        """Statistics from the file hashing phase."""
        ...

    @property
    def upload_stats(self) -> SummaryStatistics:
        """Statistics from the S3 upload phase."""
        ...


class FileSystemLocation:
    """
    A file system location in a storage profile.

    Storage profiles define how paths are classified for job attachments.
    LOCAL paths are uploaded with the job, while SHARED paths are assumed
    to be accessible to workers and are skipped.

    Attributes:
        name: Human-readable location name.
        path: File system path.
        location_type: "LOCAL" or "SHARED".

    Example:
        >>> loc = FileSystemLocation(
        ...     name="ProjectFiles",
        ...     path="/projects",
        ...     location_type="LOCAL",
        ... )
    """

    def __init__(
        self,
        name: str,
        path: str,
        location_type: str,
    ) -> None:
        """
        Create a new file system location.

        Args:
            name: Human-readable location name.
            path: File system path (absolute).
            location_type: Either "LOCAL" (files uploaded) or "SHARED"
                (files skipped, assumed accessible to workers).

        Raises:
            ValueError: If location_type is not "LOCAL" or "SHARED".
        """
        ...

    @property
    def name(self) -> str:
        """Human-readable location name."""
        ...

    @property
    def path(self) -> str:
        """File system path."""
        ...

    @property
    def location_type(self) -> str:
        """Location type: "LOCAL" or "SHARED"."""
        ...


class StorageProfile:
    """
    Storage profile with file system locations.

    A storage profile defines how paths are classified during job submission.
    Files under LOCAL locations are uploaded, while files under SHARED
    locations are skipped (assumed to be on shared storage accessible to workers).

    Example:
        >>> profile = StorageProfile(locations=[
        ...     FileSystemLocation("Projects", "/projects", "LOCAL"),
        ...     FileSystemLocation("SharedAssets", "/mnt/shared", "SHARED"),
        ... ])
    """

    def __init__(self, locations: list[FileSystemLocation]) -> None:
        """
        Create a new storage profile.

        Args:
            locations: List of FileSystemLocation objects defining
                how paths should be classified.
        """
        ...


class Manifest:
    """
    Job attachment manifest.

    A manifest describes the files in a job attachment, including their
    paths, hashes, sizes, and modification times.

    Attributes:
        version: Manifest format version string.
        hash_alg: Hash algorithm used (e.g., "xxh128").
        total_size: Total size of all files in bytes.
        file_count: Number of files in the manifest.
    """

    def encode(self) -> str:
        """
        Encode the manifest to canonical JSON string.

        Returns:
            JSON string representation of the manifest.

        Raises:
            ValueError: If encoding fails.
        """
        ...

    @property
    def version(self) -> str:
        """Manifest format version string (e.g., "2023-03-03")."""
        ...

    @property
    def hash_alg(self) -> str:
        """Hash algorithm used (e.g., "xxh128")."""
        ...

    @property
    def total_size(self) -> int:
        """Total size of all files in bytes."""
        ...

    @property
    def file_count(self) -> int:
        """Number of files in the manifest."""
        ...

    def is_v2023(self) -> bool:
        """
        Check if this is a v2023-03-03 format manifest.

        Returns:
            True if manifest version is "2023-03-03".
        """
        ...

    def is_v2025(self) -> bool:
        """
        Check if this is a v2025-12-04-beta format manifest.

        Returns:
            True if manifest version is "2025-12-04-beta".
        """
        ...


class AttachmentError(Exception):
    """
    Base exception for all attachment-related errors.

    This is the parent class for all exceptions raised by the
    rusty_attachments module.
    """

    ...


class StorageError(AttachmentError):
    """
    Exception raised for S3 storage operation failures.

    This exception is raised when S3 operations fail, such as
    upload failures, permission errors, or network issues.
    """

    ...


class ValidationError(AttachmentError):
    """
    Exception raised for input validation failures.

    This exception is raised when input validation fails, such as
    missing required files or invalid path configurations.
    """

    ...


class ProgressInfo:
    """
    Progress information passed to callbacks.

    This is a dictionary-like object passed to progress callbacks
    during hashing and upload operations.

    Attributes:
        phase: Current operation phase ("Walking", "Hashing", "Building", "Complete").
        current_path: Path of the file currently being processed, or None.
        files_processed: Number of files processed so far.
        total_files: Total number of files to process, or None if unknown.
        bytes_processed: Bytes processed so far.
        total_bytes: Total bytes to process, or None if unknown.
    """

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

    This async function uploads input files to S3 Content-Addressable Storage
    and returns the attachments JSON payload for the Deadline Cloud CreateJob API.

    The function performs the following steps:

    1. Expands input directories to individual files
    2. Groups paths by asset root, respecting storage profile
    3. Hashes files and creates manifests
    4. Uploads file contents to S3 CAS (skipping duplicates)
    5. Uploads manifest JSON to S3
    6. Returns attachments payload for CreateJob API

    Args:
        region: AWS region (e.g., "us-west-2").
        s3_location: S3Location configuration for CAS storage.
        manifest_location: ManifestLocation for manifest storage.
        asset_references: AssetReferences defining input files and outputs.
        storage_profile: Optional StorageProfile for path classification.
            Files under SHARED locations are skipped.
        options: Optional BundleSubmitOptions for customization.
        progress_callback: Optional callback function receiving progress info.
            The callback receives a dict with progress information and should
            return True to continue or False to cancel the operation.

    Returns:
        BundleSubmitResult containing:
            - ``attachments_json``: JSON string for CreateJob API
            - ``hashing_stats``: Statistics from hashing phase
            - ``upload_stats``: Statistics from upload phase

    Raises:
        StorageError: If S3 operations fail.
        ValidationError: If input validation fails.
        RuntimeError: If client creation fails.

    Example:
        Basic usage::

            result = await submit_bundle_attachments_py(
                region="us-west-2",
                s3_location=S3Location("bucket", "DeadlineCloud", "Data", "Manifests"),
                manifest_location=ManifestLocation("bucket", "DeadlineCloud", "farm-xxx", "queue-xxx"),
                asset_references=AssetReferences(["/path/to/files"], ["/path/to/outputs"]),
            )

            # Use in CreateJob API
            deadline_client.create_job(
                farmId="farm-xxx",
                queueId="queue-xxx",
                template=job_template,
                attachments=result.attachments_json,
            )

        With progress callback::

            def on_progress(info: dict) -> bool:
                print(f"{info['phase']}: {info['files_processed']}/{info['total_files']}")
                return True  # Continue

            result = await submit_bundle_attachments_py(
                ...,
                progress_callback=on_progress,
            )

        With storage profile::

            profile = StorageProfile([
                FileSystemLocation("Local", "/projects", "LOCAL"),
                FileSystemLocation("Shared", "/mnt/nfs", "SHARED"),
            ])

            result = await submit_bundle_attachments_py(
                ...,
                storage_profile=profile,
            )
    """
    ...


def decode_manifest(json: str) -> Manifest:
    """
    Decode a manifest from JSON string.

    Parses a JSON string containing a job attachment manifest and returns
    a Manifest object.

    Args:
        json: JSON string containing the manifest.

    Returns:
        Manifest object representing the parsed manifest.

    Raises:
        ValueError: If JSON is invalid or not a valid manifest format.

    Example:
        >>> manifest = decode_manifest('{"hashAlg": "xxh128", ...}')
        >>> print(manifest.version)
        '2023-03-03'
        >>> print(manifest.file_count)
        42
    """
    ...
