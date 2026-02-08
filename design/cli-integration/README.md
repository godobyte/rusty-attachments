# CLI Integration Design: rusty-attachments ↔ deadline-cloud

## Core Prompt & Direction

This directory contains the analysis of each CLI command group in `deadline-cloud` and the
design for where to integrate `rusty-attachments` (Rust) to replace the Python job attachments
primitives. The goal is to find the **min-cut / max-flow** boundary — the narrowest interface
between the Python CLI/orchestration layer and the compute-heavy primitives that Rust replaces.

## Methodology

For each CLI group we trace the call chain:

```
CLI (click commands)
  → API layer (business logic)
    → Primitives (hashing, diffing, globbing, S3 I/O, manifest encode/decode)
```

The **cut line** is placed so that:
1. Python keeps: CLI argument parsing, config/credential resolution, boto3 session setup,
   user prompts, progress bar rendering, Deadline API calls (CreateJob, GetQueue, etc.)
2. Rust takes: file scanning/globbing, hashing, manifest creation/diff/merge/encode/decode,
   CAS upload/download, S3 check cache, hash cache, path grouping by storage profile.

## Command Groups Analyzed

| Document | CLI Group | Key Commands |
|----------|-----------|-------------|
| [manifest.md](manifest.md) | `deadline manifest` | snapshot, diff, download, upload |
| [attachment.md](attachment.md) | `deadline attachment` | upload, download |
| [bundle-submit.md](bundle-submit.md) | `deadline bundle submit` | submit (+ gui-submit) |
| [others.md](others.md) | all other groups | download-output, sync-output, handle-web-url + non-beneficial |
| [pyo3-bindings.md](pyo3-bindings.md) | PyO3 API design | data structures, binding functions, integration flow |

## Shared Integration Pattern

All three CLIs converge on the same Rust boundary. The PyO3 bindings crate (`crates/python/`)
exposes async functions that accept pre-resolved credentials and return structured results.
Python remains responsible for:

- `click` CLI framework, argument parsing, `--json` output formatting
- `_apply_cli_options_to_config()` → ConfigParser resolution
- `boto3.Session` creation and queue-role assumption
- `_ProgressBarCallbackManager` rendering
- Deadline service API calls (GetQueue, GetJob, ListStepDependencies, CreateJob)

Rust takes over at the point where file I/O and S3 data transfer begin.
