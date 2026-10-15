#!/usr/bin/env python3
"""Delta-reduce a LaTeX root while preserving a stock/aiTeX DVI mismatch.

The reducer works on source lines in a caller-selected range. Everything
outside that range, and every other file in the arXiv project, is held fixed.
A candidate is interesting only when both engines exit cleanly, produce valid
DVI after the requested number of passes, and their ordered setchar streams
still differ. Results are memoized by content hash.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True
import arxiv_holdout as holdout


ROOT = Path(__file__).resolve().parent.parent


def compile_candidate(project: Path, root: Path, candidate: bytes, binary: Path,
                      fmt: Path, overlay: Path, passes: int, timeout: int) -> bool:
    streams = []
    for engine in ("stock", "aitex"):
        work = project.parent / engine
        shutil.copytree(project, work)
        work_root = work / root.relative_to(project)
        work_root.write_bytes(candidate)
        cwd = work_root.parent
        env = dict(os.environ)
        env.update({"LC_ALL": "C", "TZ": "UTC", "TEXMFHOME": str(overlay),
                    "AITEX_STACK_MB": "64", "max_print_line": "1000",
                    "AITEX_MAX_COMMANDS": "2000000000",
                    "AITEX_MAX_STEPS": "2000000000"})
        if engine == "stock":
            command = ["pdflatex", "-output-format=dvi", "-file-line-error",
                       "-interaction=nonstopmode", "-no-shell-escape",
                       "-halt-on-error", work_root.name]
            output = cwd / f"{work_root.stem}.dvi"
        else:
            command = [str(binary), "pass", work_root.name, "--fmt", str(fmt),
                       "-file-line-error", "-halt-on-error", "-o", "out.dvi"]
            output = cwd / "out.dvi"
        try:
            for _ in range(passes):
                process = subprocess.run(command, cwd=cwd, env=env,
                                         stdout=subprocess.DEVNULL,
                                         stderr=subprocess.DEVNULL,
                                         timeout=timeout)
                if process.returncode != 0:
                    return False
        except subprocess.TimeoutExpired:
            return False
        info = holdout.dvi_info(output)
        if not info.get("valid"):
            return False
        streams.append((info["pages"], info["characters"],
                        info["characters_sha256"]))
    return streams[0] != streams[1]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("id", help="locked arXiv ID, for example 2608.26344v1")
    parser.add_argument("--start-line", type=int, default=1,
                        help="first reducible one-based line")
    parser.add_argument("--end-line", type=int,
                        help="last reducible one-based line (default: EOF)")
    parser.add_argument("--passes", type=int, default=2)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--binary", type=Path,
                        default=ROOT / "target" / "release" / "aitex")
    parser.add_argument("--format", type=Path,
                        default=holdout.CORPUS / "format" / "fmt.afmt")
    parser.add_argument("--overlay", type=Path, default=holdout.TEXMF_OVERLAY)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    lock = json.loads(holdout.SOURCES.read_text())
    item = next((entry for entry in lock["entries"] if entry["id"] == args.id), None)
    if item is None or item.get("status") != "ready":
        raise SystemExit("ID is not a ready source in the locked corpus")
    project = holdout.CORPUS / "sources" / args.id.replace("/", "_")
    root = project / item["root"]
    lines = root.read_bytes().splitlines(keepends=True)
    lo = args.start_line - 1
    hi = args.end_line if args.end_line is not None else len(lines)
    if not 0 <= lo < hi <= len(lines):
        raise SystemExit("invalid reduction line range")
    prefix, units, suffix = lines[:lo], lines[lo:hi], lines[hi:]
    output = args.output or (holdout.CORPUS / "reductions" /
                             f"{args.id.replace('/', '_')}.tex")
    memo: dict[str, bool] = {}
    attempts = 0

    with tempfile.TemporaryDirectory(prefix=f"aitex-reduce-{args.id}.") as temp:
        staged = Path(temp) / "source"
        shutil.copytree(project, staged)
        staged_root = staged / item["root"]

        def interesting(parts: list[bytes]) -> bool:
            nonlocal attempts
            body = b"".join([*prefix, *parts, *suffix])
            digest = hashlib.sha256(body).hexdigest()
            if digest not in memo:
                attempts += 1
                trial = Path(temp) / f"trial-{attempts}"
                trial.mkdir()
                memo[digest] = compile_candidate(
                    staged, staged_root, body, args.binary.resolve(),
                    args.format.resolve(), args.overlay.resolve(), args.passes,
                    args.timeout)
                shutil.rmtree(trial, ignore_errors=True)
                # compile_candidate creates fixed sibling directories.
                shutil.rmtree(Path(temp) / "stock", ignore_errors=True)
                shutil.rmtree(Path(temp) / "aitex", ignore_errors=True)
                print(f"test {attempts}: {len(parts)} lines "
                      f"{'KEEP' if memo[digest] else 'drop rejected'}", flush=True)
            return memo[digest]

        if not interesting(units):
            raise SystemExit("the selected source/range does not reproduce a clean DVI mismatch")
        granularity = 2
        while len(units) >= 2:
            chunk = math.ceil(len(units) / granularity)
            reduced = False
            for start in range(0, len(units), chunk):
                candidate = units[:start] + units[start + chunk:]
                if candidate and interesting(candidate):
                    units = candidate
                    granularity = max(2, granularity - 1)
                    reduced = True
                    break
            if not reduced:
                if granularity >= len(units):
                    break
                granularity = min(len(units), granularity * 2)

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(b"".join([*prefix, *units, *suffix]))
    metadata = {
        "source_id": args.id, "source_root": item["root"],
        "source_tree_sha256": item["tree_sha256"],
        "range": [args.start_line, args.end_line or len(lines)],
        "original_range_lines": hi - lo, "reduced_range_lines": len(units),
        "predicate_attempts": attempts,
        "binary_sha256": holdout.file_sha256(args.binary.resolve()),
        "format_sha256": holdout.file_sha256(args.format.resolve()),
    }
    holdout.write_json(output.with_suffix(".json"), metadata)
    print(f"wrote {output}: {hi - lo} -> {len(units)} reducible lines")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
