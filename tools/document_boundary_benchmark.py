#!/usr/bin/env python3
"""Compare the v0.3 document-boundary reuse with its full-pass parent."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import platform
import re
import shutil
import signal
import statistics
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests/perf/document-boundary.tex"
DEFAULT_OUTPUT = ROOT / "target/qualification/document-boundary-v1.results.json"
METRICS_PREFIX = "AITEX_METRICS "
SCHEMA = 1


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def command_line(argv: list[str], cwd: Path = ROOT) -> str | None:
    try:
        result = subprocess.run(
            argv,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return next((line for line in result.stdout.splitlines() if line.strip()), None)


def parse_time(path: Path) -> dict[str, float | int | None]:
    text = path.read_text(errors="replace") if path.is_file() else ""
    if platform.system() == "Darwin":
        summary = re.search(r"([0-9.]+) real\s+([0-9.]+) user\s+([0-9.]+) sys", text)
        rss = re.search(r"^\s*(\d+)\s+maximum resident set size\s*$", text, re.M)
        return {
            "cpu_seconds": (
                round(float(summary.group(2)) + float(summary.group(3)), 6)
                if summary
                else None
            ),
            "max_rss_kib": round(int(rss.group(1)) / 1024) if rss else None,
        }

    def number(label: str, cast: type) -> int | float | None:
        match = re.search(rf"^\s*{re.escape(label)}:\s*([0-9.]+)\s*$", text, re.M)
        return cast(match.group(1)) if match else None

    user = number("User time (seconds)", float)
    system = number("System time (seconds)", float)
    return {
        "cpu_seconds": (
            round(user + system, 6)
            if user is not None and system is not None
            else None
        ),
        "max_rss_kib": number("Maximum resident set size (kbytes)", int),
    }


def timed_command(
    argv: list[str], cwd: Path, env: dict[str, str], timeout: int
) -> tuple[dict[str, Any], str]:
    descriptor, raw_path = tempfile.mkstemp(prefix=".document-boundary-time.", dir=cwd)
    os.close(descriptor)
    time_path = Path(raw_path)
    command = (
        ["/usr/bin/time", "-l", "-o", str(time_path), *argv]
        if platform.system() == "Darwin"
        else ["/usr/bin/time", "-v", "-o", str(time_path), "--", *argv]
    )
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            raw_output, _ = process.communicate(timeout=timeout)
            returncode = process.returncode
            timed_out = False
        except subprocess.TimeoutExpired as error:
            raw_output = error.output or b""
            os.killpg(process.pid, signal.SIGKILL)
            remainder, _ = process.communicate()
            raw_output += remainder or b""
            returncode = 124
            timed_out = True
        usage = parse_time(time_path)
    finally:
        time_path.unlink(missing_ok=True)
    return (
        {
            "returncode": returncode,
            "wall_seconds": round(time.monotonic() - started, 6),
            "timed_out": timed_out,
            **usage,
        },
        raw_output.decode("utf-8", "replace"),
    )


def benchmark_env() -> dict[str, str]:
    env = dict(os.environ)
    env.update(
        {
            "SOURCE_DATE_EPOCH": "1000000000",
            "FORCE_SOURCE_DATE": "1",
            "LC_ALL": "C",
            "TZ": "UTC",
            "max_print_line": "1000",
            "AITEX_MAX_COMMANDS": "2000000000",
            "AITEX_MAX_STEPS": "2000000000",
            "AITEX_STACK_MB": "64",
            "AITEX_METRICS": "1",
        }
    )
    return env


def parse_metrics(output: str) -> list[dict[str, Any]]:
    result = []
    for line in output.splitlines():
        if line.startswith(METRICS_PREFIX):
            result.append(json.loads(line[len(METRICS_PREFIX) :]))
    return result


def run_sample(
    root: Path,
    mode: str,
    binary: Path,
    format_path: Path,
    timeout: int,
) -> dict[str, Any]:
    root.mkdir(parents=True)
    shutil.copy2(FIXTURE, root / "main.tex")
    command = [
        str(binary),
        "main.tex",
        "--fmt",
        str(format_path),
        "--no-cache",
        "--no-banner",
        "--verbose",
        "-halt-on-error",
        "-file-line-error",
        "-o",
        "main.dvi",
    ]
    measured, output = timed_command(command, root, benchmark_env(), timeout)
    artifact = root / "main.dvi"
    metrics = parse_metrics(output)
    sample = {
        **measured,
        "mode": mode,
        "tex_passes": output.count("aitex build: TeX pass"),
        "document_boundary_hits": output.count("document boundary hit"),
        "document_boundary_misses": output.count("document boundary miss"),
        "converged": "aitex build: converged after" in output,
        "format_sources": [item.get("format_source") for item in metrics],
        "tex_total_ms": [item.get("tex_total_ms") for item in metrics],
        "output_bytes": artifact.stat().st_size if artifact.is_file() else None,
        "output_sha256": sha256(artifact) if artifact.is_file() else None,
        "transcript_sha256": hashlib.sha256(output.encode()).hexdigest(),
    }
    if measured["returncode"] != 0 or not sample["converged"]:
        raise RuntimeError(f"{mode} build failed:\n{output[-3000:]}")
    if sample["tex_passes"] != 3 or len(metrics) != 3:
        raise RuntimeError(f"{mode} did not execute the fixture's three-pass contract")
    return sample


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def summarize(samples: list[dict[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {"samples": len(samples)}
    for field in ("wall_seconds", "cpu_seconds", "max_rss_kib"):
        values = [float(sample[field]) for sample in samples if sample.get(field) is not None]
        if values:
            result[field] = {
                "p50": round(statistics.median(values), 6),
                "p95": round(percentile(values, 0.95), 6),
            }
    return result


def ratio(candidate: float | int | None, fallback: float | int | None) -> float | None:
    if candidate is None or fallback in (None, 0):
        return None
    return round(float(candidate) / float(fallback), 6)


def binary_identity(binary: Path, format_path: Path, commit: str) -> dict[str, Any]:
    return {
        "commit": commit,
        "binary_sha256": sha256(binary),
        "format_sha256": sha256(format_path),
        "version": command_line([str(binary), "--version"]),
    }


def validate_result(result: dict[str, Any]) -> None:
    if result.get("schema") != SCHEMA:
        raise ValueError("unsupported document-boundary result schema")
    if result.get("fixture", {}).get("sha256") != sha256(FIXTURE):
        raise ValueError("document-boundary fixture hash is stale")
    comparison = result.get("comparison", {})
    if not comparison.get("exact_dvi"):
        raise ValueError("document-boundary result did not preserve exact DVI")
    if not comparison.get("candidate_boundary_observed"):
        raise ValueError("candidate never reused the document boundary")
    if not comparison.get("wall_gain_observed"):
        raise ValueError("candidate did not beat the full-pass fallback")
    if comparison.get("retained_boundary_limit") != 1:
        raise ValueError("document-boundary retention is not recorded as bounded")


def run(args: argparse.Namespace) -> dict[str, Any]:
    candidate_binary = args.candidate_binary.resolve()
    candidate_format = args.candidate_format.resolve()
    fallback_binary = args.fallback_binary.resolve()
    fallback_format = args.fallback_format.resolve()
    for path in (candidate_binary, candidate_format, fallback_binary, fallback_format):
        if not path.is_file():
            raise SystemExit(f"required benchmark input is absent: {path}")

    samples: dict[str, list[dict[str, Any]]] = {"candidate": [], "fallback": []}
    with tempfile.TemporaryDirectory(prefix="aitex-document-boundary-") as temporary:
        scratch = Path(temporary)
        for repetition in range(args.warmups + args.samples):
            measured = repetition >= args.warmups
            sample_number = repetition - args.warmups
            order = ("fallback", "candidate") if repetition % 2 == 0 else ("candidate", "fallback")
            current: dict[str, dict[str, Any]] = {}
            for mode in order:
                binary, format_path = (
                    (candidate_binary, candidate_format)
                    if mode == "candidate"
                    else (fallback_binary, fallback_format)
                )
                current[mode] = run_sample(
                    scratch / f"{repetition}-{mode}", mode, binary, format_path, args.timeout
                )
            if current["candidate"]["output_sha256"] != current["fallback"]["output_sha256"]:
                raise RuntimeError("candidate and fallback DVI bytes differ")
            if current["candidate"]["document_boundary_hits"] != 1:
                raise RuntimeError("candidate did not take exactly one document-boundary hit")
            if current["fallback"]["document_boundary_hits"] != 0:
                raise RuntimeError("fallback unexpectedly reports document-boundary reuse")
            if measured:
                for mode in samples:
                    samples[mode].append(current[mode])
                print(f"[document-boundary] {sample_number + 1}/{args.samples}", flush=True)

    summaries = {mode: summarize(values) for mode, values in samples.items()}
    candidate_wall = summaries["candidate"]["wall_seconds"]["p50"]
    fallback_wall = summaries["fallback"]["wall_seconds"]["p50"]
    hashes = {
        sample["output_sha256"] for values in samples.values() for sample in values
    }
    result = {
        "schema": SCHEMA,
        "description": "engine-observed document-boundary reuse versus full-pass parent",
        "created_at": utc_now(),
        "fixture": {"path": str(FIXTURE.relative_to(ROOT)), "sha256": sha256(FIXTURE)},
        "protocol": {
            "route": "complete-build DVI",
            "cache": "persistent caches disabled",
            "samples": args.samples,
            "warmups": args.warmups,
            "order": "sequential, counterbalanced by repetition",
            "clock": "SOURCE_DATE_EPOCH=1000000000; UTC; C locale",
            "worker_budget": 1,
        },
        "environment": {
            "platform": platform.platform(),
            "architecture": platform.machine(),
            "logical_cpus": os.cpu_count(),
            "python": platform.python_version(),
        },
        "candidate": binary_identity(candidate_binary, candidate_format, args.candidate_commit),
        "fallback": binary_identity(fallback_binary, fallback_format, args.fallback_commit),
        "runs": {mode: {"summary": summaries[mode], "samples": values} for mode, values in samples.items()},
        "comparison": {
            "exact_dvi": len(hashes) == 1,
            "output_sha256": next(iter(hashes)) if len(hashes) == 1 else None,
            "candidate_boundary_observed": all(
                sample["document_boundary_hits"] == 1 for sample in samples["candidate"]
            ),
            "retained_boundary_limit": 1,
            "wall_p50_ratio": ratio(candidate_wall, fallback_wall),
            "cpu_p50_ratio": ratio(
                summaries["candidate"].get("cpu_seconds", {}).get("p50"),
                summaries["fallback"].get("cpu_seconds", {}).get("p50"),
            ),
            "rss_p50_ratio": ratio(
                summaries["candidate"].get("max_rss_kib", {}).get("p50"),
                summaries["fallback"].get("max_rss_kib", {}).get("p50"),
            ),
            "wall_gain_observed": candidate_wall < fallback_wall,
        },
    }
    validate_result(result)
    return result


def self_test() -> None:
    assert percentile([1.0, 2.0, 3.0], 0.5) == 2.0
    assert ratio(3, 4) == 0.75
    text = FIXTURE.read_text(encoding="utf-8")
    assert "250000" in text and "late.toc" in text and "\\begin{document}" in text
    print("document-boundary benchmark self-test: fixture and statistics valid")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-binary", type=Path)
    parser.add_argument("--candidate-format", type=Path)
    parser.add_argument("--candidate-commit")
    parser.add_argument("--fallback-binary", type=Path)
    parser.add_argument("--fallback-format", type=Path)
    parser.add_argument("--fallback-commit")
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--check", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.check is not None:
        validate_result(json.loads(args.check.read_text(encoding="utf-8")))
        print(f"document-boundary benchmark result valid: {args.check}")
        return
    required = (
        "candidate_binary",
        "candidate_format",
        "candidate_commit",
        "fallback_binary",
        "fallback_format",
        "fallback_commit",
    )
    missing = [name.replace("_", "-") for name in required if getattr(args, name) is None]
    if missing:
        parser.error("missing benchmark inputs: " + ", ".join(missing))
    if args.samples < 1 or args.warmups < 0 or args.timeout < 1:
        parser.error("samples/timeout must be positive and warmups cannot be negative")
    result = run(args)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(
        "document-boundary benchmark: "
        f"wall={result['comparison']['wall_p50_ratio']:.3f}x "
        f"cpu={result['comparison']['cpu_p50_ratio']:.3f}x "
        f"rss={result['comparison']['rss_p50_ratio']:.3f}x; exact DVI"
    )


if __name__ == "__main__":
    main()
