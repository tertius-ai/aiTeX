#!/usr/bin/env python3
"""Fetch and verify the versioned arXiv sources in corpus.lock.json."""

from __future__ import annotations

import argparse
import gzip
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
from pathlib import Path, PurePosixPath

sys.dont_write_bytecode = True
from corpus_lock import tree_fingerprint


ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "qualification" / "corpus.lock.json"
USER_AGENT = "aiTeX-qualification/1.0 (versioned compiler test corpus)"


def clean_text(entries: list[dict]) -> str:
    return "".join(f"corpus/arxiv/{item['id']}\t{item['root']}\n"
                   for item in entries)


def unpack(raw: bytes, destination: Path) -> None:
    try:
        archive = tarfile.open(fileobj=io.BytesIO(raw), mode="r:*")
    except tarfile.ReadError:
        (destination / "main.tex").write_bytes(gzip.decompress(raw))
        return
    with archive:
        for member in archive.getmembers():
            path = PurePosixPath(member.name)
            if (not member.isfile() or path.is_absolute() or ".." in path.parts):
                continue
            target = destination.joinpath(*path.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            if source is not None:
                with target.open("wb") as output:
                    shutil.copyfileobj(source, output)


def verify(project: Path, item: dict) -> str | None:
    if not (project / item["root"]).is_file():
        return f"root {item['root']} is absent"
    digest, files, size = tree_fingerprint(project)
    if digest != item["tree_sha256"]:
        return f"tree hash differs ({digest})"
    if files != item["files"] or size != item["bytes"]:
        return f"inventory differs ({files} files, {size} bytes)"
    return None


def fetch(item: dict, arxiv: Path) -> None:
    destination = arxiv / item["id"]
    if destination.exists():
        error = verify(destination, item)
        if error:
            raise RuntimeError(f"{item['id']}: existing directory {error}; refusing to replace it")
        print(f"  cached {item['id']}")
        return
    request = urllib.request.Request(item["url"], headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=120) as response:
        raw = response.read()
    temporary = Path(tempfile.mkdtemp(prefix=f".{item['id']}.", dir=arxiv))
    try:
        unpack(raw, temporary)
        error = verify(temporary, item)
        if error:
            raise RuntimeError(f"{item['id']}: downloaded source {error}")
        os.replace(temporary, destination)
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)
    print(f"  fetched {item['id']}")


def capture(clean: Path) -> dict:
    entries = []
    for line in clean.read_text().splitlines():
        if not line.strip():
            continue
        directory, root = line.split("\t", 1)
        project = Path(directory)
        if not project.is_absolute():
            project = ROOT / project
        identity = project.name
        digest, files, size = tree_fingerprint(project)
        dvi = project / f"{Path(root).stem}.dvi"
        if not dvi.is_file():
            raise SystemExit(f"{identity}: reference DVI is absent")
        dvitype = shutil.which("dvitype")
        if not dvitype:
            raise SystemExit("dvitype is absent")
        text = subprocess.run([dvitype, str(dvi)], stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, text=True,
                              check=True).stdout
        pages = text.count("beginning of page")
        detail = subprocess.run(
            [dvitype, "-output-level=2", "-page-start=*", str(dvi)],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
            check=True).stdout
        characters = detail.count("setchar") + detail.count("set_char_")
        entries.append({
            "id": identity,
            "url": f"https://arxiv.org/e-print/{identity}",
            "root": root,
            "tree_sha256": digest,
            "files": files,
            "bytes": size,
            "reference_pages": pages,
            "reference_characters": characters,
        })
    return {"schema": 1, "description": "Versioned aiTeX compiler parity corpus",
            "entries": entries}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--offline", action="store_true",
                        help="verify cached sources without making requests")
    parser.add_argument("--capture", metavar="CLEAN", type=Path,
                        help="print lockfile JSON from an existing clean list")
    parser.add_argument("--corpus-dir", type=Path, default=ROOT / "corpus",
                        help="populate this corpus directory instead of the checkout's")
    args = parser.parse_args()
    if args.capture:
        print(json.dumps(capture(args.capture), indent=2))
        return 0
    lock = json.loads(LOCK.read_text())
    entries = lock["entries"]
    arxiv = args.corpus_dir / "arxiv"
    meta = args.corpus_dir / "meta"
    arxiv.mkdir(parents=True, exist_ok=True)
    meta.mkdir(parents=True, exist_ok=True)
    for index, item in enumerate(entries):
        if args.offline:
            error = verify(arxiv / item["id"], item)
            if error:
                raise SystemExit(f"{item['id']}: {error}")
            print(f"  ok     {item['id']}")
        else:
            fetch(item, arxiv)
            if index + 1 < len(entries) and not (arxiv / entries[index + 1]["id"]).exists():
                time.sleep(3)
    (meta / "clean.txt").write_text(clean_text(entries))
    print(f"locked corpus: {len(entries)} source trees match")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
