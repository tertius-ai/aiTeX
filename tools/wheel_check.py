#!/usr/bin/env python3
"""Verify that an aiTeX wheel contains the native executable and notices."""

from __future__ import annotations

import argparse
import os
import zipfile
from pathlib import Path


def one(names: list[str], suffix: str) -> str:
    matches = [name for name in names if name.endswith(suffix)]
    if len(matches) != 1:
        raise SystemExit(f"wheel: expected one {suffix}, found {len(matches)}")
    return matches[0]


def check(wheel: Path) -> None:
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
        executable = one(names, ".data/scripts/aitex")
        notices = one(
            names, ".data/data/share/doc/aitex/THIRD_PARTY_NOTICES.md"
        )
        one(names, ".dist-info/licenses/LICENSE-MIT")
        one(names, ".dist-info/licenses/LICENSE-APACHE")
        one(names, ".dist-info/sboms/aitex-cli.cyclonedx.json")
        wheel_metadata = one(names, ".dist-info/WHEEL")
        metadata = archive.read(wheel_metadata).decode("utf-8")
        tags = [
            line.removeprefix("Tag: ")
            for line in metadata.splitlines()
            if line.startswith("Tag: ")
        ]
        if not tags or any(tag.endswith("-any") for tag in tags):
            raise SystemExit(f"wheel: expected native platform tag, found {tags}")
        mode = archive.getinfo(executable).external_attr >> 16
        if not mode & 0o111:
            raise SystemExit(f"wheel: {executable} is not executable ({oct(mode)})")
        if archive.getinfo(notices).file_size == 0:
            raise SystemExit("wheel: third-party notice is empty")

    print(f"wheel: {wheel.name}")
    print(f"wheel: tag={','.join(tags)} executable={oct(mode & 0o777)}")
    print("wheel: local-format=generated on first document build")
    print(f"wheel: archive={os.path.getsize(wheel) / 1_000_000:.1f} MB")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wheel", type=Path, nargs="+")
    args = parser.parse_args()
    for wheel in args.wheel:
        check(wheel)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
