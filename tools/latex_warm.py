#!/usr/bin/env python3
"""Qualify aiTeX's persistent output cache on the locked DVI corpus.

The main holdout compares normal cold processes: both implementations restore
their system LaTeX format, execute the document's class/package preamble, and
then typeset. This companion experiment asks a different question: how much
does aiTeX save on an unchanged converged build when ``--cache`` can validate
and restore the complete output artifact?

The locked cold result selects papers where stock DVI exists. Each selected
source is then stabilized afresh with the current tools. Three passes over the
same auxiliary state are measured and validated: stock LaTeX, current aiTeX
populating an empty cache, and current aiTeX hitting the resulting exact
converged-output artifact. Host-lexed class/project snapshots are forbidden in
the v0.2 release path.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import shutil
import statistics
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True
import arxiv_holdout as holdout


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT = ROOT / "qualification" / "holdout-200-v02.warm.json"
DEFAULT_REPORT = ROOT / "docs" / "HOLDOUT-200-V02-WARM.md"


def stage_ok(stage: dict) -> bool:
    output = stage["output"]
    return (stage["returncode"] == 0 and output.get("valid", False)
            and output.get("pages", 0) > 0)


def measured_pass(argv: list[str], cwd: Path, env: dict[str, str],
                  timeout: int, memory_limit_mib: int) -> tuple[dict, str]:
    code, wall, output, timed_out, measured = holdout.run_command(
        argv, cwd, env, timeout, memory_limit_mib)
    cpu = None
    if measured["user_seconds"] is not None and measured["system_seconds"] is not None:
        cpu = measured["user_seconds"] + measured["system_seconds"]
    return ({
        "returncode": code,
        "seconds": round(wall, 6),
        "cpu_seconds": round(cpu, 6) if cpu is not None else None,
        "timed_out": timed_out,
        **measured,
    }, output)


def aux_fingerprint(cwd: Path) -> str:
    digest = hashlib.sha256()
    paths = [path for path in cwd.rglob("*") if path.is_file()
             and path.suffix.lower() in {".aux", ".toc", ".lof", ".lot", ".out"}]
    for path in sorted(paths):
        digest.update(path.relative_to(cwd).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def stabilize(argv: list[str], cwd: Path, env: dict[str, str], timeout: int,
              memory_limit_mib: int) -> tuple[bool, int, str]:
    """Reach a fixed auxiliary state without including setup in measurements."""
    previous = None
    output = ""
    for attempt in range(1, 7):
        code, _, output, timed_out, _ = holdout.run_command(
            argv, cwd, env, timeout, memory_limit_mib)
        if code != 0 or timed_out:
            return False, attempt, output
        state = aux_fingerprint(cwd)
        if state == previous:
            return True, attempt, output
        previous = state
    return False, 6, output


def compact_comparison(comparison: dict) -> dict:
    return {
        "valid": comparison["valid"],
        "equivalent": comparison["equivalent"],
        "drawing_equivalent": comparison["drawing_equivalent"],
        "specials_equivalent": comparison["specials_equivalent"],
        **({"first_difference": comparison["first_difference"]}
           if "first_difference" in comparison else {}),
    }


def benchmark_one(item: dict, binary: Path, fmt: Path, texmf: Path,
                  timeout: int, memory_limit_mib: int) -> dict:
    source = holdout.CORPUS / "sources" / item["id"].replace("/", "_")
    with tempfile.TemporaryDirectory(prefix=f"aitex-warm-{item['id']}.") as tmp:
        base = Path(tmp)
        stock_project = base / "stock"
        candidate_project = base / "candidate"
        shutil.copytree(source, stock_project)
        shutil.copytree(source, candidate_project)
        stock_root = stock_project / item["root"]
        candidate_root = candidate_project / item["root"]
        stock_cwd = stock_root.parent
        candidate_cwd = candidate_root.parent
        cache = candidate_project / ".aitex-cache"
        env = dict(os.environ)
        env.update({
            "SOURCE_DATE_EPOCH": "1000000000",
            "FORCE_SOURCE_DATE": "1",
            "max_print_line": "1000",
            "AITEX_MAX_COMMANDS": "2000000000",
            "AITEX_MAX_STEPS": "2000000000",
            "AITEX_STACK_MB": "64",
            "LC_ALL": "C",
            "TZ": "UTC",
            "TEXMFHOME": f"!!{texmf}",
            "TEXMFDBS": f"{texmf}:{env.get('TEXMFDBS', '')}",
            # Exercise even normally sub-threshold classes in this
            # qualification. Production admission remains conservative.
            "AITEX_CLASS_CACHE_MIN_MS": "0",
        })
        stock_command = [
            "latex", "-file-line-error", "-interaction=nonstopmode",
            "-no-shell-escape", "-halt-on-error", stock_root.name,
        ]
        uncached_command = [
            str(binary), "pass", candidate_root.name, "--fmt", str(fmt),
            "-file-line-error", "-halt-on-error", "-o", "warm.dvi",
        ]
        cached_command = [
            str(binary), "pass", candidate_root.name, "--fmt", str(fmt), "--cache", str(cache),
            "-file-line-error", "-halt-on-error", "-o", "warm.dvi",
        ]
        stock_stable, stock_setup_passes, stock_setup_output = stabilize(
            stock_command, stock_cwd, env, timeout, memory_limit_mib)
        candidate_stable, candidate_setup_passes, candidate_setup_output = stabilize(
            uncached_command, candidate_cwd, env, timeout, memory_limit_mib)
        if not stock_stable or not candidate_stable:
            return {
                "id": item["id"], "status": "setup_failed",
                "stock_setup_passes": stock_setup_passes,
                "candidate_setup_passes": candidate_setup_passes,
                "error": (stock_setup_output if not stock_stable
                          else candidate_setup_output)[-1000:],
            }

        stock, stock_output = measured_pass(
            stock_command, stock_cwd, env, timeout, memory_limit_mib)
        stock_path = stock_cwd / f"{stock_root.stem}.dvi"
        stock["output"] = holdout.dvi_info(stock_path, env)
        stock["correctness"] = {
            "valid": bool(stock["returncode"] == 0 and stock["output"].get("valid"))
        }

        cold, cold_output = measured_pass(
            cached_command, candidate_cwd, env, timeout, memory_limit_mib)
        candidate_path = candidate_cwd / "warm.dvi"
        cold["output"] = holdout.dvi_info(candidate_path, env)
        cold["project_snapshot_written"] = "project snapshot written" in cold_output
        cold["project_snapshot_hit"] = "project snapshot hit" in cold_output
        cold["class_snapshot_written"] = "class snapshot written" in cold_output
        cold["class_snapshot_hit"] = "class snapshot hit" in cold_output
        cold["output_cache_written"] = "persistent output cache written" in cold_output
        cold["output_cache_hit"] = "persistent output cache hit" in cold_output
        cold["correctness"] = compact_comparison(
            holdout.compare_dvi_semantics(stock_path, candidate_path, env))

        hit, hit_output = measured_pass(
            cached_command, candidate_cwd, env, timeout, memory_limit_mib)
        hit["output"] = holdout.dvi_info(candidate_path, env)
        hit["project_snapshot_written"] = "project snapshot written" in hit_output
        hit["project_snapshot_hit"] = "project snapshot hit" in hit_output
        hit["output_cache_written"] = "persistent output cache written" in hit_output
        hit["output_cache_hit"] = "persistent output cache hit" in hit_output
        hit["correctness"] = compact_comparison(
            holdout.compare_dvi_semantics(stock_path, candidate_path, env))

        valid = bool(
            stock["correctness"]["valid"]
            and cold["returncode"] == 0
            and cold["output_cache_written"] and not cold["output_cache_hit"]
            and not cold["project_snapshot_written"]
            and not cold["project_snapshot_hit"]
            and not cold["class_snapshot_written"]
            and not cold["class_snapshot_hit"]
            and cold["correctness"]["equivalent"]
            and hit["returncode"] == 0
            and hit["output_cache_hit"] and not hit["output_cache_written"]
            and not hit["project_snapshot_written"]
            and not hit["project_snapshot_hit"]
            and hit["correctness"]["equivalent"]
        )
        return {
            "id": item["id"], "status": "valid" if valid else "invalid",
            "stock_setup_passes": stock_setup_passes,
            "candidate_setup_passes": candidate_setup_passes,
            "stock": stock, "cold": cold, "hit": hit,
        }


def ratio(a: float | None, b: float | None) -> str:
    return f"{a / b:.2f}×" if a is not None and b else "n/a"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cold", type=Path, default=holdout.RESULTS)
    parser.add_argument("--sources", type=Path, default=holdout.SOURCES)
    parser.add_argument("--binary", type=Path,
                        default=ROOT / "target" / "release" / "aitex")
    parser.add_argument("--format", type=Path,
                        default=ROOT / "target" / "performance" / "fmt.afmt")
    parser.add_argument("--texmf-overlay", type=Path, default=holdout.TEXMF_OVERLAY)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--memory-limit-mib", type=int, default=4096)
    parser.add_argument("--jobs", type=int, default=3)
    parser.add_argument("--limit", type=int, default=0,
                        help="run only the first N eligible papers (development only)")
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")

    cold = json.loads(args.cold.read_text())
    sources = json.loads(args.sources.read_text())
    binary = args.binary.resolve()
    fmt = args.format.resolve()
    if not binary.is_file():
        raise SystemExit(f"binary is absent: {binary}")
    if not fmt.is_file():
        raise SystemExit(f"LaTeX format is absent: {fmt}; run tools/mkfmt.sh")
    texmf_overlay = args.texmf_overlay.resolve()
    filename_database = texmf_overlay / "ls-R"
    if not filename_database.is_file():
        raise SystemExit(f"TeX overlay filename database is absent: {filename_database}; "
                         "run tools/texlive_overlay.py")

    by_id = {entry["id"]: entry for entry in sources["entries"]}
    selected = [
        entry for entry in cold["entries"]
        if entry["run"] and stage_ok(entry["run"]["pdftex_dvi"])
    ]
    if args.limit:
        selected = selected[:args.limit]
    def submit_arguments(entry: dict) -> tuple:
        return (by_id[entry["source"]["id"]], binary, fmt,
                texmf_overlay, args.timeout,
                args.memory_limit_mib)

    completed_by_id = {}
    if args.jobs == 1:
        iterator = ((entry, benchmark_one(*submit_arguments(entry)))
                    for entry in selected)
        for index, (entry, result) in enumerate(iterator, 1):
            completed_by_id[result["id"]] = result
            print(f"[{index:2d}/{len(selected)}] {result['id']} "
                  f"status={result['status']} "
                  f"setup={result.get('candidate_setup_passes', 0)}",
                  flush=True)
    else:
        with concurrent.futures.ProcessPoolExecutor(max_workers=args.jobs) as executor:
            futures = {
                executor.submit(benchmark_one, *submit_arguments(entry)): entry
                for entry in selected
            }
            for index, future in enumerate(concurrent.futures.as_completed(futures), 1):
                entry = futures[future]
                identity = entry["source"]["id"]
                try:
                    result = future.result()
                except Exception as error:  # preserve a complete denominator
                    result = {"id": identity, "status": "worker_error",
                              "error": repr(error)}
                completed_by_id[result["id"]] = result
                print(f"[{index:2d}/{len(selected)}] {result['id']} "
                      f"status={result['status']} "
                      f"setup={result.get('candidate_setup_passes', 0)}",
                      flush=True)
    completed = [completed_by_id[entry["source"]["id"]] for entry in selected]

    valid = [entry for entry in completed if entry["status"] == "valid"]
    result = {
        "schema": 5,
        "description": "Locked-corpus persistent LaTeX output-cache qualification",
        "created_at": holdout.utc_now(),
        "cold_results_sha256": holdout.file_sha256(args.cold),
        "sources_sha256": holdout.file_sha256(args.sources),
        "runner_sha256": holdout.file_sha256(Path(__file__)),
        "selection": "locked roots whose stock DVI route succeeded",
        "cold_compiler": cold["compiler"],
        "binary_sha256": holdout.file_sha256(binary),
        "format_sha256": holdout.file_sha256(fmt),
        "texmf_overlay": str(texmf_overlay),
        "texmf_filename_database_sha256": holdout.file_sha256(filename_database),
        "jobs": args.jobs,
        "selected": len(selected),
        "valid_output_cache_hits": len(valid),
        "entries": completed,
    }
    holdout.write_json(args.output, result)

    def values(where: str, field: str) -> list[float]:
        return [entry[where][field] for entry in valid
                if entry[where].get(field) is not None]

    hit_cpu = values("hit", "cpu_seconds")
    cold_ai_cpu = values("cold", "cpu_seconds")
    stock_cpu = values("stock", "cpu_seconds")
    hit_wall = values("hit", "seconds")
    cold_ai_wall = values("cold", "seconds")
    stock_wall = values("stock", "seconds")
    hit_rss = [entry["hit"]["max_rss_kib"] / 1024 for entry in valid]
    cold_ai_rss = [entry["cold"]["max_rss_kib"] / 1024 for entry in valid]
    stock_rss = [entry["stock"]["max_rss_kib"] / 1024 for entry in valid]
    result_path = args.output.resolve()
    try:
        result_label = result_path.relative_to(ROOT)
    except ValueError:
        result_label = result_path
    lines = [
        "# Persistent LaTeX cache qualification", "",
        "The locked 191-root holdout selects every paper whose stock-DVI route",
        "succeeded. Each selected source is stabilized afresh, then stock LaTeX,",
        "an aiTeX exact-output-cache population pass, and the resulting hit are",
        "measured against the same auxiliary state. Every aiTeX DVI stream must",
        "match the fresh stock stream exactly, and the disabled class/project",
        "snapshot layers must not be published.", "",
        f"- Eligible locked stock-DVI successes: {len(selected)}",
        f"- Valid persistent output-cache sets: {len(valid)}/{len(selected)}",
        f"- Exact converged-output hits: {len(valid)}/{len(selected)}",
        "- Class/project snapshots published: 0", "",
        "| Stabilized pass | Aggregate wall | Aggregate CPU | Median RSS |",
        "|---|---:|---:|---:|",
        (f"| Stock LaTeX | {sum(stock_wall):.2f}s | {sum(stock_cpu):.2f}s | "
         f"{statistics.median(stock_rss):.1f} MiB |"),
        (f"| aiTeX, populate document/output cache | {sum(cold_ai_wall):.2f}s | "
         f"{sum(cold_ai_cpu):.2f}s | {statistics.median(cold_ai_rss):.1f} MiB |"),
        (f"| aiTeX, converged-output cache hit | {sum(hit_wall):.2f}s | "
         f"{sum(hit_cpu):.2f}s | {statistics.median(hit_rss):.1f} MiB |"),
        "",
        f"Warm/cold aiTeX CPU ratio: {ratio(sum(hit_cpu), sum(cold_ai_cpu))}.",
        f"Warm/stock LaTeX CPU ratio: {ratio(sum(hit_cpu), sum(stock_cpu))}.", "",
        "Setup, source copying, auxiliary convergence, and DVI validation are outside",
        "the timed region. Every reported pass has an output-validity or exact-DVI",
        "oracle result. The exact per-paper results and source, runner, binary, format,",
        "and cold-selection hashes are stored in",
        f"`{result_label}`.", "",
    ]
    args.report.write_text("\n".join(lines))
    print(f"results sha256: {holdout.file_sha256(args.output)}")
    print("\n".join(lines))
    return 0 if len(valid) == len(selected) else 1


if __name__ == "__main__":
    raise SystemExit(main())
