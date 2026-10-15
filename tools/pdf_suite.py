#!/usr/bin/env python3
"""Run aiTeX's varied, redistributable PDF qualification suite.

The suite consumes only locked project-authored sources, deterministic generated
assets, and the repository's provenance-recorded OFL font fixtures. Generated
PDFs and rasters remain inside a temporary directory.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True
import pdf_fixture_assets
import pdf_qualification as common
import pdf_size


ROOT = Path(__file__).resolve().parent.parent
SUITE = ROOT / "qualification" / "pdf-suite-v1.json"
CORPUS_LOCK = ROOT / "qualification" / "pdf-corpus-v1.json"
DEFAULT_OUTPUT = ROOT / "target" / "qualification" / "pdf-suite-v1.results.json"
DEFAULT_BINARY = ROOT / "target" / "release" / "aitex"
DEFAULT_FORMAT = ROOT / ".aitex" / "format" / "fmt.afmt"
DEFAULT_POSITIONED_BINARY = ROOT / "target" / "release" / "examples" / "pdf_qualification"
DRIVER_IMAGE = ROOT / "tests" / "pdf" / "driver-image.eps"
SCHEMA = 1


class SuiteError(RuntimeError):
    pass


def load_json(path: Path) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SuiteError(f"cannot read {path.relative_to(ROOT)}: {error}") from error
    if not isinstance(value, dict):
        raise SuiteError(f"{path.relative_to(ROOT)} is not a JSON object")
    return value


def verify_suite() -> dict[str, object]:
    suite = load_json(SUITE)
    if suite.get("schema") != SCHEMA or not isinstance(suite.get("cases"), list):
        raise SuiteError("unsupported PDF suite schema")
    ids: set[str] = set()
    for raw in suite["cases"]:
        if not isinstance(raw, dict):
            raise SuiteError("PDF suite case is not an object")
        try:
            case_id = str(raw["id"])
            kind = str(raw["kind"])
            source = ROOT / str(raw["source"])
            samples = int(raw["samples"])
            quick_samples = int(raw["quick_samples"])
        except (KeyError, TypeError, ValueError) as error:
            raise SuiteError("malformed PDF suite case") from error
        if case_id in ids:
            raise SuiteError(f"duplicate PDF suite case: {case_id}")
        ids.add(case_id)
        if kind not in {"tex", "unicode", "positioned"}:
            raise SuiteError(f"unsupported kind for {case_id}: {kind}")
        if not source.is_file():
            raise SuiteError(f"missing source for {case_id}: {raw['source']}")
        if samples < 1 or quick_samples < 1:
            raise SuiteError(f"sample count must be positive for {case_id}")
        if raw.get("reference") not in {None, "pdftex", "dvipdfmx"}:
            raise SuiteError(f"unsupported reference for {case_id}")
        if raw.get("raster") not in {"all", "sampled"}:
            raise SuiteError(f"unsupported raster policy for {case_id}")
    return suite


def verify_case_sources_locked(
    suite: dict[str, object], corpus_lock: dict[str, object]
) -> None:
    locked = {str(entry["path"]) for entry in corpus_lock["entries"]}
    missing = sorted(
        str(case["source"])
        for case in suite["cases"]
        if str(case["source"]) not in locked
    )
    if missing:
        raise SuiteError("suite sources absent from the corpus lock: " + ", ".join(missing))


def require_tools(cases: list[dict[str, object]], skip_stock: bool) -> None:
    commands = {"pdfinfo", "pdffonts", "pdftotext", "pdftoppm", "gs"}
    if not skip_stock:
        references = {case.get("reference") for case in cases}
        if "pdftex" in references:
            commands.update({"latexmk", "pdflatex"})
        if "dvipdfmx" in references:
            commands.update({"latexmk", "latex", "dvipdfmx"})
    missing = sorted(command for command in commands if shutil.which(command) is None)
    if missing:
        raise SuiteError("missing required tools: " + ", ".join(missing))


def expected_pages(case: dict[str, object], quick: bool) -> int:
    explicit = case.get("expected_pages")
    if explicit is not None:
        return int(explicit)
    pages = case.get("quick_pages") if quick else case.get("pages")
    if pages is None:
        raise SuiteError(f"case {case['id']} has no page count")
    return int(pages) + int(case.get("expected_pages_offset", 0))


def generated_asset_identity() -> dict[str, dict[str, object]]:
    with tempfile.TemporaryDirectory(prefix="aitex-pdf-assets.") as raw:
        paths = pdf_fixture_assets.generate(Path(raw))
        return {
            name: {"bytes": path.stat().st_size, "sha256": common.sha256(path)}
            for name, path in sorted(paths.items())
        }


def requested_pages(case: dict[str, object], quick: bool) -> int | None:
    value = case.get("quick_pages") if quick else case.get("pages")
    return int(value) if value is not None else None


def stage_tex(directory: Path, case: dict[str, object], quick: bool, prefix: str = "") -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    source = ROOT / str(case["source"])
    # The staged wrapper is itself main.tex. Keep a source with that common
    # basename distinct instead of replacing it with a self-input loop.
    staged_name = "fixture.tex" if source.name == "main.tex" else source.name
    shutil.copy2(source, directory / staged_name)
    pages = requested_pages(case, quick)
    definitions = prefix
    if pages is not None:
        definitions += f"\\def\\aitexqualificationpages{{{pages}}}\n"
    main = directory / "main.tex"
    main.write_text(definitions + f"\\input{{{staged_name}}}\n", encoding="utf-8")
    if case["profile"] == "graphics":
        shutil.copy2(DRIVER_IMAGE, directory / DRIVER_IMAGE.name)
        pdf_fixture_assets.generate(directory)
    return main


def candidate_measurement(
    directory: Path,
    case: dict[str, object],
    quick: bool,
    binary: Path,
    format_path: Path,
    positioned_binary: Path,
    timeout: int,
    *,
    classic_xref: bool = False,
) -> tuple[dict[str, object], Path]:
    if case["kind"] == "positioned":
        directory.mkdir(parents=True, exist_ok=True)
        pdf = directory / "main.pdf"
        argv = [str(positioned_binary), str(pdf)]
        if classic_xref:
            argv.append("--classic-xref")
        measurement, output = common.timed_command(
            argv, directory, common.qualification_env(), timeout
        )
        match = re.search(
            r'^AITEX_POSITIONED_METRICS\s+(\{.*\})$', output, re.MULTILINE
        )
        if match is None:
            raise SuiteError("positioned producer emitted no metrics")
        try:
            metrics = json.loads(match.group(1))
        except json.JSONDecodeError as error:
            raise SuiteError("positioned producer emitted invalid metrics") from error
        measurement.update(
            {
                "pdf_bytes": pdf.stat().st_size,
                "pdf_sha256": common.sha256(pdf),
                "passes": 1,
                "pdf_total_ms": float(metrics["pdf_total_ms"]),
                "pdf_assembly_ms": None,
            }
        )
        return measurement, pdf

    stage_tex(directory, case, quick)
    env = common.qualification_env()
    if classic_xref:
        env["AITEX_PDF_CLASSIC_XREF"] = "1"
    if case["profile"] == "graphics":
        env["AITEX_IMAGE_TRACE"] = "1"
    argv = [
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
    ]
    if case["kind"] == "unicode":
        argv.insert(2, "--unicode")
    measurement, output = common.timed_command(argv, directory, env, timeout)
    pdf = directory / "main.pdf"
    if not pdf.is_file() or not pdf.read_bytes().startswith(b"%PDF-"):
        raise SuiteError(f"aiTeX produced no PDF for {case['id']}")
    metrics = common.parse_aitex_metrics(output)
    image_trace = []
    for line in output.splitlines():
        if not line.startswith("aitex-image "):
            continue
        record: dict[str, object] = {}
        for field in shlex.split(line[len("aitex-image "):]):
            key, separator, value = field.partition("=")
            if not separator:
                continue
            if key.endswith("_bytes") or key.endswith("_us"):
                record[key] = int(value)
            elif value in {"true", "false"}:
                record[key] = value == "true"
            else:
                record[key] = value
        image_trace.append(record)
    measurement.update(
        {
            "pdf_bytes": pdf.stat().st_size,
            "pdf_sha256": common.sha256(pdf),
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
            "images": image_trace or None,
        }
    )
    return measurement, pdf


def add_measurements(*measurements: dict[str, object]) -> dict[str, object]:
    return {
        "wall_seconds": round(
            sum(float(item.get("wall_seconds") or 0.0) for item in measurements), 6
        ),
        "cpu_seconds": round(
            sum(float(item.get("cpu_seconds") or 0.0) for item in measurements), 6
        ),
        "max_rss_kib": max(
            (int(item.get("max_rss_kib") or 0) for item in measurements), default=0
        ),
    }


def reference_measurement(
    directory: Path,
    case: dict[str, object],
    quick: bool,
    timeout: int,
    *,
    classic_xref: bool = False,
) -> tuple[dict[str, object], Path]:
    reference = case.get("reference")
    prefix = "\\pdfobjcompresslevel=0\n" if classic_xref and reference == "pdftex" else ""
    stage_tex(directory, case, quick, prefix)
    env = common.qualification_env()
    if reference == "pdftex":
        measurement, _ = common.timed_command(
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
    elif reference == "dvipdfmx":
        tex_measurement, _ = common.timed_command(
            [
                "latexmk",
                "-dvi",
                "-interaction=nonstopmode",
                "-halt-on-error",
                "-file-line-error",
                "main.tex",
            ],
            directory,
            env,
            timeout,
        )
        driver_argv = ["dvipdfmx", "-q"]
        if classic_xref:
            driver_argv.extend(["-V", "4"])
        driver_argv.extend(["-o", "main.pdf", "main.dvi"])
        driver_measurement, _ = common.timed_command(
            driver_argv, directory, env, timeout
        )
        measurement = add_measurements(tex_measurement, driver_measurement)
    else:
        raise SuiteError(f"case {case['id']} has no reference producer")
    pdf = directory / "main.pdf"
    if not pdf.is_file() or not pdf.read_bytes().startswith(b"%PDF-"):
        raise SuiteError(f"reference produced no PDF for {case['id']}")
    measurement.update({"pdf_bytes": pdf.stat().st_size, "pdf_sha256": common.sha256(pdf)})
    return measurement, pdf


def extract_text(path: Path, output: Path) -> tuple[str, dict[str, object]]:
    common.run_checked(["pdftotext", "-layout", str(path), str(output)])
    text = output.read_text(encoding="utf-8", errors="replace")
    return text, {
        "bytes": output.stat().st_size,
        "sha256": common.sha256(output),
    }


def require_markers(
    text: str, case: dict[str, object], pages: int
) -> dict[str, object]:
    profile = str(case["profile"])
    markers: tuple[str, ...]
    sequence_pattern: str | None = None
    if profile == "prose":
        markers = ("Synthetic PDF qualification", "Generated section", "affine office")
        sequence_pattern = r"Generated section\s+(\d+)"
    elif profile == "classic-fonts":
        markers = ("Classic font matrix", "Classic font qualification page", "office affine")
        sequence_pattern = r"Classic font matrix\s+(\d+)"
    elif profile == "annotations":
        markers = ("Annotation page", "Alpha URI", "another internal destination")
        sequence_pattern = r"Annotation page\s+(\d+)"
    elif profile == "graphics":
        markers = ("Raster and vector page", "Deterministic RGB PNG", "Original vector EPS")
        sequence_pattern = r"Raster and vector page\s+(\d+)"
    elif profile == "scale":
        markers = ("aiTeX scale qualification page",)
        sequence_pattern = r"aiTeX scale qualification page\s+(\d+)"
    elif profile == "unicode":
        markers = (
            "Ordinary UTF-8 and OpenType shaping",
            "office affine",
            "नमस्ते",
            "日本語中文测试",
            "ภาษาไทย",
            "ສະບາຍດີ",
            "minimum variable width office",
        )
    elif profile == "positioned":
        # Poppler currently separates some positioned Indic and Southeast Asian
        # clusters. The driver tests independently verify exact logical
        # ActualText; these markers still catch missing runs in the rendered PDF.
        markers = (
            "office affine",
            "नमस्त",
            "रत",
            "日本語中文测试",
            "ภาษาไทย",
            "ສະບາຍ",
            "ພາສາລາວ",
            "variable width",
            "variable office",
            "٠",
        )
    else:
        raise SuiteError(f"unknown validation profile: {profile}")
    missing = [marker for marker in markers if marker not in text]
    if missing:
        raise SuiteError(f"{case['id']} extraction is missing: {', '.join(missing)}")
    result: dict[str, object] = {"markers": list(markers)}
    if sequence_pattern is not None:
        sequence = [int(value) for value in re.findall(sequence_pattern, text)]
        expected = list(range(1, pages + 1))
        if sequence != expected:
            raise SuiteError(
                f"{case['id']} extracted sequence differs: "
                f"{len(sequence)} entries rather than {pages}"
            )
        result["sequence_entries"] = len(sequence)
        result["sequence_sha256"] = hashlib.sha256(
            ",".join(map(str, sequence)).encode()
        ).hexdigest()
    return result


def selected_page_numbers(pages: int, policy: str) -> list[int] | None:
    if policy == "all":
        return None
    return sorted({1, (pages + 1) // 2, pages})


def render_pages(
    path: Path,
    directory: Path,
    pages: int,
    policy: str,
    timeout: int,
) -> tuple[dict[str, object], list[Path]]:
    selected = selected_page_numbers(pages, policy)
    if selected is None:
        result, paths = common.render_poppler(path, directory, timeout)
        result["selection"] = "all"
        return result, paths
    directory.mkdir(parents=True)
    paths = []
    for page in selected:
        output = directory / f"page-{page:05d}"
        common.run_checked(
            [
                "pdftoppm",
                "-gray",
                "-r",
                "72",
                "-f",
                str(page),
                "-l",
                str(page),
                "-singlefile",
                str(path),
                str(output),
            ],
            timeout,
        )
        rendered = output.with_suffix(".pgm")
        if not rendered.is_file():
            raise SuiteError(f"Poppler did not render page {page} of {path}")
        paths.append(rendered)
    return {
        "pages": len(paths),
        "document_pages": pages,
        "selected_pages": selected,
        "selection": "sampled",
        "sha256": common.combined_sha256(paths),
    }, paths


def validate(
    path: Path,
    directory: Path,
    case: dict[str, object],
    pages: int,
    timeout: int,
    render: bool,
) -> tuple[dict[str, object], list[Path]]:
    directory.mkdir(parents=True)
    info = common.pdf_info(path)
    if info["pages"] != pages:
        raise SuiteError(
            f"{case['id']} has {info['pages']} pages; expected {pages}"
        )
    fonts = common.font_info(path)
    font_rows = "\n".join(str(row) for row in fonts["rows"])
    profile = str(case["profile"])
    if profile == "classic-fonts" and int(fonts["count"]) < 15:
        raise SuiteError("classic font matrix embedded fewer than 15 font resources")
    if profile == "unicode" and (
        int(fonts["count"]) < 10
        or font_rows.count("CID TrueType") < 6
        or "Type 1" not in font_rows
    ):
        raise SuiteError("Unicode CLI font inventory is incomplete")
    if profile == "positioned" and (
        int(fonts["count"]) < 12
        or "Identity-V" not in font_rows
        or "CID Type 0C" not in font_rows
        or "CID TrueType" not in font_rows
    ):
        raise SuiteError("positioned native font inventory is incomplete")
    text, text_info = extract_text(path, directory / "text.txt")
    text_info.update(require_markers(text, case, pages - int(case.get("expected_pages_offset", 0))))
    common.run_checked(
        ["gs", "-q", "-dNOPAUSE", "-dBATCH", "-sDEVICE=nullpage", str(path)],
        timeout,
    )
    validators: dict[str, bool | None] = {"ghostscript": True}
    for name, command in (
        ("mutool", ["mutool", "info", str(path)]),
        ("qpdf", ["qpdf", "--check", str(path)]),
    ):
        if shutil.which(command[0]):
            common.run_checked(command, timeout)
            validators[name] = True
        else:
            validators[name] = None
    links = None
    if case["profile"] in {"prose", "annotations"}:
        links = common.link_info(path)
    if case["profile"] == "positioned" and str(info.get("tagged")).lower() != "yes":
        raise SuiteError("positioned qualification PDF is not tagged")
    result: dict[str, object] = {
        "info": info,
        "fonts": fonts,
        "links": links,
        "text": text_info,
        "validators": validators,
        "quartz": common.render_quartz(path, directory / "quartz"),
    }
    rasters: list[Path] = []
    if render:
        result["poppler"], rasters = render_pages(
            path, directory / "poppler", pages, str(case["raster"]), timeout
        )
    return result, rasters


def annotation_breakdown(path: Path, case: dict[str, object], pages: int) -> dict[str, int] | None:
    profile = case["profile"]
    if profile == "prose":
        requested = pages - int(case.get("expected_pages_offset", 0))
        return common.classic_annotation_info(path, requested)
    if profile != "annotations":
        return None
    data = path.read_bytes()
    links = data.count(b"/Subtype /Link")
    red = data.count(b"/Border [0 0 1] /C [1 0 0]")
    cyan = data.count(b"/Border [0 0 1] /C [0 1 1]")
    if links < pages * 6 or red < pages * 3 or cyan < pages * 3:
        raise SuiteError(
            f"annotation classic PDF lost boxed links: links={links}, red={red}, cyan={cyan}"
        )
    return {"link_annotations": links, "red_boxed_links": red, "cyan_boxed_links": cyan}


def summarize(samples: list[dict[str, object]], deterministic_sha: str) -> dict[str, object]:
    result = common.summarize(samples)
    result["deterministic_pdf"] = all(
        sample["pdf_sha256"] == deterministic_sha for sample in samples
    )
    return result


def run_case(
    case: dict[str, object],
    work: Path,
    args: argparse.Namespace,
) -> dict[str, object]:
    case_id = str(case["id"])
    pages = expected_pages(case, args.quick)
    sample_count = int(case["quick_samples"] if args.quick else case["samples"])
    candidate_samples = []
    candidate_pdf = None
    for index in range(sample_count):
        sample, candidate_pdf = candidate_measurement(
            work / f"candidate-{index}",
            case,
            args.quick,
            args.binary,
            args.format,
            args.positioned_binary,
            args.timeout,
        )
        candidate_samples.append(sample)
    assert candidate_pdf is not None
    deterministic_sha = str(candidate_samples[0]["pdf_sha256"])
    determinism_probe = None
    if sample_count == 1:
        probe, _ = candidate_measurement(
            work / "candidate-determinism",
            case,
            args.quick,
            args.binary,
            args.format,
            args.positioned_binary,
            args.timeout,
        )
        deterministic_sha = str(probe["pdf_sha256"])
        determinism_probe = {
            "pdf_bytes": probe["pdf_bytes"],
            "pdf_sha256": probe["pdf_sha256"],
        }
    candidate_summary = summarize(candidate_samples, deterministic_sha)
    candidate_summary["determinism_probe"] = determinism_probe
    if not candidate_summary["deterministic_pdf"]:
        raise SuiteError(f"{case_id} candidate PDF is not byte deterministic")

    classic_sample, classic_pdf = candidate_measurement(
        work / "candidate-classic-xref",
        case,
        args.quick,
        args.binary,
        args.format,
        args.positioned_binary,
        args.timeout,
        classic_xref=True,
    )
    candidate_breakdown = pdf_size.analyze(classic_pdf)
    candidate_breakdown["path"] = f"temporary/{case_id}/candidate-classic-xref.pdf"
    if case["profile"] == "graphics":
        images = candidate_breakdown["images"]
        signatures = {
            (image["color_space"], image["bits_per_component"])
            for image in images
        }
        required = {
            ("/DeviceRGB", 8),
            ("/DeviceGray", 1),
            ("/DeviceGray", 8),
        }
        if not required.issubset(signatures) or not any(
            str(image["color_space"]).startswith("[/Indexed ")
            and image["bits_per_component"] == 2
            and str(image["mask"]).startswith("/Mask [")
            for image in images
        ):
            raise SuiteError("graphics matrix lost a native PNG color or bit-depth path")
        if len(images) != 6 or len(candidate_breakdown["forms"]) != 1:
            raise SuiteError(
                "graphics matrix did not share repeated image/EPS resources: "
                f"images={len(images)}, forms={len(candidate_breakdown['forms'])}"
            )
    candidate_validation, candidate_rasters = validate(
        candidate_pdf,
        work / "validation-candidate",
        case,
        pages,
        args.timeout,
        not args.skip_raster,
    )
    candidate_annotations = annotation_breakdown(classic_pdf, case, pages)

    stock_samples = []
    stock_summary = None
    stock_validation = None
    comparison = None
    stock_classic_sample = None
    stock_breakdown = None
    if case.get("reference") is not None and not args.skip_stock:
        stock_pdf = None
        for index in range(sample_count):
            sample, stock_pdf = reference_measurement(
                work / f"reference-{index}", case, args.quick, args.timeout
            )
            stock_samples.append(sample)
        assert stock_pdf is not None
        stock_summary = common.summarize(stock_samples)
        stock_classic_sample, stock_classic_pdf = reference_measurement(
            work / "reference-classic-xref",
            case,
            args.quick,
            args.timeout,
            classic_xref=True,
        )
        stock_breakdown = pdf_size.analyze(stock_classic_pdf)
        stock_breakdown["path"] = f"temporary/{case_id}/reference-classic-xref.pdf"
        stock_validation, stock_rasters = validate(
            stock_pdf,
            work / "validation-reference",
            case,
            pages,
            args.timeout,
            not args.skip_raster,
        )
        if candidate_validation["links"] != stock_validation["links"]:
            raise SuiteError(f"{case_id} URI annotation inventories differ")
        raster = (
            common.compare_rasters(
                stock_rasters,
                candidate_rasters,
                max_normalized_mae=float(
                    case.get(
                        "raster_max_normalized_mae",
                        args.gates["raster_max_normalized_mae"],
                    )
                ),
                fuzz_fraction=float(args.gates["raster_fuzz_fraction"]),
                max_outside_fraction=float(
                    case.get(
                        "raster_max_outside_fraction",
                        args.gates["raster_max_outside_fraction"],
                    )
                ),
            )
            if not args.skip_raster
            else None
        )
        ratio = int(candidate_samples[0]["pdf_bytes"]) / int(stock_samples[0]["pdf_bytes"])
        maximum = float(args.gates["max_comparable_pdf_size_ratio"])
        if ratio > maximum:
            raise SuiteError(
                f"{case_id} PDF size ratio {ratio:.3f} exceeds {maximum:.3f}"
            )
        comparison = {
            "pdf_size_ratio": round(ratio, 6),
            "poppler_raster": raster,
            "classic_xref_breakdown": common.compare_breakdowns(
                candidate_breakdown, stock_breakdown
            ),
        }

    if args.preview_dir is not None and candidate_rasters:
        target = args.preview_dir / case_id
        target.mkdir(parents=True, exist_ok=True)
        selected = {
            "first": candidate_rasters[0],
            "middle": candidate_rasters[len(candidate_rasters) // 2],
            "last": candidate_rasters[-1],
        }
        for label, source in selected.items():
            shutil.copy2(source, target / f"{label}.pgm")

    return {
        "id": case_id,
        "kind": case["kind"],
        "profile": case["profile"],
        "source": case["source"],
        "source_sha256": common.sha256(ROOT / str(case["source"])),
        "reference": case.get("reference"),
        "pages": pages,
        "samples_requested": sample_count,
        "performance_gates": {
            key: float(case.get(key, args.gates[key]))
            for key in (
                "max_matching_environment_pdf_stage_absolute_ms",
                "max_matching_environment_pdf_assembly_absolute_ms",
                "max_matching_environment_rss_absolute_kib",
            )
        },
        "raster_policy": case["raster"],
        "candidate": {
            **candidate_summary,
            "validation": candidate_validation,
            "classic_xref_sample": classic_sample,
            "classic_xref_breakdown": candidate_breakdown,
            "classic_xref_annotations": candidate_annotations,
        },
        "stock": (
            {
                **stock_summary,
                "validation": stock_validation,
                "classic_xref_sample": stock_classic_sample,
                "classic_xref_breakdown": stock_breakdown,
            }
            if stock_summary is not None
            else None
        ),
        "comparison": comparison,
    }


def environment() -> dict[str, object]:
    values = {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "rustc": common.command_line(["rustc", "--version"]),
        "pdflatex": common.command_line(["pdflatex", "--version"]),
        "latexmk": common.command_line(["latexmk", "--version"]),
        "dvipdfmx": common.command_line(["dvipdfmx", "--version"]),
        "pdfinfo": common.command_line(["pdfinfo", "-v"]),
        "ghostscript": common.command_line(["gs", "--version"]),
    }
    encoded = json.dumps(values, sort_keys=True, separators=(",", ":")).encode()
    values["fingerprint_sha256"] = hashlib.sha256(encoded).hexdigest()
    return values


def median_value(case: dict[str, object], key: str) -> float | None:
    value = case["candidate"].get(key)
    return float(value) if value is not None else None


def baseline_comparison(
    result: dict[str, object], baseline: dict[str, object], gates: dict[str, object]
) -> dict[str, object]:
    current_cases = {case["id"]: case for case in result["cases"]}
    baseline_cases = {case["id"]: case for case in baseline.get("cases", [])}
    manifest_unchanged = (
        result["suite"]["manifest_sha256"]
        == baseline.get("suite", {}).get("manifest_sha256")
    )
    # Gate policy is stored in the suite manifest alongside the workloads. A
    # policy-only edit must not discard otherwise comparable measurements; the
    # per-case check below proves that every input affecting the measurement is
    # unchanged before applying the current policy.
    inputs_unchanged = result["suite"]["corpus_lock_sha256"] == baseline.get(
        "suite", {}
    ).get("corpus_lock_sha256")
    mode_matching = result["suite"]["quick"] == baseline.get("suite", {}).get(
        "quick"
    )
    same_environment = (
        result["environment"]["fingerprint_sha256"]
        == baseline.get("environment", {}).get("fingerprint_sha256")
    )
    if not inputs_unchanged or not mode_matching:
        return {
            "inputs_unchanged": inputs_unchanged,
            "manifest_unchanged": manifest_unchanged,
            "mode_matching": mode_matching,
            "matching_environment": same_environment,
            "cases": {},
        }
    comparisons = {}
    for case_id, current in current_cases.items():
        previous = baseline_cases.get(case_id)
        if (
            previous is None
            or previous.get("source_sha256") != current["source_sha256"]
            or previous.get("pages") != current["pages"]
            or previous.get("kind") != current["kind"]
            or previous.get("profile") != current["profile"]
            or previous.get("source") != current["source"]
            or previous.get("samples_requested") != current["samples_requested"]
        ):
            continue
        current_bytes = int(current["candidate"]["samples"][0]["pdf_bytes"])
        previous_bytes = int(previous["candidate"]["samples"][0]["pdf_bytes"])
        size_regression = current_bytes / previous_bytes - 1.0
        if size_regression > float(gates["max_pdf_size_regression"]):
            raise SuiteError(
                f"{case_id} grew {size_regression:.1%} from the locked baseline"
            )
        item: dict[str, object] = {
            "pdf_size_regression": round(size_regression, 6),
            "matching_environment": same_environment,
        }
        if same_environment:
            for key, gate_name, absolute_gate_name in (
                (
                    "median_pdf_total_ms",
                    "max_matching_environment_pdf_stage_regression",
                    "max_matching_environment_pdf_stage_absolute_ms",
                ),
                (
                    "median_pdf_assembly_ms",
                    "max_matching_environment_pdf_assembly_regression",
                    "max_matching_environment_pdf_assembly_absolute_ms",
                ),
                (
                    "median_max_rss_kib",
                    "max_matching_environment_rss_regression",
                    "max_matching_environment_rss_absolute_kib",
                ),
            ):
                current_value = median_value(current, key)
                previous_value = median_value(previous, key)
                if current_value is None or previous_value in {None, 0.0}:
                    continue
                regression = current_value / previous_value - 1.0
                absolute_increase = current_value - previous_value
                item[key + "_regression"] = round(regression, 6)
                item[key + "_absolute_increase"] = round(absolute_increase, 3)
                exceeds_absolute_gate = absolute_gate_name is None or (
                    absolute_increase
                    > float(
                        current.get("performance_gates", {}).get(
                            absolute_gate_name, gates[absolute_gate_name]
                        )
                    )
                )
                if regression > float(gates[gate_name]) and exceeds_absolute_gate:
                    raise SuiteError(
                        f"{case_id} {key} regressed {regression:.1%} on a matching environment"
                    )
        comparisons[case_id] = item
    return {
        "inputs_unchanged": True,
        "manifest_unchanged": manifest_unchanged,
        "mode_matching": True,
        "matching_environment": same_environment,
        "cases": comparisons,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--format", type=Path, default=DEFAULT_FORMAT)
    parser.add_argument("--positioned-binary", type=Path, default=DEFAULT_POSITIONED_BINARY)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--case", action="append", dest="case_ids")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--skip-stock", action="store_true")
    parser.add_argument("--skip-raster", action="store_true")
    parser.add_argument("--list", action="store_true", help="list cases after verifying manifests")
    parser.add_argument("--preview-dir", type=Path)
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    args.format = args.format.resolve()
    args.positioned_binary = args.positioned_binary.resolve()
    try:
        suite = verify_suite()
        corpus_lock = common.verify_corpus_lock()
        verify_case_sources_locked(suite, corpus_lock)
        all_cases = suite["cases"]
        if args.case_ids:
            selected = set(args.case_ids)
            cases = [case for case in all_cases if case["id"] in selected]
            unknown = selected - {str(case["id"]) for case in cases}
            if unknown:
                raise SuiteError("unknown cases: " + ", ".join(sorted(unknown)))
        else:
            cases = all_cases
        if args.list:
            for case in cases:
                print(f"{case['id']}\t{case['profile']}\t{case.get('reference') or 'self'}")
            return 0
        if args.timeout < 1:
            parser.error("--timeout must be positive")
        if any(case["kind"] != "positioned" for case in cases):
            if not args.binary.is_file() or not os.access(args.binary, os.X_OK):
                parser.error(f"build the release binary first: {args.binary}")
            if not args.format.is_file():
                parser.error(f"build the aiTeX format first: {args.format}")
        if any(case["kind"] == "positioned" for case in cases):
            if not args.positioned_binary.is_file() or not os.access(args.positioned_binary, os.X_OK):
                parser.error(
                    "build the positioned producer first: "
                    "cargo build --release -p aitex-driver --example pdf_qualification"
                )
        require_tools(cases, args.skip_stock)
        args.gates = suite["gates"]
        commit, dirty = common.git_identity()
        env = environment()
        results = []
        with tempfile.TemporaryDirectory(prefix="aitex-pdf-suite.") as raw:
            work = Path(raw)
            for case in cases:
                print(f"pdf suite: {case['id']}", flush=True)
                results.append(run_case(case, work / str(case["id"]), args))

        ratios = [
            float(case["comparison"]["pdf_size_ratio"])
            for case in results
            if case["comparison"] is not None
        ]
        aggregate = {
            "comparable_cases": len(ratios),
            "geometric_mean_pdf_size_ratio": (
                round(math.exp(statistics.fmean(math.log(value) for value in ratios)), 6)
                if ratios
                else None
            ),
            "maximum_pdf_size_ratio": max(ratios, default=None),
            "all_gates_passed": True,
        }
        result: dict[str, object] = {
            "schema": SCHEMA,
            "suite": {
                "manifest": str(SUITE.relative_to(ROOT)),
                "manifest_sha256": common.sha256(SUITE),
                "corpus_lock": str(CORPUS_LOCK.relative_to(ROOT)),
                "corpus_lock_sha256": common.sha256(CORPUS_LOCK),
                "name": suite["name"],
                "quick": args.quick,
                "contains_third_party_document_text": False,
                "generated_assets": (
                    generated_asset_identity()
                    if any(case["profile"] == "graphics" for case in cases)
                    else {}
                ),
            },
            "compiler": {
                "git_commit": commit,
                "git_dirty": dirty,
                "binary_sha256": common.sha256(args.binary) if args.binary.is_file() else None,
                "format_sha256": common.sha256(args.format) if args.format.is_file() else None,
                "positioned_binary_sha256": (
                    common.sha256(args.positioned_binary)
                    if args.positioned_binary.is_file()
                    else None
                ),
            },
            "environment": env,
            "gates": suite["gates"],
            "cases": results,
            "aggregate": aggregate,
        }
        if args.baseline is not None:
            result["baseline"] = baseline_comparison(
                result, load_json(args.baseline), suite["gates"]
            )
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    except (common.QualificationError, SuiteError, OSError, ValueError) as error:
        print(f"pdf suite: {error}", file=sys.stderr)
        return 1

    print(f"pdf suite: wrote {args.output}")
    if ratios:
        print(
            f"  {len(ratios)} comparable cases: geometric mean "
            f"{aggregate['geometric_mean_pdf_size_ratio']:.3f}x, "
            f"maximum {aggregate['maximum_pdf_size_ratio']:.3f}x"
        )
    checks = "determinism, extraction, and validation"
    if not args.skip_raster:
        checks = "determinism, extraction, rendering, and validation"
    print(f"  {len(results)} cases passed {checks}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
