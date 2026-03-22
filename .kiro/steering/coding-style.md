# Rust Coding Style Guidelines

## Documentation

### Function Documentation
Every function must include:
1. A single-line summary of what the function does
2. Documentation for each argument using `# Arguments` section
3. Return value documentation using `# Returns` section (if non-trivial)

```rust
/// Calculate the expected number of chunks for a file.
///
/// # Arguments
/// * `size` - Total file size in bytes
/// * `chunk_size` - Size of each chunk in bytes (0 disables chunking)
///
/// # Returns
/// Number of chunks needed, minimum 1.
pub fn expected_chunk_count(size: u64, chunk_size: u64) -> usize {
    // ...
}
```

## Type Annotations

### Explicit Types on Variables
Always annotate `let` bindings and function-local variables with explicit types so readers can understand the code without IDE assistance:

```rust
// Good - explicit types
let chunks: Vec<ChunkInfo> = Vec::new();
let offset: u64 = 0;
let dir_index: HashMap<&str, usize> = HashMap::new();

// Avoid - implicit types
let chunks = Vec::new();
let offset = 0;
let dir_index = HashMap::new();
```

### Exception: Obvious Types
Type annotations may be omitted when the type is immediately obvious from the right-hand side:

```rust
// OK - type is obvious from constructor
let manifest = AssetManifest::new(paths);
let error = ManifestError::UnknownVersion(version);
```

## Function Design

### Single Focused Purpose
Primitive/utility functions should have a single, focused purpose:

```rust
// Good - single purpose
pub fn needs_chunking(size: u64, chunk_size: u64) -> bool;
pub fn generate_chunks(size: u64, chunk_size: u64) -> Vec<ChunkInfo>;
pub fn cas_key(hash: &str, algorithm: HashAlgorithm) -> String;

// Avoid - multiple responsibilities
pub fn process_and_upload_file(...) -> Result<...>;  // Split into process_file() and upload_file()
```

### Composition Over Complexity
Build complex operations by composing simple primitives:

```rust
// Primitives
fn hash_file(path: &Path) -> Result<String, Error>;
fn check_exists(client: &Client, key: &str) -> Result<bool, Error>;
fn upload_bytes(client: &Client, key: &str, data: &[u8]) -> Result<(), Error>;

// Composed operation
fn upload_if_missing(client: &Client, path: &Path) -> Result<UploadResult, Error> {
    let hash: String = hash_file(path)?;
    let key: String = cas_key(&hash, HashAlgorithm::Xxh128);
    
    if check_exists(client, &key)? {
        return Ok(UploadResult::skipped());
    }
    
    let data: Vec<u8> = std::fs::read(path)?;
    upload_bytes(client, &key, &data)?;
    Ok(UploadResult::uploaded(data.len() as u64))
}
```


## Performance: Allocation-Conscious Hot Paths

### Map Keys on Hot Paths: Use `Arc<str>` Over `String`
When a `HashMap` or `DashMap` key is cloned frequently (iterators, snapshots, lookups), use `Arc<str>` instead of `String`. Clone is an atomic refcount bump (~1ns) instead of a heap allocation + memcpy (~50ns).

```rust
// Good - clone is a refcount bump
let map: DashMap<Arc<str>, State> = DashMap::new();
let key: Arc<str> = Arc::from(format!("{}:{}", root_id, path_key));
map.insert(key.clone(), state);  // cheap clone

// Avoid - clone allocates on every call
let map: DashMap<String, State> = DashMap::new();
let key: String = format!("{}:{}", root_id, path_key);
map.insert(key.clone(), state);  // heap alloc + memcpy
```

### Provide `&str` Variants for Lookup Methods
When a struct wraps a map with computed keys, provide both `by_parts(a, b)` (allocates the key) and `by_composite(key: &str)` (zero-alloc) variants. Callers on hot paths that already have the composite key should use the zero-alloc variant.

```rust
// Good - zero-alloc lookup when caller already has the key
pub fn get_state_composite(&self, composite: &str) -> Option<State> {
    self.map.get(composite).map(|e| e.clone())
}

// Convenience - allocates, for callers that have parts
pub fn get_state(&self, root_id: &str, path_key: &str) -> Option<State> {
    let key: Arc<str> = composite_key(root_id, path_key);
    self.map.get(&key).map(|e| e.clone())
}
```

### Avoid Full-Map Iteration When Incremental Is Possible
When a background task needs to process map entries, prefer a push-based queue over periodic full iteration. Use a lock-free queue (`crossbeam::SegQueue`) to collect new entries, and drain it each cycle.

```rust
// Good - O(new entries) per cycle
let new_keys: Vec<Arc<str>> = tracker.drain_new_keys();
for key in &new_keys {
    process(key);
}

// Avoid - O(total entries) per cycle, locks each DashMap shard
let all_keys: Vec<Arc<str>> = tracker.pending_keys();
for key in &all_keys {
    process(key);
}
```

### Normalize-on-Check Before Allocating
For string normalization (path cleanup, case folding), check if the input is already normalized before allocating a new string. Most inputs on hot paths are already clean.

```rust
// Good - zero-alloc in the common case
fn normalize_path(path: &str) -> Cow<'_, str> {
    let trimmed: &str = path.trim_matches('/');
    if trimmed == path && !path.contains("//") {
        Cow::Borrowed(path)  // already normalized, no alloc
    } else {
        let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
        Cow::Owned(parts.join("/"))
    }
}

// Avoid - always allocates even when input is clean
fn normalize_path(path: &str) -> String {
    let parts: Vec<&str> = path.trim_matches('/').split('/').filter(|p| !p.is_empty()).collect();
    parts.join("/")
}
```

### Bound Concurrent Fan-Out
When fanning out async operations (e.g., S3 HEAD requests), use `buffer_unordered(limit)` instead of `join_all`. This bounds in-flight requests to match the connection pool size and avoids resource exhaustion.

```rust
// Good - bounded concurrency
use futures::stream::{self, StreamExt};
let results: Vec<_> = stream::iter(keys.iter().map(|k| check_key(k)))
    .buffer_unordered(50)
    .collect()
    .await;

// Avoid - unbounded, may exhaust connection pool
let results: Vec<_> = futures::future::join_all(
    keys.iter().map(|k| check_key(k))
).await;
```
