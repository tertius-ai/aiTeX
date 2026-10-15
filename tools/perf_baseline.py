#!/usr/bin/env python3
"""Versioned aiTeX edit-loop and compiler performance benchmark.

Every timed compiler invocation validates its output. Candidate DVI is checked
against stock LaTeX's normalized drawing stream; final PDF is checked for page
count, extracted text, and a locked raster tolerance. The larger 191-paper
warm experiment remains in ``latex_warm.py``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import re
import shutil
import statistics
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path

sys.dont_write_bytecode = True
import arxiv_holdout as holdout


ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "tests" / "perf"
DEFAULT_BUILD_FIXTURE = ROOT / "tests" / "build"
DEFAULT_BINARY = ROOT / "target" / "release" / "aitex"
DEFAULT_FORMAT = ROOT / "target" / "performance" / "fmt.afmt"
DEFAULT_RESULT = ROOT / "qualification" / "performance-baseline-v1.json"
METRICS_PREFIX = "AITEX_METRICS "
SCHEMA = 1


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tree_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    for item in sorted(value for value in path.rglob("*") if value.is_file()):
        digest.update(item.relative_to(path).as_posix().encode())
        digest.update(b"\0")
        digest.update(item.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def command_line(argv: list[str]) -> str | None:
    try:
        result = subprocess.run(argv, check=True, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True)
    except (OSError, subprocess.CalledProcessError):
        return None
    return result.stdout.splitlines()[0] if result.stdout else ""


def png_chunk(kind: bytes, body: bytes) -> bytes:
    return (struct.pack(">I", len(body)) + kind + body
            + struct.pack(">I", zlib.crc32(kind + body) & 0xffffffff))


def write_png(path: Path, rgb: tuple[int, int, int]) -> None:
    width, height = 96, 64
    rows = b"".join(b"\0" + bytes(rgb) * width for _ in range(height))
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", header)
        + png_chunk(b"IDAT", zlib.compress(rows, 9))
        + png_chunk(b"IEND", b"")
    )


def stage_fixture(destination: Path) -> None:
    shutil.copytree(FIXTURE, destination)
    write_png(destination / "figure.png", (31, 107, 199))


def stage_default_build_fixture(destination: Path) -> None:
    shutil.copytree(DEFAULT_BUILD_FIXTURE, destination)


def mutate_default_build(project: Path, edit: str) -> None:
    if edit in ("clean", "unchanged"):
        return
    if edit == "source":
        path = project / "main.tex"
        path.write_text(path.read_text().replace(
            "Section~\\ref{sec:native} cites Knuth",
            "The section at~\\ref{sec:native} cites Knuth"))
    elif edit == "bibliography":
        path = project / "refs.bib"
        path.write_text(path.read_text().replace("year = {1984}", "year = {1985}"))
    elif edit == "index":
        path = project / "main.tex"
        path.write_text(path.read_text().replace(
            "\\index{native scheduler}", "\\index{native build scheduler}"))
    else:
        raise ValueError(f"unknown default-build edit: {edit}")


def expand_incremental_fixture(project: Path, paragraphs: int = 1200) -> None:
    source = project / "main.tex"
    added = []
    for number in range(1, paragraphs + 1):
        marker = (" PERFORMANCE-INCREMENTAL-LATE-BASELINE"
                  if number == paragraphs - 100 else "")
        added.append(
            f"Incremental paragraph {number}{marker} carries deterministic prose "
            "through line breaking and page construction for the P3 edit loop.\n\n")
    text = source.read_text()
    source.write_text(text.replace(
        "\\bibliographystyle{plain}", "".join(added) + "\\bibliographystyle{plain}"))


def mutate(project: Path, edit: str) -> None:
    if edit == "no_op":
        return
    if edit == "paragraph":
        path = project / "main.tex"
        path.write_text(path.read_text().replace(
            "PERFORMANCE-PARAGRAPH-BASELINE",
            "PERFORMANCE-PARAGRAPH-EDITED"))
    elif edit == "late_paragraph":
        path = project / "main.tex"
        text = path.read_text()
        if "PERFORMANCE-INCREMENTAL-LATE-BASELINE" in text:
            text = text.replace(
                "PERFORMANCE-INCREMENTAL-LATE-BASELINE",
                "PERFORMANCE-INCREMENTAL-LATE-EDITED")
        else:
            text = text.replace(
                "makes image invalidation observable.",
                "makes late paragraph edits observable.")
        path.write_text(text)
    elif edit == "preamble":
        path = project / "main.tex"
        path.write_text(path.read_text().replace(
            "\\newcommand{\\benchmarkword}{baseline}",
            "\\newcommand{\\benchmarkword}{edited}"))
    elif edit == "included_file":
        path = project / "section.tex"
        path.write_text(path.read_text().replace(
            "PERFORMANCE-INCLUDE-BASELINE",
            "PERFORMANCE-INCLUDE-EDITED"))
    elif edit == "bibliography":
        path = project / "refs.bib"
        path.write_text(path.read_text().replace(
            "PERFORMANCE-BIBLIOGRAPHY-BASELINE",
            "PERFORMANCE-BIBLIOGRAPHY-EDITED"))
    elif edit == "image":
        write_png(project / "figure.png", (190, 58, 46))
        path = project / "figure.eps"
        path.write_text(path.read_text().replace(
            "0.12 0.42 0.78 setrgbcolor",
            "0.75 0.23 0.18 setrgbcolor"))
    else:
        raise ValueError(f"unknown edit: {edit}")


def benchmark_env() -> dict[str, str]:
    env = dict(os.environ)
    env.update({
        "SOURCE_DATE_EPOCH": "1000000000",
        "FORCE_SOURCE_DATE": "1",
        "LC_ALL": "C",
        "TZ": "UTC",
        "max_print_line": "1000",
        "AITEX_MAX_COMMANDS": "2000000000",
        "AITEX_MAX_STEPS": "2000000000",
        "AITEX_STACK_MB": "64",
        "AITEX_METRICS": "1",
    })
    return env


def parse_internal(output: str) -> dict | None:
    for line in output.splitlines():
        if line.startswith(METRICS_PREFIX):
            try:
                return json.loads(line[len(METRICS_PREFIX):])
            except json.JSONDecodeError:
                return None
    return None


def cache_is_primed(state: str | None) -> bool:
    """A reusable project state may be served as a snapshot or exact output."""
    return state in ("hit", "output-hit")


def expected_edit_cache(edit: str) -> str:
    if edit == "no_op":
        return "output-hit"
    return "miss"


def measured(argv: list[str], cwd: Path, env: dict[str, str],
             timeout: int) -> tuple[dict, str]:
    code, wall, output, timed_out, usage = holdout.run_command(
        argv, cwd, env, timeout, 4096)
    user = usage.get("user_seconds")
    system = usage.get("system_seconds")
    cpu = user + system if user is not None and system is not None else None
    item = {
        "returncode": code,
        "wall_seconds": round(wall, 6),
        "cpu_seconds": round(cpu, 6) if cpu is not None else None,
        "max_rss_kib": usage.get("max_rss_kib"),
        "filesystem_read_bytes": usage.get("filesystem_read_bytes"),
        "filesystem_write_bytes": usage.get("filesystem_write_bytes"),
        "timed_out": timed_out,
    }
    internal = parse_internal(output)
    if internal is not None:
        item["aitex"] = internal
        item["startup_overhead_ms"] = round(max(
            0.0, wall * 1000 - internal.get("internal_total_ms", 0.0)), 3)
    return item, output


def tex_command(engine: str, route: str, binary: Path, fmt: Path,
                cache: Path | None = None, root: str = "main.tex",
                extra: list[str] | None = None) -> list[str]:
    if engine == "aitex":
        command = [str(binary), "pass", root, "--fmt", str(fmt),
                   "-file-line-error", "-halt-on-error", "-o",
                   f"{Path(root).stem}.{route}"]
        if cache is not None:
            command += ["--cache", str(cache)]
        if extra is not None:
            command += extra
        return command
    executable = "latex" if route == "dvi" else "pdflatex"
    return [executable, "-interaction=nonstopmode", "-halt-on-error", root]


def latexmk_command(route: str, root: str = "main.tex") -> list[str]:
    output_mode = "-dvi" if route == "dvi" else "-pdf"
    return ["latexmk", output_mode, "-interaction=nonstopmode",
            "-halt-on-error", "-file-line-error", root]


def aux_fingerprint(project: Path) -> str:
    digest = hashlib.sha256()
    for suffix in ("aux", "bbl", "toc", "out", "lof", "lot"):
        path = project / f"main.{suffix}"
        digest.update(suffix.encode())
        digest.update(b"\0")
        if path.is_file():
            digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def build_fingerprint(project: Path) -> str:
    digest = hashlib.sha256()
    for suffix in ("aux", "bbl", "toc", "out", "lof", "lot", "idx", "ind"):
        path = project / f"main.{suffix}"
        digest.update(suffix.encode())
        digest.update(b"\0")
        if path.is_file():
            digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def aggregate(commands: list[dict]) -> dict:
    result: dict[str, object] = {"commands": commands}
    for field in ("wall_seconds", "cpu_seconds", "filesystem_read_bytes",
                  "filesystem_write_bytes"):
        values = [item[field] for item in commands if item.get(field) is not None]
        result[field] = round(sum(values), 6) if values else None
    rss = [item["max_rss_kib"] for item in commands
           if item.get("max_rss_kib") is not None]
    result["max_rss_kib"] = max(rss) if rss else None
    result["returncode"] = next(
        (item["returncode"] for item in commands if item["returncode"] != 0), 0)
    result["timed_out"] = any(item["timed_out"] for item in commands)
    return result


def converge(engine: str, route: str, project: Path, binary: Path, fmt: Path,
             env: dict[str, str], timeout: int, cache: Path | None = None,
             force_bibtex: bool = False) -> tuple[dict, str]:
    commands: list[dict] = []
    output = ""
    last_state = None
    bibtex_ran = False
    tex_passes = 0
    for _ in range(8):
        item, output = measured(
            tex_command(engine, route, binary, fmt, cache),
            project, env, timeout)
        item["kind"] = "tex"
        commands.append(item)
        tex_passes += 1
        if item["returncode"] != 0:
            break
        aux = project / "main.aux"
        needs_bibtex = aux.is_file() and "\\bibdata" in aux.read_text(errors="replace")
        if needs_bibtex and (not bibtex_ran or force_bibtex):
            bib, _ = measured(["bibtex", "main"], project, env, timeout)
            bib["kind"] = "bibtex"
            commands.append(bib)
            bibtex_ran = True
            force_bibtex = False
            last_state = None
            if bib["returncode"] != 0:
                break
            continue
        state = aux_fingerprint(project)
        if last_state == state and tex_passes >= 2:
            break
        last_state = state
    result = aggregate(commands)
    result["tex_passes"] = tex_passes
    result["bibtex_passes"] = sum(item.get("kind") == "bibtex" for item in commands)
    result["converged"] = (result["returncode"] == 0 and last_state is not None
                           and aux_fingerprint(project) == last_state)
    return result, output


def stock_default_build(project: Path, env: dict[str, str], timeout: int,
                        force_bibtex: bool = False) -> tuple[dict, str]:
    """Complete the versioned build fixture using classical TeX programs."""
    commands: list[dict] = []
    output = ""
    before = build_fingerprint(project)
    converged = False
    tex_passes = 0
    for _ in range(8):
        index_before = ((project / "main.idx").read_bytes()
                        if (project / "main.idx").is_file() else None)
        item, output = measured(
            ["pdflatex", "-interaction=nonstopmode", "-halt-on-error", "main.tex"],
            project, env, timeout)
        item["kind"] = "tex"
        commands.append(item)
        tex_passes += 1
        if item["returncode"] != 0:
            break

        tool_ran = False
        aux = project / "main.aux"
        needs_bibtex = (aux.is_file()
                        and "\\bibdata" in aux.read_text(errors="replace"))
        if needs_bibtex and (force_bibtex or not (project / "main.bbl").is_file()):
            bibtex, _ = measured(["bibtex", "main"], project, env, timeout)
            bibtex["kind"] = "bibtex"
            commands.append(bibtex)
            force_bibtex = False
            tool_ran = True
            if bibtex["returncode"] != 0:
                break

        index = project / "main.idx"
        index_after = index.read_bytes() if index.is_file() else None
        if index_after is not None and (
                not (project / "main.ind").is_file() or index_after != index_before):
            makeindex, _ = measured(["makeindex", "main.idx"], project, env, timeout)
            makeindex["kind"] = "makeindex"
            commands.append(makeindex)
            tool_ran = True
            if makeindex["returncode"] != 0:
                break

        after = build_fingerprint(project)
        if not tool_ran and after == before:
            converged = True
            break
        before = after

    result = aggregate(commands)
    result["tex_passes"] = tex_passes
    result["bibtex_passes"] = sum(
        item.get("kind") == "bibtex" for item in commands)
    result["makeindex_passes"] = sum(
        item.get("kind") == "makeindex" for item in commands)
    result["converged"] = converged
    return result, output


def default_build_command(binary: Path, fmt: Path) -> list[str]:
    # No `build`, `-o`, or `--cache`: this is deliberately the ordinary CLI.
    return [str(binary), "main.tex", "--fmt", str(fmt),
            "-file-line-error", "-halt-on-error", "--no-banner"]


def default_build_activity(sample: dict, output: str) -> None:
    sample["tex_passes"] = output.count("aitex build: TeX pass")
    sample["bibtex_passes"] = output.count("aitex build: running BibTeX")
    sample["makeindex_passes"] = output.count("aitex build: running MakeIndex")
    sample["output_cache_hits"] = output.count("persistent output cache hit")
    sample["converged"] = "aitex build: converged after" in output


def default_build_sample(base: Path, scenario: str, binary: Path, fmt: Path,
                         env: dict[str, str], timeout: int) -> tuple[dict, dict]:
    reference = base / "reference"
    candidate = base / "candidate"
    stage_default_build_fixture(reference)
    stage_default_build_fixture(candidate)
    candidate_env = dict(env)
    candidate_env["XDG_CACHE_HOME"] = str((base / "cache-home").resolve())

    if scenario != "clean":
        prepared, output = stock_default_build(reference, env, timeout)
        if prepared["returncode"] != 0 or not prepared["converged"]:
            raise RuntimeError(f"cannot prepare stock default build:\n{output[-3000:]}")
        prepared_ours, output = measured(
            default_build_command(binary, fmt), candidate, candidate_env, timeout)
        require_sample(prepared_ours, output, "prepare aitex default build")
        default_build_activity(prepared_ours, output)
        if not prepared_ours["converged"]:
            raise RuntimeError(f"aitex preparation did not converge:\n{output[-3000:]}")
        mutate_default_build(reference, scenario)
        mutate_default_build(candidate, scenario)

    stock, stock_output = stock_default_build(
        reference, env, timeout, force_bibtex=scenario == "bibliography")
    ours, our_output = measured(
        default_build_command(binary, fmt), candidate, candidate_env, timeout)
    require_sample(stock, stock_output, f"stock default build {scenario}")
    require_sample(ours, our_output, f"aitex default build {scenario}")
    default_build_activity(ours, our_output)
    if not stock["converged"] or not ours["converged"]:
        raise RuntimeError(
            f"default build did not converge: stock={stock} aitex={ours}")
    check = correctness(reference / "main.pdf", candidate / "main.pdf", "pdf",
                        base / "correctness", env)
    if not check["equivalent"]:
        raise RuntimeError(f"default-build correctness failed: {check}")
    stock["correctness"] = check
    ours["correctness"] = check
    ours["implicit_cache_root"] = str(base / "cache-home" / "aitex")
    return stock, ours


def prime_cache(project: Path, cache: Path, binary: Path, fmt: Path,
                route: str, env: dict[str, str], timeout: int) -> None:
    for _ in range(4):
        item, output = measured(
            tex_command("aitex", route, binary, fmt, cache),
            project, env, timeout)
        if item["returncode"] != 0:
            raise RuntimeError(f"cannot prime cache in {project}:\n{output[-2000:]}")
        if cache_is_primed((item.get("aitex") or {}).get("cache")):
            return
    raise RuntimeError(f"cache did not stabilize in {project}")


def output_path(project: Path, route: str, root: str = "main.tex") -> Path:
    return project / f"{Path(root).stem}.{route}"


def correctness(reference: Path, candidate: Path, route: str,
                scratch: Path, env: dict[str, str]) -> dict:
    if route == "dvi":
        result = holdout.compare_dvi_semantics(reference, candidate, env)
        return {
            "valid": result["valid"],
            "equivalent": result["equivalent"],
            "drawing_equivalent": result["drawing_equivalent"],
            "specials_equivalent": result["specials_equivalent"],
            "reference_pages": result["reference"].get("pages"),
            "candidate_pages": result["candidate"].get("pages"),
            **({"first_difference": result["first_difference"]}
               if "first_difference" in result else {}),
        }
    ref_info = holdout.pdf_info(reference)
    our_info = holdout.pdf_info(candidate)
    ref_text = holdout.extracted_text(reference)
    our_text = holdout.extracted_text(candidate)
    scratch.mkdir()
    raster = holdout.compare_pdf_rasters(reference, candidate, scratch)
    valid = bool(ref_info["valid"] and our_info["valid"] and raster["valid"])
    equivalent = bool(valid and ref_info["pages"] == our_info["pages"]
                      and ref_text is not None and ref_text == our_text
                      and raster["equivalent"])
    return {
        "valid": valid,
        "equivalent": equivalent,
        "text_equivalent": ref_text is not None and ref_text == our_text,
        "reference_pages": ref_info["pages"],
        "candidate_pages": our_info["pages"],
        "raster_equivalent": raster["equivalent"],
        "worst_normalized_mae": raster.get("worst_normalized_mae"),
        "worst_fuzz_fraction": raster.get("worst_fuzz_fraction"),
        **({"error": raster["error"]} if "error" in raster else {}),
    }


def require_sample(sample: dict, output: str, label: str) -> None:
    if sample["returncode"] != 0 or sample["timed_out"]:
        raise RuntimeError(f"{label} failed:\n{output[-3000:]}")


def prepared_pair(base: Path, edit: str, route: str, binary: Path, fmt: Path,
                  env: dict[str, str], timeout: int,
                  cache_mode: str) -> tuple[Path, Path, Path | None]:
    reference = base / "reference"
    candidate = base / "candidate"
    stage_fixture(reference)
    stage_fixture(candidate)
    for engine, project in (("stock", reference), ("aitex", candidate)):
        result, output = converge(engine, route, project, binary, fmt, env, timeout)
        if result["returncode"] != 0 or not result["converged"]:
            raise RuntimeError(f"cannot prepare {engine} {route}:\n{output[-3000:]}")
    cache = candidate / ".aitex-cache" if cache_mode in ("miss", "hit") else None
    if cache_mode == "hit":
        assert cache is not None
        prime_cache(candidate, cache, binary, fmt, route, env, timeout)
    mutate(reference, edit)
    mutate(candidate, edit)
    return reference, candidate, cache


def measure_pair_sample(base: Path, edit: str, route: str, binary: Path,
                        fmt: Path, env: dict[str, str], timeout: int,
                        cache_mode: str) -> tuple[dict, dict]:
    reference, candidate, cache = prepared_pair(
        base, edit, route, binary, fmt, env, timeout, cache_mode)
    stock, stock_output = measured(
        tex_command("stock", route, binary, fmt), reference, env, timeout)
    require_sample(stock, stock_output, f"stock {route} {edit}")
    ours, our_output = measured(
        tex_command("aitex", route, binary, fmt, cache), candidate, env, timeout)
    require_sample(ours, our_output, f"aitex {route} {edit}")
    check = correctness(output_path(reference, route), output_path(candidate, route),
                        route, base / "correctness", env)
    if not check["equivalent"]:
        raise RuntimeError(f"correctness failed for {route} {edit}: {check}")
    stock["correctness"] = check
    ours["correctness"] = check
    if cache_mode == "hit":
        expected = expected_edit_cache(edit)
        got = (ours.get("aitex") or {}).get("cache")
        if got != expected:
            raise RuntimeError(f"{edit}: expected cache {expected}, got {got}")
    if cache_mode == "miss" and (ours.get("aitex") or {}).get("cache") != "miss":
        raise RuntimeError("cache-miss scenario did not report a miss")
    return stock, ours


def class_cache_sample(base: Path, route: str, binary: Path, fmt: Path,
                       env: dict[str, str], timeout: int) -> tuple[dict, dict, dict]:
    """Time a shared class hit without permitting a project snapshot hit."""
    reference = base / "reference"
    uncached = base / "uncached"
    candidate = base / "candidate"
    seed = base / "seed"
    for project in (reference, uncached, candidate, seed):
        stage_fixture(project)
        source = project / "main.tex"
        source.write_text(source.read_text().replace(
            "\\documentclass{article}", "\\documentclass{scrartcl}"))
    for engine, project in (("stock", reference), ("aitex", uncached),
                            ("aitex", candidate)):
        ready, output = converge(engine, route, project, binary, fmt, env, timeout)
        if ready["returncode"] != 0 or not ready["converged"]:
            raise RuntimeError(f"cannot prepare class-cache {engine}:\n{output[-3000:]}")

    cache = base / "shared-cache"
    prime_command = [str(binary), "cache", "warm", "main.tex", "--fmt", str(fmt),
                     "--cache", str(cache), "-file-line-error", "-halt-on-error",
                     "-o", f"main.{route}", "--no-project-cache"]
    prime, prime_output = measured(prime_command, seed, env, timeout)
    require_sample(prime, prime_output, f"class-cache seed {route}")
    if (prime.get("aitex") or {}).get("class_cache") != "miss":
        raise RuntimeError("class-cache seed did not report a miss")

    stock, stock_output = measured(
        tex_command("stock", route, binary, fmt), reference, env, timeout)
    baseline, baseline_output = measured(
        tex_command("aitex", route, binary, fmt), uncached, env, timeout)
    ours, our_output = measured(
        tex_command("aitex", route, binary, fmt, cache,
                    extra=["--no-project-cache"]), candidate, env, timeout)
    require_sample(stock, stock_output, f"stock class-cache {route}")
    require_sample(baseline, baseline_output, f"uncached class baseline {route}")
    require_sample(ours, our_output, f"aitex class-cache {route}")
    if (ours.get("aitex") or {}).get("class_cache") != "hit":
        raise RuntimeError("shared document-class scenario did not report a hit")
    check = correctness(output_path(reference, route), output_path(candidate, route),
                        route, base / "correctness", env)
    if not check["equivalent"]:
        raise RuntimeError(f"class-cache correctness failed for {route}: {check}")
    baseline_check = correctness(
        output_path(reference, route), output_path(uncached, route), route,
        base / "baseline-correctness", env)
    if not baseline_check["equivalent"]:
        raise RuntimeError(
            f"uncached class baseline correctness failed for {route}: {baseline_check}")
    stock["correctness"] = check
    baseline["correctness"] = baseline_check
    ours["correctness"] = check
    return stock, baseline, ours


def process_tree(pid: int) -> list[int]:
    """Best-effort Linux process tree for request-scoped daemon accounting."""
    result, pending = [], [pid]
    while pending:
        current = pending.pop()
        if current in result:
            continue
        result.append(current)
        children = Path(f"/proc/{current}/task/{current}/children")
        try:
            pending.extend(int(value) for value in children.read_text().split())
        except (OSError, ValueError):
            pass
    return result


def process_counters(pid: int) -> dict[str, int | None]:
    ticks = 0
    read_bytes = 0
    write_bytes = 0
    rss_kib = 0
    measured_ticks = False
    for child in process_tree(pid):
        try:
            fields = Path(f"/proc/{child}/stat").read_text().split()
            ticks += int(fields[13]) + int(fields[14])
            measured_ticks = True
        except (OSError, ValueError, IndexError):
            pass
        try:
            io = dict(line.split(":", 1) for line in
                      Path(f"/proc/{child}/io").read_text().splitlines())
            read_bytes += int(io.get("read_bytes", "0"))
            write_bytes += int(io.get("write_bytes", "0"))
        except (OSError, ValueError):
            pass
        try:
            status = Path(f"/proc/{child}/status").read_text()
            match = re.search(r"^VmRSS:\s+(\d+) kB$", status, re.M)
            rss_kib += int(match.group(1)) if match else 0
        except OSError:
            pass
    return {
        "ticks": ticks if measured_ticks else None,
        "read_bytes": read_bytes,
        "write_bytes": write_bytes,
        "rss_kib": rss_kib or None,
    }


def watch_frame(process: subprocess.Popen[str], request: bool,
                timeout: int) -> tuple[dict, str]:
    before = process_counters(process.pid)
    started = time.monotonic()
    if request:
        assert process.stdin is not None
        process.stdin.write("build\n")
        process.stdin.flush()
    transcript: list[str] = []
    internal = None
    deadline = started + timeout
    assert process.stdout is not None
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if not line:
            if process.poll() is not None:
                raise RuntimeError("aitex watch exited before a ready frame")
            continue
        transcript.append(line)
        if line.startswith(METRICS_PREFIX):
            internal = parse_internal(line)
        if line.startswith(("AITEX_WATCH ready ", "AITEX_FORK_WATCH ready ")):
            match = re.search(r"sequence=(\d+) code=(\d+) duration_ms=([0-9.]+)", line)
            if match is None:
                raise RuntimeError(f"invalid watch frame: {line.rstrip()}")
            elapsed = time.monotonic() - started
            after = process_counters(process.pid)
            tick_rate = os.sysconf("SC_CLK_TCK") if before["ticks"] is not None else None
            cpu = ((after["ticks"] - before["ticks"]) / tick_rate
                   if tick_rate and after["ticks"] is not None else None)
            child_user = re.search(r"child_user_ms=([0-9.]+)", line)
            child_system = re.search(r"child_system_ms=([0-9.]+)", line)
            child_rss = re.search(r"child_max_rss_kib=(\d+)", line)
            child_read = re.search(r"child_read_bytes=(\d+)", line)
            child_write = re.search(r"child_write_bytes=(\d+)", line)
            if cpu is not None and child_user is not None and child_system is not None:
                cpu += (float(child_user.group(1)) + float(child_system.group(1))) / 1000
            rss_kib = after["rss_kib"]
            if rss_kib is not None and child_rss is not None:
                rss_kib += int(child_rss.group(1))
            read_bytes = max(
                0, int(after["read_bytes"] or 0) - int(before["read_bytes"] or 0))
            write_bytes = max(
                0, int(after["write_bytes"] or 0) - int(before["write_bytes"] or 0))
            if child_read is not None:
                read_bytes += int(child_read.group(1))
            if child_write is not None:
                write_bytes += int(child_write.group(1))
            sample = {
                "returncode": int(match.group(2)),
                "wall_seconds": round(elapsed, 6),
                "cpu_seconds": round(cpu, 6) if cpu is not None else None,
                "max_rss_kib": rss_kib,
                "filesystem_read_bytes": read_bytes,
                "filesystem_write_bytes": write_bytes,
                "timed_out": False,
                "watch_sequence": int(match.group(1)),
                "watch_duration_ms": float(match.group(3)),
            }
            if internal is not None:
                sample["aitex"] = internal
                sample["startup_overhead_ms"] = round(max(
                    0.0, elapsed * 1000 - internal.get("internal_total_ms", 0.0)), 3)
            return sample, "".join(transcript)
    process.kill()
    raise RuntimeError("aitex watch request timed out")


def watch_sample(base: Path, edit: str, route: str, binary: Path, fmt: Path,
                 env: dict[str, str], timeout: int,
                 service: str = "watch",
                 incremental: bool = False,
                 long_body: bool = False) -> tuple[dict, dict]:
    reference = base / "reference"
    candidate = base / "candidate"
    stage_fixture(reference)
    stage_fixture(candidate)
    if long_body:
        expand_incremental_fixture(reference)
        expand_incremental_fixture(candidate)
    for engine, project in (("stock", reference), ("aitex", candidate)):
        ready, output = converge(engine, route, project, binary, fmt, env, timeout)
        if ready["returncode"] != 0 or not ready["converged"]:
            raise RuntimeError(f"cannot prepare watch {engine}:\n{output[-3000:]}")
    cache = candidate / ".aitex-cache"
    prime_cache(candidate, cache, binary, fmt, route, env, timeout)

    command = [str(binary), service, "main.tex", "--fmt", str(fmt),
               "--cache", str(cache), "-file-line-error", "-halt-on-error",
               "-o", f"main.{route}"]
    if service == "watch":
        command.insert(2, "--stdin")
    service_env = dict(env)
    if incremental:
        if service != "watch":
            raise ValueError("body checkpoints currently require portable watch mode")
        service_env["AITEX_INCREMENTAL"] = "1"
    else:
        service_env.pop("AITEX_INCREMENTAL", None)
    process = subprocess.Popen(
        command, cwd=candidate, env=service_env, stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    try:
        first, first_output = watch_frame(process, False, timeout)
        require_sample(first, first_output, f"watch startup {route}")
        warm, warm_output = watch_frame(process, True, timeout)
        require_sample(warm, warm_output, f"watch warmup {route}")
        if incremental and "body checkpoint hit" not in warm_output:
            raise RuntimeError("incremental watch warmup did not resume a body checkpoint")
        # Apply the edit while the service is live. This makes the timed frame
        # exercise dependency invalidation and in-process asset identity,
        # rather than merely repeating a build that happened after an edit.
        mutate(reference, edit)
        mutate(candidate, edit)
        ours, our_output = watch_frame(process, True, timeout)
        require_sample(ours, our_output, f"watch request {route}")
        if incremental:
            if "body checkpoint hit" not in our_output:
                raise RuntimeError("incremental watch request did not resume a body checkpoint")
            expected_edit = ("no source edit" if edit == "no_op"
                             else "source suffix changed")
            if expected_edit not in our_output:
                raise RuntimeError(
                    f"incremental watch request did not report {expected_edit!r}")
        expected_cache = "output-hit" if edit == "no_op" else "miss"
        if (ours.get("aitex") or {}).get("cache") != expected_cache:
            raise RuntimeError(
                f"{service} request expected cache {expected_cache}, got "
                f"{(ours.get('aitex') or {}).get('cache')}"
            )
    finally:
        if process.poll() is None:
            assert process.stdin is not None
            process.stdin.write("quit\n")
            process.stdin.flush()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

    stock, stock_output = measured(
        tex_command("stock", route, binary, fmt), reference, env, timeout)
    require_sample(stock, stock_output, f"stock watch comparison {route}")
    check = correctness(output_path(reference, route), output_path(candidate, route),
                        route, base / "correctness", env)
    if not check["equivalent"]:
        raise RuntimeError(f"watch correctness failed for {route} {edit}: {check}")
    stock["correctness"] = check
    ours["correctness"] = check
    return stock, ours


def incremental_suite(args: argparse.Namespace, binary: Path, fmt: Path,
                      env: dict[str, str], samples: int,
                      started: float) -> dict:
    """Focused P3 latency/memory comparison against the P2 watch path."""
    modes: list[dict] = []
    with tempfile.TemporaryDirectory(prefix="aitex-incremental-performance-") as temporary:
        scratch = Path(temporary)
        for incremental in (False, True):
            prefix = "incremental_watch" if incremental else "watch_control"
            cache = ("body-checkpoint" if incremental
                     else "persistent-project-snapshot")
            for edit in ("no_op", "late_paragraph"):
                for route in ("dvi", "pdf"):
                    stock_samples, our_samples = [], []
                    for repetition in range(samples):
                        stock, ours = watch_sample(
                            scratch / f"{prefix}-{edit}-{route}-{repetition}",
                            edit, route, binary, fmt, env, args.timeout,
                            incremental=incremental, long_body=True)
                        stock_samples.append(stock)
                        our_samples.append(ours)
                        print(
                            f"[{prefix}_{edit}/{route}] "
                            f"{repetition + 1}/{samples}", flush=True)
                    add_pair(modes, f"{prefix}_{edit}", route, edit, cache,
                             stock_samples, our_samples)

    return {
        "schema": SCHEMA,
        "description": "aiTeX P3 incremental body-checkpoint benchmark",
        "created_at": holdout.utc_now(),
        "suite": "incremental",
        "configuration": {
            "samples": samples,
            "timeout_seconds": args.timeout,
            "source_date_epoch": 1000000000,
            "percentiles": "linear interpolation at p50 and p95",
            "timing_scope": "one framed request in an already-warm watch process",
            "cpu_scope": "live process-tree counters over the framed request",
            "rss_scope": "live process-tree RSS after the framed request",
            "correctness": {
                "dvi": "normalized positioned drawing and special streams",
                "pdf": ("validity, page count, normalized extracted text, "
                        "and locked raster tolerance"),
            },
        },
        "environment": machine_identity(binary, fmt),
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "modes": modes,
    }


def aitex_cold_source_command(binary: Path) -> list[str]:
    # Say `--ini` explicitly. Normal binaries create/discover their local
    # LaTeX format cache; relying on ambient cache state made this control
    # measure the wrong startup mode.
    return [str(binary), "pass", "cold.tex", "--ini",
            "-file-line-error", "-halt-on-error", "-o", "cold.dvi"]


def cold_source_sample(base: Path, binary: Path, env: dict[str, str],
                       timeout: int) -> tuple[dict, dict]:
    reference = base / "reference"
    candidate = base / "candidate"
    stage_fixture(reference)
    stage_fixture(candidate)
    latex_ltx = Path(subprocess.check_output(
        ["kpsewhich", "latex.ltx"], text=True).strip())
    lines = latex_ltx.read_bytes().splitlines(keepends=True)
    kernel = b"".join(lines[:-4])
    # INITEX deliberately starts braces as ordinary characters. latex.ltx uses
    # that fact to prove no format was preloaded, then installs its own
    # catcodes. Setting them first triggers the kernel's bootstrap error.
    prefix = b"\\input kernel\n"
    document = (b"\\documentclass{article}\n\\begin{document}\n"
                b"\\section{Cold source}\n"
                + (b"The quick brown fox exercises a deterministic paragraph.\n\n" * 20)
                + b"\\end{document}\n")
    for project in (reference, candidate):
        (project / "kernel.tex").write_bytes(kernel)
        (project / "cold.tex").write_bytes(prefix + document)
    stock_command = ["pdftex", "-ini", "-etex", "-interaction=nonstopmode",
                     "-halt-on-error", "-output-format=dvi", "cold.tex"]
    our_command = aitex_cold_source_command(binary)
    stock, stock_output = measured(stock_command, reference, env, timeout)
    ours, our_output = measured(our_command, candidate, env, timeout)
    require_sample(stock, stock_output, "stock cold source")
    require_sample(ours, our_output, "aitex cold source")
    check = correctness(reference / "cold.dvi", candidate / "cold.dvi", "dvi",
                        base / "correctness", env)
    if not check["equivalent"]:
        raise RuntimeError(f"cold-source correctness failed: {check}")
    stock["correctness"] = check
    ours["correctness"] = check
    return stock, ours


def long_document_sample(base: Path, binary: Path, fmt: Path,
                         env: dict[str, str], timeout: int,
                         sections: int = 1000) -> tuple[dict, dict]:
    reference = base / "reference"
    candidate = base / "candidate"
    reference.mkdir(parents=True)
    candidate.mkdir(parents=True)
    paragraph = ("The quick brown fox jumps over the lazy dog while office "
                 "traffic passes the coffee shop near the railway station.")
    body = ["\\documentclass{article}", "\\begin{document}"]
    for number in range(1, sections + 1):
        body.append(f"\\section{{Section {number}}}")
        body.extend([paragraph, ""] * 6)
    body.append("\\end{document}")
    source = "\n".join(body) + "\n"
    for project in (reference, candidate):
        (project / "long.tex").write_text(source)
    stock, stock_output = measured(
        tex_command("stock", "dvi", binary, fmt, root="long.tex"),
        reference, env, timeout)
    ours, our_output = measured(
        tex_command("aitex", "dvi", binary, fmt, root="long.tex"),
        candidate, env, timeout)
    require_sample(stock, stock_output, "stock long document")
    require_sample(ours, our_output, "aitex long document")
    check = correctness(reference / "long.dvi", candidate / "long.dvi", "dvi",
                        base / "correctness", env)
    if not check["equivalent"]:
        raise RuntimeError(f"long-document correctness failed: {check}")
    stock["correctness"] = check
    ours["correctness"] = check
    stock["sections"] = ours["sections"] = sections
    return stock, ours


def convergence_pair_sample(base: Path, route: str, binary: Path, fmt: Path,
                            env: dict[str, str], timeout: int,
                            edit: str | None = None,
                            run_latexmk: bool = False
                            ) -> tuple[dict, dict, dict | None]:
    reference = base / "reference"
    candidate = base / "candidate"
    stage_fixture(reference)
    stage_fixture(candidate)
    latexmk_project = base / "latexmk"
    if run_latexmk:
        stage_fixture(latexmk_project)
    candidate_cache = candidate / ".aitex-cache"
    if edit is not None:
        projects = [("stock", reference), ("aitex", candidate)]
        if run_latexmk:
            projects.append(("stock", latexmk_project))
        for engine, project in projects:
            ready, output = converge(engine, route, project, binary, fmt, env, timeout)
            if ready["returncode"] != 0 or not ready["converged"]:
                raise RuntimeError(f"cannot prepare convergence edit:\n{output[-3000:]}")
        prime_cache(candidate, candidate_cache, binary, fmt, route, env, timeout)
        mutate(reference, edit)
        mutate(candidate, edit)
        if run_latexmk:
            mutate(latexmk_project, edit)
    stock, stock_output = converge(
        "stock", route, reference, binary, fmt, env, timeout,
        force_bibtex=edit == "bibliography")
    ours, our_output = converge(
        "aitex", route, candidate, binary, fmt, env, timeout,
        cache=candidate_cache, force_bibtex=edit == "bibliography")
    require_sample(stock, stock_output, f"stock convergence {route}")
    require_sample(ours, our_output, f"aitex convergence {route}")
    if not stock["converged"] or not ours["converged"]:
        raise RuntimeError(f"convergence limit reached: stock={stock} ours={ours}")
    check = correctness(output_path(reference, route), output_path(candidate, route),
                        route, base / "correctness", env)
    if not check["equivalent"]:
        raise RuntimeError(f"convergence correctness failed: {check}")
    stock["correctness"] = check
    ours["correctness"] = check
    latexmk = None
    if run_latexmk:
        latexmk, latexmk_output = measured(
            latexmk_command(route), latexmk_project, env, timeout)
        require_sample(latexmk, latexmk_output, f"latexmk convergence {route}")
        latexmk_check = correctness(
            output_path(reference, route), output_path(latexmk_project, route),
            route, base / "correctness-latexmk", env)
        if not latexmk_check["equivalent"]:
            raise RuntimeError(
                f"latexmk convergence correctness failed: {latexmk_check}")
        latexmk["correctness"] = latexmk_check
    return stock, ours, latexmk


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * quantile
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def nested(sample: dict, path: str) -> float | int | None:
    value: object = sample
    for key in path.split("."):
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value if isinstance(value, (int, float)) else None


def summarize(samples: list[dict]) -> dict:
    fields = [
        "wall_seconds", "cpu_seconds", "max_rss_kib",
        "filesystem_read_bytes", "filesystem_write_bytes", "startup_overhead_ms",
        "aitex.cache_validation_ms", "aitex.cache_load_ms",
        "aitex.snapshot_restore_ms", "aitex.format_restore_ms",
        "aitex.format_decode_count",
        "aitex.tex_class_ms", "aitex.tex_preamble_ms", "aitex.tex_body_ms",
        "aitex.class_snapshot_write_ms", "aitex.snapshot_write_ms",
        "aitex.tex_total_ms", "aitex.shipout_ms", "aitex.pdf_font_ms",
        "aitex.pdf_image_ms", "aitex.pdf_assembly_ms", "aitex.pdf_total_ms",
        "aitex.resolver_local_queries", "aitex.resolver_local_hits",
        "aitex.resolver_helper_starts", "aitex.resolver_helper_start_failures",
        "aitex.resolver_helper_start_ms", "aitex.resolver_helper_queries",
        "aitex.resolver_helper_hits", "aitex.resolver_helper_query_ms",
        "aitex.resolver_fallback_processes", "aitex.resolver_fallback_ms",
        "aitex.resolver_pk_processes", "aitex.resolver_generated_pk",
        "aitex.resolver_pk_ms", "aitex.resolver_extractbb_processes",
        "aitex.resolver_extractbb_ms",
        "aitex.snapshot_bytes",
    ]
    result = {}
    for field in fields:
        values = [float(value) for sample in samples
                  if (value := nested(sample, field)) is not None]
        if values:
            result[field] = {
                "p50": round(percentile(values, 0.50) or 0.0, 6),
                "p95": round(percentile(values, 0.95) or 0.0, 6),
            }
    return result


def mode(mode_id: str, engine: str, route: str, workload: str,
         cache: str, samples: list[dict]) -> dict:
    return {
        "id": mode_id,
        "status": "measured",
        "engine": engine,
        "route": route,
        "workload": workload,
        "cache": cache,
        "samples": samples,
        "summary": summarize(samples),
    }


def add_pair(modes: list[dict], scenario: str, route: str, workload: str,
             cache: str, stock: list[dict], ours: list[dict]) -> None:
    stock_engine = "latex" if route == "dvi" else "pdflatex"
    modes.append(mode(f"{scenario}.{route}.{stock_engine}", stock_engine,
                      route, workload, "system-format", stock))
    modes.append(mode(f"{scenario}.{route}.aitex", "aitex", route,
                      workload, cache, ours))


def machine_identity(binary: Path, fmt: Path) -> dict:
    cpu = None
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        match = re.search(r"^model name\s*:\s*(.+)$", cpuinfo.read_text(), re.M)
        cpu = match.group(1) if match else None
    memory = None
    meminfo = Path("/proc/meminfo")
    if meminfo.is_file():
        match = re.search(r"^MemTotal:\s*(\d+) kB$", meminfo.read_text(), re.M)
        memory = int(match.group(1)) * 1024 if match else None
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT,
                            stdout=subprocess.PIPE, text=True, check=True).stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT,
                           stdout=subprocess.PIPE, text=True, check=True).stdout != ""
    return {
        "platform": platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "cpu_model": cpu,
        "logical_cpus": os.cpu_count(),
        "memory_bytes": memory,
        "git_commit": commit,
        "git_dirty": dirty,
        "binary_sha256": sha256(binary),
        "format_sha256": sha256(fmt),
        "fixture_sha256": tree_sha256(FIXTURE),
        "default_build_fixture_sha256": tree_sha256(DEFAULT_BUILD_FIXTURE),
        "tools": {
            name: command_line(command) for name, command in {
                "aitex": [str(binary), "--version"],
                "rustc": ["rustc", "--version"],
                "latex": ["latex", "--version"],
                "pdflatex": ["pdflatex", "--version"],
                "latexmk": ["latexmk", "--version"],
                "bibtex": ["bibtex", "--version"],
                "makeindex": ["makeindex", "--version"],
                "mutool": ["mutool", "-v"],
                "imagemagick": ["compare", "-version"],
                "gnu_time": ["/usr/bin/time", "--version"],
            }.items()
        },
    }


def run_suite(args: argparse.Namespace) -> dict:
    binary = args.binary.resolve()
    fmt = args.format.resolve()
    if args.build:
        subprocess.run(["cargo", "build", "--release", "-p", "aitex-cli",
                        "--locked"], cwd=ROOT, check=True)
    if not binary.is_file():
        raise SystemExit(f"binary is absent: {binary}; pass --build")
    fmt.parent.mkdir(parents=True, exist_ok=True)
    env = benchmark_env()
    mkfmt_env = dict(env)
    mkfmt_env["AITEX_BINARY"] = str(binary)
    subprocess.run([str(ROOT / "tools" / "mkfmt.sh"), str(fmt.parent)],
                   cwd=ROOT, env=mkfmt_env, check=True)
    if fmt.name != "fmt.afmt":
        generated = fmt.parent / "fmt.afmt"
        generated.replace(fmt)

    samples = 1 if args.smoke else args.samples
    modes: list[dict] = []
    latexmk_available = shutil.which("latexmk") is not None
    started = time.monotonic()
    if args.incremental:
        return incremental_suite(args, binary, fmt, env, samples, started)
    with tempfile.TemporaryDirectory(prefix="aitex-performance-") as temporary:
        scratch = Path(temporary)
        for scenario in ("clean", "unchanged", "source", "bibliography", "index"):
            stock_samples, our_samples = [], []
            for repetition in range(samples):
                stock, ours = default_build_sample(
                    scratch / f"default-build-{scenario}-{repetition}", scenario,
                    binary, fmt, env, args.timeout)
                stock_samples.append(stock)
                our_samples.append(ours)
                print(f"[default_build_{scenario}/pdf] "
                      f"{repetition + 1}/{samples}", flush=True)
            add_pair(modes, f"default_build_{scenario}", "pdf", scenario,
                     "platform-default", stock_samples, our_samples)

        scenarios = ([
            ("format", "no_op", "disabled"),
            ("cache_miss", "no_op", "miss"),
            ("edit_no_op", "no_op", "hit"),
        ] if args.smoke else [
            ("format", "no_op", "disabled"),
            ("cache_miss", "no_op", "miss"),
            ("edit_no_op", "no_op", "hit"),
            ("edit_paragraph", "paragraph", "hit"),
            ("edit_preamble", "preamble", "hit"),
            ("edit_included_file", "included_file", "hit"),
            ("edit_image", "image", "hit"),
        ])
        routes = ("dvi", "pdf")
        for scenario, edit, cache_mode in scenarios:
            for route in routes:
                stock_samples, our_samples = [], []
                for repetition in range(samples):
                    base = scratch / f"{scenario}-{route}-{repetition}"
                    stock, ours = measure_pair_sample(
                        base, edit, route, binary, fmt, env, args.timeout, cache_mode)
                    stock_samples.append(stock)
                    our_samples.append(ours)
                    print(f"[{scenario}/{route}] {repetition + 1}/{samples}", flush=True)
                add_pair(modes, scenario, route, edit, cache_mode,
                         stock_samples, our_samples)

        for edit in ("no_op", "paragraph", "image"):
            for route in routes:
                stock_samples, our_samples = [], []
                for repetition in range(samples):
                    stock, ours = watch_sample(
                        scratch / f"watch-{edit}-{route}-{repetition}", edit,
                        route, binary, fmt, env, args.timeout)
                    stock_samples.append(stock)
                    our_samples.append(ours)
                    print(f"[watch_{edit}/{route}] {repetition + 1}/{samples}",
                          flush=True)
                add_pair(modes, f"watch_{edit}", route, edit,
                         "exact-output-or-full-pass", stock_samples, our_samples)

        if platform.system() == "Linux":
            for route in routes:
                stock_samples, our_samples = [], []
                for repetition in range(samples):
                    stock, ours = watch_sample(
                        scratch / f"fork-watch-no-op-{route}-{repetition}",
                        "no_op", route, binary, fmt, env, args.timeout,
                        service="fork-watch")
                    stock_samples.append(stock)
                    our_samples.append(ours)
                    print(f"[fork_watch_no_op/{route}] {repetition + 1}/{samples}",
                          flush=True)
                add_pair(modes, "fork_watch_no_op", route, "no_op",
                         "exact-output", stock_samples, our_samples)

        if not args.smoke:
            stock_samples, our_samples = [], []
            for repetition in range(samples):
                stock, ours = long_document_sample(
                    scratch / f"long-document-{repetition}", binary, fmt,
                    env, args.timeout)
                stock_samples.append(stock)
                our_samples.append(ours)
                print(f"[long_document_1000/dvi] {repetition + 1}/{samples}",
                      flush=True)
            add_pair(modes, "long_document_1000", "dvi", "single-pass",
                     "disabled", stock_samples, our_samples)

            stock_samples, our_samples = [], []
            for repetition in range(samples):
                stock, ours = cold_source_sample(
                    scratch / f"cold-source-{repetition}", binary, env, args.timeout)
                stock_samples.append(stock)
                our_samples.append(ours)
                print(f"[cold_source/dvi] {repetition + 1}/{samples}", flush=True)
            modes.append(mode("cold_source.dvi.pdftex_init", "pdftex-init", "dvi",
                              "single-pass", "none", stock_samples))
            modes.append(mode("cold_source.dvi.aitex", "aitex", "dvi",
                              "single-pass", "none", our_samples))

            for edit in (None, "bibliography"):
                scenario = "convergence_clean" if edit is None else "edit_bibliography"
                for route in ("dvi", "pdf"):
                    stock_samples, our_samples, latexmk_samples = [], [], []
                    for repetition in range(samples):
                        stock, ours, latexmk = convergence_pair_sample(
                            scratch / f"{scenario}-{route}-{repetition}", route,
                            binary, fmt, env, args.timeout, edit,
                            latexmk_available)
                        stock_samples.append(stock)
                        our_samples.append(ours)
                        if latexmk is not None:
                            latexmk_samples.append(latexmk)
                        print(f"[{scenario}/{route}] {repetition + 1}/{samples}",
                              flush=True)
                    add_pair(modes, scenario, route, "latexmk-style-convergence",
                             "project-preamble", stock_samples, our_samples)
                    if latexmk_samples:
                        modes.append(mode(
                            f"{scenario}.{route}.latexmk", "latexmk", route,
                            "frontend-convergence", "latexmk-rule-database",
                            latexmk_samples))

    modes.extend([
        {
            "id": "cold_source.pdf.aitex",
            "status": "not_supported",
            "reason": ("the current PDF package prelude assumes a preloaded LaTeX format; "
                       "cold-source DVI is measured and format-based PDF is measured"),
        },
    ])
    if not latexmk_available:
        modes.append({
            "id": "convergence.latexmk",
            "status": "unavailable",
            "reason": ("manual convergence is still measured, but the external "
                       "latexmk frontend is not installed on this machine"),
        })
    return {
        "schema": SCHEMA,
        "description": "aiTeX versioned deterministic performance baseline",
        "created_at": holdout.utc_now(),
        "suite": "smoke" if args.smoke else "baseline",
        "configuration": {
            "samples": samples,
            "timeout_seconds": args.timeout,
            "source_date_epoch": 1000000000,
            "percentiles": "linear interpolation at p50 and p95",
            "timing_scope": ("one-shot compiler process or framed live-service request; "
                             "fixture staging and correctness excluded"),
            "cpu_scope": ("GNU time user+system for one-shot runs; live parent counters plus "
                          "completed fork-child rusage for services"),
            "rss_scope": ("GNU time maximum RSS for one-shot runs; live process-tree RSS plus "
                          "completed fork-child peak RSS for services"),
            "io_scope": ("Linux process counters, with completed fork-child ru_inblock/"
                         "ru_oublock converted from 512-byte units"),
            "phase_overlap": "shipout is nested within TeX execution; PDF subphases partition PDF total",
            "correctness": {
                "dvi": "normalized positioned drawing and special streams",
                "pdf": "validity, page count, normalized extracted text, and locked raster tolerance",
            },
        },
        "environment": machine_identity(binary, fmt),
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "modes": modes,
    }


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="aitex-perf-selftest-") as temporary:
        base = Path(temporary)
        stage_fixture(base / "fixture")
        png = base / "fixture" / "figure.png"
        assert png.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
        mutate(base / "fixture", "paragraph")
        assert "PARAGRAPH-EDITED" in (base / "fixture" / "main.tex").read_text()
        stage_default_build_fixture(base / "default-build")
        mutate_default_build(base / "default-build", "index")
        assert "native build scheduler" in (
            base / "default-build" / "main.tex").read_text()
        assert percentile([1, 2, 3], 0.5) == 2
        parsed = parse_internal('x\nAITEX_METRICS {"schema":1,"cache":"hit"}\n')
        assert parsed == {"schema": 1, "cache": "hit"}
        assert cache_is_primed("hit") and cache_is_primed("output-hit")
        assert not cache_is_primed("miss")
        assert expected_edit_cache("no_op") == "output-hit"
        assert expected_edit_cache("paragraph") == "miss"
        assert expected_edit_cache("preamble") == "miss"
        assert "--ini" in aitex_cold_source_command(Path("aitex"))
    print("performance benchmark self-test: OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--format", type=Path, default=DEFAULT_FORMAT)
    parser.add_argument("--output", type=Path, default=DEFAULT_RESULT)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--build", action="store_true")
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--incremental", action="store_true",
                        help="run only the focused P3 watch comparison")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if args.samples < 1:
        parser.error("--samples must be positive")
    if args.smoke and args.incremental:
        parser.error("--smoke and --incremental are mutually exclusive")
    result = run_suite(args)
    if args.output == DEFAULT_RESULT and args.smoke:
        output = ROOT / "target" / "performance-smoke.json"
    elif args.output == DEFAULT_RESULT and args.incremental:
        output = ROOT / "qualification" / "performance-p3.json"
    else:
        output = args.output
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(f"wrote {output} ({sha256(output)})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
