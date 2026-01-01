# VFS Integration Tests

End-to-end integration tests for the VFS that require actual FUSE mounting and S3 access.

## Prerequisites

1. Valid AWS credentials with access to the S3 bucket
2. FUSE installed (`libfuse-dev` on Linux, `macfuse` on macOS)
3. The manifest file `job_bundles_manifest_v2.json` in the workspace root

## Running the Tests

```bash
# Source credentials first
source creds.sh

# Run the full test suite
./crates/vfs/tests/integration/vfs_e2e_test.sh
```

## Test Coverage

### Group 1: Read Existing Files
- Read small file from subdirectory
- Read medium file (~3MB)
- Read file from deep subdirectory

### Group 2: Write New Small Files
- Create file in root directory
- Create file in existing subdirectory
- Create new directory and write file
- Create nested directories and write file

### Group 3: Modify Existing Files (COW)
- Append to existing file (triggers COW)
- Overwrite portion of file

### Group 4: Large File Operations
- Write 1MB file
- Write 10MB file (tests chunking)
- Checksum verification after write

### Group 5: Directory Operations
- List existing directory
- List new directory with new files
- Verify no duplicate directory entries (regression)

### Group 6: Delete Operations
- Delete new file
- Delete empty directory

### Group 7: Edge Cases
- Empty file creation
- File with spaces in name
- Truncate file (shrink)
- Extend file via truncate
- Multiple sequential writes

### Group 8: Concurrent Access
- 5 parallel reads of same file
- 5 parallel writes to different files

### Group 9: Cache Verification
- Verify dirty files written to cache directory

## Notes

- Tests run against real S3 data, so they require network access
- The VFS is mounted in writable mode with COW support
- Cache directory is created in `/tmp/vfs-cache-test-<pid>`
- Log file is written to `/tmp/vfs-test-<pid>.log`
- Cleanup happens automatically on exit (including Ctrl+C)

## Troubleshooting

If tests fail:
1. Check the log file for VFS errors
2. Ensure credentials are valid: `aws sts get-caller-identity`
3. Verify FUSE is working: `fusermount3 --version`
4. Check if mountpoint is already in use: `mountpoint ./vfs`
