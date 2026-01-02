#!/bin/bash
# VFS Read/Write Performance Test Script
# Run this while perf is recording to capture read path behavior

VFS_DIR="./vfs"
TEMP_DIR="/tmp/vfs-test-data"

echo "=== VFS Read/Write Performance Test ==="
echo "Start time: $(date)"

# Create temp directory for test data
mkdir -p "$TEMP_DIR"

# --- Phase 1: Write large files to VFS ---
echo ""
echo "=== Phase 1: Writing large files to VFS ==="

# Create 3 large files (300MB each) with random data
echo "Creating 300MB test file 1..."
dd if=/dev/urandom of="$VFS_DIR/test_large_file_1.bin" bs=1M count=300 status=progress 2>&1

echo "Creating 300MB test file 2..."
dd if=/dev/urandom of="$VFS_DIR/test_large_file_2.bin" bs=1M count=300 status=progress 2>&1

echo "Creating 300MB test file 3..."
dd if=/dev/urandom of="$VFS_DIR/test_large_file_3.bin" bs=1M count=300 status=progress 2>&1

# --- Phase 2: Read manifest files (CAS read path) ---
echo ""
echo "=== Phase 2: Reading manifest files (CAS read path) ==="

# Read some of the larger manifest files
echo "Reading blender scene file (2.8MB)..."
cat "$VFS_DIR/blender/cycles/3.6/car_sample_five_frames_fast_job/scene/bmw27_cpu.blend" > /dev/null

echo "Reading cinema4d scene file (7MB)..."
cat "$VFS_DIR/cinema4d/arnold/2025/space_pirate_toy/scene/Space-Pirate-Toy.c4d" > /dev/null

echo "Reading texture file (3MB)..."
cat "$VFS_DIR/cinema4d/physical/2025/animated_text_no_cache/scene/tex/mxn_basic_cardboard_2k_normal.jpg" > /dev/null

echo "Reading aftereffects project (3.1MB)..."
cat "$VFS_DIR/aftereffects/aftereffects/24.6/afterfx_titleflip_sample/scene/titleFlip.aep" > /dev/null

# Read multiple times to exercise cache
echo "Re-reading files to test cache hits..."
for i in {1..3}; do
    cat "$VFS_DIR/blender/cycles/3.6/car_sample_five_frames_fast_job/scene/bmw27_cpu.blend" > /dev/null
    cat "$VFS_DIR/cinema4d/arnold/2025/space_pirate_toy/scene/Space-Pirate-Toy.c4d" > /dev/null
done

# --- Phase 3: Read back newly written files (dirty file read path) ---
echo ""
echo "=== Phase 3: Reading back newly written files (dirty read path) ==="

echo "Reading back test_large_file_1.bin..."
cat "$VFS_DIR/test_large_file_1.bin" > /dev/null

echo "Reading back test_large_file_2.bin..."
cat "$VFS_DIR/test_large_file_2.bin" > /dev/null

echo "Reading back test_large_file_3.bin..."
cat "$VFS_DIR/test_large_file_3.bin" > /dev/null

# Read multiple times
echo "Re-reading dirty files..."
for i in {1..3}; do
    cat "$VFS_DIR/test_large_file_1.bin" > /dev/null
    cat "$VFS_DIR/test_large_file_2.bin" > /dev/null
done

# --- Phase 4: Mixed read/write pattern ---
echo ""
echo "=== Phase 4: Mixed read/write pattern ==="

# Create a file, read it, append to it, read again
echo "Creating mixed_test.bin..."
dd if=/dev/urandom of="$VFS_DIR/mixed_test.bin" bs=1M count=100 status=progress 2>&1

echo "Reading mixed_test.bin..."
cat "$VFS_DIR/mixed_test.bin" > /dev/null

echo "Appending to mixed_test.bin..."
dd if=/dev/urandom bs=1M count=50 status=progress >> "$VFS_DIR/mixed_test.bin" 2>&1

echo "Reading mixed_test.bin after append..."
cat "$VFS_DIR/mixed_test.bin" > /dev/null

# --- Phase 5: Small file reads (metadata heavy) ---
echo ""
echo "=== Phase 5: Small file reads (metadata heavy) ==="

echo "Reading many small files..."
for f in "$VFS_DIR"/aftereffects/aftereffects/24.6/afterfx_titleflip_sample/scripts/*.py; do
    cat "$f" > /dev/null
done

for f in "$VFS_DIR"/cinema4d/physical/2024/simple_cube/*.yaml; do
    cat "$f" > /dev/null
done

# --- Cleanup ---
echo ""
echo "=== Cleanup ==="
echo "Removing test files from VFS..."
rm -f "$VFS_DIR/test_large_file_1.bin"
rm -f "$VFS_DIR/test_large_file_2.bin"
rm -f "$VFS_DIR/test_large_file_3.bin"
rm -f "$VFS_DIR/mixed_test.bin"

echo ""
echo "=== Test Complete ==="
echo "End time: $(date)"
echo ""
echo "Now stop perf recording with Ctrl+C and run:"
echo "  sudo perf report -i /tmp/vfs-read-perf.data --stdio --no-children > perf/vfs-read-perf-analysis.txt"
