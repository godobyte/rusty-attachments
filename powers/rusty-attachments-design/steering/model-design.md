# Model Design Summary

**Full doc:** `design/model-design.md`  
**Status:** ✅ IMPLEMENTED in `crates/model/`

## Purpose
Defines manifest data structures for v2023-03-03 and v2025-12-04-beta formats with serde serialization.

## Key Types

### Enums
- `ManifestVersion`: V2023_03_03, V2025_12_04_beta
- `ManifestType`: Snapshot (full state), Diff (changes only)
- `HashAlgorithm`: Xxh128

### V2023 Format
- `ManifestPath`: path, hash, size, mtime
- `AssetManifest`: hash_alg, manifest_version, paths[], total_size

### V2025 Format
- `ManifestFilePath`: path, hash?, size?, mtime?, runnable, chunkhashes?, symlink_target?, deleted
- `ManifestDirectoryPath`: path, deleted
- `AssetManifest`: adds dirs[], manifest_type, parent_manifest_hash?

### Unified Wrapper
```rust
enum Manifest {
    V2023_03_03(v2023::AssetManifest),
    V2025_12_04_beta(v2025::AssetManifest),
}
```

## Validation Rules
- Diff type MUST have parent_manifest_hash
- Non-deleted entries need exactly one of: hash, chunkhashes, symlink_target
- Non-symlink entries need size and mtime

## When to Read Full Doc
- Implementing manifest parsing/encoding
- Adding new manifest fields
- Understanding version compatibility
