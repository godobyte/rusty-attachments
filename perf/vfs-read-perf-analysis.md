# VFS Read Path Performance Analysis

Date: 2026-01-02

## Test Configuration
- Command: `sudo perf record -g -F 999 -o perf/vfs-read-perf.data -- ./target/release/examples/mount_vfs job_bundles_manifest_v2.json ./vfs --writable --cache-dir /tmp/vfs-cache`
- Workload: 3x 300MB file writes, manifest file reads, dirty file reads
- Samples: 166K of event 'cpu-clock:ppp'

## Top-Level Breakdown

| Overhead | Command | Symbol | Description |
|----------|---------|--------|-------------|
| 22.00% | vfs-io-worker | `__memmove_avx_unaligned_erms` | Memory copy operations |
| 20.06% | vfs-io-worker | `do_user_addr_fault` | Page fault handling |
| 10.57% | vfs-io-worker | `_raw_spin_unlock_irqrestore` | Kernel spinlock (page mgmt) |
| 8.30% | vfs-io-worker | `clear_page_rep` | Page zeroing |
| 2.14% | vfs-io-worker | `charge_memcg` | Memory cgroup accounting |
| 1.93% | vfs-io-worker | `do_anonymous_page` | Anonymous page allocation |
| 0.67% | mount_vfs | `_raw_spin_unlock_irqrestore` | FUSE request completion |
| 0.37% | mount_vfs | `read` | FUSE device read |
| 0.35% | vfs-io-worker | `_aesni_ctr32_ghash_6x` | TLS decryption (S3) |

## Key Findings

### 1. Memory Operations Dominate (~63% of CPU)

The profile is dominated by memory-related operations:

| Category | CPU % | Root Cause |
|----------|-------|------------|
| memmove | 22.00% | Data copying in vfs-io-worker |
| Page faults | 20.06% | Triggered by memmove into new pages |
| Page clearing | 8.30% | Kernel zeroing new pages |
| Spinlocks (page mgmt) | 10.57% | LRU list management, page allocation |
| memcg accounting | 2.14% | Memory cgroup tracking |

**Root cause**: All page faults originate from `__memmove_avx_unaligned_erms` - the kernel is allocating new pages on-demand as data is copied into buffers.

### 2. Async Executor Overhead is Minimal (~0.2%)

Unlike the write path before optimization, the async executor shows minimal overhead:
- `AsyncExecutor::block_on_internal`: 0.20%
- `vfs-async-execu` futex: 0.09%

This suggests the sync write path optimization is working well.

### 3. VFS Userspace Code is Negligible (<1%)

| Function | CPU % |
|----------|-------|
| `fuser::request::Request::dispatch` | 0.02% |
| `DashMap::_get` | 0.01% |
| `WritableVfs::write` | 0.01% |
| `WritableVfs::read` | 0.00% |
| `DirtyFileManager::write_sync` | 0.01% |
| `DirtyFileManager::is_dirty` | 0.00% |
| `MemoryPool::modify_dirty_in_place_with_slice` | 0.00% |

**Total VFS userspace code: <0.1%**

### 4. S3/Network Operations (~0.5%)

TLS decryption and network I/O are minimal:
- `_aesni_ctr32_ghash_6x`: 0.35%
- `crc_fast::algorithm::process_large_aligned`: 0.11%
- Network stack: <0.1%

### 5. FUSE Protocol Overhead (~1%)

- `mount_vfs read()`: 0.37%
- `fuse_dev_do_read`: 0.05%
- `fuse_dev_do_write`: 0.02%
- `fuse_request_end`: 0.01%

## Comparison with Write Path (V4)

| Metric | Write V4 | Read | Notes |
|--------|----------|------|-------|
| Futex overhead | ~0% | ~0.3% | Both optimized |
| memmove | 4.2% | 22.0% | Read path copies more data |
| Page faults | ~2% | 20.0% | Read allocates more pages |
| VFS userspace | ~2% | <0.1% | Read path is simpler |
| Total kernel overhead | ~35% | ~63% | Read is memory-bound |

## Why Read Path Has Higher Memory Overhead

The read workload (3x 300MB writes + reads) causes:

1. **Large buffer allocations**: Reading 300MB files requires allocating large buffers
2. **Page fault cascade**: Each new page triggers:
   - `do_anonymous_page` - allocate page
   - `clear_page_rep` - zero the page
   - `folio_add_lru` - add to LRU list
   - `charge_memcg` - account to cgroup
3. **Data copying**: S3 data → TLS decrypt → buffer → FUSE response

## Optimization Opportunities

### 1. Pre-allocate Read Buffers (Potential: ~30% reduction)

**Problem**: Page faults during memmove account for 20% of CPU.

**Solution**: Pre-allocate and pre-fault buffers in the memory pool.

```rust
/// Pre-allocate a buffer and touch all pages to avoid faults during reads.
fn prefault_buffer(size: usize) -> Vec<u8> {
    let mut buf: Vec<u8> = vec![0u8; size];
    // Touch every page to ensure it's mapped
    for i in (0..size).step_by(4096) {
        buf[i] = 0;
    }
    buf
}
```

### 2. Use mmap with MAP_POPULATE for Large Reads

**Problem**: Anonymous page allocation is expensive.

**Solution**: For large files, use mmap with `MAP_POPULATE` to pre-fault pages.

```rust
use std::os::unix::io::AsRawFd;

fn mmap_prefault(size: usize) -> *mut u8 {
    unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_POPULATE,
            -1,
            0,
        ) as *mut u8
    }
}
```

### 3. Buffer Pool for Reuse

**Problem**: Each read allocates new buffers that get freed.

**Solution**: Maintain a pool of pre-allocated buffers.

```rust
struct BufferPool {
    small: Vec<Vec<u8>>,   // 64KB buffers
    medium: Vec<Vec<u8>>,  // 1MB buffers
    large: Vec<Vec<u8>>,   // 16MB buffers
}

impl BufferPool {
    fn acquire(&mut self, size: usize) -> Vec<u8> {
        // Return from pool or allocate new
    }
    
    fn release(&mut self, buf: Vec<u8>) {
        // Return to pool for reuse
    }
}
```

### 4. Zero-Copy Read Path (Potential: ~22% reduction)

**Problem**: Data is copied multiple times: S3 → decrypt buffer → pool → FUSE response.

**Solution**: Use `splice()` or direct buffer passing where possible.

For FUSE reads of cached data:
```rust
// Instead of copying to a new buffer, return a reference to pool data
fn read_cached(&self, inode: u64, offset: u64, size: u32) -> &[u8] {
    // Return slice directly from memory pool
}
```

### 5. Huge Pages for Large Allocations

**Problem**: 4KB page allocation overhead for large files.

**Solution**: Use transparent huge pages (THP) or explicit huge pages.

```rust
// Enable THP for the process
std::fs::write("/proc/self/coredump_filter", "0x7f")?;

// Or use madvise
unsafe {
    libc::madvise(ptr, size, libc::MADV_HUGEPAGE);
}
```

## Implementation Phases

### Phase 1: Buffer Pool (Low effort, ~10% improvement)
- Implement reusable buffer pool
- Avoid repeated allocation/deallocation
- Pre-fault buffers on creation

### Phase 2: Pre-allocation Strategy (Medium effort, ~15% improvement)
- Pre-allocate buffers based on expected file sizes
- Use `MAP_POPULATE` for large allocations
- Consider huge pages for >2MB allocations

### Phase 3: Zero-Copy Reads (High effort, ~20% improvement)
- Return references to pool data instead of copying
- Investigate FUSE splice support
- Minimize intermediate buffers

## Expected Improvement Targets

| Phase | Target Reduction | Cumulative |
|-------|------------------|------------|
| Phase 1 | 10% | 10% |
| Phase 2 | 15% | 25% |
| Phase 3 | 20% | 45% |

## Conclusion

The read path is **memory-bound**, not CPU-bound. The VFS userspace code is highly optimized (<0.1% overhead). The bottleneck is kernel page allocation and memory copying.

Unlike the write path optimization (which targeted futex contention), read path optimization should focus on:
1. **Reducing page faults** through pre-allocation
2. **Reducing memory copies** through buffer reuse and zero-copy
3. **Using huge pages** for large allocations

The sync write path optimization is working well - async executor overhead is negligible.
