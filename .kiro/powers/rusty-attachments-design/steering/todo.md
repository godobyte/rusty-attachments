# TODO Design Summary

**Full doc:** `design/todo.md`  
**Status:** Living document tracking implementation progress

## Purpose
Track remaining work, skipped features, and implementation status.

## Design Documents Status

### ✅ Completed
- Model design, Common module, Storage design
- Manifest storage, File system, Hash cache, S3 check cache
- Storage profiles, Job submission, Manifest utilities
- Path mapping, Bindings, Utilities
- VFS (read-only and writable)

## Implementation TODO

### Core
- [ ] Business logic to upload a manifest
- [ ] Logic to upload the manifest file
- [ ] File folder scanning, snapshot folder, diff a folder
- [ ] Manifest utilities: diff manifest, merge manifest

### Caches
- [ ] Hash cache SQLite backend
- [ ] S3 check cache SQLite backend

### Testing
- [ ] Fuzz testing with weird file paths
- [ ] Edge cases: merging manifests with time ordering
- [ ] Compatibility with Python manifest file names
- [ ] Roundtrip tests: Python create → Rust read → Python read

### Features
- [ ] S3 object tags for manifests
- [ ] Path mapping utilities
- [ ] Storage profile utilities

### CLI Design
- [ ] Disk capacity validation before download
- [ ] Detailed error guidance messages

### Bindings
- [ ] Python bindings (PyO3)
- [ ] WASM bindings

## Skipped Features (with rationale)

| Feature | Reason |
|---------|--------|
| AssetSync Orchestrator | Application-level, not core library |
| VFS Mode | Separate system component (worker-agent) |
| S3 Check Cache Integrity | Optimization, cache handles staleness via TTL |
| Local Manifest Writing | Debugging feature, snapshot mode covers offline |
| Asset Root Remapping | Convenience feature, path mapping is separate |
| S3 Key Fallback (no extension) | Backwards compat for very old data |
| Progress Tracker Threading | CRT handles internally, Rust async differs |
| Download Summary Extensions | Application-level reporting |
| Snapshot Mode (Local Copy) | Debugging/testing, can be CLI wrapper |

## Python Function Mapping

All Python functions from `deadline-cloud` have been analyzed and mapped to design documents. Key mappings:

| Python Function | Design Coverage |
|-----------------|-----------------|
| `get_manifest_from_s3` | manifest-storage.md |
| `get_output_manifests_by_asset_root` | manifest-storage.md |
| `download_files` | storage-design.md |
| `merge_asset_manifests` | manifest-utils.md |
| `_get_new_copy_file_path` | storage-design.md |

## When to Read Full Doc
- Checking implementation status
- Understanding skipped feature rationale
- Python function mapping details
- Planning next implementation phase
