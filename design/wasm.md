# WASM Backend Design

**Status:** 🚧 DESIGN PHASE  
**Target crate:** `crates/wasm/`  
**JS Package:** `crates/wasm/deadlinejs/`

## Overview

This document describes the design for a WebAssembly (WASM) backend that enables Job Attachments V1 (v2023-03-03) and V2 (v2025-12-04-beta) functionality in web browsers. The WASM module exposes the Rust manifest types directly, while the JavaScript layer handles browser file I/O and S3 uploads using AWS SDK v3.

## Goals

1. **Mirror Rust APIs**: Expose `Manifest`, `ManifestPath`, `ManifestFilePath`, etc. directly to JS
2. **File Hashing**: Compute XXH128 hashes in the browser using WASM
3. **S3 Upload**: Upload files to S3 CAS using AWS SDK for JavaScript v3
4. **Progress Reporting**: Stream progress updates to JavaScript callbacks
5. **V1 and V2 Support**: Full support for both manifest versions
6. **Web Worker Support**: Abstract execution context for main thread or Web Workers

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          Browser Environment                             │
├─────────────────────────────────────────────────────────────────────────┤
│  ┌──────────────┐                                                        │
│  │  HTML/JS UI  │  (File picker, credentials, progress display)          │
│  └──────┬───────┘                                                        │
│         │                                                                │
│         ▼                                                                │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                    deadlinejs Package                            │    │
│  ├─────────────────────────────────────────────────────────────────┤    │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │    │
│  │  │   common    │  │   storage   │  │    hash     │              │    │
│  │  │  (types,    │  │  (S3 ops,   │  │ (executor   │              │    │
│  │  │  constants) │  │  upload)    │  │  abstraction│              │    │
│  │  └─────────────┘  └──────┬──────┘  └──────┬──────┘              │    │
│  │         ▲                │                │                      │    │
│  │         └────────────────┴────────────────┘                      │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                          │                │                             │
│         ┌────────────────┘                └────────────────┐            │
│         ▼                                                  ▼            │
│  ┌─────────────────┐                              ┌─────────────────┐   │
│  │  WASM Module    │                              │   AWS SDK v3    │   │
│  │  (Rust)         │                              │   (S3 Client)   │   │
│  │  • Manifest     │                              │                 │   │
│  │  • ManifestPath │                              │                 │   │
│  │  • Xxh3Hasher   │                              │                 │   │
│  │  • CAS utils    │                              │                 │   │
│  └─────────────────┘                              └─────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
```

## Dependency Graph

```
                    ┌─────────────────┐
                    │  deadlinejs     │  (entry point, re-exports)
                    └────────┬────────┘
                             │
            ┌────────────────┼────────────────┐
            ▼                ▼                ▼
    ┌───────────┐    ┌───────────┐    ┌───────────┐
    │  storage  │    │   hash    │    │  common   │
    │           │    │           │    │           │
    │ S3 client │    │ executor  │    │ types     │
    │ upload    │    │ streaming │    │ constants │
    │ CAS utils │    │ workers   │    │ progress  │
    └─────┬─────┘    └─────┬─────┘    └───────────┘
          │                │                ▲
          │                │                │
          └────────────────┴────────────────┘
                           │
                           ▼
                    ┌─────────────────┐
                    │   WASM Module   │
                    │ (rusty-attach-  │
                    │  ments-wasm)    │
                    └─────────────────┘
```

**Dependency Rules:**
- `common` has no internal dependencies (leaf module)
- `hash` depends on `common` and WASM
- `storage` depends on `common`, WASM, and AWS SDK
- No circular dependencies allowed

## WASM Module API

The WASM module mirrors the Rust `crates/model` types directly.

### Manifest Types (from `crates/model`)

```typescript
// ============================================================
// Version Enums
// ============================================================

type ManifestVersion = 'v2023-03-03' | 'v2025-12-04-beta';
type ManifestType = 'SNAPSHOT' | 'DIFF';
type HashAlgorithm = 'xxh128';

// ============================================================
// V1 Manifest (v2023-03-03)
// ============================================================

/** File entry in V1 manifest - mirrors crates/model/src/v2023_03_03.rs */
interface ManifestPath {
  readonly path: string;
  readonly hash: string;
  readonly size: bigint;
  readonly mtime: bigint;  // microseconds since epoch
}

/** Create a V1 file entry */
function createManifestPath(
  path: string,
  hash: string,
  size: bigint,
  mtime: bigint
): ManifestPath;

// ============================================================
// V2 Manifest (v2025-12-04-beta)
// ============================================================

/** Directory entry in V2 manifest - mirrors ManifestDirectoryPath */
interface ManifestDirectoryPath {
  readonly name: string;
  readonly delete?: boolean;
}

/** File entry in V2 manifest - mirrors ManifestFilePath */
interface ManifestFilePath {
  readonly name: string;
  readonly hash?: string;
  readonly size?: bigint;
  readonly mtime?: bigint;
  readonly runnable?: boolean;
  readonly chunkhashes?: readonly string[];
  readonly symlink_target?: string;
  readonly delete?: boolean;
}

/** Create a V2 regular file entry */
function createManifestFilePath(
  name: string,
  hash: string,
  size: bigint,
  mtime: bigint,
  runnable?: boolean
): ManifestFilePath;

/** Create a V2 chunked file entry */
function createChunkedFilePath(
  name: string,
  chunkhashes: string[],
  size: bigint,
  mtime: bigint
): ManifestFilePath;

/** Create a V2 symlink entry */
function createSymlinkPath(name: string, target: string): ManifestFilePath;

/** Create a V2 deleted file marker */
function createDeletedFilePath(name: string): ManifestFilePath;

/** Create a V2 directory entry */
function createDirectoryPath(name: string): ManifestDirectoryPath;

/** Create a V2 deleted directory marker */
function createDeletedDirectoryPath(name: string): ManifestDirectoryPath;

// ============================================================
// Manifest Class (unified wrapper)
// ============================================================

/**
 * Manifest wrapper - mirrors crates/model/src/lib.rs Manifest enum.
 * Backed by Rust, returned from decode/create operations.
 */
class Manifest {
  /** Decode from JSON string, auto-detecting version */
  static decode(json: string): Manifest;
  
  /** Create V1 manifest from file entries */
  static createV1(paths: ManifestPath[]): Manifest;
  
  /** Create V2 snapshot manifest */
  static createV2Snapshot(
    dirs: ManifestDirectoryPath[],
    files: ManifestFilePath[]
  ): Manifest;
  
  /** Create V2 diff manifest */
  static createV2Diff(
    dirs: ManifestDirectoryPath[],
    files: ManifestFilePath[],
    parentHash: string
  ): Manifest;
  
  /** Encode to canonical JSON string */
  encode(): string;
  
  /** Get manifest version */
  get version(): ManifestVersion;
  
  /** Get hash algorithm */
  get hashAlg(): HashAlgorithm;
  
  /** Get total size of all files */
  get totalSize(): bigint;
  
  /** Get number of file entries */
  get fileCount(): number;
  
  /** Get number of directory entries (V2 only, 0 for V1) */
  get directoryCount(): number;
  
  /** Get manifest type (V2 only, SNAPSHOT for V1) */
  get manifestType(): ManifestType;
  
  /** Get parent manifest hash (V2 diff only) */
  get parentManifestHash(): string | undefined;
  
  /** Check if V1 */
  isV1(): boolean;
  
  /** Check if V2 */
  isV2(): boolean;
  
  /** Validate manifest structure */
  validate(): void;  // throws on error
  
  /** Get all file paths (V1) */
  getPaths(): ManifestPath[];  // V1 only, throws for V2
  
  /** Get all files (V2) */
  getFiles(): ManifestFilePath[];  // V2 only, throws for V1
  
  /** Get all directories (V2) */
  getDirectories(): ManifestDirectoryPath[];  // V2 only, throws for V1
  
  /**
   * Iterate over all content hashes (for CAS upload).
   * Includes chunk hashes for chunked files.
   */
  getAllHashes(): string[];
}

// ============================================================
// Hashing API
// ============================================================

/**
 * Streaming hasher for incremental XXH128 computation.
 * Use for hashing large files in chunks.
 */
class Xxh3Hasher {
  constructor();
  
  /** Add data chunk to hash computation */
  update(data: Uint8Array): void;
  
  /** Finalize and return 32-char hex hash */
  finishHex(): string;
  
  /** Reset hasher for reuse */
  reset(): void;
}

/** Hash a complete Uint8Array, returns 32-char hex string */
function hashBytes(data: Uint8Array): string;

// ============================================================
// CAS Utilities
// ============================================================

/** Chunk size for V2 format (256MB) */
const CHUNK_SIZE_V2: bigint;

/** Generate S3 CAS key from hash: "{hash}.xxh128" */
function casKey(hash: string, algorithm?: HashAlgorithm): string;

/** Parse CAS key to extract hash and algorithm */
function parseCasKey(key: string): { hash: string; algorithm: string } | null;

/** Check if file needs chunking based on size */
function needsChunking(fileSize: bigint): boolean;

/** Calculate expected chunk count for a file size */
function expectedChunkCount(fileSize: bigint): number;

/** Calculate chunk boundaries for a file */
interface ChunkInfo {
  readonly index: number;
  readonly offset: bigint;
  readonly length: bigint;
}
function calculateChunks(fileSize: bigint): ChunkInfo[];
```

## JavaScript Package API (deadlinejs)

### Module: `deadlinejs/common`

Shared types and constants. No dependencies on other modules.

```typescript
// ============================================================
// Re-export WASM types
// ============================================================

export type { 
  ManifestVersion, 
  ManifestType, 
  HashAlgorithm,
  ManifestPath,
  ManifestDirectoryPath,
  ManifestFilePath,
  ChunkInfo,
} from 'rusty-attachments-wasm';

export { 
  Manifest,
  Xxh3Hasher,
  hashBytes,
  casKey,
  parseCasKey,
  needsChunking,
  expectedChunkCount,
  calculateChunks,
  createManifestPath,
  createManifestFilePath,
  createChunkedFilePath,
  createSymlinkPath,
  createDeletedFilePath,
  createDirectoryPath,
  createDeletedDirectoryPath,
  CHUNK_SIZE_V2,
} from 'rusty-attachments-wasm';

// ============================================================
// Constants
// ============================================================

/** Default read chunk size for streaming file reads (1MB) */
export const READ_CHUNK_SIZE: number = 1024 * 1024;

/** Small file threshold for parallel uploads (80MB) */
export const SMALL_FILE_THRESHOLD: bigint = 80n * 1024n * 1024n;

// ============================================================
// Progress Callback
// ============================================================

export type ProgressPhase = 
  | 'hashing' 
  | 'checking' 
  | 'uploading' 
  | 'complete';

export interface ProgressUpdate {
  readonly phase: ProgressPhase;
  readonly currentFile?: string;
  readonly filesProcessed: number;
  readonly totalFiles: number;
  readonly bytesProcessed: bigint;
  readonly totalBytes: bigint;
  readonly filesSkipped?: number;
  readonly bytesSkipped?: bigint;
}

/** Progress callback. Return false to cancel. */
export type ProgressCallback = (progress: ProgressUpdate) => boolean;

/** No-op progress callback */
export const noOpProgress: ProgressCallback = () => true;

// ============================================================
// Error Types
// ============================================================

export class AttachmentError extends Error {
  constructor(message: string, public readonly code: string) {
    super(message);
    this.name = 'AttachmentError';
  }
}

export class StorageError extends AttachmentError {
  constructor(message: string, public readonly statusCode?: number) {
    super(message, 'STORAGE_ERROR');
    this.name = 'StorageError';
  }
}

export class ValidationError extends AttachmentError {
  constructor(message: string) {
    super(message, 'VALIDATION_ERROR');
    this.name = 'ValidationError';
  }
}

export class CancelledError extends AttachmentError {
  constructor() {
    super('Operation cancelled', 'CANCELLED');
    this.name = 'CancelledError';
  }
}

// ============================================================
// File Info (browser File with metadata)
// ============================================================

/** File with computed hash and relative path */
export interface HashedFile {
  readonly file: File;
  readonly relativePath: string;
  readonly hash: string;
  readonly size: bigint;
  readonly mtime: bigint;
  readonly chunkHashes?: readonly string[];
}
```

### Module: `deadlinejs/hash`

File hashing with Web Worker abstraction. Depends on `common` and WASM.

```typescript
import type { ProgressCallback, HashedFile } from './common';

// ============================================================
// Hash Executor Interface
// ============================================================

/**
 * Abstract interface for hash execution.
 * Allows swapping between main thread and Web Worker implementations.
 */
export interface HashExecutor {
  /**
   * Hash a single file.
   * 
   * @param file - Browser File object
   * @param relativePath - Relative path for manifest entry
   * @returns Hashed file with hash and optional chunk hashes
   */
  hashFile(file: File, relativePath: string): Promise<HashedFile>;

  /**
   * Hash multiple files with progress reporting.
   * 
   * @param files - Array of files with relative paths
   * @param onProgress - Optional progress callback
   * @returns Array of hashed files
   */
  hashFiles(
    files: Array<{ file: File; relativePath: string }>,
    onProgress?: ProgressCallback
  ): Promise<HashedFile[]>;

  /** Dispose of executor resources (terminate workers) */
  dispose(): void;
}

// ============================================================
// Implementations
// ============================================================

/** Hash executor that runs on the main thread */
export class MainThreadExecutor implements HashExecutor {
  constructor();
  hashFile(file: File, relativePath: string): Promise<HashedFile>;
  hashFiles(
    files: Array<{ file: File; relativePath: string }>,
    onProgress?: ProgressCallback
  ): Promise<HashedFile[]>;
  dispose(): void;
}

/** Hash executor that runs in a dedicated Web Worker */
export class WorkerExecutor implements HashExecutor {
  constructor(workerUrl?: string | URL);
  hashFile(file: File, relativePath: string): Promise<HashedFile>;
  hashFiles(
    files: Array<{ file: File; relativePath: string }>,
    onProgress?: ProgressCallback
  ): Promise<HashedFile[]>;
  dispose(): void;
}

/** Hash executor using a pool of Web Workers */
export class PooledWorkerExecutor implements HashExecutor {
  constructor(poolSize?: number, workerUrl?: string | URL);
  hashFile(file: File, relativePath: string): Promise<HashedFile>;
  hashFiles(
    files: Array<{ file: File; relativePath: string }>,
    onProgress?: ProgressCallback
  ): Promise<HashedFile[]>;
  dispose(): void;
}

// ============================================================
// Factory
// ============================================================

export type ExecutorType = 'main' | 'worker' | 'pool';

/** Create hash executor based on environment */
export function createHashExecutor(
  type?: ExecutorType,
  options?: { poolSize?: number; workerUrl?: string | URL }
): HashExecutor;

// ============================================================
// Low-Level (for worker script)
// ============================================================

/** Hash file data in chunks using streaming */
export async function hashFileStreaming(
  file: File,
  chunkSize?: number
): Promise<{ hash: string; chunkHashes?: string[] }>;
```

### Module: `deadlinejs/storage`

S3 operations using AWS SDK v3. Depends on `common` and WASM.

```typescript
import type { ProgressCallback, HashedFile } from './common';
import type { Manifest } from './common';
import type { AwsCredentialIdentity } from '@aws-sdk/types';

// ============================================================
// Configuration Types
// ============================================================

/** AWS credentials - compatible with AWS SDK v3 */
export type Credentials = 
  | AwsCredentialIdentity 
  | (() => Promise<AwsCredentialIdentity>);

/** S3 location for CAS storage */
export interface S3Location {
  readonly bucket: string;
  readonly rootPrefix: string;
  readonly casPrefix: string;       // default: "Data"
  readonly manifestPrefix: string;  // default: "Manifests"
}

/** Create S3Location with defaults */
export function createS3Location(
  bucket: string,
  rootPrefix: string,
  casPrefix?: string,
  manifestPrefix?: string
): S3Location;

/** Manifest location for input manifest uploads */
export interface ManifestLocation {
  readonly bucket: string;
  readonly rootPrefix: string;
  readonly farmId: string;
  readonly queueId: string;
}

/** Storage settings */
export interface StorageSettings {
  readonly region: string;
  readonly credentials: Credentials;
  readonly expectedBucketOwner?: string;
}

// ============================================================
// Result Types
// ============================================================

export interface TransferStatistics {
  readonly filesProcessed: number;
  readonly filesTransferred: number;
  readonly filesSkipped: number;
  readonly bytesTransferred: bigint;
  readonly bytesSkipped: bigint;
}

export interface UploadResult {
  readonly manifest: Manifest;
  readonly manifestHash: string;
  readonly manifestS3Key: string;
  readonly stats: TransferStatistics;
  readonly attachments: AttachmentsJson;
}

// ============================================================
// Attachments JSON (for CreateJob API)
// ============================================================

export interface ManifestProperties {
  readonly rootPath: string;
  readonly rootPathFormat: 'posix' | 'windows';
  readonly inputManifestPath?: string;
  readonly inputManifestHash?: string;
  readonly outputRelativeDirectories?: readonly string[];
  readonly fileSystemLocationName?: string;
}

export interface AttachmentsJson {
  readonly manifests: readonly ManifestProperties[];
  readonly fileSystem: 'COPIED' | 'VIRTUAL';
}

// ============================================================
// S3 Client Wrapper
// ============================================================

export interface ObjectInfo {
  readonly key: string;
  readonly size: bigint;
  readonly lastModified?: Date;
  readonly etag?: string;
}

/** S3 client wrapper using AWS SDK v3 */
export class S3Client {
  constructor(settings: StorageSettings);

  /** Check if object exists, return size or null */
  headObject(bucket: string, key: string): Promise<bigint | null>;

  /** Upload data to S3 */
  putObject(
    bucket: string,
    key: string,
    data: Uint8Array | Blob,
    contentType?: string,
    metadata?: Record<string, string>
  ): Promise<void>;

  /** Download object as bytes */
  getObject(bucket: string, key: string): Promise<Uint8Array>;

  /** List objects with prefix */
  listObjects(bucket: string, prefix: string): Promise<ObjectInfo[]>;
}

// ============================================================
// Upload Orchestrator
// ============================================================

/** Options for upload operations */
export interface UploadOptions {
  readonly concurrency?: number;           // default: 4
  readonly skipExistenceCheck?: boolean;   // default: false
  readonly onProgress?: ProgressCallback;
}

/** Orchestrate uploads to S3 CAS */
export class UploadOrchestrator {
  constructor(client: S3Client, location: S3Location);

  /** Check which hashes already exist in CAS */
  checkExistence(
    hashes: string[],
    onProgress?: ProgressCallback
  ): Promise<Set<string>>;

  /** Upload hashed files to CAS, skipping existing */
  uploadFiles(
    files: HashedFile[],
    existingHashes: Set<string>,
    onProgress?: ProgressCallback
  ): Promise<TransferStatistics>;

  /** Upload manifest file with metadata */
  uploadManifest(
    manifest: Manifest,
    assetRoot: string,
    location: ManifestLocation
  ): Promise<{ s3Key: string; hash: string }>;
}

// ============================================================
// CAS Key Utilities (convenience re-exports)
// ============================================================

/** Generate full S3 CAS key with prefix */
export function fullCasKey(
  location: S3Location, 
  hash: string, 
  algorithm?: HashAlgorithm
): string;

/** Generate manifest S3 key */
export function manifestKey(
  location: ManifestLocation,
  manifestHash: string
): string;
```

### Module: `deadlinejs` (Entry Point)

Main entry point with high-level API.

```typescript
// Re-export all modules
export * from './common';
export * from './hash';
export * from './storage';

import type { 
  ProgressCallback, 
  HashedFile,
  ManifestVersion,
} from './common';
import type { Manifest } from './common';
import type { 
  S3Location, 
  ManifestLocation, 
  StorageSettings,
  UploadResult,
} from './storage';
import type { HashExecutor } from './hash';

// ============================================================
// High-Level API
// ============================================================

export interface UploadAttachmentsOptions {
  readonly manifestVersion?: ManifestVersion;  // default: 'v2025-12-04-beta'
  readonly concurrency?: number;               // default: 4
  readonly skipExistenceCheck?: boolean;       // default: false
  readonly hashExecutor?: HashExecutor;        // default: auto-detect
  readonly onProgress?: ProgressCallback;
}

/**
 * Create manifest and upload files to S3 CAS.
 * 
 * @param files - Files from file picker or drag-drop
 * @param rootPath - Asset root path for manifest metadata
 * @param settings - AWS credentials and region
 * @param s3Location - S3 bucket and prefix configuration
 * @param manifestLocation - Farm/queue IDs for manifest path
 * @param options - Upload options
 * @returns Upload result with manifest and statistics
 */
export async function uploadAttachments(
  files: FileList | File[],
  rootPath: string,
  settings: StorageSettings,
  s3Location: S3Location,
  manifestLocation: ManifestLocation,
  options?: UploadAttachmentsOptions
): Promise<UploadResult>;

/**
 * Hash files without uploading.
 * 
 * @param files - Files to hash
 * @param rootPath - Root path for relative path calculation
 * @param options - Hash options
 * @returns Array of hashed files
 */
export async function hashFilesOnly(
  files: FileList | File[],
  rootPath: string,
  options?: {
    manifestVersion?: ManifestVersion;
    hashExecutor?: HashExecutor;
    onProgress?: ProgressCallback;
  }
): Promise<HashedFile[]>;

/**
 * Create manifest from hashed files.
 * 
 * @param hashedFiles - Previously hashed files
 * @param manifestVersion - Version to create (default: V2)
 * @returns Manifest backed by Rust
 */
export function createManifestFromFiles(
  hashedFiles: HashedFile[],
  manifestVersion?: ManifestVersion
): Manifest;

/**
 * Upload existing manifest and its files to S3.
 * Use when you already have a Manifest object.
 * 
 * @param manifest - Manifest to upload
 * @param files - Files matching manifest entries
 * @param rootPath - Asset root path
 * @param settings - AWS credentials
 * @param s3Location - S3 location
 * @param manifestLocation - Manifest location
 * @param options - Upload options
 */
export async function uploadManifestAndFiles(
  manifest: Manifest,
  files: Map<string, File>,  // relativePath -> File
  rootPath: string,
  settings: StorageSettings,
  s3Location: S3Location,
  manifestLocation: ManifestLocation,
  options?: UploadAttachmentsOptions
): Promise<UploadResult>;
```

## Implementation Plan

### Phase 1: WASM Module Extensions

1. **Add `Xxh3Hasher` class**
   - Streaming hasher for chunked file processing
   - `update()`, `finishHex()`, `reset()` methods

2. **Add `hashBytes()` function**
   - One-shot hash for small data

3. **Add CAS utilities**
   - `casKey()`, `parseCasKey()`
   - `needsChunking()`, `expectedChunkCount()`, `calculateChunks()`
   - `CHUNK_SIZE_V2` constant

4. **Extend `Manifest` class**
   - `createV1()`, `createV2Snapshot()`, `createV2Diff()` static methods
   - `getAllHashes()` for CAS upload iteration
   - `getPaths()`, `getFiles()`, `getDirectories()` accessors

5. **Add entry creation functions**
   - `createManifestPath()` for V1
   - `createManifestFilePath()`, `createChunkedFilePath()`, etc. for V2

### Phase 2: JavaScript Package - Common & Hash

1. **Create `deadlinejs/common`**
   - Re-export WASM types
   - Constants, error classes, progress types

2. **Create `deadlinejs/hash`**
   - `HashExecutor` interface
   - `MainThreadExecutor` implementation
   - `WorkerExecutor` implementation
   - `PooledWorkerExecutor` implementation
   - `hashFileStreaming()` low-level function

3. **Create worker script**
   - Load WASM in worker context
   - Handle file hashing messages

### Phase 3: JavaScript Package - Storage

1. **Create `deadlinejs/storage`**
   - `S3Client` wrapper using `@aws-sdk/client-s3`
   - `UploadOrchestrator` for coordinated uploads
   - CAS key utilities

2. **Implement upload pipeline**
   - Existence checking with `HeadObject`
   - Parallel uploads with concurrency control
   - Progress reporting

### Phase 4: Integration & Demo

1. **Create entry point**
   - `uploadAttachments()` high-level function
   - `hashFilesOnly()`, `createManifestFromFiles()`

2. **Create demo HTML page**
   - File picker UI
   - Credentials input
   - Progress display
   - Results with attachments JSON

## File Structure

```
crates/wasm/
├── Cargo.toml
├── src/
│   ├── lib.rs              # Main WASM exports
│   ├── hasher.rs           # Xxh3Hasher wrapper
│   ├── manifest.rs         # Extended Manifest methods
│   ├── entries.rs          # Entry creation functions
│   └── cas.rs              # CAS utilities
├── pkg/                    # wasm-pack output (generated)
└── deadlinejs/
    ├── package.json
    ├── tsconfig.json
    ├── src/
    │   ├── index.ts        # Entry point, high-level API
    │   ├── common/
    │   │   ├── index.ts
    │   │   ├── types.ts
    │   │   ├── errors.ts
    │   │   └── progress.ts
    │   ├── hash/
    │   │   ├── index.ts
    │   │   ├── executor.ts
    │   │   ├── main-thread.ts
    │   │   ├── worker.ts
    │   │   ├── pooled.ts
    │   │   └── streaming.ts
    │   └── storage/
    │       ├── index.ts
    │       ├── types.ts
    │       ├── client.ts
    │       ├── orchestrator.ts
    │       └── cas.ts
    ├── worker/
    │   └── hash-worker.ts
    └── examples/
        ├── upload-demo.html
        └── upload-demo.ts
```

## Browser Considerations

### WASM Sandbox Limitations

WASM runs in a sandboxed environment with no direct filesystem access. The `std::fs` module is unavailable in `wasm32-unknown-unknown` targets. This means:

- WASM code cannot read files directly from disk
- Browser file access requires JavaScript APIs (`File`, `FileReader`, `<input type="file">`)
- JavaScript must read file bytes and pass them to WASM functions

This is why the `Xxh3Hasher` class exposes a streaming interface to JavaScript — large files must be read in chunks by JS and fed incrementally to the WASM hasher, rather than having Rust read the file directly.

### File Access

```typescript
// Stream file in chunks for hashing
async function* readFileChunks(file: File, chunkSize: number): AsyncGenerator<Uint8Array> {
  let offset: number = 0;
  while (offset < file.size) {
    const slice: Blob = file.slice(offset, offset + chunkSize);
    const buffer: ArrayBuffer = await slice.arrayBuffer();
    yield new Uint8Array(buffer);
    offset += chunkSize;
  }
}
```

### Memory Management

- Process files in 1MB chunks to avoid memory pressure
- Release WASM memory after large operations
- Use streaming APIs where possible

### CORS Requirements

S3 bucket must have CORS configured:

```json
{
  "CORSRules": [{
    "AllowedOrigins": ["*"],
    "AllowedMethods": ["GET", "PUT", "HEAD"],
    "AllowedHeaders": ["*"],
    "ExposeHeaders": ["ETag", "x-amz-meta-*"],
    "MaxAgeSeconds": 3600
  }]
}
```

### Credentials

Compatible with AWS SDK v3 credential providers:

```typescript
import type { AwsCredentialIdentity } from '@aws-sdk/types';
import { fromCognitoIdentityPool } from '@aws-sdk/credential-providers';

// Static credentials (dev only)
const staticCreds: AwsCredentialIdentity = {
  accessKeyId: 'AKIA...',
  secretAccessKey: '...',
  sessionToken: '...',
};

// Cognito Identity (production)
const cognitoCreds = fromCognitoIdentityPool({
  identityPoolId: 'us-west-2:...',
  clientConfig: { region: 'us-west-2' },
});

// Usage
await uploadAttachments(files, rootPath, {
  region: 'us-west-2',
  credentials: cognitoCreds,
}, s3Location, manifestLocation);
```

## Dependencies

### Rust (WASM)
- `wasm-bindgen` - JS interop
- `serde-wasm-bindgen` - Serde to JS values
- `xxhash-rust` - XXH128 implementation
- `rusty-attachments-model` - Manifest types

### JavaScript (deadlinejs)
- `@aws-sdk/client-s3` - S3 operations
- `@aws-sdk/types` - Credential types

### Dev Dependencies
- `typescript`
- `vite` or `esbuild`
- `vitest`

## Security Considerations

1. **Credentials**: Never store in browser storage
2. **CORS**: Restrict AllowedOrigins in production
3. **Bucket Policy**: Use ExpectedBucketOwner
4. **CSP**: Allow WASM execution and worker scripts

## Testing Strategy

1. **WASM Unit Tests**: `wasm-bindgen-test`
2. **JS Unit Tests**: Vitest
3. **Integration Tests**: Vitest with LocalStack S3
4. **Manual Testing**: Demo page

## References

- [wasm-bindgen Guide](https://rustwasm.github.io/wasm-bindgen/)
- [AWS SDK v3 for JavaScript](https://docs.aws.amazon.com/AWSJavaScriptSDK/v3/latest/)
- [File API - MDN](https://developer.mozilla.org/en-US/docs/Web/API/File_API)
- [Web Workers - MDN](https://developer.mozilla.org/en-US/docs/Web/API/Web_Workers_API)
