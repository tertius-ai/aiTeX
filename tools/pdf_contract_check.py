#!/usr/bin/env python3
"""Audit the checked-in PDF/font/language qualification without rebuilding it.

The full suite is intentionally expensive and host-tool dependent.  This gate
keeps its inputs and evidence honest on every done-check: fixture hashes,
coverage rows, measurements, correctness oracles, and the stock size ceiling
must all remain mutually consistent.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
CORPUS_PATH = ROOT / "qualification/pdf-corpus-v1.json"
MANIFEST_PATH = ROOT / "qualification/pdf-suite-v1.json"
RESULTS_PATH = ROOT / "qualification/pdf-suite-v1.results.json"

EXPECTED_CASES = {
    "classic-prose": ("tex", "prose", "pdftex", "all"),
    "classic-fonts": ("tex", "classic-fonts", "pdftex", "all"),
    "annotations": ("tex", "annotations", "pdftex", "all"),
    "graphics": ("tex", "graphics", "dvipdfmx", "all"),
    "high-page-count": ("tex", "scale", "pdftex", "sampled"),
    "native-unicode-cli": ("unicode", "unicode", None, "all"),
    "native-positioned-fonts": ("positioned", "positioned", None, "all"),
}
REQUIRED_CORPUS_IDS = {
    "synthetic-book-v1",
    "classic-font-workload-v1",
    "annotation-workload-v1",
    "graphics-workload-v1",
    "graphics-asset-generator-v1",
    "graphics-vector-v1",
    "many-page-scale",
    "native-multilingual",
    "native-unicode-product",
    "native-positioned-workload-v1",
    "locked-opentype-fixtures",
    "font-noto-latin",
    "font-amiri-arabic",
    "font-noto-devanagari",
    "font-noto-hebrew",
    "font-noto-cjk-sc",
    "font-noto-thai",
    "font-noto-lao",
    "font-junicode-variable",
    "font-source-serif-cff",
    "font-source-serif-cff2-variable",
    "font-ofl-license",
}
PRIVATE_MARKERS = ("analytic stacks", "clausen", "scholze", "dropbox")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")


class ContractError(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"cannot read {path.relative_to(ROOT)}: {error}") from error
    require(isinstance(value, dict), f"{path.relative_to(ROOT)} is not a JSON object")
    return value


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def check_corpus(corpus: dict[str, Any]) -> dict[str, dict[str, Any]]:
    require(corpus.get("schema") == 1, "PDF corpus schema is not 1")
    entries = corpus.get("entries")
    require(isinstance(entries, list) and entries, "PDF corpus has no entries")

    by_id: dict[str, dict[str, Any]] = {}
    by_path: dict[str, dict[str, Any]] = {}
    for entry in entries:
        require(isinstance(entry, dict), "PDF corpus contains a non-object entry")
        entry_id = entry.get("id")
        relative = entry.get("path")
        require(isinstance(entry_id, str) and entry_id, "PDF corpus entry has no id")
        require(entry_id not in by_id, f"duplicate PDF corpus id: {entry_id}")
        require(isinstance(relative, str) and relative, f"{entry_id}: missing path")
        candidate = Path(relative)
        require(not candidate.is_absolute() and ".." not in candidate.parts,
                f"{entry_id}: path escapes repository")
        require(relative not in by_path, f"duplicate PDF corpus path: {relative}")
        require(all(marker not in relative.lower() for marker in PRIVATE_MARKERS),
                f"{entry_id}: private-source marker in path")
        path = ROOT / candidate
        require(path.is_file(), f"{entry_id}: fixture is missing: {relative}")
        expected_hash = entry.get("sha256")
        require(isinstance(expected_hash, str) and HEX_64.fullmatch(expected_hash) is not None,
                f"{entry_id}: invalid SHA-256")
        require(sha256(path) == expected_hash, f"{entry_id}: fixture hash changed")
        for field in ("provenance", "license", "gate"):
            require(isinstance(entry.get(field), str) and entry[field].strip(),
                    f"{entry_id}: missing {field}")
        by_id[entry_id] = entry
        by_path[relative] = entry

    missing = sorted(REQUIRED_CORPUS_IDS - by_id.keys())
    require(not missing, f"PDF corpus lost required coverage: {', '.join(missing)}")
    return by_path


def check_manifest(manifest: dict[str, Any], corpus_paths: dict[str, dict[str, Any]]) -> dict[str, dict[str, Any]]:
    require(manifest.get("schema") == 1, "PDF suite schema is not 1")
    require(manifest.get("name") == "aitex-varied-pdf-qualification-v1",
            "unexpected PDF suite name")
    gates = manifest.get("gates", {})
    require(gates.get("max_comparable_pdf_size_ratio") == 1.25,
            "classic PDF size ceiling is not 1.25x")

    cases = manifest.get("cases")
    require(isinstance(cases, list), "PDF suite cases are missing")
    by_id: dict[str, dict[str, Any]] = {}
    for case in cases:
        require(isinstance(case, dict), "PDF suite contains a non-object case")
        case_id = case.get("id")
        require(case_id in EXPECTED_CASES, f"unexpected PDF suite case: {case_id}")
        require(case_id not in by_id, f"duplicate PDF suite case: {case_id}")
        kind, profile, reference, raster = EXPECTED_CASES[case_id]
        require((case.get("kind"), case.get("profile"), case.get("reference"), case.get("raster"))
                == (kind, profile, reference, raster),
                f"{case_id}: qualification contract changed")
        source = case.get("source")
        require(source in corpus_paths, f"{case_id}: source is not in the locked corpus")
        by_id[case_id] = case

    require(set(by_id) == set(EXPECTED_CASES), "PDF suite does not contain exactly seven required cases")
    scale = by_id["high-page-count"]
    require(scale.get("pages") == 10_000, "scale case is not 10,000 pages")
    return by_id


def check_metrics(case_id: str, candidate: dict[str, Any]) -> None:
    for field in ("median_wall_seconds", "median_max_rss_kib", "median_pdf_total_ms"):
        require(isinstance(candidate.get(field), (int, float)) and candidate[field] > 0,
                f"{case_id}: missing positive {field}")
    # GNU and BSD time report process CPU at centisecond resolution. The
    # standalone positioned writer can complete below that floor, where 0.0
    # is a measured value rather than a missing sample.
    cpu = candidate.get("median_cpu_seconds")
    require(isinstance(cpu, (int, float)) and cpu >= 0,
            f"{case_id}: missing nonnegative median_cpu_seconds")
    # The direct positioned API is one measured writer boundary.  Its total is
    # the replacement acceptance criterion for a separately unavailable
    # assembly subphase; all CLI cases must expose that subphase explicitly.
    assembly = candidate.get("median_pdf_assembly_ms")
    if case_id == "native-positioned-fonts":
        require(assembly is None, f"{case_id}: expected explicit unavailable assembly subphase")
    else:
        require(isinstance(assembly, (int, float)) and assembly > 0,
                f"{case_id}: missing positive median_pdf_assembly_ms")


def check_validation(case_id: str, validation: dict[str, Any], *, expected_pages: int) -> None:
    info = validation.get("info", {})
    require(info.get("pages") == expected_pages, f"{case_id}: wrong validated page count")
    fonts = validation.get("fonts", {})
    rows = fonts.get("rows")
    require(isinstance(rows, list) and rows, f"{case_id}: no embedded-font inventory")
    require(fonts.get("count") == len(rows), f"{case_id}: font count does not match inventory")
    for row in rows:
        fields = str(row).split()
        require(len(fields) >= 7, f"{case_id}: malformed pdffonts inventory row")
        embedded, subset, unicode_map = fields[-5:-2]
        require((embedded, subset) == ("yes", "yes"),
                f"{case_id}: a font is not embedded and subset")
        require(unicode_map in {"yes", "no"},
                f"{case_id}: malformed pdffonts Unicode-map flag")

    text = validation.get("text", {})
    require(isinstance(text.get("bytes"), int) and text["bytes"] > 0,
            f"{case_id}: empty text extraction")
    require(isinstance(text.get("markers"), list) and text["markers"],
            f"{case_id}: no extraction markers")
    require(HEX_64.fullmatch(str(text.get("sha256"))) is not None,
            f"{case_id}: missing extraction digest")

    poppler = validation.get("poppler", {})
    require(isinstance(poppler.get("pages"), int) and poppler["pages"] > 0,
            f"{case_id}: no Poppler pages checked")
    require(HEX_64.fullmatch(str(poppler.get("sha256"))) is not None,
            f"{case_id}: missing Poppler raster digest")
    validators = validation.get("validators", {})
    require(validators.get("ghostscript") is True, f"{case_id}: Ghostscript did not accept PDF")
    for optional in ("mutool", "qpdf"):
        require(validators.get(optional) in (True, None),
                f"{case_id}: {optional} rejected PDF")
    quartz = validation.get("quartz", {})
    require(isinstance(quartz.get("available"), bool),
            f"{case_id}: Quartz availability was not recorded")
    if quartz["available"]:
        require(HEX_64.fullmatch(str(quartz.get("first_page_sha256"))) is not None,
                f"{case_id}: missing Quartz raster digest")


def check_one_result(
    result: dict[str, Any], manifest_case: dict[str, Any], size_ceiling: float
) -> None:
    case_id = result.get("id")
    for field in ("kind", "profile", "reference", "source"):
        require(result.get(field) == manifest_case.get(field), f"{case_id}: result {field} drifted")
    source = ROOT / str(result["source"])
    require(result.get("source_sha256") == sha256(source), f"{case_id}: result source hash is stale")

    expected_pages = manifest_case.get("expected_pages")
    if expected_pages is None:
        expected_pages = int(manifest_case["pages"]) + int(manifest_case.get("expected_pages_offset", 0))
    require(result.get("pages") == expected_pages, f"{case_id}: result page count drifted")
    require(result.get("raster_policy") == manifest_case.get("raster"),
            f"{case_id}: raster policy drifted")

    candidate = result.get("candidate")
    require(isinstance(candidate, dict), f"{case_id}: candidate result is missing")
    require(candidate.get("deterministic_pdf") is True, f"{case_id}: PDF is not deterministic")
    samples = candidate.get("samples")
    require(isinstance(samples, list) and len(samples) == result.get("samples_requested"),
            f"{case_id}: incomplete measured samples")
    hashes = {sample.get("pdf_sha256") for sample in samples}
    sizes = {sample.get("pdf_bytes") for sample in samples}
    probe = candidate.get("determinism_probe")
    if probe is not None:
        hashes.add(probe.get("pdf_sha256"))
        sizes.add(probe.get("pdf_bytes"))
    require(len(hashes) == 1 and HEX_64.fullmatch(str(next(iter(hashes)))) is not None,
            f"{case_id}: clean PDF hashes differ or are invalid")
    require(len(sizes) == 1 and isinstance(next(iter(sizes)), int),
            f"{case_id}: clean PDF byte counts differ")
    check_metrics(case_id, candidate)
    check_validation(case_id, candidate.get("validation", {}), expected_pages=expected_pages)

    breakdown = candidate.get("classic_xref_breakdown", {})
    categories = breakdown.get("categories", {})
    require({"font_program", "page_content", "to_unicode"} <= categories.keys(),
            f"{case_id}: incomplete object/stream/font breakdown")
    require(isinstance(breakdown.get("pdf_bytes"), int) and breakdown["pdf_bytes"] > 0,
            f"{case_id}: missing inspectable PDF byte accounting")

    stock = result.get("stock")
    comparison = result.get("comparison")
    if manifest_case.get("reference") is None:
        require(stock is None and comparison is None,
                f"{case_id}: native case has a misleading stock comparison")
    else:
        require(isinstance(stock, dict) and stock.get("deterministic_pdf") is True,
                f"{case_id}: stock reference is missing or nondeterministic")
        check_validation(case_id + " stock", stock.get("validation", {}), expected_pages=expected_pages)
        require(isinstance(comparison, dict), f"{case_id}: stock comparison is missing")
        ratio = comparison.get("pdf_size_ratio")
        require(isinstance(ratio, (int, float)) and ratio <= size_ceiling,
                f"{case_id}: {ratio}x exceeds {size_ceiling}x stock size ceiling")
        raster = comparison.get("poppler_raster", {})
        require(raster.get("equivalent") is True, f"{case_id}: Poppler raster mismatch")
        require(isinstance(comparison.get("classic_xref_breakdown"), dict),
                f"{case_id}: stock object/stream/font comparison is missing")


def check_specialized_coverage(results: dict[str, dict[str, Any]]) -> None:
    graphics_categories = results["graphics"]["candidate"]["classic_xref_breakdown"]["categories"]
    require({"image", "form"} <= graphics_categories.keys(),
            "graphics case does not account for image and vector-form payloads")

    scale = results["high-page-count"]["candidate"]["validation"]["poppler"]
    require(scale.get("document_pages") == 10_000, "scale validator did not traverse 10,000 pages")
    require(scale.get("selected_pages") == [1, 5_000, 10_000],
            "scale raster did not sample first, middle, and last pages")

    unicode_result = results["native-unicode-cli"]["candidate"]["validation"]
    unicode_fonts = "\n".join(unicode_result["fonts"]["rows"])
    for name in ("NotoSans-Regular", "Amiri-Regular", "NotoSansHebrew", "NotoSansDevanagari",
                 "NotoSansCJK", "NotoSansThai", "NotoSansLao", "JunicodeVF"):
        require(name in unicode_fonts, f"Unicode CLI result lost {name} coverage")
    unicode_markers = "\n".join(unicode_result["text"]["markers"])
    for marker in ("office affine", "नमस्ते", "日本語中文测试", "ภาษาไทย", "ສະບາຍດີ"):
        require(marker in unicode_markers, f"Unicode CLI extraction lost {marker!r}")

    positioned = results["native-positioned-fonts"]["candidate"]["validation"]
    positioned_fonts = "\n".join(positioned["fonts"]["rows"])
    for marker in ("CID TrueType", "CID Type 0C", "Identity-H", "Identity-V"):
        require(marker in positioned_fonts, f"positioned result lost {marker} font coverage")
    require(str(positioned["info"].get("tagged")).lower() == "yes",
            "positioned semantic foundation is not tagged")


def check_results(
    results: dict[str, Any], manifest: dict[str, Any], manifest_cases: dict[str, dict[str, Any]]
) -> None:
    require(results.get("schema") == 1, "PDF results schema is not 1")
    require(results.get("gates") == manifest.get("gates"), "result gate policy is stale")
    suite = results.get("suite", {})
    require(suite.get("name") == manifest.get("name"), "result suite name is stale")
    require(suite.get("quick") is False, "checked-in result is only the quick matrix")
    require(suite.get("contains_third_party_document_text") is False,
            "result admits third-party document text")
    require(suite.get("manifest") == "qualification/pdf-suite-v1.json",
            "result points at the wrong suite manifest")
    require(suite.get("corpus_lock") == "qualification/pdf-corpus-v1.json",
            "result points at the wrong corpus lock")
    require(suite.get("manifest_sha256") == sha256(MANIFEST_PATH), "result manifest hash is stale")
    require(suite.get("corpus_lock_sha256") == sha256(CORPUS_PATH), "result corpus hash is stale")
    require(all(marker not in json.dumps(suite).lower() for marker in PRIVATE_MARKERS),
            "result suite metadata contains a private-source marker")

    compiler = results.get("compiler", {})
    require(compiler.get("git_dirty") is False, "qualification compiler was built from dirty sources")
    require(re.fullmatch(r"[0-9a-f]{40}", str(compiler.get("git_commit"))) is not None,
            "qualification compiler commit is invalid")
    for field in ("binary_sha256", "positioned_binary_sha256", "format_sha256"):
        require(HEX_64.fullmatch(str(compiler.get(field))) is not None,
                f"qualification compiler {field} is invalid")

    cases = results.get("cases")
    require(isinstance(cases, list), "PDF results contain no cases")
    by_id = {case.get("id"): case for case in cases if isinstance(case, dict)}
    require(len(by_id) == len(cases), "PDF results contain duplicate or invalid cases")
    require(set(by_id) == set(manifest_cases), "PDF results do not cover the complete manifest")
    ceiling = float(manifest["gates"]["max_comparable_pdf_size_ratio"])
    for case_id, manifest_case in manifest_cases.items():
        check_one_result(by_id[case_id], manifest_case, ceiling)
    check_specialized_coverage(by_id)

    aggregate = results.get("aggregate", {})
    require(aggregate.get("all_gates_passed") is True, "aggregate qualification gates failed")
    require(aggregate.get("comparable_cases") == 5, "aggregate stock denominator is not five cases")
    require(aggregate.get("maximum_pdf_size_ratio") <= ceiling,
            "aggregate maximum exceeds stock size ceiling")
    require(0 < aggregate.get("geometric_mean_pdf_size_ratio", 0) <= ceiling,
            "aggregate geometric mean is invalid")


def main() -> int:
    try:
        corpus = load(CORPUS_PATH)
        manifest = load(MANIFEST_PATH)
        results = load(RESULTS_PATH)
        corpus_paths = check_corpus(corpus)
        manifest_cases = check_manifest(manifest, corpus_paths)
        check_results(results, manifest, manifest_cases)
    except (ContractError, OSError, KeyError, TypeError, ValueError) as error:
        print(f"PDF qualification contract: FAIL: {error}", file=sys.stderr)
        return 1

    aggregate = results["aggregate"]
    print(
        "PDF qualification contract: "
        f"7/7 cases pass; 5 stock comparisons, "
        f"{aggregate['geometric_mean_pdf_size_ratio']:.3f}x geometric mean, "
        f"{aggregate['maximum_pdf_size_ratio']:.3f}x maximum"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
