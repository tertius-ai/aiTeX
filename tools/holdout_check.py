#!/usr/bin/env python3
"""Blocking release checks for the locked blind arXiv holdout artifact."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_RESULTS = ROOT / "target" / "qualification" / "holdout-200-current.results.json"
DEFAULT_BINARY = ROOT / "target" / "release" / "aitex"
EXPECTED_SCHEMA = 11
EXPECTED_SAMPLE_SIZE = 200
EXPECTED_READY = 191
EXPECTED_RASTER_ORACLE = {
    "pdf_raster_dpi": 72,
    "pdf_raster_max_normalized_mae": 0.02,
    "pdf_raster_fuzz_percent": 12.5,
    "pdf_raster_max_fuzz_fraction": 0.05,
}
COMPILER_SOURCE_PATHS = (
    ".cargo",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "crates/aitex-cli",
    "crates/aitex-core",
    "crates/aitex-driver",
    "crates/aitex-engine",
)

# Classic-route release policy. PDF is the primary production route and must
# remain strictly faster than stock pdfLaTeX. DVI correctness remains exact,
# while its known interpreter overhead is bounded to a 30% CPU regression.
# The DVI ceiling is debt, not the long-term target.
PDF_CPU_RATIO_LIMIT = 1.0
DVI_CPU_RATIO_LIMIT = 1.30
PDF_P95_RSS_RATIO_LIMIT = 2.0


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compiler_source_identity_error(recorded_commit: str | None) -> str | None:
    """Prove that a hash-waived hosted rebuild uses identical compiler source."""
    if not recorded_commit:
        return "results do not record the measured compiler commit"
    exists = subprocess.run(
        ["git", "cat-file", "-e", f"{recorded_commit}^{{commit}}"],
        cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        check=False,
    )
    if exists.returncode != 0:
        return f"recorded compiler commit is unavailable: {recorded_commit}"
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", recorded_commit, "HEAD"],
        cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        check=False,
    )
    if ancestor.returncode != 0:
        return f"recorded compiler commit is not an ancestor of HEAD: {recorded_commit}"
    changed = subprocess.run(
        ["git", "diff", "--quiet", recorded_commit, "HEAD", "--",
         *COMPILER_SOURCE_PATHS],
        cwd=ROOT, check=False,
    )
    if changed.returncode != 0:
        return "compiler sources differ between the measured commit and HEAD"
    untracked = subprocess.run(
        ["git", "ls-files", "--others", "--", *COMPILER_SOURCE_PATHS],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, check=False,
    )
    if untracked.returncode != 0:
        return "cannot enumerate untracked compiler inputs"
    if untracked.stdout.strip():
        first = untracked.stdout.splitlines()[0]
        return f"untracked compiler input can affect the rebuild: {first}"
    dirty = subprocess.run(
        ["git", "diff", "--quiet", "HEAD", "--", *COMPILER_SOURCE_PATHS],
        cwd=ROOT, check=False,
    )
    if dirty.returncode != 0:
        return "tracked compiler sources have uncommitted changes relative to HEAD"
    return None


def successful(stage: dict) -> bool:
    output = stage.get("output", {})
    return (stage.get("returncode") == 0 and output.get("valid", False)
            and output.get("pages", 0) > 0)


def percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    fraction = position - lower
    return ordered[lower] * (1 - fraction) + ordered[upper] * fraction


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def cpu_ratio_error(label: str, ratio: float) -> str | None:
    if label == "pdf" and ratio >= PDF_CPU_RATIO_LIMIT:
        return (f"aggregate PDF CPU ratio is {ratio:.3f}, not below stock "
                f"({PDF_CPU_RATIO_LIMIT:.2f}x)")
    if label == "dvi" and ratio > DVI_CPU_RATIO_LIMIT:
        return (f"aggregate DVI CPU ratio is {ratio:.3f}, above the "
                f"{DVI_CPU_RATIO_LIMIT:.2f}x release ceiling")
    return None


def check(results: Path, binary: Path,
          allow_rebuilt_binary: bool = False) -> tuple[list[str], dict]:
    errors: list[str] = []
    try:
        result = json.loads(results.read_text())
    except (OSError, json.JSONDecodeError) as error:
        return [f"cannot read results: {error}"], {}

    if result.get("schema") != EXPECTED_SCHEMA:
        fail(errors, f"schema is {result.get('schema')!r}, expected {EXPECTED_SCHEMA}")
    entries = result.get("entries", [])
    if len(entries) != EXPECTED_SAMPLE_SIZE:
        fail(errors, f"sample contains {len(entries)} entries, expected {EXPECTED_SAMPLE_SIZE}")
    ready = [item for item in entries
             if item.get("source", {}).get("status") == "ready"]
    if len(ready) != EXPECTED_READY:
        fail(errors, f"ready set contains {len(ready)} sources, expected {EXPECTED_READY}")

    hash_inputs = {
        "sample_sha256": ROOT / "qualification" / "holdout-200.sample.json",
        "sources_sha256": ROOT / "qualification" / "holdout-200.sources.json",
        "environment_sha256": ROOT / "qualification" / "environment.lock.json",
        "runner_sha256": ROOT / "tools" / "arxiv_holdout.py",
    }
    for field, path in hash_inputs.items():
        if not path.is_file():
            fail(errors, f"locked input is absent: {path.relative_to(ROOT)}")
        elif result.get(field) != sha256(path):
            fail(errors, f"{field} does not match {path.relative_to(ROOT)}")

    compiler = result.get("compiler", {})
    if not binary.is_file():
        fail(errors, f"release binary is absent: {binary}")
    else:
        version = subprocess.run(
            [str(binary), "--version"], stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, text=True, check=False,
        ).stdout.strip().removeprefix("aitex ")
        if compiler.get("version") != version:
            fail(errors, f"compiler version is {compiler.get('version')!r}, expected {version!r}")
        binary_matches = compiler.get("binary_sha256") == sha256(binary)
        if not binary_matches and not allow_rebuilt_binary:
            fail(errors, "results were produced by a different release binary")
        elif (not binary_matches and allow_rebuilt_binary
              and (identity_error := compiler_source_identity_error(
                  compiler.get("git_commit")))):
            fail(errors, f"hosted rebuild identity is unproven: {identity_error}")

    run_metadata = result.get("run", {})
    if run_metadata.get("measurement_schema") != EXPECTED_SCHEMA:
        fail(errors, "run measurement schema is stale")
    if run_metadata.get("passes") != 2:
        fail(errors, "holdout did not use exactly two compiler passes")
    affinity = run_metadata.get("effective_cpu_affinity")
    if (
        not isinstance(affinity, list)
        or not affinity
        or any(not isinstance(cpu, int) or cpu < 0 for cpu in affinity)
        or affinity != sorted(set(affinity))
    ):
        fail(errors, "holdout does not record a valid effective CPU affinity")
    benchmark_affinity = result.get("benchmark_environment", {}).get(
        "effective_cpu_affinity"
    )
    if benchmark_affinity != affinity:
        fail(errors, "holdout CPU-affinity records disagree")
    oracle = run_metadata.get("artifact_oracle", {})
    for field, expected in EXPECTED_RASTER_ORACLE.items():
        if oracle.get(field) != expected:
            fail(errors, f"locked oracle {field} is {oracle.get(field)!r}, expected {expected!r}")
    overlay = ROOT / "qualification" / "texlive-overlay.lock.json"
    if (not overlay.is_file()
            or run_metadata.get("texlive_overlay_lock_sha256") != sha256(overlay)):
        fail(errors, "TeX Live overlay lock hash does not match")
    filename_database = (ROOT / "corpus" / "texlive-overlay" / "root" / "usr" /
                         "share" / "texlive" / "texmf-dist" / "ls-R")
    if (not filename_database.is_file()
            or run_metadata.get("texmf_filename_database_sha256")
            != sha256(filename_database)):
        fail(errors, "TeX Live overlay filename-database hash does not match")
    pk_lock_path = ROOT / "qualification" / "texlive-overlay.pk.json"
    try:
        pk_lock = json.loads(pk_lock_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(errors, f"cannot read locked PK inventory: {error}")
    else:
        expected_pk: set[Path] = set()
        texmf = filename_database.parent
        for item in pk_lock.get("files", []):
            relative = Path(item.get("path", ""))
            expected_pk.add(relative)
            path = texmf / relative
            if not path.is_file() or sha256(path) != item.get("sha256"):
                fail(errors, f"locked PK bitmap differs: {relative}")
        pk_root = texmf / "fonts" / "pk"
        actual_pk = ({path.relative_to(texmf) for path in pk_root.rglob("*pk")
                      if path.is_file()} if pk_root.is_dir() else set())
        if actual_pk != expected_pk:
            fail(errors, "TeX Live overlay PK inventory does not match")

    evaluated: list[dict] = []
    for item in ready:
        identity = item.get("source", {}).get("id", "unknown")
        run = item.get("run")
        if not run:
            fail(errors, f"{identity}: not evaluated")
        elif "worker_error" in run:
            fail(errors, f"{identity}: worker error: {run['worker_error']}")
        else:
            evaluated.append(item)
    if len(evaluated) != EXPECTED_READY:
        fail(errors, f"evaluated {len(evaluated)}/{EXPECTED_READY} ready sources")

    ref_pdf: list[dict] = []
    ref_dvi: list[dict] = []
    for item in evaluated:
        identity = item["source"]["id"]
        run = item["run"]
        if successful(run["pdftex_pdf"]):
            ref_pdf.append(item)
            if not successful(run["aitex_pdf"]):
                fail(errors, f"{identity}: aiTeX PDF failed where pdfLaTeX succeeded")
                continue
            if not run.get("pdf_page_match", False):
                fail(errors, f"{identity}: PDF page count differs")
            raster = run.get("pdf_raster", {})
            if (raster.get("comparable", True)
                    and not raster.get("equivalent", False)):
                fail(errors, f"{identity}: rendered PDF is outside the locked tolerance")
            if run.get("aitex_draft_eps_assets"):
                fail(errors, f"{identity}: aiTeX PDF substituted an EPS draft placeholder")
            if run.get("pdf_text_quality_no_worse") is not True:
                fail(errors, f"{identity}: PDF text has more invalid code points than stock")
            # Raw equality with stock is diagnostic, not a correctness gate.
            # pdfTeX often emits replacement, control, or private-use code
            # points for math and legacy encodings; repairing those mappings
            # necessarily lowers string similarity. Page raster parity and DVI
            # drawing parity grade visible content, while the objective PDF
            # text requirement is that invalid Unicode does not increase.

        if successful(run["pdftex_dvi"]):
            ref_dvi.append(item)
            if not successful(run["aitex_dvi"]):
                fail(errors, f"{identity}: aiTeX DVI failed where stock LaTeX succeeded")
                continue
            semantic = run.get("dvi_semantics", {})
            if not semantic.get("valid", False):
                fail(errors, f"{identity}: DVI semantic comparison is invalid")
            if not semantic.get("drawing_equivalent", False):
                fail(errors, f"{identity}: positioned DVI drawing differs")
            if not semantic.get("specials_equivalent", False):
                fail(errors, f"{identity}: normalized DVI driver-special stream differs")
            if not semantic.get("equivalent", False):
                fail(errors, f"{identity}: combined DVI semantics differ")

    if not ref_pdf:
        fail(errors, "no stock PDF successes were examined")
    if not ref_dvi:
        fail(errors, "no stock DVI successes were examined")

    ratios: dict[str, float | int] = {
        "ready": len(ready),
        "evaluated": len(evaluated),
        "paired_pdf": len(ref_pdf),
        "paired_dvi": len(ref_dvi),
        "binary_identity_waived": bool(
            binary.is_file()
            and compiler.get("binary_sha256") != sha256(binary)
            and allow_rebuilt_binary
        ),
    }
    for label, pairs, reference, candidate in (
        ("pdf", ref_pdf, "pdftex_pdf", "aitex_pdf"),
        ("dvi", ref_dvi, "pdftex_dvi", "aitex_dvi"),
    ):
        successful_pairs = [item for item in pairs
                            if successful(item["run"][candidate])]
        ref_cpu = sum(item["run"][reference].get("cpu_seconds") or 0
                      for item in successful_pairs)
        our_cpu = sum(item["run"][candidate].get("cpu_seconds") or 0
                      for item in successful_pairs)
        ratio = our_cpu / ref_cpu if ref_cpu else float("inf")
        ratios[f"{label}_cpu_ratio"] = ratio
        if message := cpu_ratio_error(label, ratio):
            fail(errors, message)

    memory_pairs = [item for item in ref_pdf if successful(item["run"]["aitex_pdf"])
                    and item["run"]["pdftex_pdf"].get("peak_rss_kib") is not None
                    and item["run"]["aitex_pdf"].get("peak_rss_kib") is not None]
    if memory_pairs:
        ref_memory = [item["run"]["pdftex_pdf"]["peak_rss_kib"]
                      for item in memory_pairs]
        our_memory = [item["run"]["aitex_pdf"]["peak_rss_kib"]
                      for item in memory_pairs]
        memory_ratio = percentile(our_memory, 0.95) / percentile(ref_memory, 0.95)
        ratios["pdf_p95_rss_ratio"] = memory_ratio
        if memory_ratio > PDF_P95_RSS_RATIO_LIMIT:
            fail(errors, f"PDF p95 RSS ratio is {memory_ratio:.3f}, above the "
                 f"{PDF_P95_RSS_RATIO_LIMIT:.1f} limit")
    else:
        fail(errors, "no paired PDF memory measurements were recorded")

    return errors, ratios


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--results", type=Path, default=DEFAULT_RESULTS)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument(
        "--allow-rebuilt-binary", action="store_true",
        help=("allow a different executable hash only when git proves the "
              "measured compiler source is unchanged at HEAD; all functional "
              "and locked-artifact checks remain strict"),
    )
    args = parser.parse_args()
    errors, metrics = check(
        args.results.resolve(), args.binary.resolve(), args.allow_rebuilt_binary,
    )
    if errors:
        print(f"holdout: FAIL ({len(errors)} blocking issue(s))")
        for message in errors[:20]:
            print(f"  - {message}")
        if len(errors) > 20:
            print(f"  - ... and {len(errors) - 20} more")
        return 1
    rebuilt = ("; hosted rebuild hash accepted"
               if metrics["binary_identity_waived"] else "")
    print(
        "holdout: PASS "
        f"{metrics['evaluated']}/{metrics['ready']} evaluated; "
        f"PDF {metrics['paired_pdf']} conditional, {metrics['pdf_cpu_ratio']:.3f}x CPU, "
        f"{metrics['pdf_p95_rss_ratio']:.3f}x p95 RSS; "
        f"DVI {metrics['paired_dvi']} conditional, {metrics['dvi_cpu_ratio']:.3f}x CPU "
        f"(release ceiling {DVI_CPU_RATIO_LIMIT:.2f}x){rebuilt}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
