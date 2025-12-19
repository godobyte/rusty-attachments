//! Python bindings for rusty-attachments.

use pyo3::prelude::*;
use rusty_attachments_model::{self as model, ManifestVersion};

/// Decode a manifest from JSON string.
#[pyfunction]
fn decode_manifest(json: &str) -> PyResult<PyManifest> {
    let manifest = model::Manifest::decode(json)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
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
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))
    }

    /// Get the manifest version string.
    #[getter]
    fn version(&self) -> &'static str {
        self.inner.version().as_str()
    }

    /// Get the hash algorithm string.
    #[getter]
    fn hash_alg(&self) -> &'static str {
        self.inner.hash_alg().as_str()
    }

    /// Get the total size of all files.
    #[getter]
    fn total_size(&self) -> u64 {
        self.inner.total_size()
    }

    /// Get the number of file entries.
    #[getter]
    fn file_count(&self) -> usize {
        self.inner.file_count()
    }

    /// Check if this is a v2023-03-03 manifest.
    fn is_v2023(&self) -> bool {
        self.inner.version() == ManifestVersion::V2023_03_03
    }

    /// Check if this is a v2025-12-04-beta manifest.
    fn is_v2025(&self) -> bool {
        self.inner.version() == ManifestVersion::V2025_12_04_beta
    }
}

/// Python module for rusty-attachments.
#[pymodule]
fn rusty_attachments(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(decode_manifest, m)?)?;
    m.add_class::<PyManifest>()?;
    Ok(())
}
