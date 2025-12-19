# Rusty Attachments: Model Module Design

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
│   └── model-design.md           # This document
├── crates/
│   ├── model/                    # Core manifest model
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── version.rs        # ManifestVersion, ManifestType enums
│   │       ├── hash.rs           # HashAlgorithm enum
│   │       ├── path.rs           # ManifestPath trait + implementations
│   │       ├── manifest.rs       # AssetManifest trait + implementations
│   │       ├── v2023_03_03.rs    # v1 format implementation
│   │       ├── v2025_12_04.rs    # v2 format implementation
│   │       ├── encode.rs         # Canonical JSON encoding
│   │       ├── decode.rs         # JSON decoding with validation
│   │       └── error.rs          # Error types
│   │
│   ├── python/                   # PyO3 bindings
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   │
│   └── wasm/                     # WASM bindings
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs
│
└── python/                       # Python package wrapper
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
    pub dirs: Vec<ManifestDirectoryPath>,
    #[serde(rename = "files")]
    pub paths: Vec<ManifestFilePath>,
    pub total_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_manifest_hash: Option<String>,
}
```

### Unified Manifest Enum

```rust
// manifest.rs

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
}
```

## Validation Rules

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

### Phase 1: Core Model (Week 1)
- [ ] Set up Cargo workspace
- [ ] Implement enums (version, hash algorithm, manifest type)
- [ ] Implement v2023-03-03 structs with serde
- [ ] Implement v2025-12-04-beta structs with serde
- [ ] Add validation logic
- [ ] Unit tests for serialization/deserialization

### Phase 2: Encoding/Decoding (Week 2)
- [ ] Implement canonical JSON encoding for v1
- [ ] Implement directory index compression for v2
- [ ] Implement decode with version detection
- [ ] Add validation during decode
- [ ] Roundtrip tests

### Phase 3: Python Bindings (Week 3)
- [ ] Set up PyO3 crate
- [ ] Expose Manifest enum to Python
- [ ] Expose encode/decode functions
- [ ] Python type stubs (.pyi files)
- [ ] Integration tests with existing Python code

### Phase 4: WASM Bindings (Week 4)
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

1. Should we support streaming decode for very large manifests?
2. Do we need async support for the bindings?
3. Should validation be opt-in or always-on during decode?
