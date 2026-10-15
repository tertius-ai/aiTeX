#!/usr/bin/env python3
"""Fail when a tracked Markdown file contains a broken relative link."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parent.parent
LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")


def tracked_markdown() -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "*.md"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    # `git ls-files --cached` still reports tracked files deleted in the
    # working tree until their removal is staged. They are no longer documents
    # whose links can be checked.
    return [path for name in result.stdout.splitlines() if (path := ROOT / name).is_file()]


def local_target(raw: str) -> str | None:
    target = raw.strip()
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1]
    if target.startswith(("#", "http://", "https://", "mailto:")):
        return None
    # An optional Markdown title follows the path after whitespace.
    target = target.split(maxsplit=1)[0]
    return unquote(target.split("#", 1)[0]) or None


def main() -> int:
    broken: list[str] = []
    for document in tracked_markdown():
        text = document.read_text(encoding="utf-8")
        for line_number, line in enumerate(text.splitlines(), 1):
            for match in LINK.finditer(line):
                target = local_target(match.group(1))
                if target is None:
                    continue
                resolved = (document.parent / target).resolve()
                if not resolved.exists():
                    relative = document.relative_to(ROOT)
                    broken.append(f"{relative}:{line_number}: {target}")

    if broken:
        print("documentation links: broken relative targets:", file=sys.stderr)
        print("\n".join(broken), file=sys.stderr)
        return 1

    print("documentation links: valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
