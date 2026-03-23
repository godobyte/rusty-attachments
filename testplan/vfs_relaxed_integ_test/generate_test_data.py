#!/usr/bin/env python3
"""Generate test data files of various sizes for integration testing.

Creates a directory tree simulating a studio asset structure with ~1GB
of files across different size categories.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path

import xxhash


# File size categories — ~1GB total across various sizes
FILE_SPECS: list[tuple[str, int, int]] = [
    # (subdirectory, count, size_bytes)
    ("configs", 50, 1024),             # 50 x 1KB = 50KB
    ("scripts", 20, 102400),           # 20 x 100KB = 2MB
    ("textures", 10, 10485760),        # 10 x 10MB = 100MB
    ("geometry", 3, 104857600),        # 3 x 100MB = 300MB
    ("scenes", 1, 314572800),          # 1 x 300MB = 300MB
]


def generate_deterministic_content(seed: str, size: int) -> bytes:
    """Generate deterministic pseudo-random content from a seed.

    Uses SHA-256 in counter mode to produce repeatable content.
    This ensures the same seed always produces the same bytes,
    which is critical for hash verification.

    Args:
        seed: String seed for content generation.
        size: Number of bytes to generate.

    Returns:
        Deterministic byte content of the requested size.
    """
    chunks: list[bytes] = []
    remaining: int = size
    counter: int = 0
    while remaining > 0:
        block: bytes = hashlib.sha256(f"{seed}:{counter}".encode()).digest()
        take: int = min(len(block), remaining)
        chunks.append(block[:take])
        remaining -= take
        counter += 1
    return b"".join(chunks)


def xxh128_file(path: Path) -> str:
    """Compute XXH128 hex digest of a file.

    Args:
        path: Path to the file.

    Returns:
        A 32-character lowercase hex string.
    """
    hasher = xxhash.xxh128()
    buf_size: int = 8 * 1024 * 1024
    with open(path, "rb") as f:
        while True:
            chunk: bytes = f.read(buf_size)
            if not chunk:
                break
            hasher.update(chunk)
    return hasher.hexdigest()


def xxh128_path_key(relative_path: str) -> str:
    """Compute the path key (XXH128 of relative path) matching the Rust implementation.

    Args:
        relative_path: Posix-normalized relative path.

    Returns:
        A 32-character lowercase hex string.
    """
    return xxhash.xxh128(relative_path.encode()).hexdigest()


def generate_test_data(base_dir: Path) -> dict:
    """Generate all test data files and return a manifest.

    Args:
        base_dir: Root directory for test data.

    Returns:
        Dict with file metadata: {relative_path: {size, xxh128, path_key}}
    """
    manifest: dict = {}
    total_bytes: int = 0

    for subdir, count, size in FILE_SPECS:
        dir_path: Path = base_dir / subdir
        dir_path.mkdir(parents=True, exist_ok=True)

        for i in range(count):
            if subdir == "configs":
                name: str = f"config_{i:04d}.json"
            elif subdir == "scripts":
                name = f"script_{i:04d}.py"
            elif subdir == "textures":
                name = f"texture_{i:04d}.exr"
            elif subdir == "geometry":
                name = f"geo_cache_{i:04d}.bgeo"
            else:
                name = f"scene_{i:04d}.hip"

            file_path: Path = dir_path / name
            relative_path: str = f"{subdir}/{name}"

            # Generate deterministic content
            seed: str = f"inttest:{relative_path}"
            content: bytes = generate_deterministic_content(seed, size)

            file_path.write_bytes(content)

            # Compute hashes
            content_hash: str = xxhash.xxh128(content).hexdigest()
            path_key: str = xxh128_path_key(relative_path)

            manifest[relative_path] = {
                "size": size,
                "xxh128": content_hash,
                "path_key": path_key,
            }

            total_bytes += size

        print(
            f"  Generated {count} files in {subdir}/ "
            f"({count * size / 1024 / 1024:.1f} MB)"
        )

    print(f"  Total: {len(manifest)} files, {total_bytes / 1024 / 1024:.1f} MB")
    return manifest


def main() -> None:
    """Entry point."""
    if len(sys.argv) < 2:
        print("Usage: python3 generate_test_data.py <output_dir>")
        sys.exit(1)

    base_dir = Path(sys.argv[1])
    print(f"Generating test data in {base_dir}...")
    manifest: dict = generate_test_data(base_dir)

    manifest_path: Path = base_dir / "manifest.json"
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)
    print(f"  Manifest written to {manifest_path}")


if __name__ == "__main__":
    main()
