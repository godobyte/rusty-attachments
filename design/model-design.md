# Rusty Attachments: Model Module Design

**Status: ✅ IMPLEMENTED** - See `crates/model/`

## Overview

This document outlines the design for a Rust implementation of the Job Attachments manifest model with Python (PyO3) and WASM (wasm-bindgen) bindings.

## Goals

1. **Performance**: Rust-native parsing/encoding for large manifests (1M+ files)
2. **Cross-platform**: Single codebase for Python, WASM, and native Rust
3. **Compatibility**: Support both v2023-03-03 and v2025-12-04-beta formats
4. **Type Safety**: Strong typing with serde for JSON serialization

## Project Structure

```
rusty-attachments/
├── Cargo.toml                    # Workspace root
├── design/
│   ├── model-design.md           # This document
│   ├── storage-design.md         # Storage abstraction design
│   └── upload.md                 # Original upload prototype
├── crates/
│   ├── model/                    # Core manifest model ✅ IMPLEMENTED
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── version.rs        # ManifestVersion, ManifestType enums
│   │       ├── hash.rs           # HashAlgorithm enum
│   │       ├── manifest.rs       # Manifest enum wrapper
│   │       ├── v2023_03_03.rs    # v1 format implementation
│   │       ├── v2025_12_04.rs    # v2 format implementation
│   │       ├── encode.rs         # Canonical JSON encoding
│   │       ├── decode.rs         # JSON decoding with validation
│   │       └── error.rs          # Error types
│   │
│   ├── storage/                  # Storage abstraction ✅ IMPLEMENTED
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types.rs          # Shared data structures
│   │       ├── traits.rs         # StorageClient trait
│   │       ├── cas.rs            # CAS key generation, chunking logic
│   │       └── error.rs          # Error types
│   │
│   ├── python/                   # PyO3 bindings (pending)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   │
│   └── wasm/                     # WASM bindings (pending)
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs
│
└── python/                       # Python package wrapper (pending)
    ├── pyproject.toml
    └── rusty_attachments/
        └── __init__.py
```

## Core Data Structures

### Enums

```rust
// version.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestVersion {
    #[serde(rename = "2023-03-03")]
    V2023_03_03,
    #[serde(rename = "2025-12-04-beta")]
    V2025_12_04_beta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestType {
    Snapshot,
    Diff,
}

// hash.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HashAlgorithm {
    #[serde(rename = "xxh128")]
    Xxh128,
}
```

### Base Traits

```rust
// path.rs

/// Common interface for manifest path entries
pub trait ManifestPathEntry {
    fn path(&self) -> &str;
    fn hash(&self) -> Option<&str>;
    fn size(&self) -> Option<u64>;
    fn mtime(&self) -> Option<i64>;
    fn is_deleted(&self) -> bool;
}

/// Extended interface for v2 path entries
pub trait ManifestPathEntryV2: ManifestPathEntry {
    fn runnable(&self) -> bool;
    fn chunkhashes(&self) -> Option<&[String]>;
    fn symlink_target(&self) -> Option<&str>;
}
```

### V1 Format (v2023-03-03)

```rust
// v2023_03_03.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPath {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetManifest {
    pub hash_alg: HashAlgorithm,
    pub manifest_version: ManifestVersion,
    pub paths: Vec<ManifestPath>,
    pub total_size: u64,
}
```

### V2 Format (v2025-12-04-beta)

```rust
// v2025_12_04.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestDirectoryPath {
    pub path: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFilePath {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub runnable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunkhashes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetManifest {
    pub hash_alg: HashAlgorithm,
    pub manifest_version: ManifestVersion,
    /// Manifest type: Snapshot (full state) or Diff (changes only).
    /// Defaults to Snapshot for backwards compatibility.
    #[serde(default)]
    pub manifest_type: ManifestType,
    pub dirs: Vec<ManifestDirectoryPath>,
    #[serde(rename = "files")]
    pub paths: Vec<ManifestFilePath>,
    pub total_size: u64,
    /// Hash of the parent manifest (required for Diff type, None for Snapshot).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_manifest_hash: Option<String>,
}
```

### Unified Manifest Enum

```rust
// manifest.rs

use rusty_attachments_common::VersionNotCompatibleError;

/// Version-agnostic manifest wrapper
#[derive(Debug, Clone)]
pub enum Manifest {
    V2023_03_03(v2023_03_03::AssetManifest),
    V2025_12_04_beta(v2025_12_04::AssetManifest),
}

impl Manifest {
    pub fn decode(json: &str) -> Result<Self, ManifestError>;
    pub fn encode(&self) -> Result<String, ManifestError>;
    pub fn version(&self) -> ManifestVersion;
    pub fn hash_alg(&self) -> HashAlgorithm;
    pub fn total_size(&self) -> u64;
    pub fn file_count(&self) -> usize;
    
    /// Get the manifest type (Snapshot or Diff).
    /// 
    /// # Note
    /// v2023-03-03 only supports Snapshot type.
    pub fn manifest_type(&self) -> ManifestType {
        match self {
            // COMPAT: v2023-03-03 does not support Diff manifests
            Manifest::V2023_03_03(_) => ManifestType::Snapshot,
            Manifest::V2025_12_04_beta(m) => m.manifest_type,
        }
    }
    
    /// Get file entries as v2025 format.
    /// 
    /// # Errors
    /// Returns `VersionNotCompatibleError` for v2023-03-03 manifests.
    /// Use `paths_v2023()` for v2023 manifests instead.
    pub fn files(&self) -> Result<&[v2025_12_04::ManifestFilePath], VersionNotCompatibleError> {
        match self {
            // COMPAT: v2023-03-03 uses different path type
            Manifest::V2023_03_03(_) => Err(VersionNotCompatibleError::new(
                "files() with v2025 format",
                "v2025-12-04-beta",
            )),
            Manifest::V2025_12_04_beta(m) => Ok(&m.paths),
        }
    }
    
    /// Get directory entries.
    /// 
    /// # Errors
    /// Returns `VersionNotCompatibleError` for v2023-03-03 manifests.
    pub fn dirs(&self) -> Result<&[v2025_12_04::ManifestDirectoryPath], VersionNotCompatibleError> {
        match self {
            // COMPAT: v2023-03-03 does not track directories
            Manifest::V2023_03_03(_) => Err(VersionNotCompatibleError::new(
                "directory entries",
                "v2025-12-04-beta",
            )),
            Manifest::V2025_12_04_beta(m) => Ok(&m.dirs),
        }
    }
    
    /// Get chunk hashes for a file entry.
    /// 
    /// # Errors
    /// Returns `VersionNotCompatibleError` for v2023-03-03 manifests.
    pub fn supports_chunking(&self) -> bool {
        match self {
            // COMPAT: v2023-03-03 does not support chunked files
            Manifest::V2023_03_03(_) => false,
            Manifest::V2025_12_04_beta(_) => true,
        }
    }
    
    /// Get v2023 paths (for v2023 manifests only).
    /// 
    /// # Errors
    /// Returns error for v2025 manifests.
    pub fn paths_v2023(&self) -> Result<&[v2023_03_03::ManifestPath], VersionNotCompatibleError> {
        match self {
            Manifest::V2023_03_03(m) => Ok(&m.paths),
            Manifest::V2025_12_04_beta(_) => Err(VersionNotCompatibleError::new(
                "paths_v2023() on v2025 manifest",
                "v2023-03-03",
            )),
        }
    }
    
    /// Get the parent manifest hash (for diff manifests).
    /// 
    /// Returns `None` for snapshot manifests or v2023 manifests.
    pub fn parent_manifest_hash(&self) -> Option<&str> {
        match self {
            // COMPAT: v2023-03-03 does not support diff manifests
            Manifest::V2023_03_03(_) => None,
            Manifest::V2025_12_04_beta(m) => m.parent_manifest_hash.as_deref(),
        }
    }
}
```

## Validation Rules

### Manifest Type Validation

The `manifest_type` field determines how a manifest should be interpreted:

```rust
/// Manifest type determines the semantics of the manifest content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ManifestType {
    /// Full snapshot of directory state. All files are present.
    #[default]
    Snapshot,
    /// Diff against a parent manifest. Contains only changes.
    Diff,
}
```

**Consumer Usage:**

```rust
use rusty_attachments_model::{Manifest, ManifestType};

fn process_manifest(manifest: &Manifest) -> Result<(), Error> {
    match manifest.manifest_type() {
        ManifestType::Snapshot => {
            // Full manifest - all files are present
            // Can be used directly for download
            download_all_files(manifest)?;
        }
        ManifestType::Diff => {
            // Diff manifest - must be applied to parent first
            let parent_hash = manifest.parent_manifest_hash()
                .ok_or(Error::MissingParentHash)?;
            
            // Fetch and decode parent manifest
            let parent = fetch_manifest_by_hash(parent_hash)?;
            
            // Apply diff to get full snapshot
            let resolved = apply_diff_manifest(&parent, manifest)?;
            download_all_files(&resolved)?;
        }
    }
    Ok(())
}
```

**Validation Rules:**
- `Diff` type MUST have `parent_manifest_hash` set
- `Snapshot` type SHOULD NOT have `parent_manifest_hash` (ignored if present)
- Deleted entries (`deleted: true`) are only valid in `Diff` manifests

### V2 Path Entry Validation

```rust
impl ManifestFilePath {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.deleted {
            // Deleted entries can only have path
            if self.hash.is_some() || self.chunkhashes.is_some() 
               || self.symlink_target.is_some() || self.runnable 
               || self.size.is_some() || self.mtime.is_some() {
                return Err(ValidationError::DeletedEntryHasFields);
            }
        } else {
            // Must have exactly one of: hash, chunkhashes, symlink_target
            let content_fields = [
                self.hash.is_some(),
                self.chunkhashes.is_some(),
                self.symlink_target.is_some(),
            ];
            if content_fields.iter().filter(|&&x| x).count() != 1 {
                return Err(ValidationError::InvalidContentFields);
            }
            
            // Non-symlink entries need size and mtime
            if self.symlink_target.is_none() {
                if self.size.is_none() || self.mtime.is_none() {
                    return Err(ValidationError::MissingMetadata);
                }
            }
            
            // Chunkhashes validation
            if let Some(ref chunks) = self.chunkhashes {
                self.validate_chunkhashes(chunks)?;
            }
            
            // Symlink target validation
            if let Some(ref target) = self.symlink_target {
                self.validate_symlink_target(target)?;
            }
        }
        Ok(())
    }
}
```

## Canonical JSON Encoding

For v2 format, implement directory index compression:

```rust
impl AssetManifest {
    pub fn encode(&self) -> String {
        // 1. Sort directories lexicographically by full path
        // 2. Build directory index: path -> index
        // 3. Sort files by UTF-16 BE encoding
        // 4. Encode paths with $N/ references
        // 5. Output canonical JSON (sorted keys, no whitespace)
    }
}
```

## Implementation Plan

### Phase 1: Core Model ✅ COMPLETE
- [x] Set up Cargo workspace
- [x] Implement enums (version, hash algorithm, manifest type)
- [x] Implement v2023-03-03 structs with serde
- [x] Implement v2025-12-04-beta structs with serde
- [x] Add validation logic
- [x] Unit tests for serialization/deserialization

### Phase 2: Encoding/Decoding ✅ COMPLETE
- [x] Implement canonical JSON encoding for v1
- [x] Implement directory index compression for v2
- [x] Implement decode with version detection
- [x] Add validation during decode
- [x] Roundtrip tests

### Phase 3: Python Bindings (Pending)
- [ ] Set up PyO3 crate
- [ ] Expose Manifest enum to Python
- [ ] Expose encode/decode functions
- [ ] Python type stubs (.pyi files)
- [ ] Integration tests with existing Python code

### Phase 4: WASM Bindings (Pending)
- [ ] Set up wasm-bindgen crate
- [ ] Expose to JavaScript/TypeScript
- [ ] Build and publish npm package
- [ ] Browser and Node.js tests

## Dependencies

```toml
# crates/model/Cargo.toml
[dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
thiserror = "1.0"

[dev-dependencies]
pretty_assertions = "1.0"

# crates/python/Cargo.toml
[dependencies]
pyo3 = { version = "0.20", features = ["extension-module"] }
rusty-attachments-model = { path = "../model" }

# crates/wasm/Cargo.toml
[dependencies]
wasm-bindgen = "0.2"
serde-wasm-bindgen = "0.6"
rusty-attachments-model = { path = "../model" }
```

## Testing Strategy

1. **Unit Tests**: Each struct's validation, serialization
2. **Roundtrip Tests**: encode → decode → encode produces identical output
3. **Compatibility Tests**: Parse manifests from Python implementation
4. **Fuzz Tests**: Property-based testing with arbitrary manifests
5. **Benchmark Tests**: Performance comparison with Python implementation

## Open Questions

1. ~~Should we support streaming decode for very large manifests?~~ **Resolved:** Chunking is sufficient, no streaming needed.
2. ~~Do we need async support for the bindings?~~ **Resolved:** Yes, storage operations use async. Model crate remains sync.
3. ~~Should validation be opt-in or always-on during decode?~~ **Resolved:** Validation is always-on during decode.

---

## Related Documents

- [storage-design.md](storage-design.md) - Storage abstraction for S3 operations (upload/download)
- [upload.md](upload.md) - Original upload prototype (superseded by storage-design.md)
