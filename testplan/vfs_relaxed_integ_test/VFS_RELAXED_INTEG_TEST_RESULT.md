# Integration Test Results

**Date:** 2026-03-22  
**Account:** 224071664257 (Admin role)  
**Region:** us-west-2  
**Bucket:** adeadlineja  
**Test Data:** 84 files, 702 MB across 5 size categories

## Results: 8/8 PASSED

| Test | Result | Duration | Notes |
|------|--------|----------|-------|
| TC6: Path Key Determinism | PASS | 0.0s | 84 keys verified deterministic |
| TC2: Hash Consistency (Python) | PASS | 0.0s | XXH128 file hashes match in-memory hashes |
| TC2b: Rust Hash Unit Tests | PASS | 1.5s | All relaxed module unit tests pass |
| TC1: Happy Path | PASS | 54.3s | 84/84 files uploaded, markers verified, CAS verified |
| TC3: Idempotency | PASS | 10.3s | Duplicate request handled, marker unchanged |
| TC4: File Not Found | PASS | 3.1s | Failure marker written with correct reason |
| TC5: Priority Ordering | PASS | 22.7s | All 10 files completed |
| TC7: Large File Integrity | PASS | 10.3s | 300MB file downloaded, hash verified end-to-end |

## Bug Found and Fixed During Testing

### BUG: Upload Agent Stalls on Large File Batches

**Symptom:** Agent stuck at 155/176 files processed. No progress for 5+ minutes.

**Root Cause:** `poll_queue()` received 10 SQS messages per batch (`max_messages=10`)
and processed them sequentially. For large files (100MB+), a single hash+upload takes
30-60 seconds. By the time the agent reached message #5 in a batch, messages #6-10
had been sitting for 2-3 minutes. The 300-second SQS visibility timeout would expire
for early messages while the agent was still uploading a later one, causing those
messages to become visible again and get re-received — creating a stall loop where
the agent kept re-receiving the same messages without making progress.

**Fix applied to `utils/upload_agent.py`:**
1. Changed default `max_messages` from 10 to 1 — process one message at a time
2. Added visibility timeout extension for files >50MB — estimates processing time
   based on file size and calls `change_message_visibility` before starting the
   hash+upload

**Impact:** This is the exact bug flagged in the performance review as "sequential
upload agent is the single biggest performance constraint." The batch-of-10 approach
made it worse than pure sequential because of the visibility timeout interaction.

## AWS Resources Used

- SQS: 2 queues created and deleted (high + async priority)
- S3: 179 objects created and deleted (84 markers + ~84 CAS objects + priority test files)
- All resources cleaned up successfully

## Cleanup Verification

- 179 S3 objects deleted
- 2 SQS queues deleted
- Temp directory removed
