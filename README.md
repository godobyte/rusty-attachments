# Rusty Attachments

Rust implementation of the Job Attachments manifest model for AWS Deadline Cloud, with Python and WASM bindings.

## Features

- **v2023-03-03**: Original manifest format with files only
- **v2025-12-04-beta**: Extended format with directories, symlinks, chunking, execute bit

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── common/     # Shared utilities (path, hash, progress)
│   ├── model/      # Core manifest model
│   ├── storage/    # S3 storage abstraction
│   ├── python/     # PyO3 bindings
│   └── wasm/       # WASM bindings
└── design/         # Design documents
```

## Crates

| Crate | Description |
|-------|-------------|
| `rusty-attachments-common` | Path utilities, hash functions, progress callbacks, constants |
| `rusty-attachments-model` | Manifest structures, encode/decode, validation |
| `rusty-attachments-storage` | S3 storage traits, CAS utilities, transfer types |

## Building

```bash
# Build all crates
cargo build

# Build a specific crate
cargo build -p rusty-attachments-common
cargo build -p rusty-attachments-model
cargo build -p rusty-attachments-storage

# Check without building (faster)
cargo check
cargo check -p rusty-attachments-common
```

## Testing

```bash
# Run all tests
cargo test

# Test a specific crate
cargo test -p rusty-attachments-common
cargo test -p rusty-attachments-model
cargo test -p rusty-attachments-storage

# Run tests with output
cargo test -- --nocapture
```

## Usage (Rust)

```rust
use rusty_attachments_model::Manifest;
use rusty_attachments_common::{hash_file, normalize_for_manifest};

// Decode a manifest
let json = r#"{"hashAlg":"xxh128","manifestVersion":"2023-03-03","paths":[],"totalSize":0}"#;
let manifest = Manifest::decode(json)?;
println!("Version: {}", manifest.version());

// Hash a file
let hash = hash_file(Path::new("file.txt"))?;

// Normalize path for manifest storage
let manifest_path = normalize_for_manifest(Path::new("/project/assets/file.txt"), Path::new("/project"))?;
assert_eq!(manifest_path, "assets/file.txt");
```

## Python/WASM Bindings

```bash
# Build Python wheel (requires maturin)
cd crates/python && maturin build

# Build WASM (requires wasm-pack)
cd crates/wasm && wasm-pack build
```

## License

Apache-2.0
