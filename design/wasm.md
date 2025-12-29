# WASM Backend Design

**Status:** 🚧 DESIGN PHASE  
**Target crate:** `crates/wasm/`  
**JS Package:** `crates/wasm/deadlinejs/`

## Overview

This document describes the design for a WebAssembly (WASM) backend that enables Job Attachments V1 (v2023-03-03) and V2 (v2025-12-04-beta) functionality in web browsers. The goal is to provide equivalent functionality to `create_and_upload_manifest.py` but running entirely in the browser with JavaScript/TypeScript.

## Goals

1. **Manifest Creation**: Create V1 and V2 manifests from files selected via browser File API
2. **File Hashing**: Compute XXH128 hashes in the browser using WASM
3. **S3 Upload**: Upload files to S3 CAS using AWS SDK for JavaScript
4. **Manifest Upload**: Upload manifest files with proper metadata
5. **Progress Reporting**: Stream progress updates to JavaScript callbacks
6. **Cross-Platform**: Work in modern browsers (Chrome, Firefox, Safari, Edge)
7. **Web Worker Support**: Abstract execution context to support both main thread and Web Workers

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          Browser Environment                             │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌──────────────┐                                                        │
│  │  HTML/JS UI  │                                                        │
│  │ (File Picker)│                                                        │
│  └──────┬───────┘                                                        │
│         │                                                                │
│         ▼                                                                │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                    deadlinejs Package                            │    │
│  ├─────────────────────────────────────────────────────────────────┤    │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │    │
│  │  │   common    │  │    model    │  │   storage   │              │    │
│  │  │  (types,    │  │ (manifest   │  │  (S3 ops,   │              │    │
│  │  │  constants) │  │  builders)  │  │  upload)    │              │    │
│  │  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘              │    │
│  │         │                │                │                      │    │
│  │         └────────────────┼────────────────┘                      │    │
│  │                          ▼                                       │    │
│  │              ┌───────────────────────┐                           │    │
│  │              │   HashExecutor        │                           │    │
│  │              │   (abstraction)       │                           │    │
│  │              └───────────┬───────────┘                           │    │
│  │                          │                                       │    │
│  │         ┌────────────────┼────────────────┐                      │    │
│  │         ▼                ▼                ▼                      │    │
│  │  ┌────────────┐  ┌────────────┐  ┌────────────────┐             │    │
│  │  │MainThread  │  │WorkerThread│  │PooledWorker    │             │    │
│  │  │Executor    │  │Executor    │  │Executor        │             │    │
│  │  └────────────┘  └────────────┘  └────────────────┘             │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                          │                                               │
│         ┌────────────────┴────────────────┐                             │
│         ▼                                 ▼                             │
│  ┌─────────────────┐              ┌─────────────────┐                   │
│  │  WASM Module    │              │   AWS SDK v3    │                   │
│  │  (Rust)         │              │   (S3 Client)   │                   │
│  │  • XXH128       │              │                 │                   │
│  │  • Manifest I/O │              │                 │                   │
│  │  • CAS keys     │              │                 │                   │
│  └─────────────────┘              └─────────────────┘                   │
└─────────────────────────────────────────────────────────────────────────┘
```

### Component Responsibilities

| Component | Responsibility |
|-----------|----------------|
| **HTML/JS UI** | File selection, credentials input, progress display |
| **deadlinejs/common** | Shared types, constants, progress interfaces |
| **deadlinejs/model** | Manifest builders, encode/decode via WASM |
| **deadlinejs/storage** | S3 operations, upload orchestration |
| **HashExecutor** | Abstract hashing execution (main thread vs worker) |
| **WASM Module** | CPU-intensive operations (hashing, encoding) |
| **AWS SDK v3** | S3 operations (PutObject, HeadObject, ListObjects) |

## Dependency Graph

```
                    ┌─────────────────┐
                    │  deadlinejs     │  (entry point, re-exports)
                    └────────┬────────┘
                             │
            ┌────────────────┼────────────────┐
            ▼                ▼                ▼
    ┌───────────┐    ┌───────────┐    ┌───────────┐
    │  storage  │    │   model   │    │  common   │
    │           │    │           │    │           │
    │ S3 ops    │    │ builders  │    │ types     │
    │ upload    │    │ manifest  │    │ constants │
    │ download  │    │ encode    │    │ progress  │
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
- `model` depends on `common` and WASM
- `storage` depends on `common`, `model`, and AWS SDK
- No circular dependencies allowed

## WASM Module API

### Current API (Implemented)

```typescript
// Decode manifest from JSON
function decode_manifest(json: string): Manifest;

class Manifest {
  encode(): string;
  get version(): string;
  get hashAlg(): string;
  get totalSize(): bigint;
  get fileCount(): number;
  isV2023(): boolean;
  isV2025(): boolean;
}
```

### Proposed Extensions

```typescript
// ============================================================
// Hashing API
// ============================================================

/**
 * Streaming hasher for incremental XXH128 computation.
 * Use for hashing large files in chunks without loading entire file.
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
// Manifest Builder API (V2)
// ============================================================

/**
 * Builder for creating V2 manifests incrementally.
 * Supports files, directories, and symlinks.
 */
class ManifestBuilderV2 {
  constructor(hashAlgorithm?: string);  // default: "xxh128"
  
  /** Add a file entry */
  addFile(entry: FileEntryV2): void;
  
  /** Add a directory entry */
  addDirectory(path: string): void;
  
  /** Build the final manifest */
  build(): Manifest;
  
  /** Build as diff manifest with parent hash */
  buildDiff(parentHash: string): Manifest;
}

interface FileEntryV2 {
  path: string;           // Relative POSIX path
  hash: string;           // XXH128 hash (32 hex chars)
  size: bigint;           // File size in bytes
  mtime: number;          // Unix timestamp (seconds)
  runnable?: boolean;     // Executable flag
  chunkHashes?: string[]; // For files > 256MB
}

// ============================================================
// Manifest Builder API (V1)
// ============================================================

/**
 * Builder for creating V1 manifests (v2023-03-03).
 * Files only, no directories or symlinks.
 */
class ManifestBuilderV1 {
  constructor();
  
  /** Add a file entry */
  addFile(path: string, hash: string, size: bigint, mtime: number): void;
  
  /** Build the final manifest */
  build(): Manifest;
}

// ============================================================
// CAS Utilities
// ============================================================

/** Generate S3 CAS key from hash */
function casKey(hash: string, algorithm?: string): string;
// Returns: "{hash}.xxh128"

/** Parse CAS key to extract hash and algorithm */
function parseCasKey(key: string): { hash: string; algorithm: string } | null;

// ============================================================
// Chunking Utilities
// ============================================================

/** Chunk size for V2 format (256MB) */
const CHUNK_SIZE_V2: bigint;

/** Calculate chunk boundaries for a file */
function calculateChunks(fileSize: bigint): ChunkInfo[];

interface ChunkInfo {
  index: number;
  offset: bigint;
  length: bigint;
}

/** Check if file needs chunking */
function needsChunking(fileSize: bigint): boolean;
```

## JavaScript Package API (deadlinejs)

The JS package mirrors the Rust crate structure with clear module boundaries.

### Module: `deadlinejs/common`

Shared types, constants, and interfaces. No dependencies on other deadlinejs modules.

```typescript
// ============================================================
// Constants (mirrors crates/common/src/constants.rs)
// ============================================================

/** Chunk size for V2 format (256MB) */
export const CHUNK_SIZE_V2: bigint = 256n * 1024n * 1024n;

/** No chunking (V1 format) */
export const CHUNK_SIZE_NONE: bigint = 0n;

/** Small file threshold for parallel uploads (80MB) */
export const SMALL_FILE_THRESHOLD: bigint = 80n * 1024n * 1024n;

/** Default read chunk size for streaming (1MB) */
export const READ_CHUNK_SIZE: number = 1024 * 1024;

// ============================================================
// Hash Algorithm (mirrors crates/model/src/hash.rs)
// ============================================================

export type HashAlgorithm = 'xxh128';

export const DEFAULT_HASH_ALGORITHM: HashAlgorithm = 'xxh128';

// ============================================================
// Manifest Versions (mirrors crates/model/src/version.rs)
// ============================================================

export type ManifestVersion = 'v2023-03-03' | 'v2025-12-04-beta';

export const MANIFEST_VERSION_V1: ManifestVersion = 'v2023-03-03';
export const MANIFEST_VERSION_V2: ManifestVersion = 'v2025-12-04-beta';

// ============================================================
// Progress Callback (mirrors crates/common/src/progress.rs)
// ============================================================

export type ProgressPhase = 
  | 'scanning' 
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

/**
 * Progress callback function.
 * Return false to cancel the operation.
 */
export type ProgressCallback = (progress: ProgressUpdate) => boolean;

/** No-op progress callback */
export const noOpProgress: ProgressCallback = () => true;

// ============================================================
// Error Types (mirrors crates/common/src/error.rs)
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
// File Entry Types
// ============================================================

/** File entry for V1 manifests */
export interface FileEntryV1 {
  readonly path: string;
  readonly hash: string;
  readonly size: bigint;
  readonly mtime: number;
}

/** File entry for V2 manifests */
export interface FileEntryV2 {
  readonly path: string;
  readonly hash: string;
  readonly size: bigint;
  readonly mtime: number;
  readonly runnable?: boolean;
  readonly chunkHashes?: readonly string[];
}

/** Hashed file result from hashing operation */
export interface HashedFile {
  readonly file: File;
  readonly relativePath: string;
  readonly hash: string;
  readonly size: bigint;
  readonly mtime: number;
  readonly chunkHashes?: readonly string[];
}

// ============================================================
// Chunk Info (mirrors storage types)
// ============================================================

export interface ChunkInfo {
  readonly index: number;
  readonly offset: bigint;
  readonly length: bigint;
}
```

### Module: `deadlinejs/model`

Manifest creation and manipulation. Depends on `common` and WASM module.

```typescript
import type { 
  FileEntryV1, 
  FileEntryV2, 
  ManifestVersion,
  HashAlgorithm 
} from './common';

// Re-export WASM Manifest class with extended typing
export { Manifest } from 'rusty-attachments-wasm';

// ============================================================
// Manifest Builder V1 (mirrors crates/model v2023 format)
// ============================================================

/**
 * Builder for V1 manifests (v2023-03-03).
 * Files only, no directories or symlinks.
 */
export class ManifestBuilderV1 {
  /**
   * Create a new V1 manifest builder.
   */
  constructor();

  /**
   * Add a file entry to the manifest.
   * 
   * @param entry - File entry with path, hash, size, mtime
   */
  addFile(entry: FileEntryV1): void;

  /**
   * Build the final manifest.
   * 
   * @returns Encoded manifest ready for upload
   */
  build(): Manifest;

  /**
   * Get current file count.
   */
  get fileCount(): number;

  /**
   * Get current total size.
   */
  get totalSize(): bigint;
}

// ============================================================
// Manifest Builder V2 (mirrors crates/model v2025 format)
// ============================================================

/**
 * Builder for V2 manifests (v2025-12-04-beta).
 * Supports files, directories, symlinks, and chunking.
 */
export class ManifestBuilderV2 {
  /**
   * Create a new V2 manifest builder.
   * 
   * @param hashAlgorithm - Hash algorithm (default: 'xxh128')
   */
  constructor(hashAlgorithm?: HashAlgorithm);

  /**
   * Add a file entry to the manifest.
   * 
   * @param entry - File entry with path, hash, size, mtime, optional chunkHashes
   */
  addFile(entry: FileEntryV2): void;

  /**
   * Add a directory entry to the manifest.
   * 
   * @param path - Relative POSIX path to directory
   */
  addDirectory(path: string): void;

  /**
   * Build the final snapshot manifest.
   * 
   * @returns Encoded manifest ready for upload
   */
  build(): Manifest;

  /**
   * Build as diff manifest with parent reference.
   * 
   * @param parentHash - Hash of the parent manifest
   * @returns Encoded diff manifest
   */
  buildDiff(parentHash: string): Manifest;

  /**
   * Get current file count.
   */
  get fileCount(): number;

  /**
   * Get current directory count.
   */
  get directoryCount(): number;

  /**
   * Get current total size.
   */
  get totalSize(): bigint;
}

// ============================================================
// Utility Functions
// ============================================================

/**
 * Decode a manifest from JSON string.
 * Auto-detects version.
 * 
 * @param json - JSON string to decode
 * @returns Decoded manifest
 * @throws ValidationError if JSON is invalid
 */
export function decodeManifest(json: string): Manifest;

/**
 * Get manifest version from JSON without full decode.
 * 
 * @param json - JSON string to inspect
 * @returns Manifest version or null if invalid
 */
export function detectManifestVersion(json: string): ManifestVersion | null;
```

### Module: `deadlinejs/storage`

S3 operations and upload orchestration. Depends on `common`, `model`, and AWS SDK.

```typescript
import type { 
  ProgressCallback, 
  HashedFile,
  ChunkInfo 
} from './common';
import type { Manifest } from './model';
import type { AwsCredentialIdentity } from '@aws-sdk/types';

// ============================================================
// Configuration Types (mirrors crates/storage/src/types.rs)
// ============================================================

/**
 * AWS credentials - compatible with AWS SDK credential providers.
 * Can be a static credential object or a provider function.
 */
export type Credentials = AwsCredentialIdentity | (() => Promise<AwsCredentialIdentity>);

/**
 * S3 location configuration for CAS storage.
 */
export interface S3Location {
  readonly bucket: string;
  readonly rootPrefix: string;
  readonly casPrefix: string;       // default: "Data"
  readonly manifestPrefix: string;  // default: "Manifests"
}

/**
 * Create S3Location with defaults.
 */
export function createS3Location(
  bucket: string,
  rootPrefix: string,
  casPrefix?: string,
  manifestPrefix?: string
): S3Location;

/**
 * Manifest location for input manifest uploads.
 */
export interface ManifestLocation {
  readonly bucket: string;
  readonly rootPrefix: string;
  readonly farmId: string;
  readonly queueId: string;
}

/**
 * Storage settings for S3 operations.
 */
export interface StorageSettings {
  readonly region: string;
  readonly credentials: Credentials;
  readonly expectedBucketOwner?: string;
}

// ============================================================
// Upload Options
// ============================================================

export interface UploadOptions {
  readonly manifestVersion: ManifestVersion;
  readonly concurrency?: number;           // default: 4
  readonly skipExistenceCheck?: boolean;   // default: false
  readonly onProgress?: ProgressCallback;
}

// ============================================================
// Result Types (mirrors crates/storage/src/types.rs)
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
// Attachments JSON (mirrors crates/ja-deadline-utils)
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
// CAS Utilities (mirrors crates/storage/src/cas.rs)
// ============================================================

/**
 * Generate S3 CAS key from hash.
 * 
 * @param hash - Content hash (32 hex chars)
 * @param algorithm - Hash algorithm (default: 'xxh128')
 * @returns S3 key like "{hash}.xxh128"
 */
export function casKey(hash: string, algorithm?: HashAlgorithm): string;

/**
 * Generate full S3 CAS key with prefix.
 * 
 * @param location - S3 location config
 * @param hash - Content hash
 * @param algorithm - Hash algorithm
 * @returns Full S3 key like "{rootPrefix}/{casPrefix}/{hash}.xxh128"
 */
export function fullCasKey(
  location: S3Location, 
  hash: string, 
  algorithm?: HashAlgorithm
): string;

/**
 * Parse CAS key to extract hash and algorithm.
 * 
 * @param key - S3 key to parse
 * @returns Parsed components or null if invalid
 */
export function parseCasKey(key: string): { hash: string; algorithm: string } | null;

// ============================================================
// Chunking Utilities
// ============================================================

/**
 * Check if file needs chunking based on size.
 * 
 * @param fileSize - File size in bytes
 * @returns True if file exceeds CHUNK_SIZE_V2
 */
export function needsChunking(fileSize: bigint): boolean;

/**
 * Calculate chunk boundaries for a file.
 * 
 * @param fileSize - File size in bytes
 * @returns Array of chunk info (empty if no chunking needed)
 */
export function calculateChunks(fileSize: bigint): ChunkInfo[];

// ============================================================
// S3 Client Wrapper
// ============================================================

/**
 * S3 client wrapper for CAS operations.
 * Handles ExpectedBucketOwner and common patterns.
 */
export class S3Client {
  constructor(settings: StorageSettings);

  /**
   * Check if object exists and return size.
   * 
   * @param bucket - S3 bucket
   * @param key - S3 key
   * @returns Size in bytes or null if not found
   */
  headObject(bucket: string, key: string): Promise<bigint | null>;

  /**
   * Upload bytes to S3.
   * 
   * @param bucket - S3 bucket
   * @param key - S3 key
   * @param data - Data to upload
   * @param contentType - Optional content type
   * @param metadata - Optional user metadata
   */
  putObject(
    bucket: string,
    key: string,
    data: Uint8Array | Blob,
    contentType?: string,
    metadata?: Record<string, string>
  ): Promise<void>;

  /**
   * Download object as bytes.
   * 
   * @param bucket - S3 bucket
   * @param key - S3 key
   * @returns Downloaded data
   */
  getObject(bucket: string, key: string): Promise<Uint8Array>;

  /**
   * List objects with prefix.
   * 
   * @param bucket - S3 bucket
   * @param prefix - Key prefix
   * @returns List of object info
   */
  listObjects(bucket: string, prefix: string): Promise<ObjectInfo[]>;
}

export interface ObjectInfo {
  readonly key: string;
  readonly size: bigint;
  readonly lastModified?: Date;
  readonly etag?: string;
}

// ============================================================
// Upload Orchestrator
// ============================================================

/**
 * Orchestrate file uploads to S3 CAS.
 */
export class UploadOrchestrator {
  constructor(client: S3Client, location: S3Location);

  /**
   * Check which hashes already exist in CAS.
   * 
   * @param hashes - Hashes to check
   * @param onProgress - Optional progress callback
   * @returns Set of existing hashes
   */
  checkExistence(
    hashes: string[],
    onProgress?: ProgressCallback
  ): Promise<Set<string>>;

  /**
   * Upload hashed files to CAS, skipping existing.
   * 
   * @param files - Hashed files to upload
   * @param existingHashes - Hashes to skip
   * @param onProgress - Optional progress callback
   * @returns Transfer statistics
   */
  uploadFiles(
    files: HashedFile[],
    existingHashes: Set<string>,
    onProgress?: ProgressCallback
  ): Promise<TransferStatistics>;

  /**
   * Upload manifest file with metadata.
   * 
   * @param manifest - Manifest to upload
   * @param assetRoot - Asset root path for metadata
   * @param location - Manifest location config
   * @returns S3 key and manifest hash
   */
  uploadManifest(
    manifest: Manifest,
    assetRoot: string,
    location: ManifestLocation
  ): Promise<{ s3Key: string; hash: string }>;
}
```

### Module: `deadlinejs/hash`

File hashing with Web Worker abstraction. Depends on `common` and WASM module.

```typescript
import type { ProgressCallback, HashedFile, ChunkInfo } from './common';

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
   * @param file - File to hash
   * @param relativePath - Relative path for manifest
   * @returns Hashed file result
   */
  hashFile(file: File, relativePath: string): Promise<HashedFile>;

  /**
   * Hash multiple files with progress reporting.
   * 
   * @param files - Files to hash with relative paths
   * @param onProgress - Optional progress callback
   * @returns Array of hashed files
   */
  hashFiles(
    files: Array<{ file: File; relativePath: string }>,
    onProgress?: ProgressCallback
  ): Promise<HashedFile[]>;

  /**
   * Dispose of executor resources (e.g., terminate workers).
   */
  dispose(): void;
}

// ============================================================
// Main Thread Executor
// ============================================================

/**
 * Hash executor that runs on the main thread.
 * Simple but may block UI for large files.
 */
export class MainThreadExecutor implements HashExecutor {
  constructor();
  hashFile(file: File, relativePath: string): Promise<HashedFile>;
  hashFiles(
    files: Array<{ file: File; relativePath: string }>,
    onProgress?: ProgressCallback
  ): Promise<HashedFile[]>;
  dispose(): void;
}

// ============================================================
// Web Worker Executor
// ============================================================

/**
 * Hash executor that runs in a dedicated Web Worker.
 * Non-blocking but requires worker script setup.
 */
export class WorkerExecutor implements HashExecutor {
  /**
   * Create worker executor.
   * 
   * @param workerUrl - URL to worker script (or use default bundled worker)
   */
  constructor(workerUrl?: string | URL);
  hashFile(file: File, relativePath: string): Promise<HashedFile>;
  hashFiles(
    files: Array<{ file: File; relativePath: string }>,
    onProgress?: ProgressCallback
  ): Promise<HashedFile[]>;
  dispose(): void;
}

// ============================================================
// Pooled Worker Executor
// ============================================================

/**
 * Hash executor using a pool of Web Workers for parallelism.
 * Best for hashing many files concurrently.
 */
export class PooledWorkerExecutor implements HashExecutor {
  /**
   * Create pooled worker executor.
   * 
   * @param poolSize - Number of workers (default: navigator.hardwareConcurrency)
   * @param workerUrl - URL to worker script
   */
  constructor(poolSize?: number, workerUrl?: string | URL);
  hashFile(file: File, relativePath: string): Promise<HashedFile>;
  hashFiles(
    files: Array<{ file: File; relativePath: string }>,
    onProgress?: ProgressCallback
  ): Promise<HashedFile[]>;
  dispose(): void;
}

// ============================================================
// Factory Function
// ============================================================

export type ExecutorType = 'main' | 'worker' | 'pool';

/**
 * Create appropriate hash executor based on environment.
 * 
 * @param type - Executor type (default: auto-detect)
 * @param options - Executor-specific options
 * @returns Hash executor instance
 */
export function createHashExecutor(
  type?: ExecutorType,
  options?: { poolSize?: number; workerUrl?: string | URL }
): HashExecutor;

// ============================================================
// Low-Level Hashing (for worker script)
// ============================================================

/**
 * Hash file data in chunks using streaming.
 * Used internally by executors.
 * 
 * @param file - File to hash
 * @param chunkSize - Read chunk size (default: 1MB)
 * @returns Hash and optional chunk hashes
 */
export async function hashFileStreaming(
  file: File,
  chunkSize?: number
): Promise<{ hash: string; chunkHashes?: string[] }>;
```

### Module: `deadlinejs` (Entry Point)

Main entry point that re-exports and provides high-level API.

```typescript
// Re-export all modules
export * from './common';
export * from './model';
export * from './storage';
export * from './hash';

// ============================================================
// High-Level API
// ============================================================

import type { 
  ProgressCallback, 
  ManifestVersion,
  HashedFile 
} from './common';
import type { Manifest } from './model';
import type { 
  S3Location, 
  ManifestLocation, 
  StorageSettings,
  UploadResult,
  TransferStatistics 
} from './storage';
import type { HashExecutor } from './hash';

/**
 * Options for uploadAttachments.
 */
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
 * This is the main entry point equivalent to create_and_upload_manifest.py.
 * 
 * @param files - Files to upload (from file picker or drag-drop)
 * @param rootPath - Asset root path (for manifest metadata)
 * @param settings - AWS credentials and region
 * @param s3Location - S3 bucket and prefix configuration
 * @param manifestLocation - Farm/queue IDs for manifest path
 * @param options - Upload options
 * @returns Upload result with manifest and statistics
 * 
 * @example
 * ```typescript
 * const result = await uploadAttachments(
 *   fileInput.files,
 *   '/projects/my-job',
 *   { region: 'us-west-2', credentials: cognitoCredentials },
 *   { bucket: 'my-bucket', rootPrefix: 'DeadlineCloud', casPrefix: 'Data', manifestPrefix: 'Manifests' },
 *   { bucket: 'my-bucket', rootPrefix: 'DeadlineCloud', farmId: 'farm-123', queueId: 'queue-456' },
 *   { onProgress: (p) => console.log(p.phase, p.filesProcessed) }
 * );
 * console.log(result.attachments);
 * ```
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
 * Useful for creating manifests locally or checking what would be uploaded.
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
 * Create manifest from hashed files without uploading.
 * 
 * @param hashedFiles - Previously hashed files
 * @param manifestVersion - Manifest version to create
 * @returns Created manifest
 */
export function createManifest(
  hashedFiles: HashedFile[],
  manifestVersion?: ManifestVersion
): Manifest;
```

## Implementation Plan

### Phase 1: WASM Hashing (Core)

1. **Add `Xxh3Hasher` to WASM module**
   - Expose streaming hasher for chunked file processing
   - Add `hashBytes()` convenience function
   - Ensure proper memory management for large buffers

2. **Add chunking utilities**
   - `CHUNK_SIZE_V2` constant
   - `calculateChunks()` function
   - `needsChunking()` helper

3. **Add CAS utilities**
   - `casKey()` generation
   - `parseCasKey()` parsing

### Phase 2: Manifest Building

1. **Add `ManifestBuilderV1` for legacy**
   - Simple file-only builder
   - V1 format compatibility

2. **Add `ManifestBuilderV2` for V2**
   - Incremental file/directory addition
   - Automatic total_size calculation
   - Diff manifest support

### Phase 3: JavaScript Package Structure

1. **Create `deadlinejs/common` module**
   - Constants mirroring Rust crates/common
   - Type definitions
   - Error classes
   - Progress callback types

2. **Create `deadlinejs/model` module**
   - ManifestBuilderV1 wrapper
   - ManifestBuilderV2 wrapper
   - Decode/encode utilities

3. **Create `deadlinejs/hash` module**
   - HashExecutor interface
   - MainThreadExecutor implementation
   - WorkerExecutor implementation
   - PooledWorkerExecutor implementation
   - Factory function for auto-detection

4. **Create `deadlinejs/storage` module**
   - S3Client wrapper with AWS SDK v3
   - CAS utilities (casKey, fullCasKey, parseCasKey)
   - Chunking utilities
   - UploadOrchestrator

5. **Create `deadlinejs` entry point**
   - Re-exports from all modules
   - High-level `uploadAttachments()` function
   - `hashFilesOnly()` utility
   - `createManifest()` utility

### Phase 4: Demo Application

1. **Create example HTML page**
   - File picker UI
   - Credentials input (supports AWS SDK credential format)
   - Progress display
   - Results display with attachments JSON
   - Toggle for Web Worker vs main thread

## Browser Considerations

### File Access

```typescript
// Using File API with streaming for large files
async function* readFileChunks(file: File, chunkSize: number): AsyncGenerator<Uint8Array> {
  let offset = 0;
  while (offset < file.size) {
    const slice = file.slice(offset, offset + chunkSize);
    const buffer = await slice.arrayBuffer();
    yield new Uint8Array(buffer);
    offset += chunkSize;
  }
}
```

### Memory Management

- Process files in 64KB-1MB chunks to avoid memory pressure
- Use streaming APIs where possible
- Release WASM memory after large operations

### CORS Requirements

S3 bucket must have CORS configured:

```json
{
  "CORSRules": [
    {
      "AllowedOrigins": ["*"],
      "AllowedMethods": ["GET", "PUT", "HEAD"],
      "AllowedHeaders": ["*"],
      "ExposeHeaders": ["ETag", "x-amz-meta-*"],
      "MaxAgeSeconds": 3600
    }
  ]
}
```

### Credentials

The `Credentials` type is compatible with AWS SDK v3 credential providers:

```typescript
import type { AwsCredentialIdentity } from '@aws-sdk/types';

// Static credentials
const staticCreds: AwsCredentialIdentity = {
  accessKeyId: 'AKIA...',
  secretAccessKey: '...',
  sessionToken: '...',  // optional
};

// Cognito Identity credentials
import { fromCognitoIdentityPool } from '@aws-sdk/credential-providers';
const cognitoCreds = fromCognitoIdentityPool({
  identityPoolId: 'us-west-2:...',
  clientConfig: { region: 'us-west-2' },
});

// Usage with deadlinejs
import { uploadAttachments } from 'deadlinejs';

await uploadAttachments(files, rootPath, {
  region: 'us-west-2',
  credentials: cognitoCreds,  // or staticCreds
}, s3Location, manifestLocation);
```

Options for browser credentials:
1. **Static credentials**: User provides access key/secret (dev/testing only)
2. **Cognito Identity Pool**: Federated identity for production
3. **STS AssumeRoleWithWebIdentity**: Via OIDC provider
4. **Custom provider**: Any function returning `Promise<AwsCredentialIdentity>`

## File Structure

```
crates/wasm/
├── Cargo.toml
├── src/
│   ├── lib.rs              # Main WASM exports
│   ├── hasher.rs           # Xxh3Hasher wrapper
│   ├── builder_v1.rs       # ManifestBuilderV1
│   ├── builder_v2.rs       # ManifestBuilderV2
│   └── utils.rs            # CAS key utilities, chunking
├── pkg/                    # wasm-pack output (generated)
└── deadlinejs/             # TypeScript package
    ├── package.json
    ├── tsconfig.json
    ├── src/
    │   ├── index.ts        # Entry point, re-exports, high-level API
    │   ├── common/
    │   │   ├── index.ts    # Re-exports
    │   │   ├── constants.ts
    │   │   ├── types.ts
    │   │   ├── errors.ts
    │   │   └── progress.ts
    │   ├── model/
    │   │   ├── index.ts    # Re-exports
    │   │   ├── builder-v1.ts
    │   │   ├── builder-v2.ts
    │   │   └── decode.ts
    │   ├── hash/
    │   │   ├── index.ts    # Re-exports, factory
    │   │   ├── executor.ts # HashExecutor interface
    │   │   ├── main-thread.ts
    │   │   ├── worker.ts
    │   │   ├── pooled.ts
    │   │   └── streaming.ts # Low-level hash streaming
    │   └── storage/
    │       ├── index.ts    # Re-exports
    │       ├── types.ts    # S3Location, ManifestLocation, etc.
    │       ├── cas.ts      # CAS key utilities
    │       ├── chunking.ts # Chunk calculation
    │       ├── client.ts   # S3Client wrapper
    │       └── orchestrator.ts # UploadOrchestrator
    ├── worker/
    │   └── hash-worker.ts  # Web Worker script
    └── examples/
        ├── upload-demo.html
        └── upload-demo.ts
```

## Dependencies

### Rust (WASM)
- `wasm-bindgen` - JS interop
- `serde-wasm-bindgen` - Serde to JS values
- `xxhash-rust` - XXH128 implementation (already used)
- `rusty-attachments-model` - Manifest types (already used)

### JavaScript (deadlinejs)
- `@aws-sdk/client-s3` - S3 operations
- `@aws-sdk/lib-storage` - Multipart uploads (for large files)
- `@aws-sdk/types` - Credential types

### Dev Dependencies
- `typescript` - Type checking
- `vite` or `esbuild` - Bundling
- `vitest` - Unit testing

## Security Considerations

1. **Credentials**: Never store AWS credentials in browser storage (localStorage, sessionStorage)
2. **CORS**: Restrict AllowedOrigins in production to your domain
3. **Bucket Policy**: Use ExpectedBucketOwner for all operations
4. **Content Validation**: Validate file types/sizes before upload
5. **CSP**: Ensure Content-Security-Policy allows WASM execution and worker scripts

## Testing Strategy

1. **WASM Unit Tests**: `wasm-bindgen-test` for Rust code
2. **JS Unit Tests**: Vitest for TypeScript modules
3. **Integration Tests**: Vitest with LocalStack S3 mock
4. **E2E Tests**: Playwright with real S3 bucket (optional)
5. **Manual Testing**: Demo page with configurable settings

## Open Questions

1. ~~**Web Workers**: Should hashing run in a Web Worker?~~ → Yes, with abstraction layer
2. **Streaming Uploads**: Use `@aws-sdk/lib-storage` Upload for multipart large files?
3. **Offline Support**: Cache manifests in IndexedDB for resume capability?
4. **Bundle Size**: Tree-shake AWS SDK to minimize bundle? Consider separate entry points.
5. **Node.js Support**: Should deadlinejs also work in Node.js (for CLI tools)?

## References

- [wasm-bindgen Guide](https://rustwasm.github.io/wasm-bindgen/)
- [AWS SDK v3 for JavaScript](https://docs.aws.amazon.com/AWSJavaScriptSDK/v3/latest/)
- [AWS SDK Credential Providers](https://docs.aws.amazon.com/AWSJavaScriptSDK/v3/latest/modules/_aws_sdk_credential_providers.html)
- [File API - MDN](https://developer.mozilla.org/en-US/docs/Web/API/File_API)
- [Web Workers - MDN](https://developer.mozilla.org/en-US/docs/Web/API/Web_Workers_API)
- [Streams API - MDN](https://developer.mozilla.org/en-US/docs/Web/API/Streams_API)
