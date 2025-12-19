# JA Manifest v2 Upload Design

## Overview

Functional prototype for uploading input files to S3 Content-Addressable Storage (CAS) with support for the v2 manifest format's chunking feature.

---

## Data Structures

### Input

```rust
struct InputFile {
    local_path: Path
    size: u64
    hash: String        // xxh128 of whole file
    mtime: u64
    runnable: bool      // POSIX execute bit
}

struct S3Options {
    bucket: String
    cas_prefix: String      // e.g., "Data"
    manifest_prefix: String // e.g., "Manifests/farm-xxx/queue-yyy"
}
```

### Output

```rust
struct ChunkInfo {
    s3_key: String      // "{cas_prefix}/{chunk_hash}.xxh128"
    size: u64
    hash: String        // xxh128 of this chunk
    offset: u64         // 0 for small files (single chunk)
}

struct UploadResult {
    input_file: InputFile
    chunks: Vec<ChunkInfo>  // Always populated, len=1 for small files
    uploaded: bool          // Any chunks actually uploaded?
}
```

---

## Function Signature

```rust
fn upload(
    files: Vec<InputFile>,
    s3_opts: S3Options,
    chunk_size: u64 = 268435456  // 256MB default
) -> Vec<UploadResult>
```

---

## Upload Logic

### Size Validation

S3 behavior for interrupted uploads:
- **Regular PutObject:** Atomic - partial uploads don't exist
- **Multipart Upload:** Parts retained until completed/aborted or lifecycle cleanup

Size mismatch indicates corruption or incomplete upload - re-upload in this case.

```rust
fn should_upload(s3_key: &str, expected_size: u64) -> bool {
    match head_object(s3_key) {
        Ok(metadata) => metadata.content_length != expected_size,  // Size mismatch = re-upload
        Err(NotFound) => true,  // Doesn't exist = upload
        Err(e) => handle_error(e)
    }
}
```

### File Processing

All files treated uniformly - small files are single-chunk files with `offset: 0`.

```rust
fn process_file(file: InputFile, chunk_size: u64, cas_prefix: &str) -> UploadResult {
    let mut chunks = vec![];
    let mut any_uploaded = false;
    
    for offset in (0..file.size).step_by(chunk_size) {
        let chunk_data = read_chunk(&file.local_path, offset, chunk_size);
        let chunk_hash = xxh128(&chunk_data);
        let chunk_size_actual = chunk_data.len();
        let s3_key = format!("{}/{}.xxh128", cas_prefix, chunk_hash);
        
        if should_upload(&s3_key, chunk_size_actual) {
            upload_bytes(&chunk_data, &s3_key);
            any_uploaded = true;
        }
        
        chunks.push(ChunkInfo { 
            s3_key, 
            size: chunk_size_actual, 
            hash: chunk_hash, 
            offset 
        });
    }
    
    UploadResult { input_file: file, chunks, uploaded: any_uploaded }
}
```

---

## S3 Key Structure

Content-Addressable Storage layout:

```
s3://{bucket}/{cas_prefix}/{chunk_hash}.xxh128
```

Example:
```
s3://my-bucket/Data/abc123def456789.xxh128
```

---

## Design Decisions

1. **Unified chunk model** - Small files are single-chunk files, keeping code paths consistent
2. **Size validation** - Re-upload if S3 object size doesn't match expected (handles interrupted uploads)
3. **CAS deduplication** - Check existence before upload to avoid redundant transfers
4. **256MB chunk size** - Balances resumability with overhead (0.5TB file = 2000 chunks)

---

## Manifest Generation

The `UploadResult` maps directly to v2 manifest format:

- **Small file (1 chunk):** Use `hash` field
- **Large file (>1 chunk):** Use `chunkhashes` array

```json
// Small file
{ "name": "$1/small.txt", "hash": "abc123...", "size": 1024, "mtime": 123456 }

// Large file  
{ "name": "$1/large.blend", "chunkhashes": ["abc...", "def...", "ghi..."], "size": 800000000, "mtime": 123456 }
```

---

## Python Implementation

```python
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional
import xxhash
import boto3
from botocore.exceptions import ClientError


# ============== Data Structures ==============

@dataclass
class InputFile:
    local_path: Path
    size: int
    hash: str           # xxh128 of whole file
    mtime: int          # unix timestamp
    runnable: bool = False  # POSIX execute bit


@dataclass
class S3Options:
    bucket: str
    cas_prefix: str         # e.g., "Data"
    manifest_prefix: str    # e.g., "Manifests/farm-xxx/queue-yyy"


@dataclass
class ChunkInfo:
    s3_key: str     # "{cas_prefix}/{chunk_hash}.xxh128"
    size: int
    hash: str       # xxh128 of this chunk
    offset: int     # 0 for small files (single chunk)


@dataclass
class UploadResult:
    input_file: InputFile
    chunks: list[ChunkInfo] = field(default_factory=list)
    uploaded: bool = False  # Any chunks actually uploaded?


# ============== Core Functions ==============

DEFAULT_CHUNK_SIZE = 256 * 1024 * 1024  # 256MB


def should_upload(s3_client, bucket: str, s3_key: str, expected_size: int) -> bool:
    """Check if we need to upload - either doesn't exist or size mismatch."""
    try:
        response = s3_client.head_object(Bucket=bucket, Key=s3_key)
        # Size mismatch = re-upload (handles interrupted uploads)
        return response['ContentLength'] != expected_size
    except ClientError as e:
        if e.response['Error']['Code'] == '404':
            return True  # Doesn't exist = upload
        raise


def read_chunk(file_path: Path, offset: int, chunk_size: int) -> bytes:
    """Read a chunk of data from file at given offset."""
    with open(file_path, 'rb') as f:
        f.seek(offset)
        return f.read(chunk_size)


def hash_chunk(data: bytes) -> str:
    """Compute xxh128 hash of data."""
    return xxhash.xxh128(data).hexdigest()


def upload_bytes(s3_client, bucket: str, s3_key: str, data: bytes) -> None:
    """Upload bytes to S3."""
    s3_client.put_object(Bucket=bucket, Key=s3_key, Body=data)


def process_file(
    s3_client,
    file: InputFile,
    s3_opts: S3Options,
    chunk_size: int = DEFAULT_CHUNK_SIZE
) -> UploadResult:
    """Process a single file - upload chunks as needed."""
    chunks: list[ChunkInfo] = []
    any_uploaded = False
    
    offset = 0
    while offset < file.size:
        # Read chunk
        chunk_data = read_chunk(file.local_path, offset, chunk_size)
        chunk_hash = hash_chunk(chunk_data)
        chunk_size_actual = len(chunk_data)
        s3_key = f"{s3_opts.cas_prefix}/{chunk_hash}.xxh128"
        
        # Upload if needed
        if should_upload(s3_client, s3_opts.bucket, s3_key, chunk_size_actual):
            upload_bytes(s3_client, s3_opts.bucket, s3_key, chunk_data)
            any_uploaded = True
        
        chunks.append(ChunkInfo(
            s3_key=s3_key,
            size=chunk_size_actual,
            hash=chunk_hash,
            offset=offset
        ))
        
        offset += chunk_size
    
    return UploadResult(input_file=file, chunks=chunks, uploaded=any_uploaded)


def upload(
    files: list[InputFile],
    s3_opts: S3Options,
    chunk_size: int = DEFAULT_CHUNK_SIZE,
    session: Optional[boto3.Session] = None
) -> list[UploadResult]:
    """
    Upload input files to S3 CAS with chunking support.
    
    Args:
        files: List of input files to upload
        s3_opts: S3 bucket and prefix configuration
        chunk_size: Chunk size in bytes (default 256MB)
        session: Optional boto3 session
    
    Returns:
        List of UploadResult with chunk mappings
    """
    session = session or boto3.Session()
    s3_client = session.client('s3')
    
    results: list[UploadResult] = []
    for file in files:
        result = process_file(s3_client, file, s3_opts, chunk_size)
        results.append(result)
    
    return results


# ============== Helper: Generate Manifest Entry ==============

def to_manifest_entry(result: UploadResult, dir_index: Optional[int] = None) -> dict:
    """Convert UploadResult to v2 manifest file entry."""
    file = result.input_file
    filename = file.local_path.name
    name = f"${dir_index}/{filename}" if dir_index is not None else filename
    
    entry = {
        "name": name,
        "size": file.size,
        "mtime": file.mtime
    }
    
    if len(result.chunks) == 1:
        # Small file - single hash
        entry["hash"] = result.chunks[0].hash
    else:
        # Large file - chunk hashes
        entry["chunkhashes"] = [chunk.hash for chunk in result.chunks]
    
    # Only include runnable if true (per v2 spec - canonical format)
    if file.runnable:
        entry["runnable"] = True
    
    return entry


# ============== Example Usage ==============

if __name__ == "__main__":
    # Example files
    files = [
        InputFile(
            local_path=Path("/path/to/small_file.txt"),
            size=1024,
            hash="abc123def456",
            mtime=1712972834
        ),
        InputFile(
            local_path=Path("/path/to/large_file.blend"),
            size=800_000_000,  # ~800MB = 4 chunks
            hash="xyz789abc012",
            mtime=1712972900
        ),
    ]
    
    s3_opts = S3Options(
        bucket="my-deadline-bucket",
        cas_prefix="Data",
        manifest_prefix="Manifests/farm-123/queue-456"
    )
    
    # Upload
    results = upload(files, s3_opts)
    
    # Generate manifest entries
    for result in results:
        entry = to_manifest_entry(result, dir_index=0)
        print(f"File: {result.input_file.local_path.name}")
        print(f"  Chunks: {len(result.chunks)}")
        print(f"  Uploaded: {result.uploaded}")
        print(f"  Manifest entry: {entry}")
        print()
```

---

## CRT Integration Notes

When using boto3 with AWS CRT (`awscrt` package installed), the S3 client handles:

| Feature | CRT Handles | Your Code Handles |
|---------|-------------|-------------------|
| Multipart upload parts | ✅ Automatic | - |
| Transfer parallelism | ✅ Automatic | - |
| Retries | ✅ Automatic (C layer) | - |
| Memory-efficient streaming | ✅ Automatic | - |
| CAS 256MB chunking | - | ✅ Your logic |
| Deduplication check | - | ✅ Your logic |

For best performance with CRT, use `send_filepath` parameter instead of streaming body:

```python
from awscrt.s3 import S3Client, S3RequestType

# CRT-optimized upload
s3_request = s3_client.make_request(
    request=request,
    type=S3RequestType.PUT_OBJECT,
    send_filepath="/path/to/chunk_file"  # CRT reads directly from file
)
```

---

## Future Work (TODO)

### 1. Symlink Support
V2 manifest format supports relative symlinks within the directory tree. Requires:
- Security validation: symlink must point within manifest root
- New field: `symlink_target: Optional[str]` in InputFile
- Manifest entry: `"symlink": {"name": "$N/target_file"}`

### 2. Content-Type Headers
V2 requires specific Content-Type when storing manifests in S3:
- Snapshot: `application/x-deadline-manifest-YYYY-MM-DD`
- Diff: `application/x-deadline-manifest-diff-YYYY-MM-DD`

### 3. Hash Algorithm Flexibility
Current design assumes xxh128. Should support:
- Recording algorithm in manifest metadata
- S3 key extension matching algorithm (`.xxh128`, etc.)

### 4. Large Scale Validation
Per v2 spec, must validate at:
- 1M+ files
- 1M+ directories
- 1M+ files in single directory

Current sequential processing may need batching/streaming for scale.
