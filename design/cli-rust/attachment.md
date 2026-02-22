# Pure-Rust CLI: `ra attachment`

## Commands

### `ra attachment download`

Downloads CAS file contents referenced by local manifest files.

```
ra attachment download \
    -m <manifest_file>... \
    [--s3-root-uri <uri>] \
    [--path-mapping-rules <json_file>] \
    [--farm-id <id>] \
    [--queue-id <id>] \
    [--profile <aws_profile>] \
    [--conflict-resolution SKIP|OVERWRITE|CREATE_COPY] \
    [--json]
```

#### Implementation

```
main()
  → parse_args()
  → resolve_s3_settings(s3_root_uri, farm_id, queue_id, profile)
  → resolve_credentials(profile)
  → load_path_mapping_rules(path_mapping_rules)  // Option<Vec<PathMappingRule>>
  → resolve_conflict_resolution(flag, config)
  │
  → for each manifest_path:
  │    → Manifest::decode(read(manifest_path))
  │    → resolve_destination(manifest_filename, path_mapping_rules)
  │    → DownloadOrchestrator::download_manifest_contents(
  │          manifest, destination_root, conflict_resolution, progress_callback
  │      )
  │    → accumulate TransferStatistics
  │
  → format_output(total_stats, json)
```

#### Data Flow

```
┌──────────────┐     ┌──────────────┐     ┌──────────────────────┐
│ Local         │     │ Manifest     │     │ S3 CAS               │
│ manifest.json │────►│ decode()     │────►│ GET {hash}.xxh128    │
│               │     │ per file:    │     │ per file (parallel)  │
└──────────────┘     │  hash, path  │     └──────────┬───────────┘
                      │  size, mtime │                │
                      └──────────────┘                ▼
                                              ┌──────────────────┐
                                              │ Local filesystem  │
                                              │ write + mtime     │
                                              │ + permissions     │
                                              │ + conflict res.   │
                                              └──────────────────┘
```

#### Path Mapping Resolution

The manifest filename encodes the hashed source path. The download command
matches this hash against path mapping rules to determine the destination:

```rust
fn resolve_destination(
    manifest_filename: &str,
    rules: &[PathMappingRule],
    hash_alg: HashAlgorithm,
) -> String {
    for rule in rules {
        let hashed: String = hash_data(rule.source_path.as_bytes(), hash_alg);
        if manifest_filename.contains(&hashed) {
            return rule.destination_path.clone();
        }
    }
    // Fallback: current directory / manifest name
    format!("./{}", manifest_filename)
}
```

This matches the Python behavior in `_attachment_download()` where
`rule.get_hashed_source_path()` is compared against the filename.

#### Improvements over Python

1. **CRT parallel downloads** — `DownloadOrchestrator` uses CRT's internal
   connection pooling and multipart downloads. Python uses
   `ThreadPoolExecutor` limited by GIL and boto3 connection pool.

2. **Zero-copy file writes** — CRT streams directly to file descriptors.
   Python downloads to memory then writes.

3. **Parallel mtime restoration** — File metadata operations happen in
   parallel with downloads. Python restores mtime sequentially after each
   download.

4. **Conflict resolution in Rust** — `CREATE_COPY` suffix generation is
   done in Rust without Python string operations.

---

### `ra attachment upload`

Uploads CAS file contents referenced by local manifest files.

```
ra attachment upload \
    -m <manifest_file>... \
    -r <root_dir>... \
    [--path-mapping-rules <json_file>] \
    [--s3-root-uri <uri>] \
    [--upload-manifest-path <prefix>] \
    [--farm-id <id>] \
    [--queue-id <id>] \
    [--profile <aws_profile>] \
    [--json]
```

#### Implementation

```
main()
  → parse_args()
  → validate: exactly one of (path_mapping_rules, root_dirs) must be provided
  → resolve_s3_settings(s3_root_uri, farm_id, queue_id, profile)
  → resolve_credentials(profile)
  → load_path_mapping_rules(path_mapping_rules, root_dirs)
  → open_s3_check_cache(config.cache_dir)
  │
  → for each (manifest_path, rule) in zip(manifests, rules):
  │    → Manifest::decode(read(manifest_path))
  │    → build_s3_metadata(rule.source_path, rule.source_path_format)
  │    → UploadOrchestrator::upload_manifest_contents(
  │          manifest, rule.destination_path, progress_callback
  │      )
  │    → upload_input_manifest(manifest, source_root, metadata)
  │    → collect UploadManifestInfo { s3_key, hash, source_path }
  │
  → format_output(results, json)
```

#### S3 Metadata Construction

Matches the Python behavior for ASCII/non-ASCII asset root encoding:

```rust
fn build_s3_metadata(asset_root: &str, location_name: Option<&str>) -> ManifestS3Metadata {
    match location_name {
        Some(name) => ManifestS3Metadata::with_location(asset_root, name),
        None => ManifestS3Metadata::new(asset_root),
    }
}
```

The `ManifestS3Metadata` type in `crates/storage` already handles the
ASCII vs JSON-encoded metadata distinction.

#### Improvements over Python

1. **S3CheckCache with batch lookups** — `exists_batch()` checks multiple
   keys in a single SQLite query. Python checks one at a time.

2. **CRT multipart uploads** — Large files use CRT's automatic multipart
   with configurable part size. Python uses boto3's `upload_file()` which
   has higher overhead per part.

3. **Pipelined hash+upload** — Files are hashed and uploaded concurrently.
   Python hashes all files first, then uploads all files.

4. **No S3AssetUploader wrapper** — Direct `UploadOrchestrator` call
   eliminates the Python class instantiation and method dispatch overhead.
