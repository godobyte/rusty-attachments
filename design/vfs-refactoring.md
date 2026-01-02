# VFS Memory Pool Hash Optimization Refactoring

**Status: ✅ IMPLEMENTED**

The folded hash optimization is complete. `ContentId::Hash` now uses `u64` instead of `String`.
See `crates/vfs/src/memory_pool_v2.rs` and `crates/common/src/hash.rs` for the implementation.

## Problem Analysis

The current VFS memory pool design has a critical performance issue:

```rust
// Current design - PERFORMANCE DISASTER:
pub enum ContentId {
    Hash(String),      // ← STRING ALLOCATION FOR EVERY BLOCK KEY
    Inode(u64),
}
```

**Issues:**
1. **Heap Fragmentation**: Every hash-based block key allocates a 32-character String on the heap
2. **Cache Locality**: Hash lookups require pointer chasing through heap-allocated strings
3. **Memory Overhead**: 32-byte strings + heap metadata vs 8-byte integers
4. **Hash Recomputation**: No caching of folded hash values, repeated string parsing

## Proposed Solution: Folded Hash Integers

### 1. Hash Folding Function

Add to `crates/common/src/hash.rs`:

```rust
/// Fold a 128-bit XXH3 hash into a 64-bit integer for compact storage.
///
/// Uses XOR folding to preserve hash distribution while reducing size.
/// This is suitable for hash tables and memory-efficient storage where
/// the full 128-bit hash isn't required.
///
/// # Arguments
/// * `hash_hex` - 32-character hex string (128-bit hash)
///
/// # Returns
/// 64-bit folded hash value
///
/// # Panics
/// Panics if hash_hex is not exactly 32 hex characters
pub fn fold_hash_to_u64(hash_hex: &str) -> u64 {
    assert_eq!(hash_hex.len(), 32, "Hash must be 32 hex characters");
    
    // Parse as u128, then fold upper and lower 64 bits
    let full_hash: u128 = u128::from_str_radix(hash_hex, 16)
        .expect("Invalid hex hash");
    
    let upper: u64 = (full_hash >> 64) as u64;
    let lower: u64 = full_hash as u64;
    
    // XOR fold preserves distribution
    upper ^ lower
}

/// Compute XXH128 hash and return folded 64-bit value.
///
/// # Arguments
/// * `data` - Bytes to hash
///
/// # Returns
/// 64-bit folded hash value
pub fn hash_bytes_folded(data: &[u8]) -> u64 {
    let hash_hex: String = hash_bytes(data);
    fold_hash_to_u64(&hash_hex)
}

/// Streaming hasher that can return folded 64-bit values.
impl Xxh3Hasher {
    /// Finalize and return folded 64-bit hash.
    pub fn finish_folded(&self) -> u64 {
        let full_hash: u128 = self.finish();
        let upper: u64 = (full_hash >> 64) as u64;
        let lower: u64 = full_hash as u64;
        upper ^ lower
    }
}
```

### 2. Optimized ContentId

Replace in `crates/vfs/src/memory_pool.rs`:

```rust
/// Identifier for block content - either hash-based or inode-based.
///
/// Hash-based IDs use folded 64-bit integers for memory efficiency.
/// The original hash string can be reconstructed if needed, but most
/// operations only need the folded value for lookups and comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentId {
    /// Folded hash for read-only content (64-bit for efficiency).
    /// Original hash can be reconstructed via reverse lookup if needed.
    Hash(u64),
    /// Inode-based ID for dirty content (must be flushed before eviction).
    Inode(u64),
}

impl ContentId {
    /// Create a hash-based content ID from hex string.
    ///
    /// # Arguments
    /// * `hash_hex` - 32-character hex string (128-bit hash)
    pub fn from_hash_hex(hash_hex: &str) -> Self {
        ContentId::Hash(fold_hash_to_u64(hash_hex))
    }
    
    /// Create a hash-based content ID from raw bytes.
    ///
    /// # Arguments
    /// * `data` - Bytes to hash and fold
    pub fn from_bytes(data: &[u8]) -> Self {
        ContentId::Hash(hash_bytes_folded(data))
    }
    
    /// Check if this is a hash-based (read-only) content ID.
    pub fn is_hash(&self) -> bool {
        matches!(self, ContentId::Hash(_))
    }

    /// Check if this is an inode-based (dirty) content ID.
    pub fn is_inode(&self) -> bool {
        matches!(self, ContentId::Inode(_))
    }

    /// Get the folded hash if this is a hash-based ID.
    pub fn as_hash_folded(&self) -> Option<u64> {
        match self {
            ContentId::Hash(h) => Some(*h),
            ContentId::Inode(_) => None,
        }
    }

    /// Get the inode ID if this is an inode-based ID.
    pub fn as_inode(&self) -> Option<u64> {
        match self {
            ContentId::Hash(_) => None,
            ContentId::Inode(id) => Some(*id),
        }
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentId::Hash(h) => write!(f, "hash_folded:{:016x}", h),
            ContentId::Inode(id) => write!(f, "inode:{}", id),
        }
    }
}
```

### 3. Hash Cache for Reverse Lookups

Add optional hash cache for cases where original hash strings are needed:

```rust
/// Cache for reverse hash lookups (folded u64 -> original hex string).
///
/// Most operations only need the folded hash, but some (like S3 operations)
/// need the original hash string. This cache avoids recomputation.
#[derive(Debug, Default)]
pub struct HashCache {
    folded_to_hex: std::sync::RwLock<HashMap<u64, String>>,
}

impl HashCache {
    /// Create a new hash cache.
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Insert a hash mapping.
    ///
    /// # Arguments
    /// * `hash_hex` - Original 32-character hex string
    /// * `folded` - Folded 64-bit value
    pub fn insert(&self, hash_hex: String, folded: u64) {
        let mut cache = self.folded_to_hex.write().unwrap();
        cache.insert(folded, hash_hex);
    }
    
    /// Get original hash string from folded value.
    ///
    /// # Arguments
    /// * `folded` - Folded 64-bit hash value
    ///
    /// # Returns
    /// Original hash string if cached, None otherwise
    pub fn get(&self, folded: u64) -> Option<String> {
        let cache = self.folded_to_hex.read().unwrap();
        cache.get(&folded).cloned()
    }
    
    /// Remove a hash mapping (for cleanup).
    pub fn remove(&self, folded: u64) -> Option<String> {
        let mut cache = self.folded_to_hex.write().unwrap();
        cache.remove(&folded)
    }
}
```

### 4. Updated BlockKey

```rust
impl BlockKey {
    /// Create a new block key from a content hash hex string.
    ///
    /// # Arguments
    /// * `hash_hex` - 32-character hex string (128-bit hash)
    /// * `chunk_index` - Zero-based chunk index within the file
    pub fn from_hash_hex(hash_hex: &str, chunk_index: u32) -> Self {
        Self {
            id: ContentId::from_hash_hex(hash_hex),
            chunk_index,
        }
    }
    
    /// Create a new block key from raw bytes (computes and folds hash).
    ///
    /// # Arguments
    /// * `data` - Bytes to hash
    /// * `chunk_index` - Zero-based chunk index within the file
    pub fn from_bytes(data: &[u8], chunk_index: u32) -> Self {
        Self {
            id: ContentId::from_bytes(data),
            chunk_index,
        }
    }

    /// Get the folded hash if this is a hash-based key.
    pub fn hash_folded(&self) -> Option<u64> {
        self.id.as_hash_folded()
    }
}
```

## Performance Benefits

### Memory Usage
- **Before**: 32-byte String + heap metadata per hash (~40+ bytes)
- **After**: 8-byte u64 (80% reduction)

### Cache Locality
- **Before**: Hash lookups require pointer chasing through heap
- **After**: Direct integer comparison in hash table

### Hash Table Performance
- **Before**: String hashing + heap allocation for keys
- **After**: Direct u64 hashing (much faster)

### Heap Fragmentation
- **Before**: Thousands of 32-byte allocations fragment heap
- **After**: No per-hash allocations

## Migration Strategy

### Phase 1: Add New Functions
1. Add folding functions to `crates/common/src/hash.rs`
2. Add tests for hash folding correctness
3. Ensure folding preserves hash distribution

### Phase 2: Complete ContentId Replacement
1. **REPLACE** `ContentId::Hash(String)` with `ContentId::Hash(u64)` - NO fallbacks
2. Update all constructors and accessors to use folded hashes
3. Add HashCache for reverse lookups where needed
4. **REMOVE** all old String-based hash methods

### Phase 3: Update All Callers
1. Update VFS code to use folded hashes exclusively
2. Add HashCache integration where original strings needed
3. Update all tests to use new APIs
4. **DELETE** any remaining String-based hash code paths

### Phase 4: Final Cleanup & Validation
1. **REMOVE** all deprecated/unused code paths
2. **DELETE** any orphaned String-based hash functions
3. Ensure no fallback mechanisms remain
4. Performance benchmarks and memory profiling
5. Comprehensive testing with large file sets

## Complete API Removal List

### Functions to DELETE (no deprecation):
```rust
// OLD - DELETE these from BlockKey
impl BlockKey {
    // DELETE: pub fn from_hash(hash: impl Into<String>, chunk_index: u32) -> Self
}

// OLD - DELETE these from ContentId  
impl ContentId {
    // DELETE: pub fn as_hash(&self) -> Option<&str>
}

// OLD - DELETE any String-based constructors in VFS code
```

### Updated APIs (REPLACE, don't add alongside):
```rust
// NEW - REPLACE old methods with these
impl BlockKey {
    /// Create from hex string (REPLACES from_hash)
    pub fn from_hash_hex(hash_hex: &str, chunk_index: u32) -> Self
    
    /// Create from raw bytes (NEW)
    pub fn from_bytes(data: &[u8], chunk_index: u32) -> Self
}

impl ContentId {
    /// Get folded hash (REPLACES as_hash)
    pub fn as_hash_folded(&self) -> Option<u64>
}
```

## Compatibility Notes

- **Hash Collision Risk**: Folding 128-bit to 64-bit increases collision probability from ~2^64 to ~2^32 operations. For VFS cache this is acceptable.
- **Reverse Lookup**: Original hash strings available via HashCache when needed for S3 operations
- **Deterministic**: Same input always produces same folded hash
- **Distribution**: XOR folding preserves hash distribution properties

## Testing Requirements

```rust
#[test]
fn test_hash_folding_deterministic() {
    let hash = "abcdef1234567890abcdef1234567890";
    let folded1 = fold_hash_to_u64(hash);
    let folded2 = fold_hash_to_u64(hash);
    assert_eq!(folded1, folded2);
}

#[test]
fn test_hash_folding_different_inputs() {
    let hash1 = "abcdef1234567890abcdef1234567890";
    let hash2 = "fedcba0987654321fedcba0987654321";
    let folded1 = fold_hash_to_u64(hash1);
    let folded2 = fold_hash_to_u64(hash2);
    assert_ne!(folded1, folded2);
}

#[test]
fn test_content_id_size() {
    // Verify ContentId is now 16 bytes (u64 + enum tag) instead of 32+ bytes
    assert_eq!(std::mem::size_of::<ContentId>(), 16);
}
```

## Clean Implementation Requirements

### NO Deprecated APIs
- **Zero tolerance** for deprecated methods or fallback code paths
- All old String-based hash APIs must be **DELETED**, not deprecated
- No `#[deprecated]` annotations - clean removal only
- No compatibility shims or wrapper functions

### NO Orphaned Code
- **DELETE** all unused String-based hash functions
- **REMOVE** any dead code paths that reference old ContentId::Hash(String)
- **ELIMINATE** any test code using old APIs
- **PURGE** documentation references to String-based hashes

### Clean Codebase Checklist
After implementation, verify:
- [ ] No `ContentId::Hash(String)` references anywhere
- [ ] No `BlockKey::from_hash(impl Into<String>)` method exists
- [ ] No `ContentId::as_hash() -> Option<&str>` method exists
- [ ] All VFS tests use new folded hash APIs
- [ ] All documentation updated to reflect u64 hashes
- [ ] No compiler warnings about unused code
- [ ] `cargo clippy` passes with no hash-related warnings

### Implementation Order for Clean Removal
1. **Add new folded hash functions** (additive)
2. **Update ContentId enum** (breaking change - all at once)
3. **Replace ALL callers simultaneously** (no gradual migration)
4. **Delete old methods completely** (no deprecation period)
5. **Update all tests** to use new APIs only
6. **Final cleanup pass** to remove any missed String references

This ensures a clean, modern codebase with no legacy baggage or confusing API surface.