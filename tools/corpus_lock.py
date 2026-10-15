#!/usr/bin/env python3
"""Shared deterministic hashing rules for the locked arXiv corpus."""

from __future__ import annotations

import hashlib
from pathlib import Path


# These are products of a TeX/BibTeX/makeindex run, not submission inputs.
# In particular, .bbl and .ind are intentionally absent: arXiv submissions
# commonly ship them and the document consumes them as source.
GENERATED_SUFFIXES = {
    ".aux",
    ".dvi",
    ".log",
    ".toc",
    ".lof",
    ".lot",
    ".out",
    ".blg",
    ".idx",
    ".nav",
    ".snm",
    ".brf",
    ".spl",
    ".fls",
    ".fdb_latexmk",
    ".synctex.gz",
    ".out.ps",
    ".run.xml",
    ".bcf",
}


def source_files(root: Path) -> list[Path]:
    """Return the submission inputs in a stable, repository-neutral order."""
    files = []
    for path in root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        name = path.name
        if any(name.endswith(suffix) for suffix in GENERATED_SUFFIXES):
            continue
        files.append(path)
    return sorted(files, key=lambda path: path.relative_to(root).as_posix())


def tree_fingerprint(root: Path) -> tuple[str, int, int]:
    """Hash relative names and bytes with unambiguous length prefixes."""
    digest = hashlib.sha256()
    count = 0
    size = 0
    for path in source_files(root):
        relative = path.relative_to(root).as_posix().encode("utf-8")
        body = path.read_bytes()
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(len(body).to_bytes(8, "big"))
        digest.update(body)
        count += 1
        size += len(body)
    return digest.hexdigest(), count, size
