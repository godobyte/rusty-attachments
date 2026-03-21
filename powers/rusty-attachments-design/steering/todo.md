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
- [x] Business logic to upload a manifest
- [x] Logic to upload the manifest file
- [x] File folder scanning, snapshot folder, diff a folder
- [x] Manifest utilities: diff manifest, merge manifest
- [x] Composable operations: compose, diff, filter, subtree, partition, join

### Caches
- [x] Hash cache SQLite backend
- [x] S3 check cache SQLite backend

### Testing
- [ ] Fuzz testing with weird file paths
- [ ] Edge cases: merging manifests with time ordering
- [ ] Compatibility with Python manifest file names
- [ ] Roundtrip tests: Python create → Rust read → Python read

### Features
- [ ] S3 object tags for manifests
- [x] Path mapping utilities (PathFormat in ja-deadline-utils, basic path utils in common)
- [ ] PathMappingApplier (trie-based) — not yet implemented
- [x] Storage profile utilities

### CLI
- [x] `ra manifest snapshot` — create snapshot manifest
- [x] `ra manifest diff` — diff directory against manifest
- [x] `ra attachment download` — download files from manifest
- [x] `ra attachment upload` — upload files from manifest
- [x] `ra config` — configuration management
- [ ] Disk capacity validation before download
- [ ] Detailed error guidance messages

### Bindings
- [x] Python bindings (PyO3) — Phase 1+2 complete
- [x] WASM bindings (manifest decode/encode)
- [ ] Worker sync bindings (sync_inputs, sync_outputs)

### Platform VFS
- [x] FUSE VFS (Linux/macOS) — read-only and writable
- [x] FSKit VFS (macOS 15.4+) — `crates/vfs-fskit/`
- [x] ProjFS VFS (Windows) — `crates/vfs-projfs/`

## Skipped Features (with rationale)

| Feature | Reason |
|---------|--------|
| AssetSync Orchestrator | Application-level, not core library |
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
