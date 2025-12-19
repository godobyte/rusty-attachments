# Rusty Attachments

Rust implementation of the Job Attachments manifest model for AWS Deadline Cloud, with Python and WASM bindings.

## Features

- **v2023-03-03**: Original manifest format with files only
- **v2025-12-04-beta**: Extended format with directories, symlinks, chunking, execute bit

## Project Structure

```
rusty-attachments/
├── crates/
│   ├── model/      # Core manifest model
│   ├── python/     # PyO3 bindings
│   └── wasm/       # WASM bindings
└── design/         # Design documents
```

## Building

```bash
# Build all crates
cargo build

# Run tests
cargo test

# Build Python wheel (requires maturin)
cd crates/python && maturin build

# Build WASM (requires wasm-pack)
cd crates/wasm && wasm-pack build
```

## Usage (Rust)

```rust
use rusty_attachments_model::Manifest;

let json = r#"{"hashAlg":"xxh128","manifestVersion":"2023-03-03","paths":[],"totalSize":0}"#;
let manifest = Manifest::decode(json)?;
println!("Version: {}", manifest.version());
```

## License

Apache-2.0
