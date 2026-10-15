#!/usr/bin/env python3
"""Capture or validate the external environment used to declare aiTeX done."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

sys.dont_write_bytecode = True
from corpus_lock import tree_fingerprint


ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "qualification" / "environment.lock.json"
CORPUS_LOCK = ROOT / "qualification" / "corpus.lock.json"
PACKAGES = ROOT / "qualification" / "packages.txt"

COMMANDS = {
    "tex": ["tex", "--version"],
    "pdftex": ["pdftex", "--version"],
    "latex": ["latex", "--version"],
    "kpsewhich": ["kpsewhich", "--version"],
    "dvitype": ["dvitype", "--version"],
    "ghostscript": ["gs", "--version"],
    "gawk": ["gawk", "--version"],
}

ARTIFACTS = {
    "latex.ltx": ["kpsewhich", "latex.ltx"],
    "latex.fmt": ["kpsewhich", "-engine", "pdftex", "latex.fmt"],
    "pdflatex.fmt": ["kpsewhich", "-engine", "pdftex", "pdflatex.fmt"],
    "cmr10.tfm": ["kpsewhich", "cmr10.tfm"],
}

# These two hashes preserve the generated system formats used by the original
# cold artifact. Package-manager format dumps include install-time state and
# cannot be reproduced on a fresh runner. Current gates instead build and
# verify the fixed-epoch formats in reference-formats.lock.json before any
# classical LaTeX command is run.
LEGACY_SYSTEM_FORMATS = {"latex.fmt", "pdflatex.fmt"}


def activate_reference_formats() -> Path:
    directory = Path(os.environ.get(
        "AITEX_REFERENCE_FORMATS",
        ROOT / "target" / "qualification" / "reference-formats",
    )).resolve()
    result = subprocess.run(
        [sys.executable, str(ROOT / "tools" / "reference_formats.py"),
         "--output", str(directory)],
        check=False,
    )
    if result.returncode != 0:
        raise SystemExit("could not prepare locked reference formats")
    os.environ["AITEX_REFERENCE_FORMATS"] = str(directory)
    previous = os.environ.get("TEXFORMATS", "")
    os.environ["TEXFORMATS"] = f"{directory}:{previous}"
    return directory


def output(argv: list[str]) -> str:
    result = subprocess.run(argv, check=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True)
    return result.stdout.strip()


def first_line(argv: list[str]) -> str:
    return output(argv).splitlines()[0]


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def package_names() -> list[str]:
    return [line.strip() for line in PACKAGES.read_text().splitlines()
            if line.strip() and not line.lstrip().startswith("#")]


def kpse(name: str) -> Path | None:
    result = subprocess.run(["kpsewhich", name], stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True)
    value = result.stdout.strip()
    return Path(value) if result.returncode == 0 and value else None


def artifact(name: str) -> Path | None:
    argv = ARTIFACTS[name]
    result = subprocess.run(argv, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True)
    value = result.stdout.strip()
    return Path(value) if result.returncode == 0 and value else None


def capture() -> dict:
    commands = {name: first_line(argv) for name, argv in COMMANDS.items()}
    commands["python"] = ".".join(map(str, sys.version_info[:3]))
    artifacts = {}
    for name in ARTIFACTS:
        path = artifact(name)
        if path is None:
            raise SystemExit(f"cannot capture environment: kpsewhich cannot find {name}")
        artifacts[name] = {"sha256": sha256(path)}
    packages = {}
    for name in package_names():
        path = kpse(f"{name}.sty")
        packages[name] = ({"status": "present", "sha256": sha256(path)}
                          if path else {"status": "absent"})
    return {
        "schema": 1,
        "description": "aiTeX compiler qualification reference",
        "commands": commands,
        "artifacts": artifacts,
        "packages": packages,
    }


def check_environment(expected: dict) -> list[str]:
    errors = []
    catalog = set(package_names())
    locked_packages = set(expected["packages"])
    if catalog != locked_packages:
        added = sorted(catalog - locked_packages)
        removed = sorted(locked_packages - catalog)
        errors.append(f"package catalog differs (unlocked={added}, missing={removed})")
    for name, wanted in expected["commands"].items():
        try:
            got = (".".join(map(str, sys.version_info[:3])) if name == "python"
                   else first_line(COMMANDS[name]))
        except (OSError, subprocess.CalledProcessError, KeyError) as error:
            errors.append(f"{name}: unavailable ({error})")
            continue
        if got != wanted:
            errors.append(f"{name}: expected {wanted!r}, got {got!r}")
    for name, item in expected["artifacts"].items():
        if name in LEGACY_SYSTEM_FORMATS:
            continue
        path = artifact(name)
        if path is None:
            errors.append(f"{name}: not found by kpsewhich")
        elif sha256(path) != item["sha256"]:
            errors.append(f"{name}: content hash differs")
    present = absent = 0
    for name, item in expected["packages"].items():
        path = kpse(f"{name}.sty")
        if item["status"] == "present":
            present += 1
            if path is None:
                errors.append(f"package {name}: expected present, is absent")
            elif sha256(path) != item["sha256"]:
                errors.append(f"package {name}: content hash differs")
        else:
            absent += 1
            if path is not None:
                errors.append(f"package {name}: expected absent, is now present")
    if not errors:
        print(f"environment lock: {len(expected['commands'])} tools, "
              f"{len(expected['artifacts']) - len(LEGACY_SYSTEM_FORMATS)} source artifacts, "
              f"{len(LEGACY_SYSTEM_FORMATS)} deterministic formats, "
              f"{present} packages present, {absent} explicitly absent")
    return errors


def expected_clean(entries: list[dict]) -> str:
    return "".join(f"corpus/arxiv/{item['id']}\t{item['root']}\n"
                   for item in entries)


def check_corpus(lock: dict, required: bool) -> list[str]:
    entries = lock["entries"]
    arxiv = ROOT / "corpus" / "arxiv"
    if not arxiv.is_dir() and not required:
        print(f"corpus lock: {len(entries)} entries pinned (sources not present)")
        return []
    errors = []
    for item in entries:
        project = arxiv / item["id"]
        if not project.is_dir():
            errors.append(f"corpus {item['id']}: source directory is absent")
            continue
        root = project / item["root"]
        if not root.is_file():
            errors.append(f"corpus {item['id']}: root {item['root']} is absent")
            continue
        digest, files, size = tree_fingerprint(project)
        if digest != item["tree_sha256"]:
            errors.append(f"corpus {item['id']}: source tree hash differs")
        if files != item["files"] or size != item["bytes"]:
            errors.append(f"corpus {item['id']}: source inventory differs")
    clean = ROOT / "corpus" / "meta" / "clean.txt"
    wanted = expected_clean(entries)
    if not clean.is_file():
        errors.append("corpus/meta/clean.txt is absent; run fetch_locked_corpus.py")
    elif clean.read_text() != wanted:
        errors.append("corpus/meta/clean.txt differs from the locked relative list")
    if not errors:
        print(f"corpus lock: {len(entries)} source trees match")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--capture", action="store_true",
                        help="print the current environment as lockfile JSON")
    parser.add_argument("--require-corpus", action="store_true",
                        help="fail unless every locked corpus source is present")
    args = parser.parse_args()
    activate_reference_formats()
    if args.capture:
        print(json.dumps(capture(), indent=2, sort_keys=True))
        return 0
    if not LOCK.is_file() or not CORPUS_LOCK.is_file():
        print("qualification lockfiles are absent", file=sys.stderr)
        return 2
    errors = check_environment(json.loads(LOCK.read_text()))
    errors += check_corpus(json.loads(CORPUS_LOCK.read_text()), args.require_corpus)
    if errors:
        for error in errors:
            print(f"FAIL {error}", file=sys.stderr)
        return 1
    print("qualification environment matches lock")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
