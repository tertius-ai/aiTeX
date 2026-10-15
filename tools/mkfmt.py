#!/usr/bin/env python3
"""Compatibility wrapper for aiTeX's native local-format builder."""

from __future__ import annotations

import argparse
import os
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="directory that receives fmt.afmt")
    args = parser.parse_args()

    binary = Path(os.environ.get("AITEX_BINARY", ROOT / "target/release/aitex")).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error("build first: cargo build --release --locked -p aitex-cli")
    destination = args.directory.resolve() / "fmt.afmt"
    destination.parent.mkdir(parents=True, exist_ok=True)
    return subprocess.run(
        [str(binary), "format", "--output", str(destination)],
        cwd=ROOT,
        check=False,
    ).returncode


if __name__ == "__main__":
    raise SystemExit(main())
