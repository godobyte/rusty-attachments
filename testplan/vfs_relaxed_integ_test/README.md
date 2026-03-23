# Integration Test Plan: Relaxed Consistency End-to-End

## What We're Testing

The relaxed consistency pipeline: VFS requests a file → SQS message → Upload Agent
picks it up → hashes + uploads to S3 CAS → writes completion marker → VFS polls
and finds the marker → VFS reads content from CAS.

Since the FUSE `read()` → resolve → poll → promote flow is not yet wired up in the
FUSE layer (it returns EIO for Relaxed files), we test the components that ARE
implemented end-to-end by composing them in a Python orchestrator that simulates
the VFS side.

## Test Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Test Orchestrator (testplan/run_integration.py)            │
│                                                             │
│  1. Generate ~1GB test data (various file sizes)            │
│  2. Create SQS queues                                       │
│  3. Start upload agent (background process)                 │
│  4. Send SQS file-request messages (simulating VFS)         │
│  5. Poll S3 for completion markers (simulating VFS poller)  │
│  6. Verify: marker content, CAS object, hash match          │
│  7. Cleanup                                                 │
└──────────┬──────────────────────────────────┬───────────────┘
           │ SQS messages                     │ S3 HEAD/GET
           ▼                                  ▼
┌──────────────────┐              ┌────────────────────┐
│  SQS Queues      │              │  S3 Bucket         │
│  (high + async)  │              │  adeadlineja/      │
└──────────┬───────┘              │  DeadlineCloud/    │
           │                      │    Data/           │
           ▼                      │    PendingUploads/ │
┌──────────────────┐              └────────────────────┘
│  Upload Agent    │                       ▲
│  (utils/         │───────────────────────┘
│   upload_agent.py│  hash + upload + marker
└──────────────────┘
```

## Test Data: ~1GB Across Various Sizes

| Category     | Count | Size Each  | Total   | Purpose                          |
|-------------|-------|------------|---------|----------------------------------|
| Tiny        | 100   | 1 KB       | 100 KB  | Metadata files, configs          |
| Small       | 50    | 100 KB     | 5 MB    | Scripts, small textures          |
| Medium      | 20    | 10 MB      | 200 MB  | Texture maps                     |
| Large       | 5     | 100 MB     | 500 MB  | Large textures, geometry caches  |
| XLarge      | 1     | 300 MB     | 300 MB  | Scene file, near chunk boundary  |
| **Total**   | **176** |          | **~1 GB** |                                |

## Test Cases

### TC1: Happy Path — All Files Uploaded Successfully
- Send all 176 file requests via high-priority queue
- Upload agent processes them
- Verify all 176 completion markers exist in S3
- Verify all 176 CAS objects exist
- Verify content hash in marker matches actual CAS key
- Verify file sizes in markers match source files

### TC2: Hash Consistency — Rust and Python Produce Same XXH128
- Hash a known set of files with both Python (xxhash) and Rust (xxhash-rust)
- Compare hex digests — they must be identical
- This validates the cross-language contract

### TC3: Idempotency — Duplicate Requests Don't Cause Errors
- Send the same file request twice
- Upload agent should process first, skip second (marker exists)
- Verify only one CAS upload occurred (via S3 request count or timing)

### TC4: File Not Found — Agent Writes Failure Marker
- Send a request for a file that doesn't exist on disk
- Upload agent should write a failure marker
- Verify marker has status="failed" with reason

### TC5: Priority Queues — High Priority Processed First
- Send 10 requests to async queue, then 10 to high queue
- Verify high-priority files are completed before async ones

### TC6: Path Key Determinism — Same Path Always Produces Same Key
- Compute path keys for the same relative paths from both Python and Rust
- Verify they match (this is the S3 key the VFS will poll)

### TC7: Large File Integrity — 300MB File Content Matches
- After upload, download the CAS object
- Hash the downloaded content
- Compare with the original file hash

## AWS Resources Created

- SQS: `deadline-farm-inttest-queue-inttest-file-requests-high`
- SQS: `deadline-farm-inttest-queue-inttest-file-requests-async`
- S3: `s3://adeadlineja/DeadlineCloud/Data/` (CAS objects)
- S3: `s3://adeadlineja/DeadlineCloud/PendingUploads/inttest-root/` (markers)

All test artifacts use the `inttest-root` root_id prefix for easy cleanup.

## Cleanup

The orchestrator cleans up:
1. All S3 objects under `DeadlineCloud/PendingUploads/inttest-root/`
2. All CAS objects uploaded during the test (tracked by hash)
3. Both SQS queues
4. Local test data directory
