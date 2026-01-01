#!/bin/bash
# VFS End-to-End Integration Test
#
# Usage:
#   source creds.sh
#   ./vfs_e2e_test.sh              # Run all tests
#   ./vfs_e2e_test.sh list         # List available tests
#   ./vfs_e2e_test.sh <test_name>  # Run specific test
#   ./vfs_e2e_test.sh group1       # Run test group 1
#
# Examples:
#   ./vfs_e2e_test.sh test_read_small_file
#   ./vfs_e2e_test.sh test_write_large_chunk

# Configuration
MANIFEST="job_bundles_manifest_v2.json"
MOUNTPOINT="./vfs"
CACHE_DIR="/tmp/vfs-cache-test-$$"
LOG_FILE="/tmp/vfs-test-$$.log"
VFS_PID=""
TEST_PASSED=0
TEST_FAILED=0

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# ============================================================================
# Helper Functions
# ============================================================================

cleanup() {
    echo -e "\n${YELLOW}Cleaning up...${NC}"
    if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        fusermount3 -u "$MOUNTPOINT" 2>/dev/null || fusermount -u "$MOUNTPOINT" 2>/dev/null || true
        sleep 1
    fi
    if [ -n "$VFS_PID" ] && kill -0 "$VFS_PID" 2>/dev/null; then
        kill "$VFS_PID" 2>/dev/null || true
        wait "$VFS_PID" 2>/dev/null || true
    fi
    rm -rf "$CACHE_DIR"
    echo "Log: $LOG_FILE"
    echo -e "${GREEN}Passed: $TEST_PASSED${NC} | ${RED}Failed: $TEST_FAILED${NC}"
    [ $TEST_FAILED -gt 0 ] && exit 1 || exit 0
}

trap cleanup EXIT

pass() {
    echo -e "${GREEN}✓ PASS${NC}: $1"
    TEST_PASSED=$((TEST_PASSED + 1))
}

fail() {
    echo -e "${RED}✗ FAIL${NC}: $1"
    TEST_FAILED=$((TEST_FAILED + 1))
}

start_vfs() {
    if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
        echo "VFS already mounted"
        return 0
    fi
    echo -e "${YELLOW}Starting VFS...${NC}"
    mkdir -p "$MOUNTPOINT" "$CACHE_DIR"
    cargo run -p rusty-attachments-vfs --features fuse --example mount_vfs -- \
        "$MANIFEST" "$MOUNTPOINT" --writable --cache-dir "$CACHE_DIR" > "$LOG_FILE" 2>&1 &
    VFS_PID=$!
    for i in {1..30}; do
        if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
            echo "VFS mounted (PID: $VFS_PID)"
            sleep 1
            return 0
        fi
        sleep 0.5
    done
    echo "Failed to mount VFS"; cat "$LOG_FILE"; exit 1
}

# ============================================================================
# Test Cases - Group 1: Read Operations
# ============================================================================

test_read_small_file() {
    local f="$MOUNTPOINT/aftereffects/aftereffects/24.6/afterfx_titleflip_sample/parameter_values.json"
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$size" = "263" ] && pass "Read small file (263 bytes)" || fail "Read small file - size: $size"
}

test_read_medium_file() {
    local f="$MOUNTPOINT/aftereffects/aftereffects/24.6/afterfx_titleflip_sample/scene/titleFlip.aep"
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$size" = "3162974" ] && pass "Read medium file (~3MB)" || fail "Read medium file - size: $size"
}

test_read_deep_subdir() {
    local f="$MOUNTPOINT/cinema4d/physical/2025/animated_text_no_cache/scene/fonts/arial.ttf"
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$size" = "1047208" ] && pass "Read deep subdir file (~1MB)" || fail "Read deep subdir - size: $size"
}

# ============================================================================
# Test Cases - Group 2: Write Small Files
# ============================================================================

test_write_small_root() {
    local f="$MOUNTPOINT/test_small_root.txt"
    echo -n "Hello, VFS!" > "$f"
    local content=$(cat "$f")
    [ "$content" = "Hello, VFS!" ] && pass "Write small file to root" || fail "Write small root - got: '$content'"
}

test_write_small_subdir() {
    local f="$MOUNTPOINT/aftereffects/test_in_subdir.txt"
    echo -n "Test content" > "$f"
    local content=$(cat "$f")
    [ "$content" = "Test content" ] && pass "Write to existing subdir" || fail "Write subdir - got: '$content'"
}

test_write_new_dir() {
    mkdir -p "$MOUNTPOINT/new_test_dir"
    echo -n "New dir file" > "$MOUNTPOINT/new_test_dir/file.txt"
    local content=$(cat "$MOUNTPOINT/new_test_dir/file.txt")
    [ "$content" = "New dir file" ] && pass "Create new dir and write" || fail "New dir write failed"
}

test_write_nested_dirs() {
    mkdir -p "$MOUNTPOINT/level1/level2/level3"
    echo -n "Deep" > "$MOUNTPOINT/level1/level2/level3/deep.txt"
    local content=$(cat "$MOUNTPOINT/level1/level2/level3/deep.txt")
    [ "$content" = "Deep" ] && pass "Create nested dirs and write" || fail "Nested dirs failed"
}

# ============================================================================
# Test Cases - Group 3: Modify Existing (COW)
# ============================================================================

test_modify_existing_cow() {
    local f="$MOUNTPOINT/aftereffects/aftereffects/24.6/afterfx_titleflip_sample/parameter_values.json"
    local orig=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    echo "// Modified" >> "$f"
    local new=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$new" -gt "$orig" ] && pass "COW modify - $orig -> $new bytes" || fail "COW modify failed"
}

test_overwrite_portion() {
    local f="$MOUNTPOINT/test_overwrite.txt"
    echo -n "AAAAAAAAAA" > "$f"
    echo -n "BBB" | dd of="$f" bs=1 seek=3 conv=notrunc 2>/dev/null
    local content=$(cat "$f")
    [ "$content" = "AAABBBAAAA" ] && pass "Overwrite portion" || fail "Overwrite - got: '$content'"
}

# ============================================================================
# Test Cases - Group 4: Large Files (Chunking)
# ============================================================================

test_write_1mb() {
    local f="$MOUNTPOINT/test_1mb.bin"
    dd if=/dev/urandom of="$f" bs=1M count=1 2>/dev/null
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$size" = "1048576" ] && pass "Write 1MB file" || fail "1MB write - size: $size"
}

test_write_10mb() {
    local f="$MOUNTPOINT/test_10mb.bin"
    dd if=/dev/urandom of="$f" bs=1M count=10 2>/dev/null
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$size" = "10485760" ] && pass "Write 10MB file" || fail "10MB write - size: $size"
}

test_write_100mb() {
    local f="$MOUNTPOINT/test_100mb.bin"
    dd if=/dev/urandom of="$f" bs=1M count=100 2>/dev/null
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$size" = "104857600" ] && pass "Write 100MB file" || fail "100MB write - size: $size"
}

test_write_300mb_chunk_boundary() {
    # 300MB crosses the 256MB chunk boundary
    local f="$MOUNTPOINT/test_300mb_chunked.bin"
    echo -e "${CYAN}Writing 300MB file (crosses 256MB chunk boundary)...${NC}"
    dd if=/dev/urandom of="$f" bs=1M count=300 2>/dev/null
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    local expected=$((300 * 1024 * 1024))
    [ "$size" = "$expected" ] && pass "Write 300MB (multi-chunk)" || fail "300MB write - size: $size, expected: $expected"
}

test_write_512mb_two_chunks() {
    # 512MB = exactly 2 chunks of 256MB
    local f="$MOUNTPOINT/test_512mb_2chunks.bin"
    echo -e "${CYAN}Writing 512MB file (exactly 2 chunks)...${NC}"
    dd if=/dev/urandom of="$f" bs=1M count=512 2>/dev/null
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    local expected=$((512 * 1024 * 1024))
    [ "$size" = "$expected" ] && pass "Write 512MB (2 chunks)" || fail "512MB write - size: $size"
}

test_large_file_checksum() {
    local f="$MOUNTPOINT/test_checksum.bin"
    dd if=/dev/zero bs=1M count=10 2>/dev/null | tr '\0' 'X' > "$f"
    local w_sum=$(md5sum "$f" | cut -d' ' -f1)
    sync
    local r_sum=$(md5sum "$f" | cut -d' ' -f1)
    [ "$w_sum" = "$r_sum" ] && pass "Large file checksum match" || fail "Checksum mismatch: $w_sum vs $r_sum"
}

test_chunk_boundary_read_write() {
    # Write at chunk boundary (256MB - 100 bytes to 256MB + 100 bytes)
    local f="$MOUNTPOINT/test_boundary_rw.bin"
    local chunk_size=$((256 * 1024 * 1024))
    echo -e "${CYAN}Testing read/write across 256MB chunk boundary...${NC}"
    
    # Create file slightly larger than 256MB
    dd if=/dev/zero of="$f" bs=1M count=260 2>/dev/null
    
    # Write pattern at boundary
    local boundary_offset=$((chunk_size - 50))
    echo -n "BOUNDARY_CROSSING_DATA_HERE" | dd of="$f" bs=1 seek=$boundary_offset conv=notrunc 2>/dev/null
    
    # Read back
    local readback=$(dd if="$f" bs=1 skip=$boundary_offset count=27 2>/dev/null)
    [ "$readback" = "BOUNDARY_CROSSING_DATA_HERE" ] && pass "Chunk boundary read/write" || fail "Boundary r/w - got: '$readback'"
}

# ============================================================================
# Test Cases - Group 5: Directory Operations
# ============================================================================

test_list_directory() {
    local count=$(ls -1 "$MOUNTPOINT/aftereffects" 2>/dev/null | wc -l)
    [ "$count" -gt 0 ] && pass "List directory ($count entries)" || fail "List directory empty"
}

test_list_new_dir() {
    mkdir -p "$MOUNTPOINT/list_test"
    echo "1" > "$MOUNTPOINT/list_test/a.txt"
    echo "2" > "$MOUNTPOINT/list_test/b.txt"
    echo "3" > "$MOUNTPOINT/list_test/c.txt"
    local count=$(ls -1 "$MOUNTPOINT/list_test" | wc -l)
    [ "$count" = "3" ] && pass "List new dir (3 files)" || fail "List new dir - count: $count"
}

test_no_duplicate_dirs() {
    mkdir -p "$MOUNTPOINT/no_dup_test"
    local count=$(ls -1 "$MOUNTPOINT" | grep -c "^no_dup_test$" || echo "0")
    [ "$count" = "1" ] && pass "No duplicate dirs" || fail "Duplicate dirs - count: $count"
}

# ============================================================================
# Test Cases - Group 6: Delete Operations
# ============================================================================

test_delete_new_file() {
    local f="$MOUNTPOINT/to_delete.txt"
    echo "delete me" > "$f"
    rm "$f"
    [ ! -f "$f" ] && pass "Delete new file" || fail "Delete failed - file exists"
}

test_delete_empty_dir() {
    mkdir -p "$MOUNTPOINT/empty_to_delete"
    rmdir "$MOUNTPOINT/empty_to_delete"
    [ ! -d "$MOUNTPOINT/empty_to_delete" ] && pass "Delete empty dir" || fail "Delete dir failed"
}

# ============================================================================
# Test Cases - Group 7: Edge Cases
# ============================================================================

test_empty_file() {
    local f="$MOUNTPOINT/empty.txt"
    touch "$f"
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$size" = "0" ] && pass "Create empty file" || fail "Empty file size: $size"
}

test_file_with_spaces() {
    local f="$MOUNTPOINT/file with spaces.txt"
    echo "content" > "$f"
    [ -f "$f" ] && pass "File with spaces" || fail "File with spaces not created"
}

test_truncate_shrink() {
    local f="$MOUNTPOINT/truncate_shrink.txt"
    echo "1234567890" > "$f"
    truncate -s 5 "$f"
    local content=$(cat "$f")
    [ "$content" = "12345" ] && pass "Truncate shrink" || fail "Truncate shrink - got: '$content'"
}

test_truncate_extend() {
    local f="$MOUNTPOINT/truncate_extend.txt"
    echo -n "ABC" > "$f"
    truncate -s 10 "$f"
    local size=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null)
    [ "$size" = "10" ] && pass "Truncate extend" || fail "Truncate extend - size: $size"
}

test_multiple_writes() {
    local f="$MOUNTPOINT/multi_write.txt"
    echo -n "first" > "$f"
    echo -n " second" >> "$f"
    echo -n " third" >> "$f"
    local content=$(cat "$f")
    [ "$content" = "first second third" ] && pass "Multiple writes" || fail "Multi write - got: '$content'"
}

# ============================================================================
# Test Cases - Group 8: Concurrent Access
# ============================================================================

test_concurrent_reads() {
    local f="$MOUNTPOINT/aftereffects/aftereffects/24.6/afterfx_titleflip_sample/scene/titleFlip.aep"
    local pids=""
    for i in {1..5}; do
        cat "$f" > /dev/null &
        pids="$pids $!"
    done
    local failed=0
    for pid in $pids; do
        wait $pid || failed=$((failed + 1))
    done
    [ $failed -eq 0 ] && pass "Concurrent reads (5)" || fail "Concurrent reads - $failed failed"
}

test_concurrent_writes() {
    local pids=""
    for i in {1..5}; do
        echo "content $i" > "$MOUNTPOINT/concurrent_$i.txt" &
        pids="$pids $!"
    done
    local failed=0
    for pid in $pids; do
        wait $pid || failed=$((failed + 1))
    done
    [ $failed -eq 0 ] && pass "Concurrent writes (5)" || fail "Concurrent writes - $failed failed"
}

# ============================================================================
# Test Cases - Group 9: Cache Verification
# ============================================================================

test_cache_populated() {
    local count=$(find "$CACHE_DIR" -type f 2>/dev/null | wc -l)
    [ "$count" -gt 0 ] && pass "Cache populated ($count files)" || fail "Cache empty"
}

# ============================================================================
# Test Registry
# ============================================================================

ALL_TESTS=(
    # Group 1: Read
    test_read_small_file
    test_read_medium_file
    test_read_deep_subdir
    # Group 2: Write Small
    test_write_small_root
    test_write_small_subdir
    test_write_new_dir
    test_write_nested_dirs
    # Group 3: COW
    test_modify_existing_cow
    test_overwrite_portion
    # Group 4: Large Files
    test_write_1mb
    test_write_10mb
    test_write_100mb
    test_write_300mb_chunk_boundary
    test_write_512mb_two_chunks
    test_large_file_checksum
    test_chunk_boundary_read_write
    # Group 5: Directories
    test_list_directory
    test_list_new_dir
    test_no_duplicate_dirs
    # Group 6: Delete
    test_delete_new_file
    test_delete_empty_dir
    # Group 7: Edge Cases
    test_empty_file
    test_file_with_spaces
    test_truncate_shrink
    test_truncate_extend
    test_multiple_writes
    # Group 8: Concurrent
    test_concurrent_reads
    test_concurrent_writes
    # Group 9: Cache
    test_cache_populated
)

QUICK_TESTS=(
    test_read_small_file
    test_write_small_root
    test_write_new_dir
    test_modify_existing_cow
    test_write_1mb
    test_list_directory
    test_no_duplicate_dirs
    test_delete_new_file
    test_empty_file
    test_multiple_writes
)

CHUNK_TESTS=(
    test_write_100mb
    test_write_300mb_chunk_boundary
    test_write_512mb_two_chunks
    test_chunk_boundary_read_write
)

list_tests() {
    echo "Available tests:"
    for t in "${ALL_TESTS[@]}"; do
        echo "  $t"
    done
    echo ""
    echo "Groups: quick, chunk, all"
}

run_test() {
    local test_name="$1"
    echo -e "\n${CYAN}Running: $test_name${NC}"
    $test_name
}

# ============================================================================
# Main
# ============================================================================

case "${1:-all}" in
    list)
        list_tests
        exit 0
        ;;
    quick)
        start_vfs
        echo -e "\n${YELLOW}=== Quick Tests ===${NC}"
        for t in "${QUICK_TESTS[@]}"; do run_test "$t"; done
        ;;
    chunk)
        start_vfs
        echo -e "\n${YELLOW}=== Chunk Boundary Tests ===${NC}"
        for t in "${CHUNK_TESTS[@]}"; do run_test "$t"; done
        ;;
    all)
        start_vfs
        echo -e "\n${YELLOW}=== All Tests ===${NC}"
        for t in "${ALL_TESTS[@]}"; do run_test "$t"; done
        ;;
    test_*)
        start_vfs
        run_test "$1"
        ;;
    *)
        echo "Usage: $0 [list|quick|chunk|all|test_name]"
        exit 1
        ;;
esac

echo -e "\n${YELLOW}=== Summary ===${NC}"
