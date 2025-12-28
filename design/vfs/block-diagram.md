# VFS Block Diagrams

## Block Diagram 1: Data Structures with Implemented Traits

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                                    TRAIT HIERARCHY                                       │
└─────────────────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────┐      ┌─────────────────────┐      ┌─────────────────────┐
│     INode (trait)   │      │  FileStore (trait)  │      │ WriteCache (trait)  │
│  ─────────────────  │      │  ─────────────────  │      │  ─────────────────  │
│  id()               │      │  retrieve()         │      │  write_file()       │
│  parent_id()        │      │  retrieve_range()   │      │  read_file()        │
│  name()             │      │  cas_key()          │      │  delete_file()      │
│  path()             │      │  cas_prefix()       │      │  is_deleted()       │
│  inode_type()       │      └─────────────────────┘      │  list_files()       │
│  size()             │               ▲                   │  cache_dir()        │
│  mtime()            │               │                   └─────────────────────┘
│  permissions()      │      ┌────────┴────────┐                   ▲
│  hash_algorithm()   │      │                 │          ┌────────┴────────┐
│  as_any()           │      │                 │          │                 │
└─────────────────────┘      │                 │          │                 │
         ▲                   │                 │          │                 │
         │                   ▼                 ▼          ▼                 ▼
┌────────┼────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│        │        │  │MemoryFile   │  │StorageClient │  │Materialized  │  │MemoryWrite   │
│        │        │  │Store        │  │Adapter<C>    │  │Cache         │  │Cache         │
│        │        │  │(test impl)  │  │(S3 adapter)  │  │(disk-based)  │  │(test impl)   │
│        │        │  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘
│        │        │
▼        ▼        ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│  INodeFile   │  │  INodeDir    │  │INodeSymlink  │
│  ──────────  │  │  ──────────  │  │  ──────────  │
│  content     │  │  children    │  │  target      │
│  executable  │  │  add_child() │  └──────────────┘
│  hash_alg    │  │  get_child() │
└──────────────┘  │  remove_child│
       │          └──────────────┘
       │
       ▼
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│                                    FileContent (enum)                                     │
│  ┌─────────────────────────────────┐    ┌─────────────────────────────────────────────┐  │
│  │  SingleHash(String)             │    │  Chunked(Vec<String>)                       │  │
│  │  - For small files (<256MB)     │    │  - For large files (>256MB)                 │  │
│  │  - Single CAS key               │    │  - Multiple chunk hashes                    │  │
│  └─────────────────────────────────┘    └─────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

---

## Block Diagram 2: Core Manager Dependencies

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                              MANAGER LAYER DEPENDENCIES                                  │
└─────────────────────────────────────────────────────────────────────────────────────────┘

                              ┌─────────────────────────┐
                              │      WritableVfs        │
                              │  (FUSE Filesystem impl) │
                              └───────────┬─────────────┘
                                          │
              ┌───────────────────────────┼───────────────────────────┐
              │                           │                           │
              ▼                           ▼                           ▼
┌─────────────────────────┐  ┌─────────────────────────┐  ┌─────────────────────────┐
│    DirtyFileManager     │  │    DirtyDirManager      │  │     INodeManager        │
│  ─────────────────────  │  │  ─────────────────────  │  │  ─────────────────────  │
│  dirty_metadata: RwLock │  │  dirty_dirs: RwLock     │  │  inodes: RwLock         │
│  pool: Arc<MemoryPool>  │  │  original_dirs: RwLock  │  │  path_index: RwLock     │
│  cache: Arc<WriteCache> │  │  inodes: Arc<INodeMgr>  │  │  next_id: AtomicU64     │
│  read_store: Arc<Store> │  └───────────┬─────────────┘  └───────────┬─────────────┘
│  read_cache: Option<RC> │              │                            │
│  inodes: Arc<INodeMgr>  │              │                            │
└───────────┬─────────────┘              │                            │
            │                            │                            │
            │                            └────────────────────────────┘
            │                                         │
            ▼                                         ▼
┌─────────────────────────┐              ┌─────────────────────────┐
│      MemoryPool         │              │   Arc<dyn INode>        │
│  ─────────────────────  │              │  (INodeFile/Dir/Symlink)│
│  blocks: HashMap        │              └─────────────────────────┘
│  key_index: HashMap     │
│  pending_fetches: HMap  │
│  lru_order: VecDeque    │
│  current_size: u64      │
└───────────┬─────────────┘
            │
            ▼
┌─────────────────────────┐
│      PoolBlock          │
│  ─────────────────────  │
│  key: BlockKey          │
│  data: Arc<Vec<u8>>     │
│  ref_count: AtomicUsize │
│  needs_flush: AtomicBool│
└─────────────────────────┘
```

---

## Block Diagram 3: Read from Manifest (Read-Only Path)

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                         DATA FLOW: READ FILE FROM MANIFEST                               │
└─────────────────────────────────────────────────────────────────────────────────────────┘

    FUSE read()                                                              S3 CAS
        │                                                                       │
        ▼                                                                       │
┌───────────────┐                                                               │
│ 1. DeadlineVfs│                                                               │
│    .read()    │                                                               │
└───────┬───────┘                                                               │
        │                                                                       │
        ▼                                                                       │
┌───────────────┐     ┌───────────────┐                                         │
│ 2. INodeMgr   │────▶│ 3. Get        │                                         │
│ .get_file_    │     │ FileContent   │                                         │
│  content()    │     │ (hash/chunks) │                                         │
└───────────────┘     └───────┬───────┘                                         │
                              │                                                 │
                              ▼                                                 │
                      ┌───────────────┐                                         │
                      │ 4. BlockKey   │                                         │
                      │ from_hash()   │                                         │
                      └───────┬───────┘                                         │
                              │                                                 │
                              ▼                                                 │
                      ┌───────────────┐     ┌───────────────┐                   │
                      │ 5. MemoryPool │────▶│ 6. Cache HIT? │                   │
                      │    .acquire() │     │ Return handle │                   │
                      └───────┬───────┘     └───────────────┘                   │
                              │ MISS                                            │
                              ▼                                                 │
                      ┌───────────────┐     ┌───────────────┐                   │
                      │ 7. ReadCache  │────▶│ 8. Disk HIT?  │                   │
                      │    .get()     │     │ Return data   │                   │
                      └───────┬───────┘     └───────────────┘                   │
                              │ MISS                                            │
                              ▼                                                 │
                      ┌───────────────┐     ┌───────────────┐     ┌─────────────┤
                      │ 9. FileStore  │────▶│10. S3 GET     │◀────│ S3 Bucket   │
                      │   .retrieve() │     │ Data/hash.xxh │     │ (CAS)       │
                      └───────┬───────┘     └───────┬───────┘     └─────────────┘
                              │                     │
                              ▼                     │
                      ┌───────────────┐             │
                      │11. ReadCache  │◀────────────┘
                      │   .put()      │ (write-through)
                      └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │12. MemoryPool │
                      │   .insert()   │
                      └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │13. BlockHandle│
                      │   .data()     │
                      └───────────────┘
```

### Numbered Data Transactions (Read from Manifest):

1. FUSE layer receives `read(ino, offset, size)` syscall
2. `INodeManager.get_file_content(ino)` retrieves `FileContent` enum
3. Extract hash (SingleHash) or chunk hashes (Chunked)
4. Create `BlockKey::from_hash(hash, chunk_index)`
5. `MemoryPool.acquire(key, fetch_fn)` checks in-memory cache
6. **Cache HIT**: Return `BlockHandle` immediately (lock-free read)
7. **Cache MISS**: Check `ReadCache.get(hash)` on disk
8. **Disk HIT**: Return cached bytes, insert into MemoryPool
9. **Disk MISS**: Call `FileStore.retrieve(hash, algorithm)`
10. S3 GET request to `s3://bucket/Data/{hash}.xxh128`
11. Write-through to `ReadCache.put(hash, data)`
12. Insert into `MemoryPool` with LRU tracking
13. Return `BlockHandle` with direct `Arc<Vec<u8>>` access

---

## Block Diagram 4: Write New File

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                         DATA FLOW: CREATE & WRITE NEW FILE                               │
└─────────────────────────────────────────────────────────────────────────────────────────┘

    FUSE create() + write()
        │
        ▼
┌───────────────┐
│ 1. WritableVfs│
│   .create()   │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 2. Allocate   │────▶│ 3. DirtyFile  │
│ new inode ID  │     │ Manager       │
│ (0x8000_0000+)│     │ .create_file()│
└───────────────┘     └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │ 4. DirtyFile  │
                      │ Metadata::    │
                      │ new_file()    │
                      │ state=New     │
                      └───────┬───────┘
                              │
        ┌─────────────────────┘
        │
        ▼
┌───────────────┐
│ 5. WritableVfs│
│   .write()    │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 6. DirtyFile  │────▶│ 7. MemoryPool │
│ Manager       │     │ .insert_dirty │
│ .write()      │     │ (ino, chunk,  │
└───────┬───────┘     │  data)        │
        │             └───────┬───────┘
        │                     │
        ▼                     ▼
┌───────────────┐     ┌───────────────┐
│ 8. Update     │     │ 9. PoolBlock  │
│ DirtyFile     │     │ needs_flush=  │
│ Metadata      │     │ true          │
│ (size, mtime) │     └───────┬───────┘
└───────┬───────┘             │
        │                     │
        ▼                     ▼
┌───────────────┐     ┌───────────────┐
│10. flush_to_  │────▶│11. WriteCache │
│    disk()     │     │ (Materialized │
└───────────────┘     │  Cache)       │
                      │ .write_file() │
                      └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │12. Disk File  │
                      │ cache_dir/    │
                      │ path/file.txt │
                      └───────────────┘
```

### Numbered Data Transactions (Write New File):

1. FUSE `create(parent, name, mode)` syscall received
2. Allocate new inode ID starting from `0x8000_0000` (avoids manifest conflicts)
3. `DirtyFileManager.create_file(ino, path, parent_ino)`
4. Create `DirtyFileMetadata` with `state=DirtyState::New`, `size=0`
5. FUSE `write(ino, offset, data)` syscall received
6. `DirtyFileManager.write(ino, offset, data)` called
7. `MemoryPool.insert_dirty(ino, chunk_index, data)` stores in memory
8. Update `DirtyFileMetadata`: increment size, update mtime, mark chunk dirty
9. `PoolBlock.needs_flush = true` (dirty block)
10. `flush_to_disk(ino)` assembles chunks from pool
11. `WriteCache.write_file(rel_path, assembled_data)` persists to disk
12. File written atomically (temp file + rename) to `cache_dir/path/file.txt`

---

## Block Diagram 5: Create New Directory

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                         DATA FLOW: CREATE NEW DIRECTORY                                  │
└─────────────────────────────────────────────────────────────────────────────────────────┘

    FUSE mkdir()
        │
        ▼
┌───────────────┐
│ 1. WritableVfs│
│   .mkdir()    │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 2. DirtyDir   │────▶│ 3. INodeMgr   │
│ Manager       │     │ .get(parent)  │
│ .create_dir() │     │ verify parent │
└───────┬───────┘     └───────────────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 4. Build path │────▶│ 5. Check      │
│ parent_path + │     │ already exists│
│ "/" + name    │     │ in INodeMgr   │
└───────┬───────┘     └───────────────┘
        │
        ▼
┌───────────────┐
│ 6. INodeMgr   │
│ .add_directory│
│ (new_path)    │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 7. Check if   │────▶│ 8. If not     │
│ original dir  │     │ original:     │
│ (from manifest│     │ Track as New  │
└───────────────┘     └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │ 9. DirtyDir   │
                      │ state=New     │
                      │ stored in     │
                      │ dirty_dirs map│
                      └───────────────┘
```

### Numbered Data Transactions (Create Directory):

1. FUSE `mkdir(parent, name, mode)` syscall received
2. `DirtyDirManager.create_dir(parent_id, name)` called
3. Verify parent exists via `INodeManager.get(parent_id)`
4. Build full path: `parent.path() + "/" + name`
5. Check path doesn't already exist in `INodeManager`
6. `INodeManager.add_directory(new_path)` creates `INodeDir`
7. Check if path was in `original_dirs` (from manifest)
8. If NOT original: create `DirtyDir` with `state=DirtyDirState::New`
9. Store in `dirty_dirs: HashMap<INodeId, DirtyDir>`

---

## Block Diagram 6: Read New/Dirty File

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                         DATA FLOW: READ NEW/DIRTY FILE                                   │
└─────────────────────────────────────────────────────────────────────────────────────────┘

    FUSE read()
        │
        ▼
┌───────────────┐
│ 1. WritableVfs│
│   .read()     │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 2. DirtyFile  │────▶│ 3. is_dirty?  │
│ Manager       │     │ YES: continue │
│ .is_dirty()   │     │ NO: fallback  │
└───────┬───────┘     └───────────────┘
        │ YES
        ▼
┌───────────────┐
│ 4. DirtyFile  │
│ Manager       │
│ .read(ino,    │
│  offset, size)│
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 5. Get meta:  │────▶│ 6. Calculate  │
│ file_size,    │     │ chunk range   │
│ chunk_size,   │     │ start_chunk   │
│ chunk_count   │     │ end_chunk     │
└───────────────┘     └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │ 7. For each   │
                      │ chunk in range│
                      └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐     ┌───────────────┐
                      │ 8. MemoryPool │────▶│ 9. Pool HIT?  │
                      │ .get_dirty()  │     │ Return handle │
                      └───────┬───────┘     └───────────────┘
                              │ MISS
                              ▼
                      ┌───────────────┐
                      │10. ensure_    │
                      │ chunk_in_pool │
                      │ (fetch from   │
                      │  S3 if COW)   │
                      └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │11. Slice data │
                      │ for offset/   │
                      │ size within   │
                      │ chunk         │
                      └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │12. Assemble   │
                      │ result Vec    │
                      └───────────────┘
```

### Numbered Data Transactions (Read Dirty File):

1. FUSE `read(ino, offset, size)` syscall received
2. Check `DirtyFileManager.is_dirty(ino)`
3. If dirty, use dirty path; otherwise fallback to read-only path
4. `DirtyFileManager.read(ino, offset, size)` called
5. Get metadata: `file_size`, `chunk_size` (256MB), `chunk_count`
6. Calculate `start_chunk = offset / chunk_size`, `end_chunk`
7. Iterate over chunk range
8. `MemoryPool.get_dirty(ino, chunk_index)` checks dirty block cache
9. **Pool HIT**: Return `BlockHandle` directly
10. **Pool MISS**: `ensure_chunk_in_pool()` - for COW files, fetch original from S3
11. Slice chunk data for the requested offset/size within that chunk
12. Assemble final result `Vec<u8>` from all chunks

---

## Block Diagram 7: Edit Manifest File (Copy-on-Write)

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                         DATA FLOW: EDIT MANIFEST FILE (COW)                              │
└─────────────────────────────────────────────────────────────────────────────────────────┘

    FUSE write() on manifest file
        │
        ▼
┌───────────────┐
│ 1. WritableVfs│
│   .write()    │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 2. DirtyFile  │────▶│ 3. is_dirty?  │
│ Manager       │     │ NO: do COW    │
│ .write()      │     │ YES: skip COW │
└───────┬───────┘     └───────────────┘
        │ NO (first write)
        ▼
┌───────────────┐
│ 4. cow_copy() │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│ 5. INodeMgr   │────▶│ 6. Get        │
│ .get(ino)     │     │ FileContent   │
│ .get_file_    │     │ (original     │
│  content()    │     │  hashes)      │
└───────────────┘     └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │ 7. DirtyFile  │
                      │ Metadata::    │
                      │ from_cow()    │
                      │ state=Modified│
                      │ original_hash │
                      └───────┬───────┘
                              │
                              ▼
                      ┌───────────────┐
                      │ 8. Invalidate │
                      │ MemoryPool    │
                      │ .invalidate_  │
                      │  hash()       │
                      └───────┬───────┘
                              │
        ┌─────────────────────┘
        │
        ▼
┌───────────────┐
│ 9. ensure_    │
│ chunk_in_pool │
│ (lazy fetch)  │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│10. Fetch from │────▶│11. S3 GET     │
│ S3 if needed  │     │ original chunk│
│ (first access)│     │ Data/hash.xxh │
└───────┬───────┘     └───────────────┘
        │
        ▼
┌───────────────┐
│12. Modify     │
│ chunk data    │
│ in memory     │
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│13. MemoryPool │────▶│14. PoolBlock  │
│ .update_dirty │     │ needs_flush=  │
│ (ino, chunk,  │     │ true          │
│  new_data)    │     └───────────────┘
└───────┬───────┘
        │
        ▼
┌───────────────┐     ┌───────────────┐
│15. Update     │────▶│16. flush_to_  │
│ DirtyFile     │     │ disk()        │
│ Metadata      │     │ WriteCache    │
│ mark_chunk_   │     │ .write_file() │
│ dirty()       │     └───────────────┘
└───────────────┘
```

### Numbered Data Transactions (Edit Manifest File - COW):

1. FUSE `write(ino, offset, data)` on existing manifest file
2. `DirtyFileManager.write()` checks if already dirty
3. **First write**: Need to perform Copy-on-Write
4. `cow_copy(ino)` initiates COW process
5. Get original file metadata from `INodeManager`
6. Extract `FileContent` (SingleHash or Chunked hashes)
7. Create `DirtyFileMetadata::from_cow()` with `state=Modified`, store original hashes
8. `MemoryPool.invalidate_hash()` removes stale read-only blocks for this hash
9. `ensure_chunk_in_pool(ino, chunk_index)` - lazy loading
10. If chunk not in pool, fetch from S3 using original hash
11. S3 GET `Data/{original_hash}.xxh128`
12. Modify chunk data in memory (apply write at offset)
13. `MemoryPool.update_dirty(ino, chunk, modified_data)`
14. Mark `PoolBlock.needs_flush = true`
15. Update `DirtyFileMetadata`: `mark_chunk_dirty()`, update size/mtime
16. `flush_to_disk()` persists to `WriteCache` (MaterializedCache)

---

## Summary: Key Data Structures

| Structure | Purpose | Key Fields |
|-----------|---------|------------|
| `INodeManager` | Manages all inodes (files/dirs/symlinks) | `inodes`, `path_index`, `next_id` |
| `INodeFile` | File metadata | `content: FileContent`, `size`, `mtime`, `executable` |
| `INodeDir` | Directory metadata | `children: HashMap<String, INodeId>` |
| `FileContent` | Hash reference | `SingleHash(String)` or `Chunked(Vec<String>)` |
| `MemoryPool` | LRU block cache | `blocks`, `key_index`, `lru_order`, `pending_fetches` |
| `PoolBlock` | Single cached chunk | `data: Arc<Vec<u8>>`, `ref_count`, `needs_flush` |
| `DirtyFileManager` | COW file tracking | `dirty_metadata`, `pool`, `cache`, `read_store` |
| `DirtyFileMetadata` | Modified file state | `state`, `original_hashes`, `dirty_chunks`, `size` |
| `DirtyDirManager` | Created/deleted dirs | `dirty_dirs`, `original_dirs` |
| `ReadCache` | Disk cache for S3 content | `cache_dir`, `current_size` |
| `MaterializedCache` | Disk cache for dirty files | `cache_dir`, `deleted_dir`, `meta_dir` |


---

## Block Diagram 8: Read-Only File Flow (Open + Read with Caching)

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                    DATA FLOW: READ-ONLY FILE (OPEN + READ + CACHING)                     │
└─────────────────────────────────────────────────────────────────────────────────────────┘

                                    ┌─────────────────┐
                                    │   Application   │
                                    │  (e.g., cat)    │
                                    └────────┬────────┘
                                             │
                    ┌────────────────────────┼────────────────────────┐
                    │                        │                        │
                    ▼                        ▼                        ▼
            ┌───────────────┐        ┌───────────────┐        ┌───────────────┐
            │  open(path)   │        │ read(fd,buf,n)│        │   close(fd)   │
            └───────┬───────┘        └───────┬───────┘        └───────┬───────┘
                    │                        │                        │
════════════════════╪════════════════════════╪════════════════════════╪═══════════════════
                    │              FUSE KERNEL INTERFACE              │
════════════════════╪════════════════════════╪════════════════════════╪═══════════════════
                    │                        │                        │
                    ▼                        ▼                        ▼
            ┌───────────────┐        ┌───────────────┐        ┌───────────────┐
            │ DeadlineVfs   │        │ DeadlineVfs   │        │ DeadlineVfs   │
            │ ::open()      │        │ ::read()      │        │ ::release()   │
            └───────┬───────┘        └───────┬───────┘        └───────┬───────┘
                    │                        │                        │
                    ▼                        │                        ▼
┌───────────────────────────────────┐        │                ┌───────────────┐
│           OPEN FLOW               │        │                │ Remove handle │
│                                   │        │                │ from handles  │
│  1. INodeManager.get(ino)         │        │                │ HashMap       │
│     └─▶ Verify file exists        │        │                └───────────────┘
│                                   │        │
│  2. Check inode_type == File      │        │
│     └─▶ Return EISDIR if not      │        │
│                                   │        │
│  3. Check flags (reject O_WRONLY) │        │
│     └─▶ Return EROFS if write     │        │
│                                   │        │
│  4. INodeManager.get_file_content │        │
│     └─▶ Get FileContent enum      │        │
│                                   │        │
│  5. Allocate file handle (fh)     │        │
│     └─▶ next_handle.fetch_add(1)  │        │
│                                   │        │
│  6. Store in handles HashMap:     │        │
│     handles[fh] = OpenHandle {    │        │
│       inode, path, content, size  │        │
│     }                             │        │
│                                   │        │
│  7. Return fh to kernel           │        │
└───────────────────────────────────┘        │
                                             │
                                             ▼
                    ┌─────────────────────────────────────────────────────────┐
                    │                      READ FLOW                          │
                    │                                                         │
                    │  1. Look up OpenHandle by fh                            │
                    │     └─▶ Get content (FileContent) and file_size         │
                    │                                                         │
                    │  2. Dispatch based on FileContent variant:              │
                    │     ├─▶ SingleHash(hash) → read_single_hash()           │
                    │     └─▶ Chunked(hashes) → read_chunked()                │
                    └─────────────────────────────────────────────────────────┘
                                             │
                    ┌────────────────────────┴────────────────────────┐
                    │                                                 │
                    ▼                                                 ▼
    ┌───────────────────────────────────┐         ┌───────────────────────────────────┐
    │     SINGLE HASH READ              │         │     CHUNKED READ                  │
    │     (files < 256MB)               │         │     (files > 256MB)               │
    │                                   │         │                                   │
    │  BlockKey::from_hash(hash, 0)     │         │  For each chunk in range:         │
    │           │                       │         │    start = offset / CHUNK_SIZE    │
    │           ▼                       │         │    end = (offset+size) / CHUNK_SZ │
    │  MemoryPool.acquire(key, fetch)   │         │                                   │
    │           │                       │         │  BlockKey::from_hash(hash[i], i)  │
    │           ▼                       │         │           │                       │
    │  [See CACHING FLOW below]         │         │           ▼                       │
    │           │                       │         │  MemoryPool.acquire(key, fetch)   │
    │           ▼                       │         │           │                       │
    │  Slice data[offset..offset+size]  │         │  [See CACHING FLOW below]         │
    │           │                       │         │           │                       │
    │           ▼                       │         │           ▼                       │
    │  Return bytes to kernel           │         │  Assemble chunks into result      │
    └───────────────────────────────────┘         └───────────────────────────────────┘
                    │                                                 │
                    └────────────────────────┬────────────────────────┘
                                             │
                                             ▼
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                              CACHING FLOW (MemoryPool.acquire)                           │
└─────────────────────────────────────────────────────────────────────────────────────────┘

    MemoryPool.acquire(key, fetch_fn)
                │
                ▼
    ┌───────────────────────┐
    │ 1. Check blocks cache │
    │    key_index.get(key) │
    └───────────┬───────────┘
                │
        ┌───────┴───────┐
        │               │
        ▼               ▼
    ┌───────┐       ┌───────┐
    │  HIT  │       │ MISS  │
    └───┬───┘       └───┬───┘
        │               │
        ▼               ▼
┌───────────────┐   ┌───────────────────────────────────────────────────────────────┐
│ 2. Touch LRU  │   │ 2. Check pending_fetches (thundering herd prevention)         │
│    (move to   │   │    └─▶ If another thread fetching same key, wait for it       │
│     back)     │   │                                                               │
│               │   │ 3. Check ReadCache (disk cache)                               │
│ 3. Increment  │   │    cache.get(hash)                                            │
│    ref_count  │   │         │                                                     │
│               │   │    ┌────┴────┐                                                │
│ 4. Return     │   │    │         │                                                │
│    BlockHandle│   │    ▼         ▼                                                │
│    (lock-free │   │ ┌──────┐  ┌──────┐                                            │
│     access)   │   │ │ HIT  │  │ MISS │                                            │
└───────────────┘   │ └──┬───┘  └──┬───┘                                            │
                    │    │         │                                                │
                    │    │         ▼                                                │
                    │    │    ┌─────────────────────────────────────────────────┐   │
                    │    │    │ 4. Fetch from S3 via FileStore.retrieve()       │   │
                    │    │    │                                                 │   │
                    │    │    │    StorageClientAdapter                         │   │
                    │    │    │         │                                       │   │
                    │    │    │         ▼                                       │   │
                    │    │    │    S3 GET s3://bucket/Data/{hash}.xxh128        │   │
                    │    │    │         │                                       │   │
                    │    │    │         ▼                                       │   │
                    │    │    │    Return Vec<u8> (up to 256MB)                 │   │
                    │    │    └─────────────────────────────────────────────────┘   │
                    │    │         │                                                │
                    │    │         ▼                                                │
                    │    │    ┌─────────────────────────────────────────────────┐   │
                    │    │    │ 5. Write-through to ReadCache                   │   │
                    │    │    │    cache.put(hash, data)                        │   │
                    │    │    │    └─▶ Atomic write: temp file + rename         │   │
                    │    │    └─────────────────────────────────────────────────┘   │
                    │    │         │                                                │
                    │    └─────────┤                                                │
                    │              ▼                                                │
                    │    ┌─────────────────────────────────────────────────────┐   │
                    │    │ 6. Evict if needed (LRU)                            │   │
                    │    │    while current_size + new_size > max_size:        │   │
                    │    │      evict oldest block with ref_count == 0         │   │
                    │    └─────────────────────────────────────────────────────┘   │
                    │              │                                                │
                    │              ▼                                                │
                    │    ┌─────────────────────────────────────────────────────┐   │
                    │    │ 7. Insert into MemoryPool                           │   │
                    │    │    blocks[id] = PoolBlock { key, data, ref_count=1 }│   │
                    │    │    key_index[key] = id                              │   │
                    │    │    lru_order.push_back(id)                          │   │
                    │    └─────────────────────────────────────────────────────┘   │
                    │              │                                                │
                    │              ▼                                                │
                    │    ┌─────────────────────────────────────────────────────┐   │
                    │    │ 8. Return BlockHandle                               │   │
                    │    │    - data: Arc<Vec<u8>> (lock-free read access)     │   │
                    │    │    - Drops ref_count on Drop                        │   │
                    │    └─────────────────────────────────────────────────────┘   │
                    └───────────────────────────────────────────────────────────────┘


┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                              CACHE HIERARCHY SUMMARY                                     │
└─────────────────────────────────────────────────────────────────────────────────────────┘

    ┌─────────────────────────────────────────────────────────────────────────────────┐
    │  L1: MemoryPool (in-process)                                                    │
    │  ─────────────────────────────                                                  │
    │  • Capacity: Configurable (default 8GB)                                         │
    │  • Latency: ~nanoseconds (lock-free Arc<Vec<u8>> access)                        │
    │  • Eviction: LRU when full                                                      │
    │  • Key: BlockKey { hash, chunk_index }                                          │
    │  • Deduplication: Same hash = same block (content-addressed)                    │
    └─────────────────────────────────────────────────────────────────────────────────┘
                                         │
                                         │ MISS
                                         ▼
    ┌─────────────────────────────────────────────────────────────────────────────────┐
    │  L2: ReadCache (disk)                                                           │
    │  ────────────────────                                                           │
    │  • Location: /tmp/vfs-cache/cas/ (configurable)                                 │
    │  • Latency: ~milliseconds (disk I/O)                                            │
    │  • Persistence: Survives process restart                                        │
    │  • Key: hash (flat files, hash as filename)                                     │
    │  • Write-through: Populated on S3 fetch                                         │
    └─────────────────────────────────────────────────────────────────────────────────┘
                                         │
                                         │ MISS
                                         ▼
    ┌─────────────────────────────────────────────────────────────────────────────────┐
    │  L3: S3 CAS (remote)                                                            │
    │  ───────────────────                                                            │
    │  • Location: s3://{bucket}/{root}/Data/{hash}.{algorithm}                       │
    │  • Latency: ~100ms+ (network)                                                   │
    │  • Source of truth for manifest content                                         │
    │  • Content-addressable: hash guarantees integrity                               │
    └─────────────────────────────────────────────────────────────────────────────────┘
```

### Numbered Data Transactions (Read-Only Open + Read):

**OPEN:**
1. FUSE `open(path, flags)` syscall received
2. `INodeManager.get(ino)` - verify inode exists
3. Check `inode.inode_type() == File` - reject directories with `EISDIR`
4. Check flags - reject `O_WRONLY`/`O_RDWR` with `EROFS` (read-only filesystem)
5. `INodeManager.get_file_content(ino)` - get `FileContent` enum (hash references)
6. Allocate file handle `fh` via atomic counter
7. Store `OpenHandle { inode, path, content, size }` in `handles` HashMap
8. Return `fh` to kernel

**READ:**
1. Look up `OpenHandle` by `fh` from `handles` HashMap
2. Extract `FileContent` and `file_size` from handle
3. For `SingleHash`: create single `BlockKey`, call `MemoryPool.acquire()`
4. For `Chunked`: calculate chunk range, iterate calling `MemoryPool.acquire()` per chunk
5. **L1 Cache (MemoryPool)**: Check `key_index` for existing block
   - HIT: Touch LRU, increment ref_count, return `BlockHandle`
   - MISS: Continue to L2
6. **L2 Cache (ReadCache)**: Check disk cache `cache.get(hash)`
   - HIT: Load from disk, insert into MemoryPool, return
   - MISS: Continue to L3
7. **L3 (S3)**: `FileStore.retrieve(hash, algorithm)` - fetch from S3
8. Write-through to ReadCache (atomic: temp file + rename)
9. Evict LRU blocks if MemoryPool full
10. Insert new block into MemoryPool
11. Return `BlockHandle` with `Arc<Vec<u8>>` for lock-free data access
12. Slice data for requested `offset..offset+size`
13. Return bytes to kernel → application

**RELEASE:**
1. Remove `OpenHandle` from `handles` HashMap
2. Note: MemoryPool blocks are NOT released on close (LRU eviction handles cleanup)
