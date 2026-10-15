#!/usr/bin/env python3
"""Run the layered, manifest-locked TeX ecosystem regression sweep."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = ROOT / "qualification" / "ecosystem-sweep-v1.json"
DEFAULT_BINARY = ROOT / "target" / "release" / "aitex"
DEFAULT_HOLDOUT = ROOT / "qualification" / "holdout-200-v02.results.json"


class SweepError(RuntimeError):
    pass


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def display_path(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def load_manifest(path: Path) -> dict[str, Any]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("schema") != 1:
        raise SweepError("manifest schema is not 1")
    lanes = manifest.get("lanes")
    if not isinstance(lanes, list) or not lanes:
        raise SweepError("manifest has no lanes")
    identities = [lane.get("id") for lane in lanes if isinstance(lane, dict)]
    if len(identities) != len(lanes) or any(not item for item in identities):
        raise SweepError("manifest contains an invalid lane")
    if len(set(identities)) != len(identities):
        raise SweepError("manifest contains duplicate lane IDs")
    for lane in lanes:
        modes = lane.get("modes")
        if not isinstance(modes, list) or not modes or not set(modes) <= {"pr", "full"}:
            raise SweepError(f"{lane['id']}: invalid modes")
        command = lane.get("command")
        if not isinstance(command, list) or not command or not all(
                isinstance(part, str) and part for part in command):
            raise SweepError(f"{lane['id']}: invalid command")
        minimum = lane.get("minimum_observations")
        if not isinstance(minimum, int) or minimum < 1:
            raise SweepError(f"{lane['id']}: minimum observations must be positive")
        if not lane.get("summary_regex") and not lane.get("observation_regex"):
            raise SweepError(f"{lane['id']}: no observation rule")
    return manifest


def observations(lane: dict[str, Any], output: str) -> tuple[int, int | None]:
    if pattern := lane.get("summary_regex"):
        match = re.search(pattern, output)
        if match is None:
            raise SweepError("required summary is absent")
        count = int(match.group("count"))
        total = int(match.group("total")) if "total" in match.groupdict() else None
    else:
        count = len(re.findall(lane["observation_regex"], output))
        total = None
    if count < lane["minimum_observations"]:
        raise SweepError(
            f"examined {count}, below locked minimum {lane['minimum_observations']}"
        )
    if lane.get("require_complete") and (total is None or count != total):
        raise SweepError(f"only {count}/{total} observations passed")
    return count, total


def render_command(
    parts: list[str], binary: Path, holdout_results: Path,
) -> list[str]:
    replacements = {
        "{binary}": str(binary),
        "{holdout_results}": str(holdout_results),
    }
    return [replacements.get(part, part) for part in parts]


def run_lane(
    lane: dict[str, Any], binary: Path, holdout_results: Path,
) -> tuple[dict[str, Any], str | None]:
    missing = [name for name in lane.get("requires", []) if shutil.which(name) is None]
    command = render_command(lane["command"], binary, holdout_results)
    started = time.monotonic()
    if missing:
        message = f"required tools are absent: {', '.join(missing)}"
        output = ""
        return ({
            "id": lane["id"],
            "description": lane["description"],
            "command": command,
            "seconds": 0.0,
            "returncode": None,
            "observations": 0,
            "stdout_sha256": hashlib.sha256(b"").hexdigest(),
            "passed": False,
            "error": message,
        }, message)
    try:
        process = subprocess.run(
            command,
            cwd=ROOT,
            env=dict(os.environ),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=int(lane["timeout_seconds"]),
            check=False,
        )
        output = process.stdout
        error = None
        try:
            count, total = observations(lane, output)
        except SweepError as failure:
            count, total = 0, None
            error = str(failure)
        if process.returncode != 0:
            error = f"command exited {process.returncode}" + (f"; {error}" if error else "")
        passed = error is None
        result = {
            "id": lane["id"],
            "description": lane["description"],
            "command": command,
            "seconds": round(time.monotonic() - started, 3),
            "returncode": process.returncode,
            "observations": count,
            "total": total,
            "stdout_sha256": hashlib.sha256(output.encode()).hexdigest(),
            "passed": passed,
        }
        if error:
            result["error"] = error
        return result, output if error else None
    except subprocess.TimeoutExpired as failure:
        output = failure.stdout or ""
        if isinstance(output, bytes):
            output = output.decode(errors="replace")
        message = f"timed out after {lane['timeout_seconds']}s"
        return ({
            "id": lane["id"],
            "description": lane["description"],
            "command": command,
            "seconds": round(time.monotonic() - started, 3),
            "returncode": None,
            "observations": 0,
            "stdout_sha256": hashlib.sha256(output.encode()).hexdigest(),
            "passed": False,
            "error": message,
        }, output)


def self_test() -> None:
    complete = {
        "summary_regex": r"PASS (?P<count>[0-9]+)/(?P<total>[0-9]+)",
        "minimum_observations": 2,
        "require_complete": True,
    }
    assert observations(complete, "PASS 3/3") == (3, 3)
    for output in ("PASS 0/0", "PASS 2/3", "nothing here"):
        try:
            observations(complete, output)
        except SweepError:
            pass
        else:
            raise AssertionError(f"invalid output passed: {output!r}")
    counted = {"observation_regex": r"(?m)^OK ", "minimum_observations": 2}
    assert observations(counted, "OK one\nOK two\n") == (2, None)
    print("ecosystem sweep self-test: zero, partial, and missing results are rejected")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--pr", action="store_true", help="run the curated PR subset")
    mode.add_argument("--full", action="store_true", help="run every lane")
    mode.add_argument("--self-test", action="store_true")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--holdout-results", type=Path, default=DEFAULT_HOLDOUT)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0

    selected_mode = "full" if args.full else "pr"
    manifest_path = args.manifest.resolve()
    binary = args.binary.resolve()
    holdout_results = args.holdout_results.resolve()
    try:
        manifest = load_manifest(manifest_path)
    except (OSError, json.JSONDecodeError, SweepError) as error:
        print(f"ecosystem sweep: FAIL: {error}", file=sys.stderr)
        return 2
    if not binary.is_file():
        print(f"ecosystem sweep: FAIL: release binary is absent: {binary}", file=sys.stderr)
        return 2
    lanes = [lane for lane in manifest["lanes"] if selected_mode in lane["modes"]]
    if not lanes:
        print(f"ecosystem sweep: FAIL: no {selected_mode} lanes", file=sys.stderr)
        return 2

    print(f"aiTeX ecosystem sweep ({selected_mode}): {len(lanes)} lanes", flush=True)
    results = []
    failed = False
    for lane in lanes:
        print(f"  • {lane['id']} ...", flush=True)
        result, diagnostic = run_lane(lane, binary, holdout_results)
        results.append(result)
        word = "ok" if result["passed"] else "FAIL"
        total = f"/{result['total']}" if result.get("total") is not None else ""
        print(
            f"    {word}: {result['observations']}{total} observations "
            f"in {result['seconds']:.3f}s",
            flush=True,
        )
        if not result["passed"]:
            failed = True
            print(f"    {result['error']}", file=sys.stderr)
            if diagnostic:
                lines = diagnostic.splitlines()
                for line in lines[-20:]:
                    print(f"      {line}", file=sys.stderr)

    uses_holdout = any(item["id"] == "locked-191-root-holdout" for item in results)
    artifact = {
        "schema": 1,
        "description": "aiTeX layered ecosystem regression sweep result",
        "created_at": utc_now(),
        "mode": selected_mode,
        "manifest": display_path(manifest_path),
        "manifest_sha256": sha256(manifest_path),
        "runner_sha256": sha256(Path(__file__)),
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "holdout_results": display_path(holdout_results) if uses_holdout else None,
        "holdout_results_sha256": (
            sha256(holdout_results) if uses_holdout and holdout_results.is_file() else None
        ),
        "lanes": results,
        "aggregate": {
            "passed": not failed,
            "lanes_passed": sum(item["passed"] for item in results),
            "lanes_total": len(results),
            "observations": sum(item["observations"] for item in results),
        },
    }
    output = (args.output.resolve() if args.output else
              ROOT / "target" / "qualification" / f"ecosystem-sweep-{selected_mode}.json")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    temporary.write_text(json.dumps(artifact, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, output)
    aggregate = artifact["aggregate"]
    status = "PASS" if aggregate["passed"] else "FAIL"
    print(
        f"ecosystem sweep: {status} {aggregate['lanes_passed']}/{aggregate['lanes_total']} "
        f"lanes, {aggregate['observations']} observations; evidence {output}"
    )
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
