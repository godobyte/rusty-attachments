# Pure-Rust CLI: `ra manifest`

## Commands

### `ra manifest snapshot`

Creates a manifest snapshot of a directory — the Rust equivalent of
`deadline manifest snapshot`.

```
ra manifest snapshot \
    --root <dir> \
    [--destination <dir>] \
    [--name <name>] \
    [--include <glob>]... \
    [--exclude <glob>]... \
    [--include-exclude-config <path>] \
    [--diff <manifest_file>] \
    [--force-rehash] \
    [--json]
```

#### Implementation

```
main()
  → parse_args()                          // clap
  → validate_inputs(root, destination)    // std::fs::metadata
  → build_glob_filter(include, exclude, config)  // crates/filesystem GlobFilter
  │
  ├─ [no --diff]
  │    → FileSystemScanner::snapshot()    // crates/filesystem
  │      → walk_directory()               // parallel via rayon
  │      → hash_file() per entry          // crates/common xxh128
  │      → HashCache lookup/store         // crates/storage hash_cache
  │    → write_manifest_to_dir()          // crates/model encode + std::fs::write
  │
  └─ [with --diff]
       → Manifest::decode(read(diff))     // crates/model
       → DiffEngine::diff()               // crates/filesystem
       │  ├─ [fast] mtime/size comparison
       │  └─ [force-rehash] hash comparison
       → DiffEngine::create_diff_manifest()
       → write_manifest_to_dir()
  │
  → format_output(result, json)           // output.rs
```

#### Python Parity

| Python Feature | Rust Equivalent |
|---|---|
| `_glob_paths()` with `GlobConfig` | `GlobFilter` from `crates/filesystem` |
| `_create_manifest_for_single_root()` | `FileSystemScanner::snapshot()` |
| `HashCache` (SQLite) | `crates/storage::hash_cache::SqliteHashCache` |
| `_fast_file_list_to_manifest_diff()` | `DiffEngine::diff(DiffMode::Fast)` |
| `compare_manifest()` (hash-based) | `DiffEngine::diff(DiffMode::Hash)` |
| `_write_manifest()` with timestamp naming | `write_manifest_to_dir()` with same naming convention |
| `decode_manifest()` | `Manifest::decode()` |
| Windows long path handling | `\\?\` prefix on Windows via `dunce` crate |

#### Improvements over Python

1. **Parallel directory walking** — `rayon` parallel iterator vs Python's
   sequential `os.walk`. 10-100x faster on large directory trees.

2. **Parallel hashing** — `rayon` thread pool vs Python's single-threaded
   `xxhash`. CPU-bound hashing scales linearly with cores.

3. **Hash cache with WAL** — SQLite WAL mode for concurrent reads during
   parallel hashing. Python's hash cache is single-threaded.

4. **Streaming diff** — `DiffEngine` walks and compares in a single pass.
   Python walks first, then diffs, doubling memory for file lists.

5. **Zero-copy manifest decode** — `serde` deserializes directly from bytes.
   Python reads to string, then parses JSON, then constructs objects.

---

### `ra manifest diff`

Computes file differences between a directory and an existing manifest.

```
ra manifest diff \
    --root <dir> \
    --manifest <file> \
    [--include <glob>]... \
    [--exclude <glob>]... \
    [--include-exclude-config <path>] \
    [--force-rehash] \
    [--json]
```

#### Implementation

```
main()
  → parse_args()
  → validate_inputs(root, manifest)
  → Manifest::decode(read(manifest))
  → build_glob_filter(include, exclude, config)
  → DiffEngine::diff(manifest, options)
  │  ├─ [fast] stat() each file, compare mtime/size
  │  └─ [force-rehash] hash each file, compare hashes
  → format_diff_output(result, json)
  │  ├─ [json] serde_json::to_string_pretty
  │  └─ [human] colored tree output (similar to Python's pretty_print_cli)
```

#### Improvements over Python

1. **No S3AssetManager instantiation** — Python creates an `S3AssetManager()`
   just to call `_create_manifest_file()` for hash-based diff. Rust uses
   `DiffEngine` directly.

2. **Single-pass diff** — Python globs files, then iterates manifest entries
   separately. Rust builds a lookup map from the manifest and walks the
   directory once.

3. **Colored tree output** — `termcolor` or `owo-colors` for terminal
   coloring without Python's `click.style()` overhead.

---

### `ra manifest download`

Downloads and merges manifests from S3 for a job.

```
ra manifest download <download_dir> \
    --job-id <id> \
    [--step-id <id>] \
    [--farm-id <id>] \
    [--queue-id <id>] \
    [--asset-type INPUT|OUTPUT|ALL] \
    [--profile <aws_profile>] \
    [--json]
```

#### Implementation

```
main()
  → parse_args()
  → load_config(farm_id, queue_id, profile)   // config.rs
  → resolve_credentials(profile)               // credentials.rs
  → deadline_api::get_queue(farm_id, queue_id) // deadline_api.rs
  → deadline_api::get_job(farm_id, queue_id, job_id)
  │
  ├─ [input manifests]
  │    → extract input_manifest_paths from job.attachments
  │    → for each: download_manifest(client, bucket, key)
  │
  ├─ [step dependencies]
  │    → deadline_api::list_step_dependencies()
  │    → for each dep: discover_output_manifest_keys() + download_manifest()
  │
  ├─ [output manifests]
  │    → discover_output_manifest_keys(scope)
  │    → for each: download_manifest()
  │
  → group manifests by asset root
  → merge_manifests() per root
  → encode + write to download_dir
  → format_output(results, json)
```

#### Improvements over Python

1. **Concurrent manifest downloads** — `tokio::join!` or `FuturesUnordered`
   for parallel S3 GET. Python downloads sequentially.

2. **No queue role session overhead** — Rust uses STS AssumeRole directly
   via `aws-sdk-sts`, avoiding boto3 session creation overhead.

3. **Streaming merge** — Manifests are merged as they arrive, not buffered
   in memory then merged at the end.

---

### `ra manifest upload`

Uploads a manifest file to S3.

```
ra manifest upload <manifest_file> \
    [--s3-cas-uri <uri>] \
    [--s3-manifest-prefix <prefix>] \
    [--farm-id <id>] \
    [--queue-id <id>] \
    [--profile <aws_profile>] \
    [--json]
```

#### Implementation

```
main()
  → parse_args()
  → resolve_s3_settings(s3_cas_uri, farm_id, queue_id)
  → resolve_credentials(profile)
  → read manifest_file to bytes
  → build S3 key: {cas_prefix}/Manifests/{prefix}/{filename}
  → build metadata: {"file-system-location-name": manifest_file}
  → StorageClient::put_object(bucket, key, bytes, metadata)
  → format_output(success, json)
```

#### Improvements over Python

1. **Direct CRT upload** — Single `put_object` call via CRT. Python wraps
   in `BytesIO`, passes through `S3AssetUploader.upload_bytes_to_s3()`.

2. **No session creation** — Credentials resolved once, reused for the
   single PUT operation.
