# Pure-Rust CLI Design: `ra` (rusty-attachments)

## Overview

A standalone Rust CLI binary (`ra`) that provides 1:1 functional equivalence
with the deadline-cloud Python CLI's job-attachment commands, but runs entirely
in Rust for maximum performance. No Python runtime required.

The CLI is designed for:
1. Direct use by power users who want faster attachment operations
2. Benchmarking Rust vs Python performance on identical workloads
3. Integration testing of the Rust storage/filesystem/model crates
4. CI/CD pipelines where Python installation is undesirable

## Crate: `crates/cli`

```
crates/cli/
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point, clap app definition
│   ├── config.rs             # Configuration loading (profile, region, credentials)
│   ├── credentials.rs        # AWS credential resolution (env, profile, IMDS)
│   ├── output.rs             # JSON / human-readable output formatting
│   ├── progress.rs           # Terminal progress bar rendering
│   ├── commands/
│   │   ├── mod.rs
│   │   ├── manifest.rs       # manifest snapshot, diff, download, upload
│   │   ├── attachment.rs     # attachment download, upload
│   │   ├── submit.rs         # bundle submit
│   │   ├── download_output.rs # job download-output
│   │   ├── sync_output.rs    # queue sync-output
│   │   └── config_cmd.rs     # config show, set, get, benchmark
│   └── deadline_api.rs       # Thin Deadline API client (GetQueue, GetJob, etc.)
```

## Command Structure

```
ra manifest snapshot   --root <dir> [--destination <dir>] [--diff <manifest>] ...
ra manifest diff       --root <dir> --manifest <file> [--force-rehash] ...
ra manifest download   <download_dir> --job-id <id> [--step-id <id>] ...
ra manifest upload     <manifest_file> [--s3-cas-uri <uri>] ...
ra attachment download -m <manifest>... [--s3-root-uri <uri>] ...
ra attachment upload   -m <manifest>... -r <root_dir>... [--s3-root-uri <uri>] ...
ra submit              <job_bundle_dir> [--parameter <k>=<v>]... ...
ra download-output     --job-id <id> [--step-id <id>] [--task-id <id>] ...
ra sync-output         [--checkpoint-dir <dir>] ...
ra config show
ra config set <key> <value>
ra config get <key>
ra config benchmark    [--operation snapshot|upload|download] [--file-count N] ...
```

## Design Documents

| Document | CLI Commands |
|----------|-------------|
| [manifest.md](manifest.md) | `ra manifest snapshot`, `diff`, `download`, `upload` |
| [attachment.md](attachment.md) | `ra attachment download`, `upload` |
| [submit.md](submit.md) | `ra submit` |
| [download-output.md](download-output.md) | `ra download-output` |
| [sync-output.md](sync-output.md) | `ra sync-output` |
| [config.md](config.md) | `ra config show/set/get/benchmark` |

## Shared Infrastructure

All commands share:
- `config.rs` — reads `~/.deadline/config` (same format as Python CLI)
- `credentials.rs` — AWS credential chain: env vars → profile → IMDS
- `deadline_api.rs` — minimal Deadline API client using `aws-sdk-deadline`
  for GetQueue, GetJob, ListStepDependencies, CreateJob, SearchJobs
- `output.rs` — `--json` flag support, human-readable formatting
- `progress.rs` — `indicatif` progress bars matching Python's click output

## Key Differences from Python CLI

1. No `click` framework — uses `clap` derive macros
2. No `boto3` — uses `aws-sdk-deadline` and CRT-based S3 client
3. No `asyncio.run()` bridge — native tokio async throughout
4. No GIL — true parallelism for hashing, scanning, and S3 transfers
5. Single binary — no virtualenv, no pip install, no dependency conflicts
