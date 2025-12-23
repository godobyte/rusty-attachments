//! Python bindings for rusty-attachments.

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use ja_deadline_utils::{
    self as ja, submit_bundle_attachments, AssetReferences, BundleSubmitOptions,
};
use rusty_attachments_common::ProgressCallback;
use rusty_attachments_filesystem::{GlobFilter, ScanProgress};
use rusty_attachments_model::{self as model, ManifestVersion};
use rusty_attachments_profiles::{
    FileSystemLocation, FileSystemLocationType, StorageProfile,
};
use rusty_attachments_storage::{ManifestLocation, S3Location, StorageSettings};
use rusty_attachments_storage_crt::CrtStorageClient;

// ============================================================================
// Exceptions
// ============================================================================

pyo3::create_exception!(rusty_attachments, AttachmentError, pyo3::exceptions::PyException);
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
    fn new(bucket: String, root_prefix: String, cas_prefix: String, manifest_prefix: String) -> Self {
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
            self.inner.bucket, self.inner.root_prefix, self.inner.cas_prefix, self.inner.manifest_prefix
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
    ///     manifest_version: "v2023-03-03" or "v2025-12-04-beta"
    ///     exclude_patterns: Glob patterns to exclude (e.g., ["**/*.tmp", "**/__pycache__/**"])
    #[new]
    #[pyo3(signature = (require_paths_exist=false, file_system_mode="COPIED", manifest_version="v2025-12-04-beta", exclude_patterns=None))]
    fn new(
        require_paths_exist: bool,
        file_system_mode: &str,
        manifest_version: &str,
        exclude_patterns: Option<Vec<String>>,
    ) -> PyResult<Self> {
        let version: ManifestVersion = match manifest_version {
            "v2023-03-03" => ManifestVersion::V2023_03_03,
            "v2025-12-04-beta" => ManifestVersion::V2025_12_04_beta,
            _ => {
                return Err(PyValueError::new_err(format!(
                    "Invalid manifest version: {}. Use 'v2023-03-03' or 'v2025-12-04-beta'",
                    manifest_version
                )))
            }
        };

        let glob_filter: Option<GlobFilter> = if let Some(patterns) = exclude_patterns {
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
            self.inner.name, self.inner.path, self.location_type()
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
        format!("StorageProfile(locations={})", self.inner.local_locations().len() + self.inner.shared_locations().len())
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
    let manifest = model::Manifest::decode(json)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
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
        self.inner.version() == ManifestVersion::V2025_12_04_beta
    }
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

        let client: CrtStorageClient = CrtStorageClient::new(settings)
            .await
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to create storage client: {}", e)))?;

        // Create progress callback wrapper
        let progress: Option<PyProgressCallback> = callback.map(|cb| PyProgressCallback { callback: cb });

        // Call the Rust function
        let result = submit_bundle_attachments(
            &client,
            &s3_loc,
            &manifest_loc,
            &asset_refs,
            profile.as_ref(),
            &opts,
            progress.as_ref().map(|p| p as &dyn ProgressCallback<ScanProgress>),
            None, // upload progress (TODO: add separate callback)
        )
        .await
        .map_err(|e| StorageError::new_err(e.to_string()))?;

        // Convert to Python result
        let attachments_json: String = result
            .attachments
            .to_json()
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to serialize attachments: {}", e)))?;

        Ok(PyBundleSubmitResult {
            attachments_json,
            hashing_stats: result.hashing_stats,
            upload_stats: result.upload_stats,
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
    m.add("AttachmentError", m.py().get_type_bound::<AttachmentError>())?;
    m.add("StorageError", m.py().get_type_bound::<StorageError>())?;
    m.add("ValidationError", m.py().get_type_bound::<ValidationError>())?;

    // Classes
    m.add_class::<PyS3Location>()?;
    m.add_class::<PyManifestLocation>()?;
    m.add_class::<PyAssetReferences>()?;
    m.add_class::<PyBundleSubmitOptions>()?;
    m.add_class::<PyBundleSubmitResult>()?;
    m.add_class::<PySummaryStatistics>()?;
    m.add_class::<PyFileSystemLocation>()?;
    m.add_class::<PyStorageProfile>()?;
    m.add_class::<PyManifest>()?;

    // Functions
    m.add_function(wrap_pyfunction!(submit_bundle_attachments_py, m)?)?;
    m.add_function(wrap_pyfunction!(decode_manifest, m)?)?;

    Ok(())
}
