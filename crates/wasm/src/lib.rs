//! WASM bindings for rusty-attachments.

use rusty_attachments_model::{self as model, ManifestVersion};
use wasm_bindgen::prelude::*;

/// Decode a manifest from JSON string.
#[wasm_bindgen]
pub fn decode_manifest(json: &str) -> Result<Manifest, JsError> {
    let manifest = model::Manifest::decode(json).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(Manifest { inner: manifest })
}

/// WASM wrapper for Manifest.
#[wasm_bindgen]
pub struct Manifest {
    inner: model::Manifest,
}

#[wasm_bindgen]
impl Manifest {
    /// Encode the manifest to canonical JSON string.
    pub fn encode(&self) -> Result<String, JsError> {
        self.inner
            .encode()
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Get the manifest version string.
    #[wasm_bindgen(getter)]
    pub fn version(&self) -> String {
        self.inner.version().as_str().to_string()
    }

    /// Get the hash algorithm string.
    #[wasm_bindgen(getter, js_name = "hashAlg")]
    pub fn hash_alg(&self) -> String {
        self.inner.hash_alg().as_str().to_string()
    }

    /// Get the total size of all files.
    #[wasm_bindgen(getter, js_name = "totalSize")]
    pub fn total_size(&self) -> u64 {
        self.inner.total_size()
    }

    /// Get the number of file entries.
    #[wasm_bindgen(getter, js_name = "fileCount")]
    pub fn file_count(&self) -> usize {
        self.inner.file_count()
    }

    /// Check if this is a v2023-03-03 manifest.
    #[wasm_bindgen(js_name = "isV2023")]
    pub fn is_v2023(&self) -> bool {
        self.inner.version() == ManifestVersion::V2023_03_03
    }

    /// Check if this is a v2025-12-04-beta manifest.
    #[wasm_bindgen(js_name = "isV2025")]
    pub fn is_v2025(&self) -> bool {
        self.inner.version() == ManifestVersion::V2025_12_04_beta
    }
}
