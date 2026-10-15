#!/usr/bin/env python3
"""Generate deterministic original PNG assets for PDF qualification."""

from __future__ import annotations

import argparse
import struct
import zlib
from pathlib import Path


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def chunk(kind: bytes, payload: bytes) -> bytes:
    body = kind + payload
    return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))


def png(
    width: int,
    height: int,
    color_type: int,
    rows: list[bytes],
    *,
    depth: int = 8,
    palette: bytes | None = None,
    transparency: bytes | None = None,
) -> bytes:
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color_type]
    row_bytes = (width * channels * depth + 7) // 8
    if len(rows) != height or any(len(row) != row_bytes for row in rows):
        raise ValueError("PNG rows do not match the declared dimensions")
    header = struct.pack(">IIBBBBB", width, height, depth, color_type, 0, 0, 0)
    pixels = b"".join(b"\0" + row for row in rows)
    chunks = [chunk(b"IHDR", header)]
    if palette is not None:
        chunks.append(chunk(b"PLTE", palette))
    if transparency is not None:
        chunks.append(chunk(b"tRNS", transparency))
    chunks.extend((chunk(b"IDAT", zlib.compress(pixels, level=9)), chunk(b"IEND", b"")))
    return PNG_SIGNATURE + b"".join(chunks)


def rgb_pattern(width: int, height: int) -> list[bytes]:
    rows = []
    for y in range(height):
        row = bytearray()
        for x in range(width):
            row.extend(
                (
                    (3 * x + 5 * y + (x * y) % 97) % 256,
                    (7 * x + 2 * y + (x ^ y)) % 256,
                    (11 * x + 13 * y + (x * y) % 53) % 256,
                )
            )
        rows.append(bytes(row))
    return rows


def rgba_pattern(width: int, height: int) -> list[bytes]:
    rows = []
    cx, cy = width // 2, height // 2
    radius2 = (min(width, height) * 2 // 5) ** 2
    for y in range(height):
        row = bytearray()
        for x in range(width):
            distance2 = (x - cx) ** 2 + (y - cy) ** 2
            alpha = 210 if distance2 < radius2 else 72
            row.extend(((5 * x) % 256, (3 * y) % 256, (x + 2 * y) % 256, alpha))
        rows.append(bytes(row))
    return rows


def grayscale_rows(width: int, height: int, depth: int) -> list[bytes]:
    maximum = (1 << depth) - 1
    rows = []
    for y in range(height):
        samples = [((x // 9) + (y // 7)) % (maximum + 1) for x in range(width)]
        if depth == 8:
            rows.append(bytes(samples))
            continue
        packed = bytearray()
        per_byte = 8 // depth
        for start in range(0, width, per_byte):
            value = 0
            for offset in range(per_byte):
                value <<= depth
                if start + offset < width:
                    value |= samples[start + offset]
            packed.append(value)
        rows.append(bytes(packed))
    return rows


def indexed_rows(width: int, height: int) -> list[bytes]:
    rows = []
    for y in range(height):
        packed = bytearray()
        for x in range(0, width, 4):
            samples = [((x + offset) // 12 + y // 10) % 4 for offset in range(4)]
            packed.append((samples[0] << 6) | (samples[1] << 4) | (samples[2] << 2) | samples[3])
        rows.append(bytes(packed))
    return rows


def generate(directory: Path) -> dict[str, Path]:
    directory.mkdir(parents=True, exist_ok=True)
    assets = {
        "pattern.png": png(512, 320, 2, rgb_pattern(512, 320)),
        "alpha.png": png(256, 180, 6, rgba_pattern(256, 180)),
        "gray-1.png": png(256, 96, 0, grayscale_rows(256, 96, 1), depth=1),
        "gray-8.png": png(256, 96, 0, grayscale_rows(256, 96, 8)),
        "indexed-2-trns.png": png(
            256,
            96,
            3,
            indexed_rows(256, 96),
            depth=2,
            palette=bytes((25, 45, 90, 220, 70, 55, 45, 190, 100, 245, 225, 125)),
            transparency=bytes((255, 0, 255, 255)),
        ),
    }
    # A second filename with identical source bytes exercises normalized
    # content reuse independently from repeated placement of one TeX name.
    assets["pattern-copy.png"] = assets["pattern.png"]
    paths = {}
    for name, data in assets.items():
        path = directory / name
        path.write_bytes(data)
        paths[name] = path
    return paths


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    for name, path in sorted(generate(args.directory).items()):
        print(f"{name} {path.stat().st_size}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
