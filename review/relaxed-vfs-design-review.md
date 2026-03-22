# Design & Code Review: Relaxed Consistency for On-Demand File Fetching

**Reviewer:** PE / Sr Staff  
**Date:** 2026-03-22  
**Scope:** Commits `2ff366c..abe88e2` (4 days, ~4,700 LOC added across 23 files)  
**Feature:** Async Upload — Relaxed Consistency Roots for VFS

---

## Executive Summary

This is a well-conceived feature that addresses a real customer pain point: studios with TBs of on-prem data shouldn't have to upload everything before submitting a render job. The design introduces a "relaxed consistency" mode where files are fetched on-demand from on-prem storage via an SQS-mediated upload agent, then promoted to strongly-consistent CAS objects once available.

The architecture is sound. The code quality is above average. There are several design-level concerns and a handful of code-level issues that should be addressed before this ships.

**Overall verdict:** Approve with requested changes. The foundation is solid — the issues below are refinements, not rewrites.

---

## Part 1: Design Review

### What's Good

1. **The two-level indirection (path key → completion marker → CAS) is the right call.** It preserves CAS deduplication semantics while giving the VFS a stable key to poll. The alternative (storing content at the path key) would have broken dedup and created a parallel storage model. This was correctly identified and rejected.

2. **The auto-vivify approach for INode creation is pragmatic.** FUSE resolves paths one component at a time, and without a directory listing, you can't distinguish `project/` from `file.png` at lookup time. Creating ambiguous directory INodes and converting on `open()` is the right trade-off for V1. The design doc explains this clearly.

3. **Separation of concerns is clean.** The `RelaxedFileStore` trait, `PendingFileTracker`, `RootPathResolver`, and `MemoryRelaxedStore` each have a single responsibility. The pyramid architecture (primitives → composition → CLI) is followed. The trait-based design allows testing without S3/SQS.

4. **Forward compatibility is well-planned.** `UploadCompletionMarker.chunk_hashes` is `Option<Vec<String>>` for V2 chunked upload. The SQS message has a `version` field. `FileContent::Relaxed` promotion handles both `SingleHash` and `Chunked`. These are the right seams for future evolution.

5. **The storage profile integration is thoughtful.** Both mapped (with `fileSystemLocationName`) and unmapped (dynamic `assetroot-{hash}`) cases are handled. The `relative_path` invariant — identical on submitter and worker — is a clean abstraction.

### Design Concerns

#### D1: The `MarkerEnvelope` is a Stringly-Typed Union — Use a Tagged Enum

The `MarkerEnvelope` struct uses `status: String` with `"completed"` or `"failed"` and a bag of `Option` fields. This is fragile — a typo in the status string silently produces a struct where all the important fields are `None`.

**Recommendation:** Use `#[serde(tag = "status")]` for a proper tagged enum:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum MarkerEnvelope {
    #[serde(rename = "completed")]
    Completed(UploadCompletionMarker),
    #[serde(rename = "failed")]
    Failed(UploadFailureMarker),
}
```

This eliminates the `Option` soup and makes deserialization fail loudly on unknown status values. The `UploadCompletionMarker` and `UploadFailureMarker` structs already exist — just remove the redundant `status` field from each and let the enum tag carry it.

**Severity:** Medium. This will bite you when someone adds a third status or misspells "completed".

#### D2: No Timeout Enforcement on Pending Files

The design doc specifies `max_wait_timeout` (default 30 min) and the `PendingFileTracker` records `requested_at`, but there is no code that actually enforces the timeout. The background poller increments `poll_count` but never checks elapsed time or evicts timed-out entries.

A VFS `read()` that blocks on a relaxed file will hang indefinitely if the upload agent is down. The `Notify` will never fire.

**Recommendation:** The background poller (not yet implemented, but designed in the doc) must:
1. Check `requested_at + max_wait_timeout` on each poll cycle
2. Remove timed-out entries from the `DashMap`
3. Notify waiters so they can return `EIO`

Alternatively, the FUSE `read()` handler should use `tokio::time::timeout(max_wait, notify.notified())` so each waiter has its own deadline.

**Severity:** High. Without this, a single missing file can hang a render job forever.

#### D3: No Deduplication of SQS Messages at the VFS Level

The design doc discusses dedup at the upload agent level (idempotent CAS upload + marker check), but the VFS side has no guard against sending the same SQS message repeatedly. If 100 workers all request the same file, each will call `resolve()` which (in the real S3 implementation, not yet written) would send 100 SQS messages.

The `PendingFileTracker.register()` returns the existing `Notify` if the key is already pending, which is correct for waiter coordination. But the `RelaxedFileStore.resolve()` contract says "if not uploaded, enqueue an upload request" — there's no check for "already enqueued".

**Recommendation:** `resolve()` should check `PendingFileTracker.is_pending()` before enqueuing. If already pending, skip the SQS send and just return `Pending`. The tracker already supports this — it's just not wired up in the trait contract.

**Severity:** Medium. Duplicate SQS messages are harmless (the agent handles them), but 10,000 workers × 10,000 files = 100M unnecessary SQS messages is a cost and throttling concern.

#### D4: The `readdir` Problem is Acknowledged but Unresolved

The design doc has a "TODO: Directory Enumeration via Index Manifests" section that describes the problem well: `readdir` on a relaxed root returns only previously auto-vivified children. Applications that scan directories (common in render pipelines — "find all .exr files in this folder") will see empty directories.

The proposed solution (lightweight directory index manifests from the upload agent) is good, but it's not in V1 scope. This means V1 relaxed consistency is only usable for workflows where the application opens files by exact path.

**Recommendation:** This is a known limitation, not a bug. But the design doc should be explicit about which studio workflows are supported in V1 and which require V2. Add a "V1 Limitations" section that calls this out as a hard constraint, not a nice-to-have.

**Severity:** Low (design doc clarity). But high impact if studios hit this in production without warning.

#### D5: `split_composite_key` is Fragile with Root IDs Containing Colons

`composite_key()` joins root_id and path_key with `:`. `split_composite_key()` splits on the first `:`. If a root_id ever contains a colon (unlikely but not validated), the split produces wrong results.

**Recommendation:** Either validate that root_id contains no `:` at construction time, or use a separator that can't appear in a hex string (root_id is SHAKE-256 hex, path_key is XXH128 hex — neither will contain `:`, so this is safe in practice). Add a comment documenting this invariant.

**Severity:** Low. The invariant holds today, but it's implicit.

#### D6: No Backpressure on Auto-Vivification

A malicious or buggy application could traverse millions of non-existent paths under a relaxed root, creating millions of INode entries in memory. There's no limit on auto-vivified INodes.

**Recommendation:** Add a configurable cap (e.g., `max_relaxed_inodes: usize`) to `RelaxedConsistencyOptions`. When the cap is reached, return `ENOSPC` or `ENOMEM` on further auto-vivification.

**Severity:** Medium. Memory exhaustion on the worker is a real risk for long-running jobs.

---

## Part 2: Code Review

### What's Good

1. **Consistent coding style.** Explicit type annotations on `let` bindings throughout. Doc comments on every public function with `# Arguments` and `# Returns` sections. This matches the project's coding-style.md guidelines.

2. **Thorough test coverage.** Every module has unit tests. Serde roundtrip tests for all serializable types. The `MemoryRelaxedStore` mock is well-designed for testing the resolve/poll flow. The Python test suite mocks S3/SQS correctly.

3. **The `RootPathResolver` is a clean composition primitive.** It encapsulates the submitter→worker path translation and builds `FileUploadRequest` messages with the correct `source_root_path`. The tests cover both mapped and unmapped root cases.

4. **The `PendingFileTracker` is well-implemented.** `DashMap` for lock-free concurrent access, `Notify` for waking blocked readers, composite keys for batch polling. The `test_notify_wakes_waiter` async test is a good integration test.

### Code Issues

#### C1: `normalize_path` in `utils.rs` Doesn't Handle Windows Backslashes

The design doc says "Cross-platform: Windows uses `\` separators... Normalize to posix for manifest storage." But `normalize_path()` only handles `/` separators. A path like `project\textures\file.png` from a Windows submitter would not be normalized.

```rust
fn normalize_path(path: &str) -> String {
    let trimmed: &str = path.trim_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    parts.join("/")
}
```

**Fix:** Also split on `\` and normalize to `/`:

```rust
fn normalize_path(path: &str) -> String {
    let trimmed: &str = path.trim_matches(|c| c == '/' || c == '\\');
    let parts: Vec<&str> = trimmed.split(|c: char| c == '/' || c == '\\')
        .filter(|p| !p.is_empty())
        .collect();
    parts.join("/")
}
```

Add a test: `assert_eq!(normalize_path("project\\textures\\file.png"), "project/textures/file.png")`.

**Severity:** High for cross-platform correctness. Windows workers are a supported platform (ProjFS).

#### C2: `xxh128_hex` in `utils.rs` Duplicates Existing Codebase Functionality

The comment says "Use the xxhash_rust crate which is already a transitive dependency via rusty-attachments-common." A new direct dependency on `xxhash-rust` was added to `Cargo.toml`. The existing codebase likely has a shared hashing utility.

**Recommendation:** Check if `rusty-attachments-common` exports an `xxh128_hex` function. If so, use it instead of adding a direct dependency and reimplementing. If not, add it to `common` and use it from both places. Having two independent XXH128 implementations is a divergence risk.

**Severity:** Medium. If the implementations ever diverge (e.g., different endianness), path keys won't match between VFS and upload agent.

#### C3: `resolve_source_path` in `upload_agent.py` Has a Permissive Fallback

```python
# Fallback: if the source_root_path is a valid local directory, use it
if os.path.isdir(source_root_path):
    return source_root_path
```

This bypasses the storage profile entirely. If the agent happens to have a directory at the same path as the submitter's source, it'll serve files from it even if it's not in the storage profile. This is a security concern — the storage profile is the authorization boundary.

**Recommendation:** Remove the fallback. If the source path isn't in the storage profile, reject it. The storage profile exists precisely to control which paths the agent serves.

**Severity:** High. This is a security boundary violation.

#### C4: `RelaxedResolution::Pending` Lost the `estimated_wait` Field

The design doc defines:
```rust
Pending {
    estimated_wait: Option<Duration>,
}
```

But the implementation has:
```rust
Pending,
```

The `estimated_wait` was dropped, which means callers can't implement adaptive backoff or show progress to users.

**Recommendation:** Either restore the field or add a comment explaining why it was deferred. If deferred, file a follow-up ticket.

**Severity:** Low. V1 uses fixed polling intervals anyway.

#### C5: `FileContent::Relaxed` in `fuse.rs` and `fuse_writable.rs` Returns an Error but Doesn't Trigger Resolution

Both FUSE implementations handle `FileContent::Relaxed` in their `read()` paths by returning an error:

```rust
// COMPAT: Relaxed files must be promoted before read
FileContent::Relaxed(key) => {
    Err(VfsError::ContentRetrievalFailed {
        hash: key.path_key.clone(),
        source: "Relaxed file not yet resolved".into(),
    })
}
```

This means reading a relaxed file always returns `EIO`. The resolve→poll→promote flow described in the design doc is not wired up. The `DeadlineVfs` struct doesn't hold a `RelaxedFileStore` or `PendingFileTracker`.

**Assessment:** This is clearly WIP — the FUSE integration is scaffolded but not complete. The `// COMPAT:` comment is misleading though — this isn't a version compatibility issue, it's an incomplete implementation. Use `// TODO:` instead.

**Severity:** N/A (known WIP), but fix the comment tag.

#### C6: `MemoryRelaxedStore` Uses `std::sync::RwLock` Instead of `tokio::sync::RwLock`

```rust
pub struct MemoryRelaxedStore {
    resolutions: RwLock<HashMap<String, RelaxedResolution>>,
    requests: RwLock<Vec<(String, RequestPriority)>>,
}
```

This is `std::sync::RwLock` used inside `async fn` implementations. Holding a `std::sync::RwLock` guard across an `.await` point would deadlock, but since the current implementation doesn't await while holding the guard, it's technically fine. However, it's a footgun for future modifications.

**Recommendation:** Since this is a test-only mock, it's acceptable. Add a comment: `// std::sync::RwLock is fine here — guards are never held across await points.`

**Severity:** Low. Test-only code.

#### C7: Upload Agent Doesn't Use `max_concurrent_uploads`

The `AgentConfig` has `max_concurrent_uploads: int = 8` but the `run_agent()` loop processes messages sequentially — one at a time from `poll_queue()`. There's no concurrency.

**Recommendation:** This is a V1 simplification. Add a `# TODO: implement concurrent upload with asyncio/threading` comment. For V1, sequential processing is fine — the bottleneck is network upload, not message processing.

**Severity:** Low. Performance optimization for later.

#### C8: Missing `#[cfg(test)]` Guard on `tempfile` Dependency

The `config.rs` tests use `tempfile::NamedTempFile` but I don't see `tempfile` as a dev-dependency in the diff. Either it's already a dev-dependency (likely), or the tests won't compile.

**Recommendation:** Verify `tempfile` is in `[dev-dependencies]` of `crates/vfs/Cargo.toml`.

**Severity:** Low. Build would catch this.

#### C9: `validate_relaxed_requires_vfs` Uses String Comparison for Mode

```rust
if has_relaxed_roots && file_system_mode != "VIRTUAL" {
```

String comparison for an enum-like value is fragile. If someone passes `"virtual"` (lowercase) or `"Virtual"`, validation passes incorrectly.

**Recommendation:** Either use a proper enum for `FileSystemMode` or do case-insensitive comparison. An enum is strongly preferred:

```rust
pub enum FileSystemMode {
    Copied,
    Virtual,
}
```

**Severity:** Medium. Silent misconfiguration.

#### C10: The Python Upload Agent and Rust VFS Must Agree on Hash Computation

The Python agent uses `xxhash.xxh128()` and the Rust VFS uses `xxhash_rust::xxh3::xxh3_128()`. These are the same algorithm (XXH3-128), but it's critical they produce identical output for the same input. There's no cross-language test verifying this.

**Recommendation:** Add a test (can be in the Python test suite) that hashes a known input and asserts the expected hex output. Then add the same test in Rust. If the outputs match, you have confidence. Example:

```
Input: b"hello world"
Expected XXH3-128: <compute once and hardcode>
```

**Severity:** High. If these diverge, the VFS will never find completion markers written by the agent.

---

## Part 3: Testing Assessment

### What's Covered

- Serde roundtrip for all marker types, config types, and root configs
- Config loading with defaults, missing fields, invalid JSON, empty roots
- Validation of VIRTUAL vs COPIED mode
- Path normalization and key generation
- Pending tracker registration, resolution, notification, and concurrent access
- Memory store resolve/poll/request tracking
- Root path resolver for mapped and unmapped roots
- Upload request construction with correct source_root_path translation
- Python agent: config loading, path resolution, message processing, S3 mocking

### What's Missing

1. **No cross-language hash consistency test** (C10 above)
2. **No integration test for the full resolve→poll→promote flow** — the pieces exist but aren't composed
3. **No test for the auto-vivify INode lifecycle** (lookup miss → create dir → open → convert to file → resolve → promote). This is the core user-facing flow and it has zero test coverage.
4. **No test for timeout behavior** (D2 above) — because it's not implemented
5. **No negative test for `normalize_path` with Windows separators** (C1 above)
6. **No test for `split_composite_key` with edge cases** like empty strings or multiple colons

---

## Summary of Requested Changes

| ID | Severity | Category | Summary |
|----|----------|----------|---------|
| D1 | Medium | Design | Replace `MarkerEnvelope` string union with `#[serde(tag)]` enum |
| D2 | High | Design | Implement timeout enforcement for pending files |
| D3 | Medium | Design | Add SQS dedup check in `resolve()` via `is_pending()` |
| D6 | Medium | Design | Add backpressure cap on auto-vivified INodes |
| C1 | High | Code | Handle Windows backslashes in `normalize_path` |
| C3 | High | Code | Remove permissive fallback in `resolve_source_path` |
| C5 | Low | Code | Change `// COMPAT:` to `// TODO:` for unfinished relaxed read path |
| C9 | Medium | Code | Use enum for `FileSystemMode` instead of string comparison |
| C10 | High | Code | Add cross-language hash consistency test |

Items marked High should be addressed before merging. Medium items should be addressed before GA. Low items are nice-to-haves.

---

## Closing Thoughts

This is a well-structured incremental delivery. The data structures are right, the trait boundaries are clean, and the test coverage for the primitives is solid. The main gap is that the FUSE integration (the actual resolve→poll→promote flow in `read()`) is scaffolded but not wired up — which is fine for a WIP branch, but the `// COMPAT:` comments should be `// TODO:` to make the incomplete state obvious.

The design doc is unusually thorough for a draft — the sequence diagrams, the storage profile mapping walkthrough, and the alternatives-considered sections are all valuable. The V1/V2 scope split is realistic.

Ship the primitives, wire up the FUSE integration, add the timeout enforcement, and this is ready for alpha testing.
