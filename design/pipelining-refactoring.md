# Hash+Upload Pipelining Refactoring Plan

**Date:** 2026-01-23  
**Goal:** Implement pipelined hash+upload that reads files once into memory, hashes, then uploads from the same buffer.

---

## Table of Contents

1. [Problem Statement](#problem-statement)
2. [Test Scenarios](#test-scenarios)
3. [Phase 1: Test Harness](#phase-1-test-harness)
4. [Phase 2: Baseline Benchmarks](#phase-2-baseline-benchmarks)
5. [Phase 3: Refactoring Implementation](#phase-3-refactoring-implementation)
6. [Phase 4: Post-Refactoring Benchmarks](#phase-4-post-refactoring-benchmarks)
7. [Reference Files](#reference-files)

---

## Problem Statement

### Current Rust Architecture (Sequential)

```
Phase 1: FileSystemScanner::snapshot()
  - Walk directory
  - Read each file → Hash → Discard data
  - Build manifest with hashes

Phase 2: UploadOrchestrator::upload_manifest_contents()
  - Read each file AGAIN
  - Upload to S3
```

**Issues:**
- Files read twice (once for hash, once for upload)
- No concurrent hash+upload for different files
- No hash deduplication (duplicate files uploaded multiple times)
- No memory backpressure control

### Target Architecture (Pipelined)

```
Single Phase: hash_upload_abs_manifest()
  - Read file into memory buffer
  - Hash the buffer (spawn_blocking for CPU work)
  - Upload from same buffer (async I/O)
  - Release buffer after upload completes
  - Semaphore controls memory usage
```

**Benefits:**
- Single file read
- Concurrent hash+upload across files
- Hash deduplication
- Memory bounded by semaphore

---

## Test Scenarios

### VFX Job Simulation Dataset

Realistic VFX job with scene files, textures, caches, and renders:


| Category | Count | Size Range | Total Size | Description |
|----------|-------|------------|------------|-------------|
| **Scene Files** | 5 | 10-50 MB | ~150 MB | Maya/Houdini scene files |
| **Textures (Small)** | 100 | 1-10 KB | ~500 KB | Icons, thumbnails |
| **Textures (Medium)** | 50 | 100 KB - 5 MB | ~100 MB | Diffuse, normal maps |
| **Textures (Large)** | 20 | 10-100 MB | ~1 GB | 4K/8K textures, HDRIs |
| **Geometry Caches** | 10 | 50-200 MB | ~1 GB | Alembic, VDB files |
| **Simulation Caches** | 5 | 200 MB - 1 GB | ~2.5 GB | Fluid/particle sims |
| **Render Outputs** | 20 | 5-50 MB | ~500 MB | EXR sequences |
| **Config/Scripts** | 50 | 1-100 KB | ~2 MB | Python, JSON, YAML |
| **Duplicates** | 10 | Various | ~200 MB | Same content, different names |
| **TOTAL** | ~270 | 1 KB - 1 GB | ~6 GB | |

### Test Configurations

```rust
/// Test configuration for benchmark runs.
pub struct TestConfig {
    /// Name of the test scenario.
    pub name: &'static str,
    /// File distribution.
    pub files: Vec<FileSpec>,
    /// Whether to clear caches before run.
    pub clear_caches: bool,
    /// Number of iterations for averaging.
    pub iterations: u32,
}

pub struct FileSpec {
    pub count: u32,
    pub min_size: u64,
    pub max_size: u64,
    pub name_pattern: &'static str,
}
```

### Specific Test Cases

| Test ID | Name | Files | Clear Cache | Purpose |
|---------|------|-------|-------------|---------|
| T1 | Small Files Only | 100 × 1-10 KB | Yes | Baseline small file perf |
| T2 | Medium Files Only | 50 × 1-10 MB | Yes | Typical texture upload |
| T3 | Large Files Only | 10 × 100-500 MB | Yes | Chunked file handling |
| T4 | Mixed VFX Job | 270 files (above) | Yes | Realistic workload |
| T5 | Incremental (Warm) | 270 files | No | Cache hit performance |
| T6 | Partial Change | 270 files, 10% modified | No | Incremental update |
| T7 | Duplicate Heavy | 50 files, 50% duplicates | Yes | Deduplication benefit |
| T8 | Memory Pressure | 20 × 500 MB | Yes | Backpressure testing |

---

## Phase 1: Test Harness

### 1.1 Create Test Data Generator

**File:** `crates/storage/benches/test_data.rs`

```rust
use std::path::PathBuf;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;

/// Generate test files with reproducible random content.
pub struct TestDataGenerator {
    root: PathBuf,
    rng: StdRng,
}

impl TestDataGenerator {
    pub fn new(root: PathBuf, seed: u64) -> Self {
        Self {
            root,
            rng: StdRng::seed_from_u64(seed),
        }
    }

    /// Generate a file with random content.
    pub fn generate_file(&mut self, name: &str, size: u64) -> std::io::Result<PathBuf> {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        let mut file = std::fs::File::create(&path)?;
        let mut remaining = size;
        let mut buffer = vec![0u8; 64 * 1024]; // 64KB chunks
        
        while remaining > 0 {
            let chunk_size = std::cmp::min(remaining, buffer.len() as u64) as usize;
            self.rng.fill(&mut buffer[..chunk_size]);
            std::io::Write::write_all(&mut file, &buffer[..chunk_size])?;
            remaining -= chunk_size as u64;
        }
        
        Ok(path)
    }

    /// Generate VFX job test dataset.
    pub fn generate_vfx_dataset(&mut self) -> std::io::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        
        // Scene files (5 × 10-50 MB)
        for i in 0..5 {
            let size = self.rng.gen_range(10..50) * 1024 * 1024;
            files.push(self.generate_file(&format!("scenes/scene_{:02}.ma", i), size)?);
        }
        
        // Small textures (100 × 1-10 KB)
        for i in 0..100 {
            let size = self.rng.gen_range(1..10) * 1024;
            files.push(self.generate_file(&format!("textures/small/tex_{:03}.png", i), size)?);
        }
        
        // Medium textures (50 × 100 KB - 5 MB)
        for i in 0..50 {
            let size = self.rng.gen_range(100..5000) * 1024;
            files.push(self.generate_file(&format!("textures/medium/tex_{:03}.exr", i), size)?);
        }
        
        // Large textures (20 × 10-100 MB)
        for i in 0..20 {
            let size = self.rng.gen_range(10..100) * 1024 * 1024;
            files.push(self.generate_file(&format!("textures/large/tex_{:03}.exr", i), size)?);
        }
        
        // Geometry caches (10 × 50-200 MB)
        for i in 0..10 {
            let size = self.rng.gen_range(50..200) * 1024 * 1024;
            files.push(self.generate_file(&format!("geo/cache_{:02}.abc", i), size)?);
        }
        
        // Simulation caches (5 × 200 MB - 1 GB)
        for i in 0..5 {
            let size = self.rng.gen_range(200..1000) * 1024 * 1024;
            files.push(self.generate_file(&format!("sim/sim_{:02}.vdb", i), size)?);
        }
        
        // Render outputs (20 × 5-50 MB)
        for i in 0..20 {
            let size = self.rng.gen_range(5..50) * 1024 * 1024;
            files.push(self.generate_file(&format!("renders/frame_{:04}.exr", i), size)?);
        }
        
        // Config files (50 × 1-100 KB)
        for i in 0..50 {
            let size = self.rng.gen_range(1..100) * 1024;
            files.push(self.generate_file(&format!("config/config_{:02}.json", i), size)?);
        }
        
        Ok(files)
    }

    /// Create duplicate files (same content, different names).
    pub fn create_duplicates(&mut self, source_files: &[PathBuf], count: usize) -> std::io::Result<Vec<PathBuf>> {
        let mut duplicates = Vec::new();
        for i in 0..count {
            let source = &source_files[i % source_files.len()];
            let dest = self.root.join(format!("duplicates/dup_{:02}_{}", i, source.file_name().unwrap().to_string_lossy()));
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(source, &dest)?;
            duplicates.push(dest);
        }
        Ok(duplicates)
    }
}
```


### 1.2 Create Benchmark Harness

**File:** `crates/storage/benches/hash_upload_bench.rs`

```rust
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

mod test_data;
use test_data::TestDataGenerator;

/// Metrics collected during benchmark.
#[derive(Debug, Clone, Default)]
pub struct BenchmarkMetrics {
    /// Total wall-clock time.
    pub total_time: Duration,
    /// Time spent hashing (if measurable separately).
    pub hash_time: Option<Duration>,
    /// Time spent uploading (if measurable separately).
    pub upload_time: Option<Duration>,
    /// Peak memory usage in bytes.
    pub peak_memory_bytes: u64,
    /// Total bytes processed.
    pub total_bytes: u64,
    /// Files processed.
    pub files_processed: u64,
    /// Files skipped (cache hits).
    pub files_skipped: u64,
    /// Effective throughput (bytes/sec).
    pub throughput_bytes_per_sec: f64,
}

/// Run benchmark with current (sequential) implementation.
pub async fn benchmark_sequential(
    test_dir: &PathBuf,
    s3_bucket: &str,
    s3_prefix: &str,
    clear_cache: bool,
) -> BenchmarkMetrics {
    use rusty_attachments_filesystem::{FileSystemScanner, SnapshotOptions};
    use rusty_attachments_storage::{UploadOrchestrator, S3Location};
    
    if clear_cache {
        // Clear S3 check cache and hash cache
        // TODO: Implement cache clearing
    }
    
    let start = Instant::now();
    let start_memory = get_memory_usage();
    let mut peak_memory = start_memory;
    
    // Phase 1: Hash all files
    let scanner = FileSystemScanner::new();
    let options = SnapshotOptions {
        root: test_dir.clone(),
        ..Default::default()
    };
    
    let hash_start = Instant::now();
    let manifest = scanner.snapshot(&options, None).expect("snapshot failed");
    let hash_time = hash_start.elapsed();
    
    peak_memory = peak_memory.max(get_memory_usage());
    
    // Phase 2: Upload all files
    let location = S3Location::new(s3_bucket, s3_prefix, "Data", "Manifests");
    // let orchestrator = UploadOrchestrator::new(&client, location);
    
    let upload_start = Instant::now();
    // let stats = orchestrator.upload_manifest_contents(&manifest, test_dir.to_str().unwrap(), None).await?;
    let upload_time = upload_start.elapsed();
    
    peak_memory = peak_memory.max(get_memory_usage());
    
    let total_time = start.elapsed();
    let total_bytes: u64 = manifest.total_size();
    
    BenchmarkMetrics {
        total_time,
        hash_time: Some(hash_time),
        upload_time: Some(upload_time),
        peak_memory_bytes: peak_memory - start_memory,
        total_bytes,
        files_processed: manifest.file_count() as u64,
        files_skipped: 0, // TODO: Get from stats
        throughput_bytes_per_sec: total_bytes as f64 / total_time.as_secs_f64(),
    }
}

/// Get current process memory usage.
fn get_memory_usage() -> u64 {
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if line.starts_with("VmRSS:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return kb * 1024;
                        }
                    }
                }
            }
        }
    }
    0
}

/// Print benchmark results in a table.
pub fn print_results(name: &str, metrics: &BenchmarkMetrics) {
    println!("\n=== {} ===", name);
    println!("Total time:      {:?}", metrics.total_time);
    if let Some(hash_time) = metrics.hash_time {
        println!("  Hash time:     {:?}", hash_time);
    }
    if let Some(upload_time) = metrics.upload_time {
        println!("  Upload time:   {:?}", upload_time);
    }
    println!("Peak memory:     {} MB", metrics.peak_memory_bytes / (1024 * 1024));
    println!("Total bytes:     {} MB", metrics.total_bytes / (1024 * 1024));
    println!("Files processed: {}", metrics.files_processed);
    println!("Files skipped:   {}", metrics.files_skipped);
    println!("Throughput:      {:.2} MB/s", metrics.throughput_bytes_per_sec / (1024.0 * 1024.0));
}
```

### 1.3 Create CLI Test Runner

**File:** `crates/storage/examples/bench_hash_upload.rs`

```rust
//! Benchmark runner for hash+upload performance testing.
//!
//! Usage:
//!   cargo run --example bench_hash_upload -- --test-dir /tmp/bench --bucket my-bucket --prefix test
//!
//! Options:
//!   --test-dir    Directory for test data (will be created)
//!   --bucket      S3 bucket name
//!   --prefix      S3 key prefix
//!   --generate    Generate test data (default: true)
//!   --clear-cache Clear caches before each run (default: true)
//!   --iterations  Number of iterations (default: 3)

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "bench_hash_upload")]
struct Args {
    /// Directory for test data.
    #[arg(long, default_value = "/tmp/hash_upload_bench")]
    test_dir: PathBuf,

    /// S3 bucket name.
    #[arg(long)]
    bucket: String,

    /// S3 key prefix.
    #[arg(long, default_value = "bench")]
    prefix: String,

    /// Generate test data.
    #[arg(long, default_value = "true")]
    generate: bool,

    /// Clear caches before each run.
    #[arg(long, default_value = "true")]
    clear_cache: bool,

    /// Number of iterations.
    #[arg(long, default_value = "3")]
    iterations: u32,

    /// Test scenario to run.
    #[arg(long, default_value = "vfx")]
    scenario: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    
    println!("Hash+Upload Benchmark");
    println!("=====================");
    println!("Test dir:    {}", args.test_dir.display());
    println!("Bucket:      {}", args.bucket);
    println!("Prefix:      {}", args.prefix);
    println!("Scenario:    {}", args.scenario);
    println!("Iterations:  {}", args.iterations);
    println!();

    // Generate test data if needed
    if args.generate {
        println!("Generating test data...");
        // TODO: Call TestDataGenerator
    }

    // Run benchmarks
    for i in 1..=args.iterations {
        println!("\n--- Iteration {}/{} ---", i, args.iterations);
        
        // TODO: Run benchmark_sequential
        // TODO: Print results
    }

    Ok(())
}
```

---


## Phase 2: Baseline Benchmarks

### 2.1 Metrics to Capture

| Metric | How | Why |
|--------|-----|-----|
| Total wall time | `Instant::now()` | Primary performance metric |
| Hash phase time | Separate timing | Understand phase breakdown |
| Upload phase time | Separate timing | Understand phase breakdown |
| Peak RSS memory | `/proc/self/status` | Memory efficiency |
| Bytes transferred | From stats | Verify correctness |
| Files skipped | From stats | Cache effectiveness |
| CPU utilization | `top` or `/proc/stat` | Resource usage |
| Network throughput | Bytes / upload time | Network saturation |

### 2.2 Expected Baseline Results

Based on analysis, the sequential approach should show:

| Scenario | Expected Hash Time | Expected Upload Time | Total Time |
|----------|-------------------|---------------------|------------|
| T1 (100 small) | ~0.1s | ~2s | ~2.1s |
| T2 (50 medium) | ~2s | ~10s | ~12s |
| T3 (10 large) | ~10s | ~30s | ~40s |
| T4 (VFX job) | ~30s | ~60s | ~90s |

**Key observation:** Hash and upload times are additive, not overlapping.

### 2.3 Test Environment Configuration

**S3 Bucket:** `s3://adeadlineja/rusty`

**Credentials:** Source credentials before running any tests:
```bash
source creds.sh
```

**Cache Locations to Clear:**
```bash
# Hash cache (SQLite database)
rm -rf ~/.cache/rusty-attachments/hash_cache.db

# S3 check cache (SQLite database)  
rm -rf ~/.cache/rusty-attachments/s3_check_cache.db

# Any local data cache
rm -rf ~/.cache/rusty-attachments/data_cache/
```

**Helper script for cache clearing:**

**File:** `scripts/clear_caches.sh`
```bash
#!/bin/bash
# Clear all caches for clean benchmark runs

set -e

CACHE_DIR="${HOME}/.cache/rusty-attachments"

echo "Clearing rusty-attachments caches..."

# Hash cache
if [ -f "${CACHE_DIR}/hash_cache.db" ]; then
    rm -f "${CACHE_DIR}/hash_cache.db"
    echo "  Removed hash_cache.db"
fi

# S3 check cache
if [ -f "${CACHE_DIR}/s3_check_cache.db" ]; then
    rm -f "${CACHE_DIR}/s3_check_cache.db"
    echo "  Removed s3_check_cache.db"
fi

# Data cache directory
if [ -d "${CACHE_DIR}/data_cache" ]; then
    rm -rf "${CACHE_DIR}/data_cache"
    echo "  Removed data_cache/"
fi

# S3 test prefix (optional - clears uploaded test data)
if [ "$1" == "--s3" ]; then
    echo "Clearing S3 test prefix..."
    aws s3 rm s3://adeadlineja/rusty/bench/ --recursive
    echo "  Removed s3://adeadlineja/rusty/bench/"
fi

echo "Done."
```

### 2.4 Run Baseline Tests

```bash
# Setup credentials
source creds.sh

# Clear all caches
./scripts/clear_caches.sh

# Generate test data
cargo run --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/baseline \
  --generate true \
  --scenario vfx

# Run with cache cleared (cold run)
./scripts/clear_caches.sh
cargo run --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/baseline \
  --clear-cache true \
  --iterations 3

# Run with warm cache
cargo run --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/baseline \
  --clear-cache false \
  --iterations 3
```

---

## Phase 2.5: Performance Profiling with `perf`

### 2.5.1 Why Profile?

Benchmarks tell us *how fast* the code runs, but profiling tells us *why*. We need to understand:

- Where CPU time is spent (hashing vs I/O vs overhead)
- Memory allocation patterns
- Lock contention in concurrent code
- Whether the implementation is optimal or has bottlenecks

### 2.5.2 Profiling Setup

**Prerequisites:**
```bash
# Install perf (Linux)
sudo apt-get install linux-tools-common linux-tools-generic linux-tools-$(uname -r)

# Allow perf for non-root users
echo 0 | sudo tee /proc/sys/kernel/perf_event_paranoid
echo 0 | sudo tee /proc/sys/kernel/kptr_restrict

# Install flamegraph tools
cargo install flamegraph
cargo install inferno
```

**Build with debug symbols:**
```bash
# Add to Cargo.toml or use RUSTFLAGS
[profile.release]
debug = true  # Keep debug symbols for profiling

# Or build with:
RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release --example bench_hash_upload
```

### 2.5.3 Baseline Profiling (Before Refactoring)

**CPU Profile:**
```bash
source creds.sh
./scripts/clear_caches.sh

# Record CPU profile during benchmark
perf record -g --call-graph dwarf -F 99 \
  cargo run --release --example bench_hash_upload -- \
    --test-dir /tmp/bench_data \
    --bucket adeadlineja \
    --prefix rusty/bench/perf-baseline \
    --scenario vfx \
    --iterations 1

# Generate report
perf report --hierarchy --sort dso,sym

# Generate flamegraph
perf script | inferno-collapse-perf | inferno-flamegraph > perf/baseline-flamegraph.svg
```

**Alternative: cargo-flamegraph (simpler):**
```bash
source creds.sh
./scripts/clear_caches.sh

cargo flamegraph --release --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/perf-baseline \
  --scenario vfx \
  --iterations 1

mv flamegraph.svg perf/baseline-flamegraph.svg
```

**Memory Profile (with heaptrack):**
```bash
# Install heaptrack
sudo apt-get install heaptrack heaptrack-gui

source creds.sh
./scripts/clear_caches.sh

heaptrack cargo run --release --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/perf-baseline \
  --scenario vfx \
  --iterations 1

# Analyze
heaptrack_gui heaptrack.bench_hash_upload.*.gz
```

### 2.5.4 Key Metrics to Extract from Profiles

| Metric | What to Look For | Optimal Value |
|--------|------------------|---------------|
| Hash function % | Time in xxh3/xxh128 | Should dominate CPU time |
| I/O wait % | Time in read/write syscalls | Should be low (async I/O) |
| Lock contention | Time in mutex/semaphore | Should be minimal |
| Memory allocations | Allocation count/size | Should be bounded |
| Tokio runtime % | Time in tokio internals | Should be low overhead |

**Expected baseline profile breakdown:**
```
Sequential Implementation:
├── 40% - File hashing (xxh3)
├── 30% - File I/O (read for hash, read for upload)
├── 20% - Network I/O (S3 upload)
├── 5%  - Manifest building
└── 5%  - Other overhead
```

### 2.5.5 Post-Refactoring Profiling

After implementing the pipelined version, repeat the same profiling:

```bash
source creds.sh
./scripts/clear_caches.sh

# CPU profile
cargo flamegraph --release --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/perf-pipelined \
  --scenario vfx \
  --iterations 1 \
  --implementation pipelined

mv flamegraph.svg perf/pipelined-flamegraph.svg

# Memory profile
heaptrack cargo run --release --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/perf-pipelined \
  --scenario vfx \
  --iterations 1 \
  --implementation pipelined
```

**Expected pipelined profile breakdown:**
```
Pipelined Implementation:
├── 35% - File hashing (xxh3) - overlapped with upload
├── 15% - File I/O (single read per file)
├── 35% - Network I/O (S3 upload) - overlapped with hash
├── 5%  - Memory pool management
├── 5%  - Deduplication tracking
└── 5%  - Other overhead
```

### 2.5.6 Profile Comparison Checklist

| Aspect | Baseline | Pipelined | Expected Change |
|--------|----------|-----------|-----------------|
| Total CPU time | X | Y | ~same or less |
| File read syscalls | 2N | N | -50% |
| Peak memory | Low | Bounded | Controlled increase |
| Lock contention | N/A | Low | New but minimal |
| Tokio task switches | Low | Higher | Expected increase |
| Network utilization | Bursty | Steady | More consistent |

### 2.5.7 Profiling Output Files

Store all profiling artifacts in `perf/` directory:

```
perf/
├── baseline-flamegraph.svg       # CPU flamegraph before refactoring
├── baseline-perf.data            # Raw perf data
├── baseline-heaptrack.gz         # Memory profile
├── pipelined-flamegraph.svg      # CPU flamegraph after refactoring
├── pipelined-perf.data           # Raw perf data
├── pipelined-heaptrack.gz        # Memory profile
├── comparison-report.md          # Written analysis
└── optimization-notes.md         # Ideas for further optimization
```

### 2.5.8 Identifying Optimization Opportunities

After profiling, look for:

1. **Hot spots** - Functions taking >10% of CPU time
2. **Unexpected allocations** - Memory churn in hot paths
3. **Lock contention** - Threads waiting on mutexes
4. **Syscall overhead** - Too many small I/O operations
5. **Cache misses** - Poor data locality

**Common optimizations to consider:**
- Buffer pooling to reduce allocations
- Batch small files together
- Adjust concurrency limits based on profile
- Use `spawn_blocking` strategically for CPU work
- Tune semaphore permit sizes

---

## Phase 3: Refactoring Implementation

### 3.1 New Module Structure

```
crates/storage/src/
├── hash_upload/
│   ├── mod.rs              # Public API
│   ├── options.rs          # HashUploadOptions
│   ├── pipeline.rs         # Core pipeline implementation
│   ├── memory_pool.rs      # Memory backpressure
│   ├── deduplication.rs    # Hash deduplication
│   └── progress.rs         # Progress tracking
├── upload.rs               # Existing (keep for compatibility)
└── ...
```

### 3.1.1 Preserving Composable Primitives

**Important:** The existing standalone hash and upload functions MUST be preserved. The pipelined implementation is an optimization for the common case, but the individual primitives remain valuable for:

- **Custom workflows** where users need fine-grained control
- **Testing** individual components in isolation
- **Composition** in scenarios not covered by the pipeline (e.g., hash-only for manifest generation without upload)
- **Backward compatibility** with existing code

**Functions to preserve:**

| Module | Function | Purpose |
|--------|----------|---------|
| `crates/filesystem/src/scanner.rs` | `hash_file()` | Hash a single file |
| `crates/filesystem/src/scanner.rs` | `FileSystemScanner::snapshot()` | Hash all files in directory |
| `crates/storage/src/upload.rs` | `upload_file()` | Upload a single file |
| `crates/storage/src/upload.rs` | `UploadOrchestrator::upload_manifest_contents()` | Upload all manifest files |

**Usage guidance:**

```rust
// Option 1: Use pipelined API for best performance (new)
let result = hash_upload_abs_manifest(manifest, source_root, &data_cache, ...).await?;

// Option 2: Use composable primitives for custom workflows (preserved)
let manifest = scanner.snapshot(&options, progress)?;  // Hash only
let stats = orchestrator.upload_manifest_contents(&manifest, root, progress).await?;  // Upload only

// Option 3: Mix and match for special cases
let hash = hash_file(&path)?;  // Hash single file
if !data_cache.object_exists(&hash, alg).await? {
    upload_file(&data_cache, &path, &hash).await?;  // Upload single file
}
```

### 3.2 Step-by-Step Implementation

#### Step 1: Memory Pool (Day 1)

**File:** `crates/storage/src/hash_upload/memory_pool.rs`

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Semaphore;

/// Memory pool with backpressure for pipelined operations.
/// 
/// Controls memory usage by limiting concurrent in-flight data.
/// When the pool is exhausted, new allocations block until
/// memory is released.
pub struct MemoryPool {
    /// Maximum bytes allowed.
    max_bytes: u64,
    /// Currently allocated bytes.
    allocated: AtomicU64,
    /// Semaphore for blocking when full.
    semaphore: Semaphore,
    /// Permit size (granularity of allocation).
    permit_size: u64,
}

impl MemoryPool {
    /// Create a new memory pool.
    ///
    /// # Arguments
    /// * `max_bytes` - Maximum memory to allow
    /// * `permit_size` - Size of each permit (e.g., 64MB)
    pub fn new(max_bytes: u64, permit_size: u64) -> Self {
        let permits = (max_bytes / permit_size).max(1) as usize;
        Self {
            max_bytes,
            allocated: AtomicU64::new(0),
            semaphore: Semaphore::new(permits),
            permit_size,
        }
    }

    /// Allocate memory from the pool.
    /// 
    /// Blocks if the pool is exhausted until memory is released.
    pub async fn allocate(&self, size: u64) -> MemoryPermit {
        // Calculate permits needed (round up)
        let permits_needed = ((size + self.permit_size - 1) / self.permit_size) as u32;
        
        // Acquire permits (blocks if not available)
        let permit = self.semaphore
            .acquire_many(permits_needed)
            .await
            .expect("semaphore closed");
        
        self.allocated.fetch_add(size, Ordering::Relaxed);
        
        MemoryPermit {
            size,
            permit: Some(permit),
        }
    }

    /// Try to allocate without blocking.
    pub fn try_allocate(&self, size: u64) -> Option<MemoryPermit> {
        let permits_needed = ((size + self.permit_size - 1) / self.permit_size) as u32;
        
        match self.semaphore.try_acquire_many(permits_needed) {
            Ok(permit) => {
                self.allocated.fetch_add(size, Ordering::Relaxed);
                Some(MemoryPermit {
                    size,
                    permit: Some(permit),
                })
            }
            Err(_) => None,
        }
    }

    /// Get current allocated bytes.
    pub fn allocated(&self) -> u64 {
        self.allocated.load(Ordering::Relaxed)
    }

    /// Get maximum bytes.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

/// RAII guard for allocated memory.
pub struct MemoryPermit<'a> {
    size: u64,
    permit: Option<tokio::sync::SemaphorePermit<'a>>,
}

impl Drop for MemoryPermit<'_> {
    fn drop(&mut self) {
        // Permit is automatically released when dropped
        self.permit.take();
    }
}
```


#### Step 2: Hash Deduplication (Day 1)

**File:** `crates/storage/src/hash_upload/deduplication.rs`

```rust
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

/// Tracks in-flight uploads to prevent duplicate uploads of the same hash.
///
/// When multiple files have the same content (same hash), only one upload
/// is performed. Other files wait for the first upload to complete.
pub struct UploadDeduplicator {
    /// Map of hash -> broadcast sender for completion notification.
    in_flight: Mutex<HashMap<String, broadcast::Sender<()>>>,
}

impl UploadDeduplicator {
    pub fn new() -> Self {
        Self {
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Register intent to upload a hash.
    ///
    /// Returns:
    /// - `UploadIntent::Proceed` if this is the first uploader
    /// - `UploadIntent::Wait(receiver)` if another upload is in progress
    pub fn register(&self, hash: &str) -> UploadIntent {
        let mut in_flight = self.in_flight.lock().unwrap();
        
        if let Some(sender) = in_flight.get(hash) {
            // Another upload in progress, subscribe to completion
            UploadIntent::Wait(sender.subscribe())
        } else {
            // First uploader, register and proceed
            let (sender, _) = broadcast::channel(1);
            in_flight.insert(hash.to_string(), sender);
            UploadIntent::Proceed
        }
    }

    /// Mark upload as complete.
    ///
    /// Notifies all waiters that the upload is done.
    pub fn complete(&self, hash: &str) {
        let mut in_flight = self.in_flight.lock().unwrap();
        if let Some(sender) = in_flight.remove(hash) {
            // Notify all waiters (ignore errors if no receivers)
            let _ = sender.send(());
        }
    }

    /// Mark upload as failed.
    ///
    /// Removes the registration so another uploader can try.
    pub fn failed(&self, hash: &str) {
        let mut in_flight = self.in_flight.lock().unwrap();
        in_flight.remove(hash);
    }
}

pub enum UploadIntent {
    /// Proceed with upload (first uploader).
    Proceed,
    /// Wait for existing upload to complete.
    Wait(broadcast::Receiver<()>),
}

impl Default for UploadDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}
```

#### Step 3: Options and Progress (Day 2)

**File:** `crates/storage/src/hash_upload/options.rs`

```rust
/// Options for hash+upload pipeline.
#[derive(Debug, Clone)]
pub struct HashUploadOptions {
    /// Maximum memory for in-flight data (default: 1GB).
    pub max_memory_bytes: u64,
    /// Maximum concurrent operations (default: 10).
    pub max_concurrency: usize,
    /// Chunk size for large files (default: 256MB).
    pub chunk_size: u64,
    /// Whether to use hash cache.
    pub use_hash_cache: bool,
    /// Whether to use S3 check cache.
    pub use_s3_check_cache: bool,
    /// Force rehash even if cached.
    pub force_rehash: bool,
}

impl Default for HashUploadOptions {
    fn default() -> Self {
        Self {
            max_memory_bytes: 1024 * 1024 * 1024, // 1GB
            max_concurrency: 10,
            chunk_size: 256 * 1024 * 1024, // 256MB
            use_hash_cache: true,
            use_s3_check_cache: true,
            force_rehash: false,
        }
    }
}

impl HashUploadOptions {
    /// Calculate memory based on system resources.
    pub fn auto_memory() -> u64 {
        // Similar to Python: min(16GB, max(256MB, quarter_of_total, available - 1GB))
        #[cfg(target_os = "linux")]
        {
            use std::fs;
            if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
                let mut total_kb: u64 = 0;
                let mut available_kb: u64 = 0;
                
                for line in meminfo.lines() {
                    if line.starts_with("MemTotal:") {
                        total_kb = parse_meminfo_value(line);
                    } else if line.starts_with("MemAvailable:") {
                        available_kb = parse_meminfo_value(line);
                    }
                }
                
                let quarter_total = total_kb * 1024 / 4;
                let available_minus_1gb = available_kb.saturating_sub(1024 * 1024) * 1024;
                
                let min_bytes = 256 * 1024 * 1024; // 256MB
                let max_bytes = 16 * 1024 * 1024 * 1024; // 16GB
                
                return max_bytes.min(min_bytes.max(quarter_total).max(available_minus_1gb));
            }
        }
        
        // Default fallback
        1024 * 1024 * 1024 // 1GB
    }
}

#[cfg(target_os = "linux")]
fn parse_meminfo_value(line: &str) -> u64 {
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
```

**File:** `crates/storage/src/hash_upload/progress.rs`

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Progress metadata for hash+upload operations.
#[derive(Debug, Clone)]
pub struct HashUploadProgress {
    // Totals
    pub total_files: u64,
    pub total_bytes: u64,
    
    // Hashing progress
    pub hashed_files: u64,
    pub hashed_bytes: u64,
    pub hash_skipped_files: u64,
    pub hash_skipped_bytes: u64,
    
    // Upload progress
    pub uploaded_files: u64,
    pub uploaded_bytes: u64,
    pub upload_skipped_files: u64,
    pub upload_skipped_bytes: u64,
    
    // Timing
    pub elapsed_secs: f64,
    pub transfer_rate_bytes_per_sec: f64,
    
    // Overall
    pub progress_percent: f64,
    pub message: String,
}

/// Thread-safe progress tracker.
pub struct ProgressTracker {
    start_time: Instant,
    total_files: u64,
    total_bytes: u64,
    
    hashed_files: AtomicU64,
    hashed_bytes: AtomicU64,
    hash_skipped_files: AtomicU64,
    hash_skipped_bytes: AtomicU64,
    
    uploaded_files: AtomicU64,
    uploaded_bytes: AtomicU64,
    upload_skipped_files: AtomicU64,
    upload_skipped_bytes: AtomicU64,
}

impl ProgressTracker {
    pub fn new(total_files: u64, total_bytes: u64) -> Self {
        Self {
            start_time: Instant::now(),
            total_files,
            total_bytes,
            hashed_files: AtomicU64::new(0),
            hashed_bytes: AtomicU64::new(0),
            hash_skipped_files: AtomicU64::new(0),
            hash_skipped_bytes: AtomicU64::new(0),
            uploaded_files: AtomicU64::new(0),
            uploaded_bytes: AtomicU64::new(0),
            upload_skipped_files: AtomicU64::new(0),
            upload_skipped_bytes: AtomicU64::new(0),
        }
    }

    pub fn record_hash_complete(&self, bytes: u64, skipped: bool) {
        if skipped {
            self.hash_skipped_files.fetch_add(1, Ordering::Relaxed);
            self.hash_skipped_bytes.fetch_add(bytes, Ordering::Relaxed);
        } else {
            self.hashed_files.fetch_add(1, Ordering::Relaxed);
            self.hashed_bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    pub fn record_upload_complete(&self, bytes: u64, skipped: bool) {
        if skipped {
            self.upload_skipped_files.fetch_add(1, Ordering::Relaxed);
            self.upload_skipped_bytes.fetch_add(bytes, Ordering::Relaxed);
        } else {
            self.uploaded_files.fetch_add(1, Ordering::Relaxed);
            self.uploaded_bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> HashUploadProgress {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let uploaded_bytes = self.uploaded_bytes.load(Ordering::Relaxed)
            + self.upload_skipped_bytes.load(Ordering::Relaxed);
        
        let progress_percent = if self.total_bytes > 0 {
            (uploaded_bytes as f64 / self.total_bytes as f64) * 100.0
        } else {
            0.0
        };
        
        let transfer_rate = if elapsed > 0.0 {
            uploaded_bytes as f64 / elapsed
        } else {
            0.0
        };

        HashUploadProgress {
            total_files: self.total_files,
            total_bytes: self.total_bytes,
            hashed_files: self.hashed_files.load(Ordering::Relaxed),
            hashed_bytes: self.hashed_bytes.load(Ordering::Relaxed),
            hash_skipped_files: self.hash_skipped_files.load(Ordering::Relaxed),
            hash_skipped_bytes: self.hash_skipped_bytes.load(Ordering::Relaxed),
            uploaded_files: self.uploaded_files.load(Ordering::Relaxed),
            uploaded_bytes: self.uploaded_bytes.load(Ordering::Relaxed),
            upload_skipped_files: self.upload_skipped_files.load(Ordering::Relaxed),
            upload_skipped_bytes: self.upload_skipped_bytes.load(Ordering::Relaxed),
            elapsed_secs: elapsed,
            transfer_rate_bytes_per_sec: transfer_rate,
            progress_percent,
            message: format!(
                "Hashed {:.1} MB, Uploaded {:.1} MB / {:.1} MB ({:.1}%)",
                (self.hashed_bytes.load(Ordering::Relaxed) + self.hash_skipped_bytes.load(Ordering::Relaxed)) as f64 / 1_000_000.0,
                uploaded_bytes as f64 / 1_000_000.0,
                self.total_bytes as f64 / 1_000_000.0,
                progress_percent
            ),
        }
    }
}
```


#### Step 4: Core Pipeline (Day 2-3)

**File:** `crates/storage/src/hash_upload/pipeline.rs`

```rust
use std::path::Path;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use rusty_attachments_model::HashAlgorithm;
use tokio::task::spawn_blocking;

use crate::error::StorageError;
use crate::traits::ContentAddressedDataCache;
use crate::hash_cache::HashCache;

use super::deduplication::{UploadDeduplicator, UploadIntent};
use super::memory_pool::MemoryPool;
use super::options::HashUploadOptions;
use super::progress::ProgressTracker;

/// Work item for the pipeline.
#[derive(Debug)]
pub struct WorkItem {
    /// Absolute path to the file.
    pub path: String,
    /// File size in bytes.
    pub size: u64,
    /// Modification time in microseconds.
    pub mtime: i64,
}

/// Result of processing a single file.
#[derive(Debug)]
pub struct ProcessedItem {
    /// Original path.
    pub path: String,
    /// Computed hash.
    pub hash: String,
    /// File size.
    pub size: u64,
    /// Whether upload was skipped (already existed).
    pub upload_skipped: bool,
    /// Whether hash was from cache.
    pub hash_cached: bool,
}

/// Pipelined hash+upload processor.
pub struct HashUploadPipeline<'a, C: ContentAddressedDataCache> {
    data_cache: &'a C,
    hash_cache: Option<&'a HashCache>,
    options: HashUploadOptions,
    memory_pool: Arc<MemoryPool>,
    deduplicator: Arc<UploadDeduplicator>,
    progress: Arc<ProgressTracker>,
    hash_alg: HashAlgorithm,
}

impl<'a, C: ContentAddressedDataCache> HashUploadPipeline<'a, C> {
    pub fn new(
        data_cache: &'a C,
        hash_cache: Option<&'a HashCache>,
        options: HashUploadOptions,
        total_files: u64,
        total_bytes: u64,
    ) -> Self {
        let memory_pool = Arc::new(MemoryPool::new(
            options.max_memory_bytes,
            64 * 1024 * 1024, // 64MB permit size
        ));
        
        Self {
            data_cache,
            hash_cache,
            options,
            memory_pool,
            deduplicator: Arc::new(UploadDeduplicator::new()),
            progress: Arc::new(ProgressTracker::new(total_files, total_bytes)),
            hash_alg: HashAlgorithm::Xxh128,
        }
    }

    /// Process all work items through the pipeline.
    pub async fn process(
        &self,
        items: Vec<WorkItem>,
    ) -> Result<Vec<ProcessedItem>, StorageError> {
        let results: Vec<Result<ProcessedItem, StorageError>> = stream::iter(items)
            .map(|item| self.process_item(item))
            .buffer_unordered(self.options.max_concurrency)
            .collect()
            .await;

        // Collect results, propagating first error
        let mut processed = Vec::with_capacity(results.len());
        for result in results {
            processed.push(result?);
        }
        
        Ok(processed)
    }

    /// Process a single work item.
    async fn process_item(&self, item: WorkItem) -> Result<ProcessedItem, StorageError> {
        // Step 1: Check hash cache
        let cached_hash = if self.options.use_hash_cache && !self.options.force_rehash {
            if let Some(cache) = self.hash_cache {
                cache.get(&item.path, item.size, item.mtime).await
            } else {
                None
            }
        } else {
            None
        };

        // Step 2: Check if we can skip entirely (hash cached + exists in data cache)
        if let Some(ref hash) = cached_hash {
            if self.options.use_s3_check_cache {
                if self.data_cache.object_exists(hash, self.hash_alg).await? {
                    self.progress.record_hash_complete(item.size, true);
                    self.progress.record_upload_complete(item.size, true);
                    return Ok(ProcessedItem {
                        path: item.path,
                        hash: hash.clone(),
                        size: item.size,
                        upload_skipped: true,
                        hash_cached: true,
                    });
                }
            }
        }

        // Step 3: Allocate memory and read file
        let _permit = self.memory_pool.allocate(item.size).await;
        
        let path = item.path.clone();
        let data: Vec<u8> = spawn_blocking(move || {
            std::fs::read(&path)
        })
        .await
        .map_err(|e| StorageError::Other { message: e.to_string() })?
        .map_err(|e| StorageError::IoError {
            path: item.path.clone(),
            message: e.to_string(),
        })?;

        // Step 4: Compute hash (if not cached)
        let hash = if let Some(h) = cached_hash {
            self.progress.record_hash_complete(item.size, true);
            h
        } else {
            let data_clone = data.clone();
            let hash = spawn_blocking(move || {
                use rusty_attachments_common::Xxh3Hasher;
                let mut hasher = Xxh3Hasher::new();
                hasher.update(&data_clone);
                hasher.finish_hex()
            })
            .await
            .map_err(|e| StorageError::Other { message: e.to_string() })?;
            
            self.progress.record_hash_complete(item.size, false);
            
            // Update hash cache
            if let Some(cache) = self.hash_cache {
                cache.put(&item.path, item.size, item.mtime, hash.clone()).await;
            }
            
            hash
        };

        // Step 5: Upload (with deduplication)
        let upload_skipped = self.upload_with_dedup(&hash, &data).await?;
        
        self.progress.record_upload_complete(item.size, upload_skipped);

        Ok(ProcessedItem {
            path: item.path,
            hash,
            size: item.size,
            upload_skipped,
            hash_cached: cached_hash.is_some(),
        })
    }

    /// Upload data with deduplication.
    async fn upload_with_dedup(&self, hash: &str, data: &[u8]) -> Result<bool, StorageError> {
        // Check if already exists
        if self.data_cache.object_exists(hash, self.hash_alg).await? {
            return Ok(true);
        }

        // Register upload intent
        match self.deduplicator.register(hash) {
            UploadIntent::Proceed => {
                // We're the first uploader
                let result = self.data_cache.put_object(hash, self.hash_alg, data).await;
                
                if result.is_ok() {
                    self.deduplicator.complete(hash);
                } else {
                    self.deduplicator.failed(hash);
                }
                
                result?;
                Ok(false)
            }
            UploadIntent::Wait(mut receiver) => {
                // Wait for other upload to complete
                let _ = receiver.recv().await;
                Ok(true) // Skipped because another upload handled it
            }
        }
    }

    /// Get current progress.
    pub fn progress(&self) -> super::progress::HashUploadProgress {
        self.progress.snapshot()
    }
}
```


#### Step 5: Public API (Day 3)

**File:** `crates/storage/src/hash_upload/mod.rs`

```rust
//! Pipelined hash+upload operations.
//!
//! This module provides a combined hash+upload operation that reads files
//! once into memory, computes the hash, and uploads from the same buffer.
//!
//! # Benefits over sequential approach
//!
//! - Single file read (vs. read for hash, then read for upload)
//! - Concurrent hash+upload across different files
//! - Hash deduplication (duplicate files uploaded once)
//! - Memory backpressure (bounded memory usage)
//!
//! # Example
//!
//! ```ignore
//! use rusty_attachments_storage::hash_upload::{hash_upload_abs_manifest, HashUploadOptions};
//!
//! let result = hash_upload_abs_manifest(
//!     manifest,
//!     &data_cache,
//!     Some(&hash_cache),
//!     HashUploadOptions::default(),
//!     Some(&progress_callback),
//! ).await?;
//! ```

mod deduplication;
mod memory_pool;
mod options;
mod pipeline;
mod progress;

pub use options::HashUploadOptions;
pub use progress::{HashUploadProgress, ProgressTracker};

use std::path::Path;

use rusty_attachments_model::{v2025_12, HashAlgorithm, Manifest};

use crate::error::StorageError;
use crate::hash_cache::HashCache;
use crate::traits::ContentAddressedDataCache;
use crate::types::TransferStatistics;

use pipeline::{HashUploadPipeline, ProcessedItem, WorkItem};

/// Result of hash_upload_abs_manifest operation.
pub struct HashUploadResult {
    /// Updated manifest with all hashes filled in.
    pub manifest: Manifest,
    /// Transfer statistics.
    pub statistics: TransferStatistics,
    /// Detailed progress at completion.
    pub progress: HashUploadProgress,
}

/// Hash and upload manifest contents in a pipelined manner.
///
/// This operation combines hashing and uploading into a single pass over the data,
/// avoiding the need to read files twice.
///
/// # Arguments
/// * `manifest` - Manifest with files to process (hashes may be empty)
/// * `source_root` - Root directory where files are located
/// * `data_cache` - Content-addressable storage destination
/// * `hash_cache` - Optional hash cache for efficiency
/// * `options` - Pipeline configuration options
///
/// # Returns
/// Result containing updated manifest and statistics.
pub async fn hash_upload_abs_manifest<C: ContentAddressedDataCache>(
    manifest: Manifest,
    source_root: &str,
    data_cache: &C,
    hash_cache: Option<&HashCache>,
    options: HashUploadOptions,
) -> Result<HashUploadResult, StorageError> {
    // Collect work items from manifest
    let (work_items, total_bytes) = collect_work_items(&manifest, source_root)?;
    let total_files = work_items.len() as u64;

    // Create pipeline
    let pipeline = HashUploadPipeline::new(
        data_cache,
        hash_cache,
        options,
        total_files,
        total_bytes,
    );

    // Process all items
    let processed = pipeline.process(work_items).await?;

    // Build result manifest with hashes
    let result_manifest = build_result_manifest(manifest, &processed)?;

    // Build statistics
    let progress = pipeline.progress();
    let statistics = TransferStatistics {
        files_processed: progress.hashed_files + progress.hash_skipped_files,
        files_transferred: progress.uploaded_files,
        files_skipped: progress.upload_skipped_files,
        bytes_transferred: progress.uploaded_bytes,
        bytes_skipped: progress.upload_skipped_bytes,
        errors: vec![],
    };

    Ok(HashUploadResult {
        manifest: result_manifest,
        statistics,
        progress,
    })
}

/// Collect work items from manifest.
fn collect_work_items(
    manifest: &Manifest,
    source_root: &str,
) -> Result<(Vec<WorkItem>, u64), StorageError> {
    let mut items = Vec::new();
    let mut total_bytes: u64 = 0;

    match manifest {
        Manifest::V2023_03_03(m) => {
            for path in &m.paths {
                items.push(WorkItem {
                    path: format!("{}/{}", source_root, path.path),
                    size: path.size,
                    mtime: path.mtime,
                });
                total_bytes += path.size;
            }
        }
        Manifest::V2025_12(m) => {
            for file in &m.files {
                // Skip symlinks and deleted entries
                if file.symlink_target.is_some() || file.deleted {
                    continue;
                }
                
                let size = file.size.unwrap_or(0);
                items.push(WorkItem {
                    path: format!("{}/{}", source_root, file.path),
                    size,
                    mtime: file.mtime.unwrap_or(0),
                });
                total_bytes += size;
            }
        }
    }

    Ok((items, total_bytes))
}

/// Build result manifest with hashes from processed items.
fn build_result_manifest(
    original: Manifest,
    processed: &[ProcessedItem],
) -> Result<Manifest, StorageError> {
    // Create a map of path -> hash for quick lookup
    let hash_map: std::collections::HashMap<&str, &str> = processed
        .iter()
        .map(|p| (p.path.as_str(), p.hash.as_str()))
        .collect();

    match original {
        Manifest::V2023_03_03(m) => {
            let paths: Vec<_> = m.paths.into_iter().map(|mut p| {
                // Find the hash for this path
                // Note: We need to reconstruct the full path that was used
                if let Some(hash) = hash_map.get(p.path.as_str()) {
                    p.hash = hash.to_string();
                }
                p
            }).collect();
            
            Ok(Manifest::V2023_03_03(rusty_attachments_model::v2023_03_03::AssetManifest::new(paths)))
        }
        Manifest::V2025_12(m) => {
            let files: Vec<_> = m.files.into_iter().map(|mut f| {
                if f.symlink_target.is_none() && !f.deleted {
                    if let Some(hash) = hash_map.get(f.path.as_str()) {
                        f.hash = Some(hash.to_string());
                    }
                }
                f
            }).collect();
            
            Ok(Manifest::V2025_12(v2025_12::AssetManifest {
                dirs: m.dirs,
                files,
                ..m
            }))
        }
    }
}
```

---


## Phase 4: Post-Refactoring Benchmarks

### 4.1 Expected Improvements

| Scenario | Before (Sequential) | After (Pipelined) | Improvement |
|----------|--------------------|--------------------|-------------|
| T1 (100 small) | ~2.1s | ~1.5s | ~30% |
| T2 (50 medium) | ~12s | ~8s | ~33% |
| T3 (10 large) | ~40s | ~25s | ~38% |
| T4 (VFX job) | ~90s | ~55s | ~39% |
| T7 (duplicates) | ~30s | ~15s | ~50% |

**Key improvements:**
- Hash and upload overlap for different files
- Single file read instead of two
- Duplicate files uploaded once

### 4.2 Comparison Script

```bash
# Setup credentials
source creds.sh

# Clear caches and S3 test data
./scripts/clear_caches.sh --s3

# Run both implementations on same data
echo "=== Sequential Implementation ==="
./scripts/clear_caches.sh
cargo run --release --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/sequential \
  --implementation sequential

echo "=== Pipelined Implementation ==="
./scripts/clear_caches.sh
cargo run --release --example bench_hash_upload -- \
  --test-dir /tmp/bench_data \
  --bucket adeadlineja \
  --prefix rusty/bench/pipelined \
  --implementation pipelined
```

### 4.3 Metrics Comparison Table

| Metric | Sequential | Pipelined | Delta |
|--------|------------|-----------|-------|
| Total time | | | |
| Hash time | | | N/A (overlapped) |
| Upload time | | | N/A (overlapped) |
| Peak memory | | | |
| File reads | 2× | 1× | -50% |
| Duplicate uploads | N | 1 | -N+1 |

### 4.4 Profile Comparison Analysis

After running both implementations with profiling (see Phase 2.5), compare:

**Flamegraph Comparison:**
```bash
# Open both flamegraphs side by side
firefox perf/baseline-flamegraph.svg perf/pipelined-flamegraph.svg
```

**Key questions to answer:**

1. **Did file I/O decrease?**
   - Look for `read` syscall percentage
   - Should drop from ~30% to ~15%

2. **Is hash/upload overlapping?**
   - In baseline: hash and upload are sequential blocks
   - In pipelined: should see interleaved execution

3. **Is memory bounded?**
   - Check heaptrack for peak allocation
   - Should be close to `max_memory_bytes` setting

4. **Any unexpected bottlenecks?**
   - Lock contention in deduplicator?
   - Semaphore overhead in memory pool?
   - Tokio runtime overhead?

**Write comparison report:**

**File:** `perf/comparison-report.md`
```markdown
# Hash+Upload Performance Comparison

## Test Configuration
- Date: YYYY-MM-DD
- Dataset: VFX job (~6GB, 270 files)
- S3 Bucket: s3://adeadlineja/rusty/bench/
- Machine: [specs]

## Benchmark Results

| Metric | Sequential | Pipelined | Improvement |
|--------|------------|-----------|-------------|
| Total time | Xs | Ys | Z% |
| Peak memory | X MB | Y MB | +/- Z% |
| Throughput | X MB/s | Y MB/s | Z% |

## Profile Analysis

### Sequential Implementation
- Top 5 functions by CPU time:
  1. ...
  2. ...

### Pipelined Implementation  
- Top 5 functions by CPU time:
  1. ...
  2. ...

### Key Observations
- [observation 1]
- [observation 2]

## Optimization Opportunities
- [opportunity 1]
- [opportunity 2]

## Conclusion
[summary]
```

---

## Reference Files

### Files to Read Before Starting

| File | Purpose |
|------|---------|
| `design/pipelining.md` | Background analysis |
| `crates/storage/src/upload.rs` | Current upload implementation |
| `crates/filesystem/src/scanner.rs` | Current hashing implementation |
| `crates/storage/src/traits.rs` | Storage interfaces |
| `crates/storage/src/data_cache.rs` | Data cache implementations |
| `crates/storage/src/hash_cache/mod.rs` | Hash cache interface |
| `crates/common/src/hash.rs` | Hashing utilities |

### Python Reference Files

| File | Purpose |
|------|---------|
| `context/deadline-cloud/.../hash_upload_abs_manifest.py` | Python entry point |
| `context/deadline-cloud/.../hash_upload_abs_manifest_pipeline.py` | Python pipeline base |
| `context/deadline-cloud/.../hash_upload_abs_manifest_s3_pipeline.py` | Python S3 specifics |

### Key Code Sections

**Current hashing (scanner.rs:350-400):**
```rust
// Hash the file
let hash: String = hash_file(&file.path).map_err(|e| FileSystemError::IoError {
    path: file.path.display().to_string(),
    source: e,
})?;
```

**Current upload (upload.rs:150-200):**
```rust
// Upload the file (reads file again)
self.client
    .put_object_from_file(
        &self.location.bucket,
        &s3_key,
        local_path,
        None,
        None,
        progress,
    )
    .await?;
```

**Python memory pool (_hash_upload_abs_manifest_pipeline.py:300-350):**
```python
class _MemoryPool:
    def allocate(self, size: int) -> None:
        with self._space_available:
            while self._allocated_bytes + size > self._max_bytes:
                self._space_available.wait()  # BACKPRESSURE
            self._allocated_bytes += size
```

---

## Implementation Checklist

### Phase 1: Test Harness
- [ ] Create `crates/storage/benches/test_data.rs`
- [ ] Create `crates/storage/benches/hash_upload_bench.rs`
- [ ] Create `crates/storage/examples/bench_hash_upload.rs`
- [ ] Create `scripts/clear_caches.sh`
- [ ] Generate VFX test dataset
- [ ] Verify test data generation works

### Phase 2: Baseline Benchmarks
- [ ] Run T1-T8 with sequential implementation
- [ ] Record all metrics
- [ ] Document baseline results

### Phase 2.5: Baseline Profiling
- [ ] Install perf, flamegraph, heaptrack tools
- [ ] Build with debug symbols (`debug = true` in release profile)
- [ ] Run CPU profile with `perf record` or `cargo flamegraph`
- [ ] Generate baseline flamegraph (`perf/baseline-flamegraph.svg`)
- [ ] Run memory profile with heaptrack
- [ ] Document baseline profile breakdown

### Phase 3: Refactoring
- [ ] Create `crates/storage/src/hash_upload/` module
- [ ] Implement `memory_pool.rs`
- [ ] Implement `deduplication.rs`
- [ ] Implement `options.rs`
- [ ] Implement `progress.rs`
- [ ] Implement `pipeline.rs`
- [ ] Implement `mod.rs` (public API)
- [ ] **Preserve existing composable primitives** (do NOT delete `hash_file()`, `upload_file()`, etc.)
- [ ] Add unit tests for each component
- [ ] Integration test with FileSystemDataCache

### Phase 4: Post-Refactoring Benchmarks
- [ ] Run T1-T8 with pipelined implementation
- [ ] Record all metrics
- [ ] Compare with baseline
- [ ] Document improvements

### Phase 4.5: Post-Refactoring Profiling
- [ ] Run CPU profile with pipelined implementation
- [ ] Generate pipelined flamegraph (`perf/pipelined-flamegraph.svg`)
- [ ] Run memory profile with heaptrack
- [ ] Compare flamegraphs (baseline vs pipelined)
- [ ] Write comparison report (`perf/comparison-report.md`)
- [ ] Identify any remaining optimization opportunities
- [ ] Document findings in `perf/optimization-notes.md`

---

## Notes for Multi-Context Continuation

When continuing this work in a new context:

1. **Read this file first:** `design/pipelining-refactoring.md`
2. **Setup credentials:** `source creds.sh`
3. **Check implementation status:** Look at the checklist above
4. **Read reference files:** Listed in "Reference Files" section
5. **Current phase:** Check which phase is in progress
6. **Test data location:** `/tmp/hash_upload_bench` (or as configured)
7. **S3 test bucket:** `s3://adeadlineja/rusty/bench/`

### Quick Start Commands

```bash
# Setup credentials (REQUIRED before any S3 operations)
source creds.sh

# Clear all caches
./scripts/clear_caches.sh

# Check current implementation status
ls -la crates/storage/src/hash_upload/

# Run existing tests
cargo test -p rusty-attachments-storage

# Build benchmark
cargo build --release --example bench_hash_upload

# Generate test data
cargo run --release --example bench_hash_upload -- --generate --test-dir /tmp/bench
```
