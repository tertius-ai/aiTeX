#!/usr/bin/env python3
"""Qualify PDF size and rendering on aiTeX's redistributable synthetic corpus.

The runner expands only original checked-in fixtures inside a temporary tree.
It never fetches document content and never writes PDFs into the repository.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.dont_write_bytecode = True
import pdf_size


ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "tests" / "pdf-qualification" / "synthetic-book.tex"
CORPUS_LOCK = ROOT / "qualification" / "pdf-corpus-v1.json"
DEFAULT_BINARY = ROOT / "target" / "release" / "aitex"
DEFAULT_FORMAT = ROOT / ".aitex" / "format" / "fmt.afmt"
DEFAULT_OUTPUT = ROOT / "target" / "qualification" / "pdf-synthetic-v1.json"
FONT_FIXTURES = ROOT / "crates" / "aitex-fonts" / "tests" / "fonts"
SCHEMA = 1
METRICS_PREFIX = "AITEX_METRICS "
# Boxed link annotations expose small producer-specific rectangle differences.
# This remains more than 13x tighter than the product-corpus MAE gate (0.02).
RASTER_MAX_NORMALIZED_MAE = 0.0015
RASTER_FUZZ_FRACTION = 0.05
RASTER_MAX_OUTSIDE_FRACTION = 0.005


class QualificationError(RuntimeError):
    pass


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def command_line(argv: list[str]) -> str | None:
    try:
        result = subprocess.run(
            argv,
            cwd=ROOT,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return None
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    useful = [
        line
        for line in lines
        if not line.startswith(("perl: warning:", "warning: Falling back"))
        and "locale" not in line.lower()
        and not line.startswith(("LC_", "LANG =", "are supported"))
    ]
    return useful[0] if useful else (lines[0] if lines else "")


def qualification_env() -> dict[str, str]:
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
            "AITEX_PDF_COMPRESSION_WORKERS": "4",
            "AITEX_FONT_PATH": str(FONT_FIXTURES),
        }
    )
    return env


def verify_corpus_lock() -> dict[str, object]:
    try:
        lock = json.loads(CORPUS_LOCK.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise QualificationError(f"cannot read corpus lock: {error}") from error
    if lock.get("schema") != 1 or not isinstance(lock.get("entries"), list):
        raise QualificationError("unsupported PDF corpus lock schema")
    for entry in lock["entries"]:
        try:
            path = ROOT / entry["path"]
            expected = entry["sha256"]
        except (KeyError, TypeError) as error:
            raise QualificationError("malformed PDF corpus lock entry") from error
        if not path.is_file():
            raise QualificationError(f"locked corpus input is missing: {entry['path']}")
        actual = sha256(path)
        if actual != expected:
            raise QualificationError(
                f"locked corpus input changed: {entry['path']} "
                f"({actual} != {expected})"
            )
    return lock


def parse_time(path: Path) -> dict[str, float | int | None]:
    text = path.read_text(errors="replace") if path.is_file() else ""
    if platform.system() == "Darwin":
        summary = re.search(
            r"([0-9.]+) real\s+([0-9.]+) user\s+([0-9.]+) sys", text
        )
        rss = re.search(r"^\s*(\d+)\s+maximum resident set size\s*$", text, re.M)
        return {
            "cpu_seconds": (
                round(float(summary.group(2)) + float(summary.group(3)), 6)
                if summary
                else None
            ),
            "max_rss_kib": round(int(rss.group(1)) / 1024) if rss else None,
        }

    def field(label: str, cast: type) -> int | float | None:
        match = re.search(rf"^\s*{re.escape(label)}:\s*([0-9.]+)\s*$", text, re.M)
        return cast(match.group(1)) if match else None

    user = field("User time (seconds)", float)
    system = field("System time (seconds)", float)
    return {
        "cpu_seconds": (
            round(user + system, 6) if user is not None and system is not None else None
        ),
        "max_rss_kib": field("Maximum resident set size (kbytes)", int),
    }


def timed_command(
    argv: list[str], cwd: Path, env: dict[str, str], timeout: int
) -> tuple[dict[str, object], str]:
    descriptor, raw_path = tempfile.mkstemp(prefix=".pdf-time.", dir=cwd)
    os.close(descriptor)
    time_path = Path(raw_path)
    if platform.system() == "Darwin":
        command = ["/usr/bin/time", "-l", "-o", str(time_path), *argv]
    else:
        command = ["/usr/bin/time", "-v", "-o", str(time_path), "--", *argv]
    started = time.monotonic()
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=timeout,
        )
        wall = time.monotonic() - started
        measured = parse_time(time_path)
    except subprocess.TimeoutExpired as error:
        output = (error.stdout or "") + (error.stderr or "")
        raise QualificationError(
            f"timed out after {timeout}s: {' '.join(argv)}\n{output[-2000:]}"
        ) from error
    finally:
        time_path.unlink(missing_ok=True)
    if completed.returncode != 0:
        raise QualificationError(
            f"command exited {completed.returncode}: {' '.join(argv)}\n"
            + completed.stdout[-4000:]
        )
    return (
        {
            "wall_seconds": round(wall, 6),
            "cpu_seconds": measured["cpu_seconds"],
            "max_rss_kib": measured["max_rss_kib"],
        },
        completed.stdout,
    )


def stage(directory: Path, pages: int, *, prefix: str = "") -> None:
    directory.mkdir(parents=True)
    shutil.copy2(SOURCE, directory / SOURCE.name)
    (directory / "main.tex").write_text(
        prefix
        + f"\\def\\aitexqualificationpages{{{pages}}}\n"
        f"\\input{{{SOURCE.name}}}\n",
        encoding="utf-8",
    )


def parse_aitex_metrics(output: str) -> list[dict[str, object]]:
    metrics = []
    for line in output.splitlines():
        if not line.startswith(METRICS_PREFIX):
            continue
        try:
            metrics.append(json.loads(line[len(METRICS_PREFIX) :]))
        except json.JSONDecodeError as error:
            raise QualificationError(f"invalid aiTeX metrics: {line}") from error
    if not metrics:
        raise QualificationError("aiTeX emitted no AITEX_METRICS record")
    return metrics


def build_aitex(
    directory: Path,
    binary: Path,
    format_path: Path,
    pages: int,
    timeout: int,
    *,
    classic_xref: bool = False,
) -> tuple[dict[str, object], Path]:
    stage(directory, pages)
    env = qualification_env()
    if classic_xref:
        env["AITEX_PDF_CLASSIC_XREF"] = "1"
    measurement, output = timed_command(
        [
            str(binary),
            "main.tex",
            "--fmt",
            str(format_path),
            "--no-cache",
            "--no-banner",
            "-halt-on-error",
            "-file-line-error",
            "-o",
            "main.pdf",
        ],
        directory,
        env,
        timeout,
    )
    pdf = directory / "main.pdf"
    if not pdf.is_file() or not pdf.read_bytes().startswith(b"%PDF-"):
        raise QualificationError("aiTeX did not produce a PDF")
    metrics = parse_aitex_metrics(output)
    measurement.update(
        {
            "pdf_bytes": pdf.stat().st_size,
            "pdf_sha256": sha256(pdf),
            "passes": len(metrics),
            "tex_total_ms": round(
                sum(float(item.get("tex_total_ms", 0.0)) for item in metrics), 3
            ),
            "pdf_font_ms": round(
                sum(float(item.get("pdf_font_ms", 0.0)) for item in metrics), 3
            ),
            "pdf_total_ms": round(
                sum(float(item.get("pdf_total_ms", 0.0)) for item in metrics), 3
            ),
            "pdf_assembly_ms": round(
                sum(float(item.get("pdf_assembly_ms", 0.0)) for item in metrics), 3
            ),
        }
    )
    return measurement, pdf


def build_stock(
    directory: Path, pages: int, timeout: int, *, classic_xref: bool = False
) -> tuple[dict[str, object], Path]:
    prefix = "\\pdfobjcompresslevel=0\n" if classic_xref else ""
    stage(directory, pages, prefix=prefix)
    env = qualification_env()
    measurement, _ = timed_command(
        [
            "latexmk",
            "-pdf",
            "-interaction=nonstopmode",
            "-halt-on-error",
            "-file-line-error",
            "main.tex",
        ],
        directory,
        env,
        timeout,
    )
    pdf = directory / "main.pdf"
    if not pdf.is_file() or not pdf.read_bytes().startswith(b"%PDF-"):
        raise QualificationError("latexmk did not produce a PDF")
    measurement.update(
        {"pdf_bytes": pdf.stat().st_size, "pdf_sha256": sha256(pdf)}
    )
    return measurement, pdf


def run_checked(argv: list[str], timeout: int = 300) -> str:
    try:
        result = subprocess.run(
            argv,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        output = getattr(error, "stdout", "") or ""
        raise QualificationError(f"validator failed: {' '.join(argv)}\n{output[-2000:]}")
    return result.stdout


def pdf_info(path: Path) -> dict[str, object]:
    output = run_checked(["pdfinfo", str(path)])
    fields = {}
    for line in output.splitlines():
        if ":" in line:
            key, value = line.split(":", 1)
            fields[key.strip()] = value.strip()
    try:
        pages = int(fields["Pages"])
    except (KeyError, ValueError) as error:
        raise QualificationError(f"pdfinfo returned no page count for {path}") from error
    return {
        "pages": pages,
        "pdf_version": fields.get("PDF version"),
        "tagged": fields.get("Tagged"),
    }


def link_info(path: Path) -> dict[str, object]:
    output = run_checked(["pdfinfo", "-url", str(path)])
    urls = re.findall(r"^\s*\d+\s+Annotation\s+(\S+)\s*$", output, re.M)
    if not urls:
        raise QualificationError("pdfinfo found no URI annotations")
    return {
        "uri_annotations": len(urls),
        "urls_sha256": hashlib.sha256("\n".join(urls).encode()).hexdigest(),
    }


def classic_annotation_info(path: Path, pages: int) -> dict[str, object]:
    data = path.read_bytes()
    annotations = data.count(b"/Subtype /Link")
    expected = pages * 5
    if annotations != expected:
        raise QualificationError(
            f"classic PDF has {annotations} link annotations; expected {expected}"
        )
    red_boxes = data.count(b"/Border [0 0 1] /C [1 0 0]")
    cyan_boxes = data.count(b"/Border [0 0 1] /C [0 1 1]")
    if red_boxes != pages * 4 or cyan_boxes != pages:
        raise QualificationError(
            "classic PDF did not preserve standard red internal-link and cyan URI boxes"
        )
    return {
        "link_annotations": annotations,
        "red_boxed_links": red_boxes,
        "cyan_boxed_links": cyan_boxes,
    }


def text_identity(path: Path, output: Path) -> dict[str, object]:
    run_checked(["pdftotext", "-layout", str(path), str(output)])
    text = output.read_text(encoding="utf-8", errors="replace")
    for marker in ("Synthetic PDF qualification", "Generated section", "affine office"):
        if marker not in text:
            raise QualificationError(f"extracted text is missing {marker!r}")
    sections = [int(value) for value in re.findall(r"Generated section\s+(\d+)", text)]
    if not sections or sections != list(range(1, len(sections) + 1)):
        raise QualificationError("extracted generated sections are missing or out of order")
    sequence = ",".join(map(str, sections)).encode()
    return {
        "bytes": output.stat().st_size,
        "sha256": sha256(output),
        "generated_sections": len(sections),
        "section_sequence_sha256": hashlib.sha256(sequence).hexdigest(),
        "mapped_math_symbols": {
            "sum": text.count("∑"),
            "square_root": text.count("√"),
            "integral": text.count("∫"),
        },
    }


def pdffonts_flags(row: str) -> tuple[str, str, str]:
    """Return pdffonts' embedded, subset, and ToUnicode columns.

    Font names, types, and encodings can each contain spaces, so parse the
    stable columns from the right.  A missing ToUnicode CMap is not an
    embedding failure: contextual native runs use marked-content ActualText
    when a glyph-level map cannot faithfully represent the source text.
    """
    fields = row.split()
    if len(fields) < 7:
        raise QualificationError(f"cannot parse pdffonts row: {row}")
    embedded, subset, unicode_map = fields[-5:-2]
    if any(value not in {"yes", "no"} for value in (embedded, subset, unicode_map)):
        raise QualificationError(f"cannot parse pdffonts flags: {row}")
    return embedded, subset, unicode_map


def font_info(path: Path) -> dict[str, object]:
    output = run_checked(["pdffonts", str(path)])
    rows = [line for line in output.splitlines()[2:] if line.strip()]
    if not rows:
        raise QualificationError("pdffonts found no embedded fonts")
    flags = [pdffonts_flags(row) for row in rows]
    if any(embedded != "yes" for embedded, _, _ in flags):
        raise QualificationError("qualification PDF contains an unembedded font")
    if any(subset != "yes" for _, subset, _ in flags):
        raise QualificationError("qualification PDF contains a font that is not subset")
    return {"count": len(rows), "rows": rows}


def natural_page(path: Path) -> int:
    match = re.search(r"-(\d+)\.pgm$", path.name)
    return int(match.group(1)) if match else 0


def combined_sha256(paths: list[Path]) -> str:
    digest = hashlib.sha256()
    for path in paths:
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def render_poppler(
    path: Path, directory: Path, timeout: int
) -> tuple[dict[str, object], list[Path]]:
    directory.mkdir(parents=True)
    run_checked(
        ["pdftoppm", "-gray", "-r", "72", str(path), str(directory / "page")],
        timeout,
    )
    pages = sorted(directory.glob("page-*.pgm"), key=natural_page)
    if not pages:
        raise QualificationError("Poppler produced no page renderings")
    return {"pages": len(pages), "sha256": combined_sha256(pages)}, pages


def read_pgm(path: Path) -> tuple[tuple[int, int], bytes]:
    data = path.read_bytes()
    position = 0
    tokens: list[bytes] = []
    while len(tokens) < 4:
        while position < len(data) and data[position] in b" \t\r\n":
            position += 1
        if position < len(data) and data[position] == ord("#"):
            newline = data.find(b"\n", position)
            if newline < 0:
                raise QualificationError(f"truncated PGM comment: {path}")
            position = newline + 1
            continue
        end = position
        while end < len(data) and data[end] not in b" \t\r\n":
            end += 1
        if end == position:
            raise QualificationError(f"truncated PGM header: {path}")
        tokens.append(data[position:end])
        position = end
    if tokens[0] != b"P5" or tokens[3] != b"255":
        raise QualificationError(f"unsupported PGM representation: {path}")
    if position >= len(data) or data[position] not in b" \t\r\n":
        raise QualificationError(f"PGM header has no pixel separator: {path}")
    if data[position : position + 2] == b"\r\n":
        position += 2
    else:
        position += 1
    try:
        dimensions = (int(tokens[1]), int(tokens[2]))
    except ValueError as error:
        raise QualificationError(f"invalid PGM dimensions: {path}") from error
    pixels = data[position:]
    if len(pixels) != dimensions[0] * dimensions[1]:
        raise QualificationError(f"truncated PGM pixels: {path}")
    return dimensions, pixels


def compare_rasters(
    reference: list[Path],
    candidate: list[Path],
    *,
    max_normalized_mae: float = RASTER_MAX_NORMALIZED_MAE,
    fuzz_fraction: float = RASTER_FUZZ_FRACTION,
    max_outside_fraction: float = RASTER_MAX_OUTSIDE_FRACTION,
) -> dict[str, object]:
    if len(reference) != len(candidate):
        raise QualificationError(
            f"Poppler page count differs: {len(reference)} versus {len(candidate)}"
        )
    worst_mae = 0.0
    worst_outside = 0.0
    worst_mae_page = 0
    worst_outside_page = 0
    for page_number, (ref_page, candidate_page) in enumerate(
        zip(reference, candidate), start=1
    ):
        ref_dimensions, ref_pixels = read_pgm(ref_page)
        candidate_dimensions, candidate_pixels = read_pgm(candidate_page)
        if ref_dimensions != candidate_dimensions:
            raise QualificationError(
                f"Poppler page {page_number} dimensions differ: "
                f"{ref_dimensions} versus {candidate_dimensions}"
            )
        differences = [
            abs(reference_value - candidate_value)
            for reference_value, candidate_value in zip(ref_pixels, candidate_pixels)
        ]
        mae = sum(differences) / (len(differences) * 255)
        outside = sum(
            difference / 255 > fuzz_fraction for difference in differences
        ) / len(differences)
        if mae > worst_mae:
            worst_mae = mae
            worst_mae_page = page_number
        if outside > worst_outside:
            worst_outside = outside
            worst_outside_page = page_number
    equivalent = (
        worst_mae <= max_normalized_mae
        and worst_outside <= max_outside_fraction
    )
    result = {
        "equivalent": equivalent,
        "pages": len(reference),
        "dpi": 72,
        "max_normalized_mae": max_normalized_mae,
        "fuzz_fraction": fuzz_fraction,
        "max_outside_fraction": max_outside_fraction,
        "worst_normalized_mae": round(worst_mae, 9),
        "worst_normalized_mae_page": worst_mae_page,
        "worst_outside_fraction": round(worst_outside, 9),
        "worst_outside_fraction_page": worst_outside_page,
    }
    if not equivalent:
        raise QualificationError(
            "aiTeX and pdfTeX Poppler rasters exceed tolerance: "
            f"mae={worst_mae:.6g}, outside={worst_outside:.6g}"
        )
    return result


def render_quartz(path: Path, directory: Path) -> dict[str, object]:
    executable = shutil.which("qlmanage") if platform.system() == "Darwin" else None
    if executable is None:
        return {"available": False}
    directory.mkdir(parents=True)
    run_checked([executable, "-t", "-s", "1000", "-o", str(directory), str(path)], 60)
    images = sorted(directory.glob("*.png"))
    if not images:
        raise QualificationError("Quartz Quick Look produced no PDF thumbnail")
    return {"available": True, "first_page_sha256": sha256(images[0])}


def validate_pdf(
    path: Path, directory: Path, timeout: int, render: bool
) -> tuple[dict[str, object], list[Path]]:
    directory.mkdir(parents=True)
    validators: dict[str, object] = {}
    run_checked(
        ["gs", "-q", "-dNOPAUSE", "-dBATCH", "-sDEVICE=nullpage", str(path)],
        timeout,
    )
    validators["ghostscript"] = True
    for name, command in (
        ("mutool", ["mutool", "info", str(path)]),
        ("qpdf", ["qpdf", "--check", str(path)]),
    ):
        if shutil.which(command[0]):
            run_checked(command, timeout)
            validators[name] = True
        else:
            validators[name] = None
    result: dict[str, object] = {
        "info": pdf_info(path),
        "fonts": font_info(path),
        "links": link_info(path),
        "text": text_identity(path, directory / "text.txt"),
        "validators": validators,
        "quartz": render_quartz(path, directory / "quartz"),
    }
    pages: list[Path] = []
    if render:
        result["poppler"], pages = render_poppler(
            path, directory / "poppler", timeout
        )
    return result, pages


def git_identity() -> tuple[str | None, bool | None]:
    try:
        commit = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, check=True, capture_output=True, text=True
        ).stdout.strip()
        status = subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return None, None
    return commit, bool(status)


def median(samples: list[dict[str, object]], key: str) -> float | int | None:
    values = [sample[key] for sample in samples if sample.get(key) is not None]
    return round(statistics.median(values), 6) if values else None


def summarize(samples: list[dict[str, object]]) -> dict[str, object]:
    return {
        "samples": samples,
        "median_wall_seconds": median(samples, "wall_seconds"),
        "median_cpu_seconds": median(samples, "cpu_seconds"),
        "median_max_rss_kib": median(samples, "max_rss_kib"),
        "median_tex_total_ms": median(samples, "tex_total_ms"),
        "median_pdf_font_ms": median(samples, "pdf_font_ms"),
        "median_pdf_total_ms": median(samples, "pdf_total_ms"),
        "median_pdf_assembly_ms": median(samples, "pdf_assembly_ms"),
        "deterministic_pdf": len({sample["pdf_sha256"] for sample in samples}) == 1,
    }


def compare_breakdowns(
    aitex: dict[str, object], stock: dict[str, object]
) -> dict[str, object]:
    aitex_categories = aitex["categories"]
    stock_categories = stock["categories"]
    assert isinstance(aitex_categories, dict)
    assert isinstance(stock_categories, dict)
    categories = {}
    for name in sorted(set(aitex_categories) | set(stock_categories)):
        aitex_values = aitex_categories.get(name, {})
        stock_values = stock_categories.get(name, {})
        categories[name] = {
            "aitex_objects": int(aitex_values.get("objects", 0)),
            "stock_objects": int(stock_values.get("objects", 0)),
            "object_delta": int(aitex_values.get("objects", 0))
            - int(stock_values.get("objects", 0)),
            "aitex_stored_bytes": int(aitex_values.get("stored_bytes", 0)),
            "stock_stored_bytes": int(stock_values.get("stored_bytes", 0)),
            "stored_byte_delta": int(aitex_values.get("stored_bytes", 0))
            - int(stock_values.get("stored_bytes", 0)),
            "aitex_decoded_bytes": int(aitex_values.get("decoded_bytes", 0)),
            "stock_decoded_bytes": int(stock_values.get("decoded_bytes", 0)),
            "decoded_byte_delta": int(aitex_values.get("decoded_bytes", 0))
            - int(stock_values.get("decoded_bytes", 0)),
        }
    return {
        "aitex_pdf_bytes": int(aitex["pdf_bytes"]),
        "stock_pdf_bytes": int(stock["pdf_bytes"]),
        "pdf_byte_delta": int(aitex["pdf_bytes"]) - int(stock["pdf_bytes"]),
        "aitex_non_payload_bytes": int(aitex["non_payload_bytes"]),
        "stock_non_payload_bytes": int(stock["non_payload_bytes"]),
        "non_payload_byte_delta": int(aitex["non_payload_bytes"])
        - int(stock["non_payload_bytes"]),
        "categories": categories,
    }


def require_tools(skip_stock: bool) -> None:
    commands = ["pdfinfo", "pdffonts", "pdftotext", "pdftoppm", "gs"]
    if not skip_stock:
        commands += ["latexmk", "pdflatex"]
    missing = [command for command in commands if shutil.which(command) is None]
    if missing:
        raise QualificationError("missing required tools: " + ", ".join(missing))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--format", type=Path, default=DEFAULT_FORMAT)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--pages", type=int, default=192)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--skip-stock", action="store_true")
    parser.add_argument("--skip-raster", action="store_true")
    parser.add_argument(
        "--preview-dir",
        type=Path,
        help="copy first, middle, and last aiTeX page renderings here (never PDFs)",
    )
    parser.add_argument(
        "--quick", action="store_true", help="one eight-page development sample"
    )
    args = parser.parse_args()
    if args.quick:
        args.pages = 8
        args.samples = 1
    if args.pages < 1 or args.samples < 1:
        parser.error("pages and samples must be positive")
    args.binary = args.binary.resolve()
    args.format = args.format.resolve()
    if not args.binary.is_file() or not os.access(args.binary, os.X_OK):
        parser.error(f"build the release binary first: {args.binary}")
    if not args.format.is_file():
        parser.error(f"build the aiTeX format first: {args.format}")
    try:
        require_tools(args.skip_stock)
        corpus_lock = verify_corpus_lock()
        commit, dirty = git_identity()
        with tempfile.TemporaryDirectory(prefix="aitex-pdf-qualification.") as raw:
            work = Path(raw)
            aitex_samples = []
            aitex_pdf = None
            for index in range(args.samples):
                sample, pdf = build_aitex(
                    work / f"aitex-{index}",
                    args.binary,
                    args.format,
                    args.pages,
                    args.timeout,
                )
                aitex_samples.append(sample)
                aitex_pdf = pdf
            assert aitex_pdf is not None
            classic_sample, classic_pdf = build_aitex(
                work / "aitex-classic-xref",
                args.binary,
                args.format,
                args.pages,
                args.timeout,
                classic_xref=True,
            )
            aitex_validation, aitex_rasters = validate_pdf(
                aitex_pdf,
                work / "validation-aitex",
                args.timeout,
                not args.skip_raster,
            )
            classic_breakdown = pdf_size.analyze(classic_pdf)
            classic_breakdown["path"] = "temporary/aitex-classic-xref.pdf"
            classic_annotations = classic_annotation_info(classic_pdf, args.pages)

            stock_samples = []
            stock_validation = None
            stock_pdf = None
            if not args.skip_stock:
                for index in range(args.samples):
                    sample, pdf = build_stock(
                        work / f"stock-{index}", args.pages, args.timeout
                    )
                    stock_samples.append(sample)
                    stock_pdf = pdf
                assert stock_pdf is not None
                stock_classic_sample, stock_classic_pdf = build_stock(
                    work / "stock-classic-xref",
                    args.pages,
                    args.timeout,
                    classic_xref=True,
                )
                stock_classic_breakdown = pdf_size.analyze(stock_classic_pdf)
                stock_classic_breakdown["path"] = "temporary/stock-classic-xref.pdf"
                stock_validation, stock_rasters = validate_pdf(
                    stock_pdf,
                    work / "validation-stock",
                    args.timeout,
                    not args.skip_raster,
                )
                if (
                    aitex_validation["text"]["section_sequence_sha256"]
                    != stock_validation["text"]["section_sequence_sha256"]
                ):
                    raise QualificationError(
                        "aiTeX and pdfTeX extracted section order differs"
                    )
                if aitex_validation["links"] != stock_validation["links"]:
                    raise QualificationError(
                        "aiTeX and pdfTeX URI annotation inventories differ"
                    )
                raster_comparison = (
                    compare_rasters(stock_rasters, aitex_rasters)
                    if not args.skip_raster
                    else None
                )

            if args.preview_dir is not None and aitex_rasters:
                args.preview_dir.mkdir(parents=True, exist_ok=True)
                selected = {
                    "first": aitex_rasters[0],
                    "middle": aitex_rasters[len(aitex_rasters) // 2],
                    "last": aitex_rasters[-1],
                }
                for label, source in selected.items():
                    shutil.copy2(source, args.preview_dir / f"{label}.pgm")

            aitex_summary = summarize(aitex_samples)
            if not aitex_summary["deterministic_pdf"]:
                raise QualificationError("clean aiTeX builds are not byte deterministic")
            stock_summary = summarize(stock_samples) if stock_samples else None
            comparison = None
            if stock_samples:
                comparison = {
                    "pdf_size_ratio": round(
                        int(aitex_samples[0]["pdf_bytes"])
                        / int(stock_samples[0]["pdf_bytes"]),
                        6,
                    ),
                    "raw_extracted_text_equal": (
                        aitex_validation["text"]["sha256"]
                        == stock_validation["text"]["sha256"]
                    ),
                    "extracted_section_order_equal": True,
                    "uri_annotations_equal": (
                        aitex_validation["links"] == stock_validation["links"]
                    ),
                    "poppler_raster": raster_comparison,
                    "classic_xref_breakdown": compare_breakdowns(
                        classic_breakdown, stock_classic_breakdown
                    ),
                }

            result = {
                "schema": SCHEMA,
                "corpus": {
                    "lock": str(CORPUS_LOCK.relative_to(ROOT)),
                    "lock_sha256": sha256(CORPUS_LOCK),
                    "lock_name": corpus_lock["name"],
                    "name": "synthetic-book-v1",
                    "source": str(SOURCE.relative_to(ROOT)),
                    "source_sha256": sha256(SOURCE),
                    "pages_requested": args.pages,
                    "provenance": "original synthetic source distributed with aiTeX",
                    "contains_third_party_document_text": False,
                },
                "compiler": {
                    "git_commit": commit,
                    "git_dirty": dirty,
                    "binary_sha256": sha256(args.binary),
                    "format_sha256": sha256(args.format),
                },
                "environment": {
                    "platform": platform.platform(),
                    "python": platform.python_version(),
                    "aitex": command_line([str(args.binary), "--version"]),
                    "rustc": command_line(["rustc", "--version"]),
                    "pdflatex": command_line(["pdflatex", "--version"]),
                    "latexmk": command_line(["latexmk", "--version"]),
                    "pdfinfo": command_line(["pdfinfo", "-v"]),
                    "ghostscript": command_line(["gs", "--version"]),
                },
                "aitex": {
                    **aitex_summary,
                    "validation": aitex_validation,
                    "classic_xref_sample": classic_sample,
                    "classic_xref_breakdown": classic_breakdown,
                    "classic_xref_annotations": classic_annotations,
                },
                "stock": (
                    {
                        **stock_summary,
                        "validation": stock_validation,
                        "classic_xref_sample": stock_classic_sample,
                        "classic_xref_breakdown": stock_classic_breakdown,
                    }
                    if stock_summary is not None
                    else None
                ),
                "comparison": comparison,
            }
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(
                json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
    except QualificationError as error:
        print(f"pdf qualification: {error}", file=sys.stderr)
        return 1

    print(f"pdf qualification: wrote {args.output}")
    print(
        f"  aiTeX: {aitex_samples[0]['pdf_bytes']:,} bytes, "
        f"median PDF stage {aitex_summary['median_pdf_total_ms']:.1f} ms"
    )
    if stock_samples and comparison is not None:
        print(
            f"  pdfTeX: {stock_samples[0]['pdf_bytes']:,} bytes; "
            f"ratio {comparison['pdf_size_ratio']:.3f}x"
        )
    checks = ["text", "links", "Ghostscript", "available platform checks"]
    if not args.skip_raster:
        checks.insert(2, "Poppler rasters")
    print(
        f"  {aitex_validation['info']['pages']} pages; deterministic bytes; "
        f"{', '.join(checks)} passed"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
