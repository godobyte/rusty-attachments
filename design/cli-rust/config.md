# Pure-Rust CLI: `ra config`

## Commands

### `ra config show`

Display all configuration settings.

```
ra config show [--json]
```

### `ra config get <key>`

Get a specific configuration value.

```
ra config get defaults.farm_id
ra config get defaults.queue_id
ra config get settings.conflict_resolution
ra config get settings.auto_accept
```

### `ra config set <key> <value>`

Set a configuration value.

```
ra config set defaults.farm_id farm-01234567890123456789012345678901
ra config set defaults.queue_id queue-01234567890123456789012345678901
ra config set settings.conflict_resolution CREATE_COPY
ra config set settings.auto_accept true
```

## Configuration File

Reads and writes `~/.deadline/config` — the same INI-format file used by
the Python CLI, ensuring interoperability:

```ini
[defaults]
farm_id = farm-01234567890123456789012345678901
queue_id = queue-01234567890123456789012345678901
job_id = job-01234567890123456789012345678901

[settings]
auto_accept = false
conflict_resolution = CREATE_COPY
storage_profile_id = sp-01234567890123456789012345678901

[telemetry]
opt_out = true
```

```rust
use configparser::ini::Ini;

pub struct CliConfig {
    ini: Ini,
    path: PathBuf,
}

impl CliConfig {
    /// Load config from default path (~/.deadline/config).
    pub fn load() -> Result<Self, ConfigError> {
        let path: PathBuf = Self::default_path();
        let mut ini: Ini = Ini::new();
        if path.exists() {
            ini.load(&path).map_err(|e| ConfigError::Parse(e))?;
        }
        Ok(Self { ini, path })
    }

    /// Get a setting value.
    pub fn get(&self, key: &str) -> Option<String> {
        let (section, field) = key.split_once('.')?;
        self.ini.get(section, field)
    }

    /// Set a setting value and persist.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), ConfigError> {
        let (section, field) = key.split_once('.')
            .ok_or(ConfigError::InvalidKey(key.to_string()))?;
        self.ini.set(section, field, Some(value.to_string()));
        self.ini.write(&self.path).map_err(|e| ConfigError::Write(e))
    }

    fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".deadline")
            .join("config")
    }
}
```

---

## `ra config benchmark`

Built-in benchmarking tool for comparing Rust performance against Python
on identical workloads.

```
ra config benchmark \
    [--operation snapshot|diff|upload|download|hash] \
    [--root <dir>] \
    [--file-count <N>] \
    [--file-size <bytes>] \
    [--s3-root-uri <uri>] \
    [--iterations <N>] \
    [--warmup <N>] \
    [--output-csv <path>] \
    [--json]
```

### Benchmark Operations

#### `snapshot` — Directory scanning + hashing

Measures: directory walk time, hash time, manifest creation time.

```
ra config benchmark --operation snapshot --root /projects/large_scene
```

Output:
```
Benchmark: snapshot
  Root: /projects/large_scene
  Files: 125,432
  Total size: 48.2 GB
  Iterations: 3

  Walk time:     0.234s ± 0.012s
  Hash time:     12.456s ± 0.089s  (3.87 GB/s)
  Manifest time: 0.045s ± 0.002s
  Total:         12.735s ± 0.091s

  Hash cache hit rate: 98.2% (on iterations 2+)
  Effective throughput: 48.2 GB in 12.735s = 3.78 GB/s
```

#### `diff` — Manifest comparison

```
ra config benchmark --operation diff --root /projects/scene --manifest /tmp/scene.manifest
```

#### `upload` — S3 CAS upload throughput

```
ra config benchmark --operation upload --root /projects/scene --s3-root-uri s3://bucket/prefix
```

Output:
```
Benchmark: upload
  Files: 1,234
  Total size: 2.1 GB
  New files: 45 (312 MB)
  Skipped (cached): 1,189 (1.8 GB)

  S3 check cache time: 0.089s
  HEAD requests: 45
  Upload time: 4.567s (68.3 MB/s)
  Manifest upload: 0.023s
  Total: 4.679s
```

#### `download` — S3 CAS download throughput

```
ra config benchmark --operation download --s3-root-uri s3://bucket/prefix -m /tmp/manifest.json
```

#### `hash` — Pure hashing throughput

Measures raw xxh128 hashing speed without filesystem overhead.

```
ra config benchmark --operation hash --file-count 1000 --file-size 10485760
```

Creates temporary files and hashes them to measure pure CPU throughput:

```
Benchmark: hash (xxh128)
  Files: 1,000 × 10 MB = 10 GB
  Threads: 8

  Sequential: 2.45 GB/s
  Parallel (8 threads): 14.2 GB/s
  Speedup: 5.8x
```

### Synthetic Workload Generation

For benchmarks without existing data, `--file-count` and `--file-size`
generate temporary files:

```rust
/// Generate a temporary directory with synthetic files for benchmarking.
fn generate_synthetic_workload(
    file_count: usize,
    file_size: u64,
) -> Result<tempfile::TempDir, CliError> {
    let dir: tempfile::TempDir = tempfile::tempdir()?;
    let mut rng: rand::rngs::StdRng = rand::SeedableRng::seed_from_u64(42); // deterministic

    for i in 0..file_count {
        let depth: usize = i % 5; // distribute across subdirectories
        let subdir: PathBuf = (0..depth).fold(dir.path().to_path_buf(), |p, d| p.join(format!("d{}", d)));
        std::fs::create_dir_all(&subdir)?;

        let file_path: PathBuf = subdir.join(format!("file_{:06}.bin", i));
        let mut file: std::fs::File = std::fs::File::create(&file_path)?;
        let mut remaining: u64 = file_size;
        let mut buf: Vec<u8> = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let chunk: usize = std::cmp::min(remaining, buf.len() as u64) as usize;
            rng.fill_bytes(&mut buf[..chunk]);
            file.write_all(&buf[..chunk])?;
            remaining -= chunk as u64;
        }
    }
    Ok(dir)
}
```

### CSV Output

For automated benchmarking pipelines:

```
ra config benchmark --operation snapshot --root /data --iterations 5 --output-csv results.csv
```

```csv
operation,files,total_bytes,iteration,walk_ms,hash_ms,total_ms,throughput_mbps,cache_hit_rate
snapshot,125432,51791257600,1,234,12456,12735,3878.2,0.0
snapshot,125432,51791257600,2,45,312,402,122345.6,98.2
snapshot,125432,51791257600,3,43,298,386,127876.3,98.2
```

### Implementation

```rust
#[derive(clap::Args)]
struct BenchmarkArgs {
    /// Operation to benchmark.
    #[arg(long, value_enum)]
    operation: BenchmarkOperation,

    /// Root directory for snapshot/diff/upload benchmarks.
    #[arg(long)]
    root: Option<PathBuf>,

    /// Manifest file for diff/download benchmarks.
    #[arg(long, short)]
    manifest: Option<PathBuf>,

    /// S3 root URI for upload/download benchmarks.
    #[arg(long)]
    s3_root_uri: Option<String>,

    /// Number of synthetic files to generate.
    #[arg(long, default_value = "1000")]
    file_count: usize,

    /// Size of each synthetic file in bytes.
    #[arg(long, default_value = "1048576")]
    file_size: u64,

    /// Number of benchmark iterations.
    #[arg(long, default_value = "3")]
    iterations: usize,

    /// Warmup iterations (not counted in results).
    #[arg(long, default_value = "1")]
    warmup: usize,

    /// Output results to CSV file.
    #[arg(long)]
    output_csv: Option<PathBuf>,
}

#[derive(clap::ValueEnum, Clone)]
enum BenchmarkOperation {
    Snapshot,
    Diff,
    Upload,
    Download,
    Hash,
}
```

## Improvements over Python

1. **Built-in benchmarking** — Python has no built-in benchmark tool.
   Users must write custom scripts with `time` or `cProfile`.

2. **Synthetic workload generation** — Deterministic file generation for
   reproducible benchmarks without requiring real data.

3. **Statistical output** — Mean ± stddev across iterations, throughput
   calculations, cache hit rates.

4. **CSV export** — Machine-readable output for CI/CD performance tracking
   and regression detection.

5. **Warmup iterations** — Separate warmup phase to prime OS caches and
   hash caches before measurement.

6. **Isolated hash benchmark** — Pure CPU throughput measurement without
   filesystem or network variables.
