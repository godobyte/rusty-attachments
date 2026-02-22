//! Python bindings for rusty-attachments.

#![allow(unused_variables)] // Binding functions accept params for API compatibility that Rust doesn't use yet

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use ja_deadline_utils::{
    self as ja, submit_bundle_attachments, AssetReferences, BundleSubmitOptions,
};
use rusty_attachments_common::ProgressCallback;
use rusty_attachments_filesystem::{
    DiffEngine, DiffMode, DiffOptions, DiffResult, FileSystemScanner, GlobFilter, ScanProgress,
    SnapshotOptions,
};
use rusty_attachments_model::{self as model, HashAlgorithm, Manifest, ManifestVersion, merge_manifests};
use rusty_attachments_profiles::{FileSystemLocation, FileSystemLocationType, StorageProfile};
use rusty_attachments_storage::{
    ConflictResolution, DownloadOrchestrator, ManifestLocation, S3CheckCache, S3Location,
    SqliteS3CheckCache, StorageClient, StorageSettings, TransferProgress, TransferStatistics,
    UploadOrchestrator,
    manifest_storage::{
        self, ManifestUploadResult, OutputManifestDiscoveryOptions,
        OutputManifestScope as RustOutputManifestScope, download_manifest,
        upload_input_manifest,
    },
};
use rusty_attachments_storage_crt::DefaultClient;

// ============================================================================
// Exceptions
// ============================================================================

pyo3::create_exception!(
    rusty_attachments,
    AttachmentError,
    pyo3::exceptions::PyException
);
pyo3::create_exception!(rusty_attachments, StorageError, AttachmentError);
pyo3::create_exception!(rusty_attachments, ValidationError, AttachmentError);

// ============================================================================
// S3Location
// ============================================================================

/// S3 bucket and prefix configuration for CAS storage.
#[pyclass(name = "S3Location")]
#[derive(Clone)]
struct PyS3Location {
    inner: S3Location,
}

#[pymethods]
impl PyS3Location {
    /// Create a new S3 location configuration.
    ///
    /// Args:
    ///     bucket: S3 bucket name
    ///     root_prefix: Root prefix for all operations (e.g., "DeadlineCloud")
    ///     cas_prefix: CAS data prefix (e.g., "Data")
    ///     manifest_prefix: Manifest prefix (e.g., "Manifests")
    #[new]
    fn new(
        bucket: String,
        root_prefix: String,
        cas_prefix: String,
        manifest_prefix: String,
    ) -> Self {
        Self {
            inner: S3Location::new(bucket, root_prefix, cas_prefix, manifest_prefix),
        }
    }

    #[getter]
    fn bucket(&self) -> &str {
        &self.inner.bucket
    }

    #[getter]
    fn root_prefix(&self) -> &str {
        &self.inner.root_prefix
    }

    #[getter]
    fn cas_prefix(&self) -> &str {
        &self.inner.cas_prefix
    }

    #[getter]
    fn manifest_prefix(&self) -> &str {
        &self.inner.manifest_prefix
    }

    fn __repr__(&self) -> String {
        format!(
            "S3Location(bucket='{}', root_prefix='{}', cas_prefix='{}', manifest_prefix='{}')",
            self.inner.bucket,
            self.inner.root_prefix,
            self.inner.cas_prefix,
            self.inner.manifest_prefix
        )
    }
}

// ============================================================================
// ManifestLocation
// ============================================================================

/// Location for storing/retrieving manifests.
#[pyclass(name = "ManifestLocation")]
#[derive(Clone)]
struct PyManifestLocation {
    inner: ManifestLocation,
}

#[pymethods]
impl PyManifestLocation {
    /// Create a new manifest location.
    ///
    /// Args:
    ///     bucket: S3 bucket name
    ///     root_prefix: Root prefix for all operations
    ///     farm_id: Farm ID
    ///     queue_id: Queue ID
    #[new]
    fn new(bucket: String, root_prefix: String, farm_id: String, queue_id: String) -> Self {
        Self {
            inner: ManifestLocation::new(bucket, root_prefix, farm_id, queue_id),
        }
    }

    #[getter]
    fn bucket(&self) -> &str {
        &self.inner.bucket
    }

    #[getter]
    fn root_prefix(&self) -> &str {
        &self.inner.root_prefix
    }

    #[getter]
    fn farm_id(&self) -> &str {
        &self.inner.farm_id
    }

    #[getter]
    fn queue_id(&self) -> &str {
        &self.inner.queue_id
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestLocation(bucket='{}', root_prefix='{}', farm_id='{}', queue_id='{}')",
            self.inner.bucket, self.inner.root_prefix, self.inner.farm_id, self.inner.queue_id
        )
    }
}

// ============================================================================
// AssetReferences
// ============================================================================

/// Asset references for job submission.
#[pyclass(name = "AssetReferences")]
#[derive(Clone)]
struct PyAssetReferences {
    inner: AssetReferences,
}

#[pymethods]
impl PyAssetReferences {
    /// Create new asset references.
    ///
    /// Args:
    ///     input_filenames: List of input file/directory paths to upload
    ///     output_directories: List of output directory paths to track
    ///     referenced_paths: List of paths that may not exist (optional)
    #[new]
    #[pyo3(signature = (input_filenames, output_directories, referenced_paths=None))]
    fn new(
        input_filenames: Vec<String>,
        output_directories: Vec<String>,
        referenced_paths: Option<Vec<String>>,
    ) -> Self {
        Self {
            inner: AssetReferences {
                input_filenames: input_filenames.into_iter().map(PathBuf::from).collect(),
                output_directories: output_directories.into_iter().map(PathBuf::from).collect(),
                referenced_paths: referenced_paths
                    .unwrap_or_default()
                    .into_iter()
                    .map(PathBuf::from)
                    .collect(),
            },
        }
    }

    #[getter]
    fn input_filenames(&self) -> Vec<String> {
        self.inner
            .input_filenames
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    #[getter]
    fn output_directories(&self) -> Vec<String> {
        self.inner
            .output_directories
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    #[getter]
    fn referenced_paths(&self) -> Vec<String> {
        self.inner
            .referenced_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "AssetReferences(input_filenames={:?}, output_directories={:?}, referenced_paths={:?})",
            self.input_filenames(),
            self.output_directories(),
            self.referenced_paths()
        )
    }
}

// ============================================================================
// BundleSubmitOptions
// ============================================================================

/// Options for bundle submit operation.
#[pyclass(name = "BundleSubmitOptions")]
#[derive(Clone)]
struct PyBundleSubmitOptions {
    inner: BundleSubmitOptions,
}

#[pymethods]
impl PyBundleSubmitOptions {
    /// Create new bundle submit options.
    ///
    /// Args:
    ///     require_paths_exist: If True, error on missing input files. If False, treat as references.
    ///     file_system_mode: "COPIED" or "VIRTUAL"
    ///     manifest_version: "v2023-03-03" or "v2025-12"
    ///     exclude_patterns: Glob patterns to exclude (e.g., ["**/*.tmp", "**/__pycache__/**"])
    #[new]
    #[pyo3(signature = (require_paths_exist=false, file_system_mode="COPIED", manifest_version="v2025-12", exclude_patterns=None))]
    fn new(
        require_paths_exist: bool,
        file_system_mode: &str,
        manifest_version: &str,
        exclude_patterns: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let version: ManifestVersion = match manifest_version {
            "v2023-03-03" => ManifestVersion::V2023_03_03,
            "v2025-12" => ManifestVersion::V2025_12,
            _ => {
                return Err(PyValueError::new_err(format!(
                    "Invalid manifest version: {}. Use 'v2023-03-03' or 'v2025-12-04-beta'",
                    manifest_version
                )))
            }
        };

        let glob_filter: Option<GlobFilter> =
            if let Some(patterns) = exclude_patterns {
                if patterns.is_empty() {
                    None
                } else {
                    Some(GlobFilter::exclude(patterns).map_err(|e| {
                        PyValueError::new_err(format!("Invalid glob pattern: {}", e))
                    })?)
                }
            } else {
                None
            };

        Ok(Self {
            inner: BundleSubmitOptions {
                require_paths_exist,
                file_system_mode: file_system_mode.to_string(),
                glob_filter,
                manifest_version: version,
                ..Default::default()
            },
        })
    }

    #[getter]
    fn require_paths_exist(&self) -> bool {
        self.inner.require_paths_exist
    }

    #[getter]
    fn file_system_mode(&self) -> &str {
        &self.inner.file_system_mode
    }

    fn __repr__(&self) -> String {
        format!(
            "BundleSubmitOptions(require_paths_exist={}, file_system_mode='{}')",
            self.inner.require_paths_exist, self.inner.file_system_mode
        )
    }
}

// ============================================================================
// SummaryStatistics
// ============================================================================

/// Summary statistics for an operation phase.
#[pyclass(name = "SummaryStatistics")]
#[derive(Clone)]
struct PySummaryStatistics {
    inner: ja::SummaryStatistics,
}

#[pymethods]
impl PySummaryStatistics {
    #[getter]
    fn processed_files(&self) -> u64 {
        self.inner.processed_files
    }

    #[getter]
    fn processed_bytes(&self) -> u64 {
        self.inner.processed_bytes
    }

    #[getter]
    fn files_transferred(&self) -> u64 {
        self.inner.files_transferred
    }

    #[getter]
    fn bytes_transferred(&self) -> u64 {
        self.inner.bytes_transferred
    }

    #[getter]
    fn files_skipped(&self) -> u64 {
        self.inner.files_skipped
    }

    #[getter]
    fn bytes_skipped(&self) -> u64 {
        self.inner.bytes_skipped
    }

    fn __repr__(&self) -> String {
        format!(
            "SummaryStatistics(processed_files={}, bytes_transferred={}, files_skipped={})",
            self.inner.processed_files, self.inner.bytes_transferred, self.inner.files_skipped
        )
    }
}

// ============================================================================
// BundleSubmitResult
// ============================================================================

/// Result of bundle submit operation.
#[pyclass(name = "BundleSubmitResult")]
struct PyBundleSubmitResult {
    attachments_json: String,
    hashing_stats: ja::SummaryStatistics,
    upload_stats: ja::SummaryStatistics,
}

#[pymethods]
impl PyBundleSubmitResult {
    /// Get the attachments JSON string for CreateJob API.
    #[getter]
    fn attachments_json(&self) -> &str {
        &self.attachments_json
    }

    /// Get the attachments as a Python dict.
    fn attachments_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let json_module = py.import_bound("json")?;
        let dict = json_module.call_method1("loads", (&self.attachments_json,))?;
        dict.extract()
    }

    /// Get hashing phase statistics.
    #[getter]
    fn hashing_stats(&self) -> PySummaryStatistics {
        PySummaryStatistics {
            inner: self.hashing_stats.clone(),
        }
    }

    /// Get upload phase statistics.
    #[getter]
    fn upload_stats(&self) -> PySummaryStatistics {
        PySummaryStatistics {
            inner: self.upload_stats.clone(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "BundleSubmitResult(hashing_stats={:?}, upload_stats={:?})",
            self.hashing_stats, self.upload_stats
        )
    }
}

// ============================================================================
// FileSystemLocation
// ============================================================================

/// A file system location in a storage profile.
#[pyclass(name = "FileSystemLocation")]
#[derive(Clone)]
struct PyFileSystemLocation {
    inner: FileSystemLocation,
}

#[pymethods]
impl PyFileSystemLocation {
    /// Create a new file system location.
    ///
    /// Args:
    ///     name: Location name
    ///     path: File system path
    ///     location_type: "LOCAL" or "SHARED"
    #[new]
    fn new(name: String, path: String, location_type: &str) -> PyResult<Self> {
        let loc_type: FileSystemLocationType = match location_type.to_uppercase().as_str() {
            "LOCAL" => FileSystemLocationType::Local,
            "SHARED" => FileSystemLocationType::Shared,
            _ => {
                return Err(PyValueError::new_err(format!(
                    "Invalid location type: {}. Use 'LOCAL' or 'SHARED'",
                    location_type
                )))
            }
        };

        Ok(Self {
            inner: FileSystemLocation {
                name,
                path,
                location_type: loc_type,
            },
        })
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn path(&self) -> &str {
        &self.inner.path
    }

    #[getter]
    fn location_type(&self) -> &str {
        match self.inner.location_type {
            FileSystemLocationType::Local => "LOCAL",
            FileSystemLocationType::Shared => "SHARED",
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "FileSystemLocation(name='{}', path='{}', location_type='{}')",
            self.inner.name,
            self.inner.path,
            self.location_type()
        )
    }
}

// ============================================================================
// StorageProfile
// ============================================================================

/// Storage profile with file system locations.
#[pyclass(name = "StorageProfile")]
#[derive(Clone)]
struct PyStorageProfile {
    inner: StorageProfile,
}

#[pymethods]
impl PyStorageProfile {
    /// Create a new storage profile.
    ///
    /// Args:
    ///     locations: List of FileSystemLocation objects
    #[new]
    fn new(locations: Vec<PyFileSystemLocation>) -> Self {
        let locs: Vec<FileSystemLocation> = locations.into_iter().map(|l| l.inner).collect();
        Self {
            inner: StorageProfile::with_locations(locs),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "StorageProfile(locations={})",
            self.inner.local_locations().len() + self.inner.shared_locations().len()
        )
    }
}

// ============================================================================
// Progress Callback
// ============================================================================

/// Python progress callback wrapper.
struct PyProgressCallback {
    callback: Arc<PyObject>,
}

impl ProgressCallback<ScanProgress> for PyProgressCallback {
    fn on_progress(&self, progress: &ScanProgress) -> bool {
        Python::with_gil(|py| {
            let dict = PyDict::new_bound(py);
            let _ = dict.set_item("phase", format!("{:?}", progress.phase));
            let _ = dict.set_item("current_path", progress.current_path.as_deref());
            let _ = dict.set_item("files_processed", progress.files_processed);
            let _ = dict.set_item("total_files", progress.total_files);
            let _ = dict.set_item("bytes_processed", progress.bytes_processed);
            let _ = dict.set_item("total_bytes", progress.total_bytes);

            match self.callback.call1(py, (dict,)) {
                Ok(result) => result.extract::<bool>(py).unwrap_or(true),
                Err(_) => true, // Continue on callback error
            }
        })
    }
}

// ============================================================================
// Manifest (existing)
// ============================================================================

/// Decode a manifest from JSON string.
#[pyfunction]
fn decode_manifest(json: &str) -> PyResult<PyManifest> {
    let manifest =
        model::Manifest::decode(json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(PyManifest { inner: manifest })
}

/// Python wrapper for Manifest.
#[pyclass(name = "Manifest")]
struct PyManifest {
    inner: model::Manifest,
}

#[pymethods]
impl PyManifest {
    /// Encode the manifest to canonical JSON string.
    fn encode(&self) -> PyResult<String> {
        self.inner
            .encode()
            .map_err(|e| PyValueError::new_err(e.to_string()))
    }

    #[getter]
    fn version(&self) -> &'static str {
        self.inner.version().as_str()
    }

    #[getter]
    fn hash_alg(&self) -> &'static str {
        self.inner.hash_alg().as_str()
    }

    #[getter]
    fn total_size(&self) -> u64 {
        self.inner.total_size()
    }

    #[getter]
    fn file_count(&self) -> usize {
        self.inner.file_count()
    }

    fn is_v2023(&self) -> bool {
        self.inner.version() == ManifestVersion::V2023_03_03
    }

    fn is_v2025(&self) -> bool {
        self.inner.version() == ManifestVersion::V2025_12
    }
}

// ============================================================================
// ConflictResolution
// ============================================================================

/// How to handle conflicts when downloading files that already exist locally.
struct PyConflictResolution;

impl PyConflictResolution {
    /// Parse a conflict resolution string into the Rust enum.
    ///
    /// # Arguments
    /// * `s` - One of "SKIP", "OVERWRITE", or "CREATE_COPY"
    fn from_str(s: &str) -> PyResult<ConflictResolution> {
        match s.to_uppercase().as_str() {
            "SKIP" => Ok(ConflictResolution::Skip),
            "OVERWRITE" => Ok(ConflictResolution::Overwrite),
            "CREATE_COPY" => Ok(ConflictResolution::CreateCopy),
            _ => Err(PyValueError::new_err(format!(
                "Invalid conflict resolution: {}. Use 'SKIP', 'OVERWRITE', or 'CREATE_COPY'",
                s
            ))),
        }
    }
}

// ============================================================================
// PathMappingRule
// ============================================================================

/// A path mapping rule for translating source paths to destination paths.
#[pyclass(name = "PathMappingRule")]
#[derive(Clone)]
struct PyPathMappingRule {
    source_path_format: String,
    source_path: String,
    destination_path: String,
}

#[pymethods]
impl PyPathMappingRule {
    /// Create a new path mapping rule.
    ///
    /// Args:
    ///     source_path_format: Path format of the source ("windows" or "posix")
    ///     source_path: Original path to map from
    ///     destination_path: Target path to map to
    #[new]
    fn new(source_path_format: String, source_path: String, destination_path: String) -> Self {
        Self {
            source_path_format,
            source_path,
            destination_path,
        }
    }

    #[getter]
    fn source_path_format(&self) -> &str {
        &self.source_path_format
    }

    #[getter]
    fn source_path(&self) -> &str {
        &self.source_path
    }

    #[getter]
    fn destination_path(&self) -> &str {
        &self.destination_path
    }

    fn __repr__(&self) -> String {
        format!(
            "PathMappingRule(source='{}', dest='{}', fmt='{}')",
            self.source_path, self.destination_path, self.source_path_format
        )
    }
}

// ============================================================================
// DownloadSummaryStatistics
// ============================================================================

/// Download statistics with per-root file counts.
#[pyclass(name = "DownloadSummaryStatistics")]
#[derive(Clone)]
struct PyDownloadSummaryStatistics {
    stats: TransferStatistics,
    file_counts_by_root: HashMap<String, u64>,
}

#[pymethods]
impl PyDownloadSummaryStatistics {
    #[getter]
    fn total_files(&self) -> u64 {
        self.stats.files_processed
    }

    #[getter]
    fn total_bytes(&self) -> u64 {
        self.stats.bytes_transferred + self.stats.bytes_skipped
    }

    #[getter]
    fn downloaded_files(&self) -> u64 {
        self.stats.files_transferred
    }

    #[getter]
    fn downloaded_bytes(&self) -> u64 {
        self.stats.bytes_transferred
    }

    #[getter]
    fn skipped_files(&self) -> u64 {
        self.stats.files_skipped
    }

    #[getter]
    fn skipped_bytes(&self) -> u64 {
        self.stats.bytes_skipped
    }

    #[getter]
    fn file_counts_by_root_directory(&self) -> HashMap<String, u64> {
        self.file_counts_by_root.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "DownloadSummaryStatistics(downloaded={}, skipped={}, roots={})",
            self.stats.files_transferred,
            self.stats.files_skipped,
            self.file_counts_by_root.len()
        )
    }
}

// ============================================================================
// ManifestDiffResult
// ============================================================================

/// Result of a manifest diff operation.
#[pyclass(name = "ManifestDiffResult")]
#[derive(Clone)]
struct PyManifestDiffResult {
    new: Vec<String>,
    modified: Vec<String>,
    deleted: Vec<String>,
}

#[pymethods]
impl PyManifestDiffResult {
    #[getter]
    fn new_files(&self) -> Vec<String> {
        self.new.clone()
    }

    #[getter]
    fn modified(&self) -> Vec<String> {
        self.modified.clone()
    }

    #[getter]
    fn deleted(&self) -> Vec<String> {
        self.deleted.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestDiffResult(new={}, modified={}, deleted={})",
            self.new.len(),
            self.modified.len(),
            self.deleted.len()
        )
    }
}

// ============================================================================
// ManifestSnapshotResult
// ============================================================================

/// Result of a manifest snapshot operation.
#[pyclass(name = "ManifestSnapshotResult")]
#[derive(Clone)]
struct PyManifestSnapshotResult {
    root: String,
    manifest_path: String,
}

#[pymethods]
impl PyManifestSnapshotResult {
    #[getter]
    fn root(&self) -> &str {
        &self.root
    }

    #[getter]
    fn manifest_path(&self) -> &str {
        &self.manifest_path
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestSnapshotResult(root='{}', manifest='{}')",
            self.root, self.manifest_path
        )
    }
}

// ============================================================================
// UploadManifestInfo
// ============================================================================

/// Information about an uploaded manifest.
#[pyclass(name = "UploadManifestInfo")]
#[derive(Clone)]
struct PyUploadManifestInfo {
    output_manifest_path: String,
    output_manifest_hash: String,
    source_path: String,
}

#[pymethods]
impl PyUploadManifestInfo {
    #[getter]
    fn output_manifest_path(&self) -> &str {
        &self.output_manifest_path
    }

    #[getter]
    fn output_manifest_hash(&self) -> &str {
        &self.output_manifest_hash
    }

    #[getter]
    fn source_path(&self) -> &str {
        &self.source_path
    }

    fn __repr__(&self) -> String {
        format!(
            "UploadManifestInfo(path='{}', hash='{}', source='{}')",
            self.output_manifest_path, self.output_manifest_hash, self.source_path
        )
    }
}

// ============================================================================
// ManifestDownloadEntry
// ============================================================================

/// A single downloaded manifest entry.
#[pyclass(name = "ManifestDownloadEntry")]
#[derive(Clone)]
struct PyManifestDownloadEntry {
    manifest_root: String,
    local_manifest_path: String,
}

#[pymethods]
impl PyManifestDownloadEntry {
    #[getter]
    fn manifest_root(&self) -> &str {
        &self.manifest_root
    }

    #[getter]
    fn local_manifest_path(&self) -> &str {
        &self.local_manifest_path
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestDownloadEntry(root='{}', path='{}')",
            self.manifest_root, self.local_manifest_path
        )
    }
}

// ============================================================================
// OutputManifestScope
// ============================================================================

/// Scope for output manifest discovery (job or step level).
#[pyclass(name = "OutputManifestScope")]
#[derive(Clone)]
struct PyOutputManifestScope {
    farm_id: String,
    queue_id: String,
    job_id: String,
    step_id: Option<String>,
}

#[pymethods]
impl PyOutputManifestScope {
    /// Create a new output manifest scope.
    ///
    /// Args:
    ///     farm_id: Deadline farm ID
    ///     queue_id: Deadline queue ID
    ///     job_id: Deadline job ID
    ///     step_id: Optional step ID (None = job-level scope)
    #[new]
    #[pyo3(signature = (farm_id, queue_id, job_id, step_id=None))]
    fn new(
        farm_id: String,
        queue_id: String,
        job_id: String,
        step_id: Option<String>,
    ) -> Self {
        Self {
            farm_id,
            queue_id,
            job_id,
            step_id,
        }
    }

    #[getter]
    fn farm_id(&self) -> &str {
        &self.farm_id
    }

    #[getter]
    fn queue_id(&self) -> &str {
        &self.queue_id
    }

    #[getter]
    fn job_id(&self) -> &str {
        &self.job_id
    }

    #[getter]
    fn step_id(&self) -> Option<&str> {
        self.step_id.as_deref()
    }

    fn __repr__(&self) -> String {
        format!(
            "OutputManifestScope(farm='{}', queue='{}', job='{}', step={:?})",
            self.farm_id, self.queue_id, self.job_id, self.step_id
        )
    }
}

impl PyOutputManifestScope {
    /// Convert to the Rust OutputManifestScope enum.
    fn to_rust_scope(&self) -> RustOutputManifestScope {
        match &self.step_id {
            Some(step_id) => RustOutputManifestScope::Step {
                job_id: self.job_id.clone(),
                step_id: step_id.clone(),
            },
            None => RustOutputManifestScope::Job {
                job_id: self.job_id.clone(),
            },
        }
    }
}

// ============================================================================
// ManifestDownloadSpec
// ============================================================================

/// Specification for downloading a manifest from S3 (used by incremental download).
#[pyclass(name = "ManifestDownloadSpec")]
#[derive(Clone)]
struct PyManifestDownloadSpec {
    s3_key: String,
    asset_root: String,
    last_modified: f64,
}

#[pymethods]
impl PyManifestDownloadSpec {
    /// Create a new manifest download specification.
    ///
    /// Args:
    ///     s3_key: S3 key of the manifest object
    ///     asset_root: Asset root path this manifest belongs to
    ///     last_modified: Last modified timestamp (epoch seconds) for merge ordering
    #[new]
    fn new(s3_key: String, asset_root: String, last_modified: f64) -> Self {
        Self {
            s3_key,
            asset_root,
            last_modified,
        }
    }

    #[getter]
    fn s3_key(&self) -> &str {
        &self.s3_key
    }

    #[getter]
    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    #[getter]
    fn last_modified(&self) -> f64 {
        self.last_modified
    }

    fn __repr__(&self) -> String {
        format!(
            "ManifestDownloadSpec(key='{}', root='{}', modified={})",
            self.s3_key, self.asset_root, self.last_modified
        )
    }
}

// ============================================================================
// OutputManifestDiscovery
// ============================================================================

/// Result of output manifest discovery (phase 1 of download_job_output).
///
/// Contains the discovered output paths for user interaction, plus an opaque
/// handle to the pre-fetched manifests for phase 2.
#[pyclass(name = "OutputManifestDiscovery")]
struct PyOutputManifestDiscovery {
    outputs_by_root: HashMap<String, Vec<String>>,
    manifests_handle: u64,
}

#[pymethods]
impl PyOutputManifestDiscovery {
    /// Output file paths grouped by asset root, for user display/selection.
    #[getter]
    fn outputs_by_root(&self) -> HashMap<String, Vec<String>> {
        self.outputs_by_root.clone()
    }

    /// Opaque handle to pass to download_output_files() for phase 2.
    #[getter]
    fn manifests_handle(&self) -> u64 {
        self.manifests_handle
    }

    fn __repr__(&self) -> String {
        format!(
            "OutputManifestDiscovery(roots={}, handle={})",
            self.outputs_by_root.len(),
            self.manifests_handle
        )
    }
}

// ============================================================================
// Transfer Progress Callback (for upload/download operations)
// ============================================================================

/// Python progress callback wrapper for transfer operations.
struct PyTransferProgressCallback {
    callback: Arc<PyObject>,
}

impl rusty_attachments_storage::ProgressCallback for PyTransferProgressCallback {
    fn on_progress(&self, progress: &TransferProgress) -> bool {
        Python::with_gil(|py| {
            let dict = PyDict::new_bound(py);
            let _ = dict.set_item("operation", format!("{:?}", progress.operation));
            let _ = dict.set_item("current_key", &progress.current_key);
            let _ = dict.set_item("current_bytes", progress.current_bytes);
            let _ = dict.set_item("current_total", progress.current_total);

            match self.callback.call1(py, (dict,)) {
                Ok(result) => result.extract::<bool>(py).unwrap_or(true),
                Err(_) => true,
            }
        })
    }
}

// ============================================================================
// Handle Map for two-phase download_job_output
// ============================================================================

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Global handle map for storing pre-fetched manifests between phase 1 and phase 2.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

lazy_static::lazy_static! {
    static ref MANIFEST_HANDLE_MAP: Mutex<HashMap<u64, Vec<(String, Manifest)>>> =
        Mutex::new(HashMap::new());
}

/// Store manifests and return an opaque handle.
fn store_manifests(manifests: Vec<(String, Manifest)>) -> u64 {
    let handle: u64 = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
    let mut map = MANIFEST_HANDLE_MAP.lock().unwrap();
    map.insert(handle, manifests);
    handle
}

/// Retrieve and remove manifests by handle.
fn take_manifests(handle: u64) -> Option<Vec<(String, Manifest)>> {
    let mut map = MANIFEST_HANDLE_MAP.lock().unwrap();
    map.remove(&handle)
}

// ============================================================================
// Helper: create storage client
// ============================================================================

/// Create a DefaultClient from a region string.
async fn create_client(region: &str) -> PyResult<DefaultClient> {
    let settings: StorageSettings = StorageSettings {
        region: region.to_string(),
        ..Default::default()
    };
    DefaultClient::new(settings).await.map_err(|e| {
        PyRuntimeError::new_err(format!("Failed to create storage client: {}", e))
    })
}

// ============================================================================
// Helper: resolve path mapping
// ============================================================================

/// Apply path mapping rules to resolve a source path to a destination path.
fn apply_path_mapping(
    source_path: &str,
    rules: &[PyPathMappingRule],
) -> String {
    for rule in rules {
        if source_path.starts_with(&rule.source_path) {
            let remainder: &str = &source_path[rule.source_path.len()..];
            return format!("{}{}", rule.destination_path, remainder);
        }
    }
    source_path.to_string()
}

// ============================================================================
// Main Function: submit_bundle_attachments
// ============================================================================

/// Submit a job bundle with attachments.
///
/// This function uploads input files to S3 CAS and returns the attachments
/// JSON payload for the Deadline Cloud CreateJob API.
///
/// Args:
///     region: AWS region (e.g., "us-west-2")
///     s3_location: S3Location configuration
///     manifest_location: ManifestLocation configuration
///     asset_references: AssetReferences with input files and output directories
///     storage_profile: Optional StorageProfile for path classification
///     options: Optional BundleSubmitOptions
///     progress_callback: Optional callback function receiving progress dict
///
/// Returns:
///     BundleSubmitResult with attachments_json and statistics
///
/// Example:
///     ```python
///     result = await submit_bundle_attachments(
///         region="us-west-2",
///         s3_location=S3Location("bucket", "DeadlineCloud", "Data", "Manifests"),
///         manifest_location=ManifestLocation("bucket", "DeadlineCloud", "farm-xxx", "queue-xxx"),
///         asset_references=AssetReferences(["/path/to/files"], ["/path/to/outputs"]),
///     )
///     print(result.attachments_json)
///     ```
#[pyfunction]
#[pyo3(signature = (region, s3_location, manifest_location, asset_references, storage_profile=None, options=None, progress_callback=None))]
fn submit_bundle_attachments_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_location: PyManifestLocation,
    asset_references: PyAssetReferences,
    storage_profile: Option<PyStorageProfile>,
    options: Option<PyBundleSubmitOptions>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let s3_loc: S3Location = s3_location.inner.clone();
    let manifest_loc: ManifestLocation = manifest_location.inner.clone();
    let asset_refs: AssetReferences = asset_references.inner.clone();
    let profile: Option<StorageProfile> = storage_profile.map(|p| p.inner);
    let opts: BundleSubmitOptions = options.map(|o| o.inner).unwrap_or_default();
    let callback: Option<Arc<PyObject>> = progress_callback.map(Arc::new);

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        // Create storage client
        let settings: StorageSettings = StorageSettings {
            region,
            ..Default::default()
        };

        let client: DefaultClient = DefaultClient::new(settings).await.map_err(|e| {
            PyRuntimeError::new_err(format!("Failed to create storage client: {}", e))
        })?;

        // Create progress callback wrapper
        let progress: Option<PyProgressCallback> =
            callback.map(|cb| PyProgressCallback { callback: cb });

        // Call the Rust function
        let result = submit_bundle_attachments(
            &client,
            &s3_loc,
            &manifest_loc,
            &asset_refs,
            profile.as_ref(),
            &opts,
            progress
                .as_ref()
                .map(|p| p as &dyn ProgressCallback<ScanProgress>),
            None, // upload progress (TODO: add separate callback)
        )
        .await
        .map_err(|e| StorageError::new_err(e.to_string()))?;

        // Convert to Python result
        let attachments_json: String = result.attachments.to_json().map_err(|e| {
            PyRuntimeError::new_err(format!("Failed to serialize attachments: {}", e))
        })?;

        Ok(PyBundleSubmitResult {
            attachments_json,
            hashing_stats: result.hashing_stats,
            upload_stats: result.upload_stats,
        })
    })
}

// ============================================================================
// Binding: manifest_snapshot
// ============================================================================

/// Create a manifest snapshot of a directory.
///
/// Scans the directory, hashes files (with cache), and writes the manifest
/// to the destination directory. Optionally computes a diff against an
/// existing manifest.
///
/// Args:
///     root: Root directory to snapshot
///     destination: Directory to write the manifest file
///     name: Optional manifest name (defaults to sanitized root path)
///     include: Glob include patterns
///     exclude: Glob exclude patterns
///     include_exclude_config: Path to JSON config with include/exclude
///     diff_manifest: Path to existing manifest for diff mode
///     force_rehash: If true, hash all files even in diff mode
///     hash_cache_dir: Directory for the SQLite hash cache
///     progress_callback: Optional callback(dict) -> bool
///
/// Returns:
///     ManifestSnapshotResult with root and manifest path, or None if no files found.
#[pyfunction]
#[pyo3(signature = (
    root,
    destination,
    name=None,
    include=None,
    exclude=None,
    include_exclude_config=None,
    diff_manifest=None,
    force_rehash=false,
    hash_cache_dir=None,
    progress_callback=None,
))]
fn manifest_snapshot_py<'py>(
    py: Python<'py>,
    root: String,
    destination: String,
    name: Option<String>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    include_exclude_config: Option<String>,
    diff_manifest: Option<String>,
    force_rehash: bool,
    hash_cache_dir: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let callback: Option<Arc<PyObject>> = progress_callback.map(Arc::new);

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        // Build glob filter from include/exclude patterns or config file
        let filter: GlobFilter = build_glob_filter(
            include.as_deref(),
            exclude.as_deref(),
            include_exclude_config.as_deref(),
        )?;

        // If diff mode, load the reference manifest and diff
        if let Some(diff_path) = &diff_manifest {
            let manifest_bytes: Vec<u8> = std::fs::read(diff_path)
                .map_err(|e| PyValueError::new_err(format!("Cannot read manifest: {}", e)))?;
            let json: String = String::from_utf8(manifest_bytes.clone())
                .map_err(|e| PyValueError::new_err(format!("Invalid UTF-8 in manifest: {}", e)))?;
            let reference: Manifest = Manifest::decode(&json)
                .map_err(|e| PyValueError::new_err(format!("Invalid manifest: {}", e)))?;

            let mode: DiffMode = if force_rehash {
                DiffMode::Hash
            } else {
                DiffMode::Fast
            };

            let diff_opts: DiffOptions = DiffOptions {
                root: PathBuf::from(&root),
                filter,
                mode,
                parallelism: 0,
            };

            let engine: DiffEngine = DiffEngine::new();
            let progress_cb: Option<PyProgressCallback> =
                callback.map(|cb| PyProgressCallback { callback: cb });

            let diff_result: DiffResult = engine
                .diff(&reference, &diff_opts, progress_cb.as_ref().map(
                    |p| p as &dyn ProgressCallback<ScanProgress>,
                ))
                .map_err(|e| StorageError::new_err(e.to_string()))?;

            // If no changes, return None
            if diff_result.added.is_empty()
                && diff_result.modified.is_empty()
                && diff_result.deleted.is_empty()
            {
                return Ok(None);
            }

            // Create diff manifest and write it
            let diff_manifest: Manifest = engine
                .create_diff_manifest(
                    &reference,
                    &manifest_bytes,
                    &diff_result,
                    &diff_opts,
                )
                .map_err(|e| StorageError::new_err(e.to_string()))?;

            let manifest_path: String =
                write_manifest_to_dir(&diff_manifest, &root, &destination, name.as_deref())?;

            return Ok(Some(PyManifestSnapshotResult {
                root: root.clone(),
                manifest_path,
            }));
        }

        // Full snapshot mode
        let options: SnapshotOptions = SnapshotOptions {
            root: PathBuf::from(&root),
            input_files: None,
            version: ManifestVersion::V2025_12,
            filter,
            hash_algorithm: HashAlgorithm::Xxh128,
            follow_symlinks: false,
            include_empty_dirs: true,
        };

        let scanner: FileSystemScanner = FileSystemScanner::new();
        let progress_cb: Option<PyProgressCallback> =
            callback.map(|cb| PyProgressCallback { callback: cb });

        let manifest: Manifest = scanner
            .snapshot(
                &options,
                progress_cb
                    .as_ref()
                    .map(|p| p as &dyn ProgressCallback<ScanProgress>),
            )
            .map_err(|e| StorageError::new_err(e.to_string()))?;

        if manifest.file_count() == 0 {
            return Ok(None);
        }

        let manifest_path: String =
            write_manifest_to_dir(&manifest, &root, &destination, name.as_deref())?;

        Ok(Some(PyManifestSnapshotResult {
            root,
            manifest_path,
        }))
    })
}

/// Build a GlobFilter from include/exclude patterns or a JSON config file.
///
/// Priority: config_path > explicit patterns > default (match all).
/// When both include and exclude are provided, exclude takes precedence
/// (include is assumed to be the default "match all").
fn build_glob_filter(
    include: Option<&[String]>,
    exclude: Option<&[String]>,
    config_path: Option<&str>,
) -> PyResult<GlobFilter> {
    // If config file provided, read it
    if let Some(path) = config_path {
        let json: String = std::fs::read_to_string(path)
            .map_err(|e| PyValueError::new_err(format!("Cannot read glob config: {}", e)))?;
        let config: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| PyValueError::new_err(format!("Invalid glob config JSON: {}", e)))?;

        let exc: Vec<String> = config
            .get("exclude")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if !exc.is_empty() {
            return GlobFilter::exclude(exc)
                .map_err(|e| PyValueError::new_err(format!("Invalid exclude pattern: {}", e)));
        }

        let inc: Vec<String> = config
            .get("include")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if !inc.is_empty() {
            return GlobFilter::include(inc)
                .map_err(|e| PyValueError::new_err(format!("Invalid include pattern: {}", e)));
        }
    }

    // Explicit exclude patterns take priority over include
    if let Some(exc) = exclude {
        if !exc.is_empty() {
            return GlobFilter::exclude(exc.to_vec())
                .map_err(|e| PyValueError::new_err(format!("Invalid exclude pattern: {}", e)));
        }
    }
    if let Some(inc) = include {
        if !inc.is_empty() {
            return GlobFilter::include(inc.to_vec())
                .map_err(|e| PyValueError::new_err(format!("Invalid include pattern: {}", e)));
        }
    }

    Ok(GlobFilter::default())
}

/// Write a manifest to a destination directory and return the file path.
fn write_manifest_to_dir(
    manifest: &Manifest,
    root: &str,
    destination: &str,
    name: Option<&str>,
) -> PyResult<String> {
    let encoded: String = manifest
        .encode()
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to encode manifest: {}", e)))?;

    let file_name: String = if let Some(n) = name {
        format!("{}.manifest", n)
    } else {
        let root_hash: String =
            manifest_storage::compute_manifest_name_hash(root);
        format!("{}.manifest", root_hash)
    };

    let manifest_path: PathBuf = PathBuf::from(destination).join(&file_name);
    std::fs::create_dir_all(destination)
        .map_err(|e| PyRuntimeError::new_err(format!("Cannot create directory: {}", e)))?;
    std::fs::write(&manifest_path, &encoded)
        .map_err(|e| PyRuntimeError::new_err(format!("Cannot write manifest: {}", e)))?;

    Ok(manifest_path.display().to_string())
}

// ============================================================================
// Binding: manifest_diff
// ============================================================================

/// Diff a directory against an existing manifest.
///
/// Args:
///     root: Root directory to compare
///     manifest_path: Path to the reference manifest file
///     include: Glob include patterns
///     exclude: Glob exclude patterns
///     include_exclude_config: Path to JSON config with include/exclude
///     force_rehash: If true, compare by hash instead of mtime/size
///     hash_cache_dir: Directory for the SQLite hash cache
///     progress_callback: Optional callback(dict) -> bool
///
/// Returns:
///     ManifestDiffResult with new, modified, and deleted file lists.
#[pyfunction]
#[pyo3(signature = (
    root,
    manifest_path,
    include=None,
    exclude=None,
    include_exclude_config=None,
    force_rehash=false,
    hash_cache_dir=None,
    progress_callback=None,
))]
fn manifest_diff_py<'py>(
    py: Python<'py>,
    root: String,
    manifest_path: String,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    include_exclude_config: Option<String>,
    force_rehash: bool,
    hash_cache_dir: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let callback: Option<Arc<PyObject>> = progress_callback.map(Arc::new);

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let json: String = std::fs::read_to_string(&manifest_path)
            .map_err(|e| PyValueError::new_err(format!("Cannot read manifest: {}", e)))?;
        let reference: Manifest = Manifest::decode(&json)
            .map_err(|e| PyValueError::new_err(format!("Invalid manifest: {}", e)))?;

        let filter: GlobFilter = build_glob_filter(
            include.as_deref(),
            exclude.as_deref(),
            include_exclude_config.as_deref(),
        )?;

        let mode: DiffMode = if force_rehash {
            DiffMode::Hash
        } else {
            DiffMode::Fast
        };

        let diff_opts: DiffOptions = DiffOptions {
            root: PathBuf::from(&root),
            filter,
            mode,
            parallelism: 0,
        };

        let engine: DiffEngine = DiffEngine::new();
        let progress_cb: Option<PyProgressCallback> =
            callback.map(|cb| PyProgressCallback { callback: cb });

        let result: DiffResult = engine
            .diff(
                &reference,
                &diff_opts,
                progress_cb
                    .as_ref()
                    .map(|p| p as &dyn ProgressCallback<ScanProgress>),
            )
            .map_err(|e| StorageError::new_err(e.to_string()))?;

        Ok(PyManifestDiffResult {
            new: result.added.iter().map(|f| f.path.clone()).collect(),
            modified: result.modified.iter().map(|f| f.path.clone()).collect(),
            deleted: result.deleted,
        })
    })
}

// ============================================================================
// Binding: manifest_download
// ============================================================================

/// Download and merge manifests from S3, write to local directory.
///
/// Args:
///     region: AWS region
///     s3_location: S3Location with bucket and prefixes
///     input_manifest_keys: List of (s3_key, root_path) tuples from job attachments
///     output_scope: Optional OutputManifestScope for output manifest discovery
///     download_dir: Local directory to write merged manifests
///     progress_callback: Optional callback(dict) -> bool
///
/// Returns:
///     List of ManifestDownloadEntry with root and local path for each merged manifest.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    input_manifest_keys,
    output_scope=None,
    download_dir=None,
    progress_callback=None,
))]
fn manifest_download_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    input_manifest_keys: Vec<(String, String)>,
    output_scope: Option<PyOutputManifestScope>,
    download_dir: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let s3_loc: S3Location = s3_location.inner.clone();
    let download_dir: String = download_dir.unwrap_or_else(|| ".".to_string());

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let client: DefaultClient = create_client(&region).await?;
        let mut results: Vec<PyManifestDownloadEntry> = Vec::new();

        // Group input manifests by root for merging
        let mut manifests_by_root: HashMap<String, Vec<Manifest>> = HashMap::new();

        // Download input manifests one at a time
        for (s3_key, root_path) in &input_manifest_keys {
            let (manifest, _metadata) = download_manifest(&client, &s3_loc.bucket, s3_key)
                .await
                .map_err(|e| StorageError::new_err(format!("Download manifest failed: {}", e)))?;
            manifests_by_root
                .entry(root_path.clone())
                .or_default()
                .push(manifest);
        }

        // If output scope provided, discover output manifest keys and download individually
        if let Some(scope) = &output_scope {
            let manifest_loc: ManifestLocation = ManifestLocation::from_s3_location(
                &s3_loc,
                &scope.farm_id,
                &scope.queue_id,
            );
            let rust_scope: RustOutputManifestScope = scope.to_rust_scope();
            let discovery_opts: OutputManifestDiscoveryOptions = OutputManifestDiscoveryOptions {
                scope: rust_scope,
                select_latest_per_task: true,
            };

            // Discover keys first
            let keys: Vec<String> = manifest_storage::discover_output_manifest_keys(
                &client,
                &manifest_loc,
                &discovery_opts,
            )
            .await
            .map_err(|e| StorageError::new_err(format!("Output manifest discovery failed: {}", e)))?;

            // Download each manifest individually
            for key in &keys {
                let (manifest, metadata) = download_manifest(&client, &s3_loc.bucket, key)
                    .await
                    .map_err(|e| StorageError::new_err(format!("Download manifest failed: {}", e)))?;
                manifests_by_root
                    .entry(metadata.asset_root)
                    .or_default()
                    .push(manifest);
            }
        }

        // Merge and write manifests per root
        std::fs::create_dir_all(&download_dir)
            .map_err(|e| PyRuntimeError::new_err(format!("Cannot create directory: {}", e)))?;

        for (root, manifests) in &manifests_by_root {
            let merged: Option<Manifest> = merge_manifests(manifests)
                .map_err(|e| StorageError::new_err(format!("Merge failed: {}", e)))?;

            if let Some(manifest) = merged {
                let root_hash: String =
                    manifest_storage::compute_manifest_name_hash(root);
                let file_name: String = format!("{}.manifest", root_hash);
                let local_path: PathBuf = PathBuf::from(&download_dir).join(&file_name);

                let encoded: String = manifest.encode().map_err(|e| {
                    PyRuntimeError::new_err(format!("Encode failed: {}", e))
                })?;
                std::fs::write(&local_path, &encoded).map_err(|e| {
                    PyRuntimeError::new_err(format!("Write failed: {}", e))
                })?;

                results.push(PyManifestDownloadEntry {
                    manifest_root: root.clone(),
                    local_manifest_path: local_path.display().to_string(),
                });
            }
        }

        Ok(results)
    })
}

// ============================================================================
// Binding: manifest_upload
// ============================================================================

/// Upload a manifest file to S3.
///
/// Args:
///     region: AWS region
///     s3_location: S3Location with bucket and prefixes
///     manifest_bytes: Raw manifest content as bytes
///     s3_key: Full S3 key to upload to
///     metadata: S3 object metadata key-value pairs
///
/// Returns:
///     None on success.
#[pyfunction]
fn manifest_upload_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_bytes: Vec<u8>,
    s3_key: String,
    metadata: HashMap<String, String>,
) -> PyResult<Bound<'py, PyAny>> {
    let s3_loc: S3Location = s3_location.inner.clone();

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let client: DefaultClient = create_client(&region).await?;

        client
            .put_object(
                &s3_loc.bucket,
                &s3_key,
                &manifest_bytes,
                Some("application/json"),
                Some(&metadata),
            )
            .await
            .map_err(|e| StorageError::new_err(format!("Upload failed: {}", e)))?;

        Ok(())
    })
}

// ============================================================================
// Binding: attachment_download
// ============================================================================

/// Download attachment files from S3 CAS using local manifest files.
///
/// Args:
///     region: AWS region
///     s3_location: S3Location with bucket and CAS prefix
///     manifest_paths: Local paths to manifest files
///     path_mapping_rules: Optional path mapping rules for destination resolution
///     conflict_resolution: "SKIP", "OVERWRITE", or "CREATE_COPY"
///     progress_callback: Optional callback(dict) -> bool
///
/// Returns:
///     DownloadSummaryStatistics with file counts and byte totals.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    manifest_paths,
    path_mapping_rules=None,
    conflict_resolution=None,
    progress_callback=None,
))]
fn attachment_download_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_paths: Vec<String>,
    path_mapping_rules: Option<Vec<PyPathMappingRule>>,
    conflict_resolution: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let s3_loc: S3Location = s3_location.inner.clone();
    let conflict_str: String = conflict_resolution.unwrap_or_else(|| "CREATE_COPY".to_string());
    let conflict: ConflictResolution = PyConflictResolution::from_str(&conflict_str)?;
    let rules: Vec<PyPathMappingRule> = path_mapping_rules.unwrap_or_default();
    let callback: Option<Arc<PyObject>> = progress_callback.map(Arc::new);

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let client: DefaultClient = create_client(&region).await?;
        let orchestrator: DownloadOrchestrator<'_, DefaultClient> =
            DownloadOrchestrator::new(&client, s3_loc.clone());

        let mut total_stats: TransferStatistics = TransferStatistics::default();
        let mut file_counts_by_root: HashMap<String, u64> = HashMap::new();

        // Read and decode each manifest, resolve destination via path mapping
        for manifest_path in &manifest_paths {
            let json: String = std::fs::read_to_string(manifest_path)
                .map_err(|e| PyValueError::new_err(format!("Cannot read manifest: {}", e)))?;
            let manifest: Manifest = Manifest::decode(&json)
                .map_err(|e| PyValueError::new_err(format!("Invalid manifest: {}", e)))?;

            // Default destination is the manifest's directory
            let default_root: String = Path::new(manifest_path)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or(".")
                .to_string();

            let destination_root: String = if !rules.is_empty() {
                apply_path_mapping(&default_root, &rules)
            } else {
                default_root
            };

            let transfer_cb: Option<PyTransferProgressCallback> =
                callback.clone().map(|cb| PyTransferProgressCallback { callback: cb });

            let stats: TransferStatistics = orchestrator
                .download_manifest_contents(
                    &manifest,
                    &destination_root,
                    conflict,
                    transfer_cb
                        .as_ref()
                        .map(|p| p as &dyn rusty_attachments_storage::ProgressCallback),
                )
                .await
                .map_err(|e| StorageError::new_err(format!("Download failed: {}", e)))?;

            *file_counts_by_root
                .entry(destination_root)
                .or_insert(0) += stats.files_processed;
            total_stats.merge(stats);
        }

        Ok(PyDownloadSummaryStatistics {
            stats: total_stats,
            file_counts_by_root,
        })
    })
}

// ============================================================================
// Binding: attachment_upload
// ============================================================================

/// Upload attachment files to S3 CAS using local manifest files.
///
/// Args:
///     region: AWS region
///     s3_location: S3Location with bucket and CAS prefix
///     manifest_location: ManifestLocation for manifest S3 uploads
///     manifest_paths: Local paths to manifest files
///     root_dirs: Root directories holding the actual files
///     path_mapping_rules: Optional path mapping rules
///     upload_manifest_path: Optional S3 prefix for manifest uploads
///     s3_check_cache_dir: Directory for S3 existence check cache
///     progress_callback: Optional callback(dict) -> bool
///
/// Returns:
///     List of UploadManifestInfo with S3 keys and hashes.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    manifest_location,
    manifest_paths,
    root_dirs,
    path_mapping_rules=None,
    upload_manifest_path=None,
    s3_check_cache_dir=None,
    progress_callback=None,
))]
fn attachment_upload_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_location: PyManifestLocation,
    manifest_paths: Vec<String>,
    root_dirs: Vec<String>,
    path_mapping_rules: Option<Vec<PyPathMappingRule>>,
    upload_manifest_path: Option<String>,
    s3_check_cache_dir: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let s3_loc: S3Location = s3_location.inner.clone();
    let manifest_loc: ManifestLocation = manifest_location.inner.clone();
    let rules: Vec<PyPathMappingRule> = path_mapping_rules.unwrap_or_default();
    let callback: Option<Arc<PyObject>> = progress_callback.map(Arc::new);

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let client: DefaultClient = create_client(&region).await?;

        // Build S3 check cache if directory provided
        let s3_check_cache: Option<S3CheckCache> = if let Some(cache_dir) = &s3_check_cache_dir {
            let cache_path: PathBuf = PathBuf::from(cache_dir).join("s3_check_cache.db");
            std::fs::create_dir_all(cache_dir)
                .map_err(|e| PyRuntimeError::new_err(format!("Cannot create cache dir: {}", e)))?;
            let backend: SqliteS3CheckCache = SqliteS3CheckCache::open(&cache_path)
                .map_err(|e| PyRuntimeError::new_err(format!("Cannot open S3 check cache: {}", e)))?;
            Some(S3CheckCache::new(backend))
        } else {
            None
        };

        let mut orchestrator: UploadOrchestrator<'_, DefaultClient> =
            UploadOrchestrator::new(&client, s3_loc.clone());
        if let Some(cache) = s3_check_cache {
            orchestrator = orchestrator.with_s3_check_cache(cache);
        }

        let mut results: Vec<PyUploadManifestInfo> = Vec::new();

        for (i, manifest_path) in manifest_paths.iter().enumerate() {
            let json: String = std::fs::read_to_string(manifest_path)
                .map_err(|e| PyValueError::new_err(format!("Cannot read manifest: {}", e)))?;
            let manifest: Manifest = Manifest::decode(&json)
                .map_err(|e| PyValueError::new_err(format!("Invalid manifest: {}", e)))?;

            // Resolve source root from root_dirs or path mapping
            let source_root: String = if i < root_dirs.len() {
                if !rules.is_empty() {
                    apply_path_mapping(&root_dirs[i], &rules)
                } else {
                    root_dirs[i].clone()
                }
            } else {
                ".".to_string()
            };

            let transfer_cb: Option<PyTransferProgressCallback> =
                callback.clone().map(|cb| PyTransferProgressCallback { callback: cb });

            // Upload file contents to CAS
            let _stats: TransferStatistics = orchestrator
                .upload_manifest_contents(
                    &manifest,
                    &source_root,
                    transfer_cb
                        .as_ref()
                        .map(|p| p as &dyn rusty_attachments_storage::ProgressCallback),
                )
                .await
                .map_err(|e| StorageError::new_err(format!("Upload failed: {}", e)))?;

            // Upload the manifest file itself to S3
            let upload_result: ManifestUploadResult = upload_input_manifest(
                &client,
                &manifest_loc,
                &manifest,
                &source_root,
                None, // file_system_location_name
            )
            .await
            .map_err(|e| StorageError::new_err(format!("Manifest upload failed: {}", e)))?;

            results.push(PyUploadManifestInfo {
                output_manifest_path: upload_result.s3_key,
                output_manifest_hash: upload_result.manifest_hash,
                source_path: source_root,
            });
        }

        Ok(results)
    })
}

// ============================================================================
// Binding: discover_output_manifests (phase 1 of download_job_output)
// ============================================================================

/// Discover and pre-fetch output manifests for a job.
///
/// Args:
///     region: AWS region
///     s3_location: S3Location with bucket and prefixes
///     farm_id: Deadline farm ID
///     queue_id: Deadline queue ID
///     job_id: Deadline job ID
///     step_id: Optional step ID filter
///     task_id: Optional task ID filter
///     session_action_id: Optional session action ID filter
///
/// Returns:
///     OutputManifestDiscovery with outputs_by_root for UI and opaque manifests_handle.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    farm_id,
    queue_id,
    job_id,
    step_id=None,
    task_id=None,
    session_action_id=None,
))]
fn discover_output_manifests_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    farm_id: String,
    queue_id: String,
    job_id: String,
    step_id: Option<String>,
    task_id: Option<String>,
    session_action_id: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let s3_loc: S3Location = s3_location.inner.clone();

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let client: DefaultClient = create_client(&region).await?;

        let manifest_loc: ManifestLocation =
            ManifestLocation::from_s3_location(&s3_loc, &farm_id, &queue_id);

        // Determine scope
        let scope: RustOutputManifestScope = match (&step_id, &task_id) {
            (Some(sid), Some(tid)) => RustOutputManifestScope::Task {
                job_id: job_id.clone(),
                step_id: sid.clone(),
                task_id: tid.clone(),
            },
            (Some(sid), None) => RustOutputManifestScope::Step {
                job_id: job_id.clone(),
                step_id: sid.clone(),
            },
            _ => RustOutputManifestScope::Job {
                job_id: job_id.clone(),
            },
        };

        // Discover output manifest keys
        let discovery_opts: OutputManifestDiscoveryOptions = OutputManifestDiscoveryOptions {
            scope,
            select_latest_per_task: true,
        };
        let keys: Vec<String> = manifest_storage::discover_output_manifest_keys(
            &client,
            &manifest_loc,
            &discovery_opts,
        )
        .await
        .map_err(|e| StorageError::new_err(format!("Discovery failed: {}", e)))?;

        // Download each manifest individually
        let mut outputs_by_root: HashMap<String, Vec<String>> = HashMap::new();
        let mut manifests_for_handle: Vec<(String, Manifest)> = Vec::new();

        for key in &keys {
            let (manifest, metadata) = download_manifest(&client, &s3_loc.bucket, key)
                .await
                .map_err(|e| StorageError::new_err(format!("Download failed: {}", e)))?;

            let root: String = metadata.asset_root;
            let count: usize = manifest.file_count();
            outputs_by_root
                .entry(root.clone())
                .or_default()
                .push(format!("{} files", count));
            manifests_for_handle.push((root, manifest));
        }

        // Store manifests in handle map for phase 2
        let handle: u64 = store_manifests(manifests_for_handle);

        Ok(PyOutputManifestDiscovery {
            outputs_by_root,
            manifests_handle: handle,
        })
    })
}

// ============================================================================
// Binding: download_output_files (phase 2 of download_job_output)
// ============================================================================

/// Download output files using a pre-fetched manifest handle.
///
/// Args:
///     region: AWS region
///     s3_location: S3Location with bucket and CAS prefix
///     manifests_handle: Opaque handle from discover_output_manifests()
///     root_overrides: Map of original_root -> new_root for user-selected paths
///     conflict_resolution: "SKIP", "OVERWRITE", or "CREATE_COPY"
///     progress_callback: Optional callback(dict) -> bool
///
/// Returns:
///     DownloadSummaryStatistics with file counts and byte totals.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    manifests_handle,
    root_overrides,
    conflict_resolution=None,
    progress_callback=None,
))]
fn download_output_files_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifests_handle: u64,
    root_overrides: HashMap<String, String>,
    conflict_resolution: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let s3_loc: S3Location = s3_location.inner.clone();
    let conflict_str: String = conflict_resolution.unwrap_or_else(|| "CREATE_COPY".to_string());
    let conflict: ConflictResolution = PyConflictResolution::from_str(&conflict_str)?;
    let callback: Option<Arc<PyObject>> = progress_callback.map(Arc::new);

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let client: DefaultClient = create_client(&region).await?;
        let orchestrator: DownloadOrchestrator<'_, DefaultClient> =
            DownloadOrchestrator::new(&client, s3_loc);

        // Retrieve manifests from handle map
        let manifests: Vec<(String, Manifest)> = take_manifests(manifests_handle).ok_or_else(|| {
            PyValueError::new_err(format!(
                "Invalid or expired manifests_handle: {}",
                manifests_handle
            ))
        })?;

        // Group by root and merge
        let mut by_root: HashMap<String, Vec<Manifest>> = HashMap::new();
        for (root, manifest) in manifests {
            by_root.entry(root).or_default().push(manifest);
        }

        let mut total_stats: TransferStatistics = TransferStatistics::default();
        let mut file_counts_by_root: HashMap<String, u64> = HashMap::new();

        for (root, root_manifests) in &by_root {
            let merged: Option<Manifest> = merge_manifests(root_manifests)
                .map_err(|e| StorageError::new_err(format!("Merge failed: {}", e)))?;

            if let Some(manifest) = merged {
                // Apply root override if provided
                let dest_root: &str = root_overrides.get(root).map(|s| s.as_str()).unwrap_or(root);

                let transfer_cb: Option<PyTransferProgressCallback> =
                    callback.clone().map(|cb| PyTransferProgressCallback { callback: cb });

                let stats: TransferStatistics = orchestrator
                    .download_manifest_contents(
                        &manifest,
                        dest_root,
                        conflict,
                        transfer_cb
                            .as_ref()
                            .map(|p| p as &dyn rusty_attachments_storage::ProgressCallback),
                    )
                    .await
                    .map_err(|e| StorageError::new_err(format!("Download failed: {}", e)))?;

                *file_counts_by_root
                    .entry(dest_root.to_string())
                    .or_insert(0) += stats.files_processed;
                total_stats.merge(stats);
            }
        }

        Ok(PyDownloadSummaryStatistics {
            stats: total_stats,
            file_counts_by_root,
        })
    })
}

// ============================================================================
// Binding: incremental_download
// ============================================================================

/// Download manifests and files incrementally for queue sync-output.
///
/// Args:
///     region: AWS region
///     s3_location: S3Location with bucket and CAS prefix
///     manifest_specs: List of ManifestDownloadSpec with S3 keys and ordering
///     path_mapping_rules: Path mapping rules from storage profile resolution
///     conflict_resolution: "SKIP", "OVERWRITE", or "CREATE_COPY"
///     progress_callback: Optional callback(dict) -> bool
///
/// Returns:
///     DownloadSummaryStatistics with file counts and byte totals.
#[pyfunction]
#[pyo3(signature = (
    region,
    s3_location,
    manifest_specs,
    path_mapping_rules,
    conflict_resolution=None,
    progress_callback=None,
))]
fn incremental_download_py<'py>(
    py: Python<'py>,
    region: String,
    s3_location: PyS3Location,
    manifest_specs: Vec<PyManifestDownloadSpec>,
    path_mapping_rules: Vec<PyPathMappingRule>,
    conflict_resolution: Option<String>,
    progress_callback: Option<PyObject>,
) -> PyResult<Bound<'py, PyAny>> {
    let s3_loc: S3Location = s3_location.inner.clone();
    let conflict_str: String = conflict_resolution.unwrap_or_else(|| "CREATE_COPY".to_string());
    let conflict: ConflictResolution = PyConflictResolution::from_str(&conflict_str)?;
    let callback: Option<Arc<PyObject>> = progress_callback.map(Arc::new);

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let client: DefaultClient = create_client(&region).await?;
        let orchestrator: DownloadOrchestrator<'_, DefaultClient> =
            DownloadOrchestrator::new(&client, s3_loc.clone());

        // Download all manifests from S3 individually
        let mut by_root: HashMap<String, Vec<(f64, Manifest)>> = HashMap::new();
        for spec in &manifest_specs {
            let (manifest, _metadata) = download_manifest(&client, &s3_loc.bucket, &spec.s3_key)
                .await
                .map_err(|e| StorageError::new_err(format!("Manifest download failed: {}", e)))?;
            let mapped_root: String = apply_path_mapping(&spec.asset_root, &path_mapping_rules);
            by_root
                .entry(mapped_root)
                .or_default()
                .push((spec.last_modified, manifest));
        }

        let mut total_stats: TransferStatistics = TransferStatistics::default();
        let mut file_counts_by_root: HashMap<String, u64> = HashMap::new();

        // Merge chronologically per root and download
        for (root, mut manifests_with_time) in by_root {
            manifests_with_time.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let manifests_with_ts: Vec<(Manifest, i64)> = manifests_with_time
                .into_iter()
                .map(|(ts, m)| (m, ts as i64))
                .collect();

            let merged: Option<Manifest> =
                model::merge_manifests_chronologically(&manifests_with_ts)
                    .map_err(|e| StorageError::new_err(format!("Merge failed: {}", e)))?;

            if let Some(manifest) = merged {
                let transfer_cb: Option<PyTransferProgressCallback> =
                    callback.clone().map(|cb| PyTransferProgressCallback { callback: cb });

                let stats: TransferStatistics = orchestrator
                    .download_manifest_contents(
                        &manifest,
                        &root,
                        conflict,
                        transfer_cb
                            .as_ref()
                            .map(|p| p as &dyn rusty_attachments_storage::ProgressCallback),
                    )
                    .await
                    .map_err(|e| StorageError::new_err(format!("Download failed: {}", e)))?;

                *file_counts_by_root.entry(root).or_insert(0) += stats.files_processed;
                total_stats.merge(stats);
            }
        }

        Ok(PyDownloadSummaryStatistics {
            stats: total_stats,
            file_counts_by_root,
        })
    })
}

// ============================================================================
// Module Definition
// ============================================================================

/// Python module for rusty-attachments.
#[pymodule]
fn rusty_attachments(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Exceptions
    m.add(
        "AttachmentError",
        m.py().get_type_bound::<AttachmentError>(),
    )?;
    m.add("StorageError", m.py().get_type_bound::<StorageError>())?;
    m.add(
        "ValidationError",
        m.py().get_type_bound::<ValidationError>(),
    )?;

    // Existing classes
    m.add_class::<PyS3Location>()?;
    m.add_class::<PyManifestLocation>()?;
    m.add_class::<PyAssetReferences>()?;
    m.add_class::<PyBundleSubmitOptions>()?;
    m.add_class::<PyBundleSubmitResult>()?;
    m.add_class::<PySummaryStatistics>()?;
    m.add_class::<PyFileSystemLocation>()?;
    m.add_class::<PyStorageProfile>()?;
    m.add_class::<PyManifest>()?;

    // New types
    m.add_class::<PyPathMappingRule>()?;
    m.add_class::<PyDownloadSummaryStatistics>()?;
    m.add_class::<PyManifestDiffResult>()?;
    m.add_class::<PyManifestSnapshotResult>()?;
    m.add_class::<PyUploadManifestInfo>()?;
    m.add_class::<PyManifestDownloadEntry>()?;
    m.add_class::<PyOutputManifestScope>()?;
    m.add_class::<PyManifestDownloadSpec>()?;
    m.add_class::<PyOutputManifestDiscovery>()?;

    // Existing functions
    m.add_function(wrap_pyfunction!(submit_bundle_attachments_py, m)?)?;
    m.add_function(wrap_pyfunction!(decode_manifest, m)?)?;

    // New binding functions
    m.add_function(wrap_pyfunction!(manifest_snapshot_py, m)?)?;
    m.add_function(wrap_pyfunction!(manifest_diff_py, m)?)?;
    m.add_function(wrap_pyfunction!(manifest_download_py, m)?)?;
    m.add_function(wrap_pyfunction!(manifest_upload_py, m)?)?;
    m.add_function(wrap_pyfunction!(attachment_download_py, m)?)?;
    m.add_function(wrap_pyfunction!(attachment_upload_py, m)?)?;
    m.add_function(wrap_pyfunction!(discover_output_manifests_py, m)?)?;
    m.add_function(wrap_pyfunction!(download_output_files_py, m)?)?;
    m.add_function(wrap_pyfunction!(incremental_download_py, m)?)?;

    Ok(())
}
