#!/usr/bin/env python3
"""Run the versioned one-shot/complete-build TeX compiler matrix.

The latency protocol is intentionally sequential. Every producer receives a
fresh copy of the same project, an unmeasured warm-up is available to remove
first-touch filesystem noise, and measured producer order is deterministically
counterbalanced. Output inspection and differential oracles are outside the
timed process tree.
"""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import hashlib
import json
import math
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True
import arxiv_holdout as holdout  # noqa: E402
import pdf_fixture_assets  # noqa: E402
from corpus_lock import source_files as locked_source_files  # noqa: E402
from corpus_lock import tree_fingerprint as locked_tree_fingerprint  # noqa: E402


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = ROOT / "qualification" / "compiler-build-matrix-v1.json"
DEFAULT_BINARY = ROOT / "target" / "release" / "aitex"
DEFAULT_FORMAT = ROOT / ".aitex" / "format" / "fmt.afmt"
DEFAULT_OUTPUT = ROOT / "target" / "qualification" / "compiler-build-matrix.json"
FONT_FIXTURES = ROOT / "crates" / "aitex-fonts" / "tests" / "fonts"
SCHEMA = 1
WORKFLOWS = {"one-shot", "full-build"}
ROUTES = {"dvi", "pdf"}
ORACLES = {"exact-dvi", "classic-pdf", "cross-engine-pdf"}
PLACEHOLDERS = {"{binary}", "{format}", "{root}", "{job}"}


class MatrixError(RuntimeError):
    pass


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def atomic_write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, raw_path = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    os.close(descriptor)
    temporary = Path(raw_path)
    try:
        temporary.write_text(
            json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def checkpoint_path(args: argparse.Namespace) -> Path:
    if args.state is not None:
        return args.state.resolve()
    output = args.output.resolve()
    return output.with_name(f"{output.name}.state.json")


def tree_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    for item in sorted(value for value in path.rglob("*") if value.is_file()):
        digest.update(item.relative_to(path).as_posix().encode())
        digest.update(b"\0")
        digest.update(item.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def command_version(argv: list[str]) -> str | None:
    try:
        result = subprocess.run(
            argv,
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return lines[0] if lines else None


def load_manifest(path: Path) -> dict[str, Any]:
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise MatrixError(f"cannot read manifest: {error}") from error
    if not isinstance(manifest, dict) or manifest.get("schema") != SCHEMA:
        raise MatrixError("unsupported compiler/build matrix schema")

    producers = manifest.get("producers")
    profiles = manifest.get("producer_profiles")
    cases = manifest.get("cases")
    rules = manifest.get("comparison_rules")
    tiers = manifest.get("tiers")
    if not isinstance(producers, list) or not producers:
        raise MatrixError("manifest has no producers")
    if not isinstance(profiles, dict) or not profiles:
        raise MatrixError("manifest has no producer profiles")
    if not isinstance(cases, list) or not cases:
        raise MatrixError("manifest has no curated cases")
    if not isinstance(rules, list) or not rules:
        raise MatrixError("manifest has no comparison rules")
    if not isinstance(tiers, dict) or not tiers:
        raise MatrixError("manifest has no tiers")

    producer_ids: set[str] = set()
    for producer in producers:
        if not isinstance(producer, dict):
            raise MatrixError("producer is not an object")
        producer_id = producer.get("id")
        workflow = producer.get("workflow")
        commands = producer.get("commands")
        if not isinstance(producer_id, str) or not producer_id:
            raise MatrixError("producer has no ID")
        if producer_id in producer_ids:
            raise MatrixError(f"duplicate producer: {producer_id}")
        producer_ids.add(producer_id)
        if workflow not in WORKFLOWS:
            raise MatrixError(f"{producer_id}: invalid workflow")
        if not isinstance(commands, dict) or not commands:
            raise MatrixError(f"{producer_id}: no commands")
        for target, command in commands.items():
            if ":" not in target or target.rsplit(":", 1)[1] not in ROUTES:
                raise MatrixError(f"{producer_id}: invalid target {target}")
            if (
                not isinstance(command, list)
                or not command
                or not all(isinstance(part, str) and part for part in command)
            ):
                raise MatrixError(f"{producer_id}: invalid command for {target}")
            used = {
                match.group(0)
                for part in command
                for match in re.finditer(r"\{[^}]+\}", part)
            }
            unknown = used - PLACEHOLDERS
            if unknown:
                raise MatrixError(
                    f"{producer_id}: unknown placeholders: {', '.join(sorted(unknown))}"
                )

    for profile, members in profiles.items():
        if not isinstance(members, list) or not members:
            raise MatrixError(f"producer profile {profile} is empty")
        unknown = set(members) - producer_ids
        if unknown:
            raise MatrixError(
                f"producer profile {profile} names unknown producers: "
                + ", ".join(sorted(unknown))
            )

    case_ids: set[str] = set()
    for case in cases:
        if not isinstance(case, dict):
            raise MatrixError("case is not an object")
        case_id = case.get("id")
        if not isinstance(case_id, str) or not case_id or case_id in case_ids:
            raise MatrixError(f"invalid or duplicate case ID: {case_id}")
        case_ids.add(case_id)
        project = ROOT / str(case.get("project", ""))
        root = project / str(case.get("root", ""))
        if not project.is_dir() or not root.is_file():
            raise MatrixError(f"{case_id}: project or root is absent")
        expected = case.get("tree_sha256")
        actual = tree_sha256(project)
        if expected != actual:
            raise MatrixError(f"{case_id}: tree hash is {actual}, expected {expected}")
        if case.get("producer_profile") not in profiles:
            raise MatrixError(f"{case_id}: unknown producer profile")
        if not set(case.get("routes", ())) <= ROUTES or not case.get("routes"):
            raise MatrixError(f"{case_id}: invalid routes")
        if not set(case.get("workflows", ())) <= WORKFLOWS or not case.get("workflows"):
            raise MatrixError(f"{case_id}: invalid workflows")

    rule_ids: set[str] = set()
    for rule in rules:
        rule_id = rule.get("id") if isinstance(rule, dict) else None
        if not isinstance(rule_id, str) or not rule_id or rule_id in rule_ids:
            raise MatrixError(f"invalid or duplicate comparison rule: {rule_id}")
        rule_ids.add(rule_id)
        if rule.get("route") not in ROUTES or rule.get("workflow") not in WORKFLOWS:
            raise MatrixError(f"{rule_id}: invalid route or workflow")
        if rule.get("oracle") not in ORACLES:
            raise MatrixError(f"{rule_id}: invalid oracle")
        for side in ("reference", "candidate"):
            if rule.get(side) not in producer_ids:
                raise MatrixError(f"{rule_id}: unknown {side} producer")

    for tier_id, tier in tiers.items():
        if not isinstance(tier, dict):
            raise MatrixError(f"tier {tier_id} is not an object")
        selected = tier.get("curated_cases")
        if selected != "all" and (
            not isinstance(selected, list) or not set(selected) <= case_ids
        ):
            raise MatrixError(f"tier {tier_id} has invalid curated cases")
        fraction = tier.get("corpus_fraction")
        if not isinstance(fraction, (int, float)) or not 0 <= fraction <= 1:
            raise MatrixError(f"tier {tier_id} has invalid corpus fraction")
        for field in ("samples", "warmups"):
            value = tier.get(field)
            minimum = 1 if field == "samples" else 0
            if not isinstance(value, int) or value < minimum:
                raise MatrixError(f"tier {tier_id} has invalid {field}")
        if (
            tier.get("corpus_producer_profile") is not None
            and tier["corpus_producer_profile"] not in profiles
        ):
            raise MatrixError(f"tier {tier_id} has an unknown corpus producer profile")

    corpus = manifest.get("locked_corpus")
    if not isinstance(corpus, dict):
        raise MatrixError("manifest has no locked corpus definition")
    if corpus.get("producer_profile") not in profiles:
        raise MatrixError("locked corpus has an unknown producer profile")
    source_lock = ROOT / str(corpus.get("source_lock", ""))
    if not source_lock.is_file():
        raise MatrixError("locked corpus source lock is absent")
    generators = manifest.get("generators", {})
    for generator_id, generator in generators.items():
        path = ROOT / str(generator.get("path", ""))
        if not path.is_file() or file_sha256(path) != generator.get("sha256"):
            raise MatrixError(f"generator {generator_id} is absent or changed")
    expected_fonts = manifest.get("font_fixture_tree_sha256")
    if (
        not isinstance(expected_fonts, str)
        or tree_sha256(FONT_FIXTURES) != expected_fonts
    ):
        raise MatrixError("Unicode font fixture tree is absent or changed")
    known_setups = set(generators)
    for case in cases:
        if case.get("setup") is not None and case["setup"] not in known_setups:
            raise MatrixError(f"{case['id']}: unknown setup generator")
    return manifest


def producer_map(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {producer["id"]: producer for producer in manifest["producers"]}


def selected_comparison_rules(
    manifest: dict[str, Any], wanted: list[str] | None
) -> list[dict[str, Any]]:
    rules = manifest["comparison_rules"]
    if not wanted:
        return rules
    requested = set(wanted)
    selected = [rule for rule in rules if rule["id"] in requested]
    unknown = requested - {rule["id"] for rule in selected}
    if unknown:
        raise MatrixError(
            "unknown comparison rules: " + ", ".join(sorted(unknown))
        )
    return selected


def effective_cpus() -> int:
    if hasattr(os, "sched_getaffinity"):
        try:
            return len(os.sched_getaffinity(0))
        except OSError:
            pass
    return os.cpu_count() or 1


def benchmark_env(
    worker_budget: int, project: Path | None = None, epoch: int = 1000000000
) -> dict[str, str]:
    env = dict(os.environ)
    env.update(
        {
            "SOURCE_DATE_EPOCH": str(epoch),
            "FORCE_SOURCE_DATE": "1",
            "LC_ALL": "C",
            "TZ": "UTC",
            "max_print_line": "1000",
            "AITEX_MAX_COMMANDS": "2000000000",
            "AITEX_MAX_STEPS": "2000000000",
            "AITEX_STACK_MB": "64",
            "AITEX_METRICS": "1",
            "AITEX_PDF_COMPRESSION_WORKERS": str(max(1, worker_budget)),
            "AITEX_FONT_PATH": str(FONT_FIXTURES),
        }
    )
    font_path = (project / "fonts") if project is not None else FONT_FIXTURES
    current = env.get("OSFONTDIR")
    env["OSFONTDIR"] = str(font_path) + ((os.pathsep + current) if current else "")
    return env


def set_fixed_mtime(project: Path, epoch: int) -> None:
    for path in sorted(project.rglob("*"), reverse=True):
        try:
            os.utime(path, (epoch, epoch), follow_symlinks=False)
        except OSError:
            pass
    os.utime(project, (epoch, epoch))


def stage_project(
    source: Path, destination: Path, case: dict[str, Any], epoch: int
) -> None:
    if case["source_kind"] == "locked-corpus":
        destination.mkdir(parents=True)
        for source_path in locked_source_files(source):
            relative = source_path.relative_to(source)
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source_path, target)
    else:
        shutil.copytree(source, destination)
    if case["family"] == "unicode-latex":
        shutil.copytree(FONT_FIXTURES, destination / "fonts")
    if case.get("setup") == "pdf-fixture-assets":
        pdf_fixture_assets.generate(destination)
    set_fixed_mtime(destination, epoch)


def source_texts(project: Path) -> list[str]:
    texts = []
    for path in sorted(project.rglob("*")):
        if not path.is_file() or path.suffix.lower() not in {
            ".tex",
            ".ltx",
            ".sty",
            ".cls",
        }:
            continue
        try:
            texts.append(path.read_text(encoding="utf-8", errors="replace"))
        except OSError:
            continue
    return texts


def discover_traits(project: Path, declared: list[str] | None = None) -> dict[str, Any]:
    declared = declared or []
    texts = source_texts(project)
    joined = "\n".join(texts)
    classes = sorted(
        set(re.findall(r"\\documentclass(?:\[[^]]*\])?\{([^}]+)\}", joined))
    )
    packages: set[str] = set()
    for value in re.findall(r"\\usepackage(?:\[[^]]*\])?\{([^}]+)\}", joined):
        packages.update(part.strip() for part in value.split(",") if part.strip())
    fonts = set(
        re.findall(
            r"\\(?:setmainfont|setsansfont|setmonofont|fontspec)"
            r"(?:\[[^]]*\])?\{([^}]+)\}",
            joined,
        )
    )
    fonts.update(
        re.findall(
            r"\\(?:newfontfamily|newfontface)\\[A-Za-z@]+" r"(?:\[[^]]*\])?\{([^}]+)\}",
            joined,
        )
    )
    fonts.update(
        value.strip()
        for value in re.findall(r"\\fontfamily\{([^}]+)\}", joined)
        if value.strip()
    )
    languages = set(
        re.findall(
            r"\\(?:selectlanguage|setdefaultlanguage|setotherlanguage|aitexlanguage)"
            r"\{([^}]+)\}",
            joined,
        )
    )
    babel = re.findall(r"\\usepackage\[([^]]+)\]\{(?:babel|polyglossia)\}", joined)
    for value in babel:
        languages.update(part.strip() for part in value.split(",") if part.strip())

    asset_extensions = sorted(
        {
            path.suffix.lower().lstrip(".")
            for path in project.rglob("*")
            if path.is_file()
            and path.suffix.lower() in {".png", ".jpg", ".jpeg", ".pdf", ".eps", ".svg"}
        }
    )
    features = []
    for name, pattern in {
        "cross-references": r"\\(?:label|ref|pageref|cref|tableofcontents)\b",
        "bibliography": r"\\(?:bibliography|addbibresource|printbibliography)\b",
        "index": r"\\(?:makeindex|printindex|index)\b",
        "glossary": r"\\(?:makeglossaries|printglossar|newglossaryentry)\b",
        "math": r"\\(?:begin\{(?:align|equation)|\[|frac|sum|int)\b|\$",
        "tables": r"\\begin\{(?:tabular|tabularx|longtable|array)\}",
        "graphics": r"\\(?:includegraphics|begin\{tikzpicture\})",
        "links": r"\\(?:href|hyperlink|hypertarget)\b",
    }.items():
        if re.search(pattern, joined):
            features.append(name)
    for trait in declared:
        prefix, separator, value = trait.partition(":")
        if not separator or not value:
            continue
        if prefix == "class":
            classes.append(value)
        elif prefix == "package":
            packages.add(value)
        elif prefix == "font":
            fonts.add(value)
        elif prefix == "language":
            languages.add(value)
        elif prefix == "asset":
            asset_extensions.append(value)
        elif prefix == "content":
            features.append(value)
    return {
        "declared": sorted(set(declared)),
        "documentclasses": sorted(set(classes)),
        "packages": sorted(packages),
        "fonts": sorted(fonts),
        "languages": sorted(languages),
        "asset_extensions": sorted(set(asset_extensions)),
        "features": sorted(set(features)),
        "tex_files": len(texts),
        "source_files": sum(1 for path in project.rglob("*") if path.is_file()),
        "source_bytes": sum(
            path.stat().st_size for path in project.rglob("*") if path.is_file()
        ),
    }


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
            "filesystem_read_bytes": None,
            "filesystem_write_bytes": None,
        }

    def number(label: str, cast: type) -> int | float | None:
        match = re.search(rf"^\s*{re.escape(label)}:\s*([0-9.]+)\s*$", text, re.M)
        return cast(match.group(1)) if match else None

    user = number("User time (seconds)", float)
    system = number("System time (seconds)", float)
    read_blocks = number("File system inputs", int)
    write_blocks = number("File system outputs", int)
    return {
        "cpu_seconds": (
            round(user + system, 6) if user is not None and system is not None else None
        ),
        "max_rss_kib": number("Maximum resident set size (kbytes)", int),
        "filesystem_read_bytes": read_blocks * 512 if read_blocks is not None else None,
        "filesystem_write_bytes": (
            write_blocks * 512 if write_blocks is not None else None
        ),
    }


def timed_command(
    argv: list[str], cwd: Path, env: dict[str, str], timeout: int
) -> tuple[dict[str, Any], str]:
    descriptor, raw_path = tempfile.mkstemp(prefix=".matrix-time.", dir=cwd)
    os.close(descriptor)
    time_path = Path(raw_path)
    if platform.system() == "Darwin":
        command = ["/usr/bin/time", "-l", "-o", str(time_path), *argv]
    else:
        command = ["/usr/bin/time", "-v", "-o", str(time_path), "--", *argv]
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
        except BaseException:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.communicate()
            raise
        measurement = parse_time(time_path)
    finally:
        time_path.unlink(missing_ok=True)
    output = raw_output.decode("utf-8", "replace")
    return (
        {
            "returncode": returncode,
            "timed_out": timed_out,
            "wall_seconds": round(time.monotonic() - started, 6),
            **measurement,
            "stdout_sha256": hashlib.sha256(raw_output).hexdigest(),
        },
        output,
    )


def render_command(
    command: list[str], binary: Path, format_path: Path, root: str
) -> list[str]:
    replacements = {
        "{binary}": str(binary),
        "{format}": str(format_path),
        "{root}": root,
        "{job}": Path(root).stem,
    }
    rendered = []
    for part in command:
        for placeholder, value in replacements.items():
            part = part.replace(placeholder, value)
        rendered.append(part)
    return rendered


def first_error(project: Path, output: str) -> str | None:
    logs = sorted(project.glob("*.log"), key=lambda path: path.stat().st_mtime_ns)
    text = logs[-1].read_text(errors="replace") if logs else output
    for pattern in (
        r"(?m)^! .{0,480}",
        r"(?m)^.*?:\d+: .{0,480}",
        r"(?im)^.*not found.*$",
    ):
        match = re.search(pattern, text)
        if match:
            return match.group(0)[:500]
    return None


def parse_activity(
    producer: dict[str, Any], output: str, success: bool
) -> dict[str, Any]:
    if producer["workflow"] == "one-shot":
        return {
            "tex_passes": 1 if success else 0,
            "semantic_tool_passes": 0,
            "converged": None,
        }
    if producer["engine"] == "aitex":
        tex_passes = len(re.findall(r"aitex build: TeX pass", output))
        if tex_passes == 0:
            tex_passes = output.count("AITEX_METRICS ")
        semantic = len(
            re.findall(
                r"aitex build: running (?:BibTeX|Biber|MakeIndex|MakeGlossaries)",
                output,
            )
        )
        return {
            "tex_passes": tex_passes,
            "semantic_tool_passes": semantic,
            "converged": "aitex build: converged after" in output,
        }
    rules = re.findall(r"Run number \d+ of rule '([^']+)'", output)
    tex_passes = sum(
        any(name in rule.lower() for name in ("latex", "pdftex", "xetex", "luatex"))
        for rule in rules
    )
    semantic = sum(
        any(
            name in rule.lower()
            for name in ("bibtex", "biber", "makeindex", "makeglossaries")
        )
        for rule in rules
    )
    return {
        "tex_passes": tex_passes,
        "semantic_tool_passes": semantic,
        # A successful latexmk process means its rule database reached a fixed
        # point. We retain the parsed pass counts separately.
        "converged": success,
    }


def inspect_pdf(path: Path) -> tuple[dict[str, Any], str | None]:
    if shutil.which("gs"):
        info = holdout.pdf_info(path)
    else:
        valid = path.is_file() and path.read_bytes()[:5] == b"%PDF-"
        pages = 0
        if valid and shutil.which("pdfinfo"):
            result = subprocess.run(
                ["pdfinfo", str(path)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            match = re.search(r"^Pages:\s+(\d+)$", result.stdout, re.M)
            pages = int(match.group(1)) if match else 0
        info = {
            "valid": valid and pages > 0,
            "pages": pages,
            "bytes": path.stat().st_size if path.is_file() else 0,
            "sha256": file_sha256(path) if path.is_file() else None,
        }
    text = holdout.extracted_text(path) if shutil.which("mutool") else None
    info["text"] = holdout.text_info(text)
    if path.is_file() and shutil.which("pdffonts"):
        process = subprocess.run(
            ["pdffonts", str(path)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        rows = [
            line.split() for line in process.stdout.splitlines()[2:] if line.strip()
        ]
        info["fonts"] = {
            "count": len(rows),
            "embedded": sum(len(row) >= 6 and row[4].lower() == "yes" for row in rows),
            "subset": sum(len(row) >= 6 and row[5].lower() == "yes" for row in rows),
        }
    return info, text


def inspect_artifact(
    path: Path, route: str, env: dict[str, str]
) -> tuple[dict[str, Any], str | None]:
    if route == "pdf":
        return inspect_pdf(path)
    if not shutil.which("dvitype"):
        valid = path.is_file() and path.stat().st_size > 0
        return (
            {
                "valid": valid,
                "pages": None,
                "bytes": path.stat().st_size if path.is_file() else 0,
                "sha256": file_sha256(path) if path.is_file() else None,
            },
            None,
        )
    info = holdout.dvi_info(path, env)
    if path.is_file():
        info["sha256"] = file_sha256(path)
    return info, None


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    index = (len(ordered) - 1) * quantile
    lower = math.floor(index)
    upper = math.ceil(index)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (index - lower)


def summarize(samples: list[dict[str, Any]]) -> dict[str, Any]:
    summary: dict[str, Any] = {
        "samples": len(samples),
        "successful": sum(sample.get("success", False) for sample in samples),
    }
    for field in (
        "wall_seconds",
        "cpu_seconds",
        "max_rss_kib",
        "filesystem_read_bytes",
        "filesystem_write_bytes",
        "tex_passes",
        "semantic_tool_passes",
        "output_bytes",
    ):
        values = [
            float(sample[field]) for sample in samples if sample.get(field) is not None
        ]
        if values:
            summary[field] = {
                "total": round(sum(values), 6),
                "p50": round(percentile(values, 0.5) or 0.0, 6),
                "p95": round(percentile(values, 0.95) or 0.0, 6),
            }
    return summary


def executable_available(command: list[str]) -> bool:
    executable = command[0]
    if os.path.sep in executable:
        return Path(executable).is_file() and os.access(executable, os.X_OK)
    return shutil.which(executable) is not None


def run_producer(
    scratch: Path,
    case: dict[str, Any],
    producer: dict[str, Any],
    route: str,
    repetition: str,
    binary: Path,
    format_path: Path,
    timeout: int,
    epoch: int,
    worker_budget: int,
    retain_artifact: bool,
) -> tuple[dict[str, Any], Path | None, str | None]:
    target = f"{case['family']}:{route}"
    rendered = render_command(
        producer["commands"][target], binary, format_path, case["root"]
    )
    if not executable_available(rendered):
        return (
            {
                "status": "unavailable",
                "producer": producer["id"],
                "command": rendered,
                "reason": f"executable is absent: {rendered[0]}",
            },
            None,
            None,
        )
    destination = scratch / re.sub(r"[^A-Za-z0-9_.-]", "_", repetition)
    stage_project(Path(case["source_path"]), destination, case, epoch)
    env = benchmark_env(worker_budget, destination, epoch)
    measurement, output = timed_command(rendered, destination, env, timeout)
    artifact = destination / f"{Path(case['root']).stem}.{route}"
    inspected, text = inspect_artifact(artifact, route, env)
    artifact_success = measurement["returncode"] == 0 and bool(inspected.get("valid"))
    activity = parse_activity(producer, output, artifact_success)
    success = artifact_success and activity["converged"] is not False
    sample = {
        "status": "measured",
        "producer": producer["id"],
        "command": rendered,
        **measurement,
        **activity,
        "output_bytes": inspected.get("bytes"),
        "success": success,
        "first_error": None if success else first_error(destination, output),
        "diagnostic_tail": None if success else output[-2000:],
        "artifact": inspected,
    }
    retained = artifact if artifact.is_file() and retain_artifact else None
    if not retain_artifact or retained is None:
        shutil.rmtree(destination)
    return sample, retained, text


def ordered_producers(
    case_id: str, workflow: str, route: str, repetition: int, ids: list[str]
) -> list[str]:
    return sorted(
        ids,
        key=lambda producer_id: hashlib.sha256(
            f"{case_id}\0{workflow}\0{route}\0{repetition}\0{producer_id}".encode()
        ).digest(),
    )


def selected_cases(
    manifest: dict[str, Any], tier_id: str, corpus_override: Path | None
) -> list[dict[str, Any]]:
    tier = manifest["tiers"][tier_id]
    wanted = tier["curated_cases"]
    wanted_ids = (
        {case["id"] for case in manifest["cases"]} if wanted == "all" else set(wanted)
    )
    cases = []
    for raw in manifest["cases"]:
        if raw["id"] not in wanted_ids:
            continue
        case = dict(raw)
        case["source_kind"] = "curated"
        case["source_path"] = str((ROOT / case["project"]).resolve())
        cases.append(case)

    fraction = float(tier["corpus_fraction"])
    if fraction <= 0:
        return cases
    corpus = manifest["locked_corpus"]
    lock_path = ROOT / corpus["source_lock"]
    source_lock = json.loads(lock_path.read_text(encoding="utf-8"))
    ready = [
        entry
        for entry in source_lock["entries"]
        if entry.get("status") == corpus["selection_status"]
    ]
    expected = int(corpus["expected_roots"])
    if len(ready) != expected:
        raise MatrixError(
            f"locked corpus has {len(ready)} ready roots, expected {expected}"
        )
    seed = corpus["sample_seed"]
    ranked = sorted(
        ready,
        key=lambda entry: hashlib.sha256(f"{seed}\0{entry['id']}".encode()).digest(),
    )
    count = len(ranked) if fraction == 1 else max(1, round(len(ranked) * fraction))
    selected = ranked[:count]
    source_root = corpus_override or (ROOT / corpus["source_root"])
    for entry in selected:
        project = source_root / entry["id"].replace("/", "_")
        cases.append(
            {
                "id": f"holdout:{entry['id']}",
                "corpus_id": entry["id"],
                "source_kind": "locked-corpus",
                "source_path": str(project.resolve()),
                "root": entry["root"],
                "tree_sha256": entry["tree_sha256"],
                "locked_files": entry["files"],
                "locked_bytes": entry["bytes"],
                "family": corpus["family"],
                "routes": corpus["routes"],
                "workflows": corpus["workflows"],
                "producer_profile": tier.get(
                    "corpus_producer_profile", corpus["producer_profile"]
                ),
                "traits": [f"category:{entry['primary_category']}"],
            }
        )
    return cases


def verify_case_source(case: dict[str, Any]) -> None:
    source = Path(case["source_path"])
    if not source.is_dir():
        raise MatrixError(f"{case['id']}: source tree is absent: {source}")
    if case["source_kind"] == "locked-corpus":
        actual, files, size = locked_tree_fingerprint(source)
        if files != case["locked_files"] or size != case["locked_bytes"]:
            raise MatrixError(
                f"{case['id']}: source inventory is {files} files/{size} bytes, "
                f"expected {case['locked_files']}/{case['locked_bytes']}"
            )
    else:
        actual = tree_sha256(source)
    if actual != case["tree_sha256"]:
        raise MatrixError(
            f"{case['id']}: source tree hash is {actual}, expected {case['tree_sha256']}"
        )


def compare_outputs(
    case: dict[str, Any],
    rule: dict[str, Any],
    paths: dict[str, Path],
    texts: dict[str, str | None],
    samples: dict[str, dict[str, Any]],
    base: Path,
    env: dict[str, str],
    oracle_level: str,
) -> dict[str, Any]:
    reference = rule["reference"]
    candidate = rule["candidate"]
    if reference not in samples or candidate not in samples:
        return {"status": "not-scheduled", "equivalent": None}
    ref_sample = samples[reference]
    candidate_sample = samples[candidate]
    if not ref_sample.get("success"):
        return {
            "status": "reference-failed",
            "equivalent": None,
            "reason": "the reference producer failed",
        }
    if not candidate_sample.get("success"):
        return {
            "status": "not-comparable",
            "equivalent": False,
            "reason": "the candidate failed where the reference succeeded",
        }
    ref_path = paths[reference]
    candidate_path = paths[candidate]
    if rule["oracle"] == "exact-dvi":
        if not shutil.which("dvitype") or not shutil.which("gawk"):
            return {
                "status": "oracle-unavailable",
                "equivalent": None,
                "reason": "dvitype and gawk are required for exact DVI comparison",
            }
        comparison = holdout.compare_dvi_semantics(ref_path, candidate_path, env)
        comparison["reference_artifact"] = comparison.pop("reference")
        comparison["candidate_artifact"] = comparison.pop("candidate")
        return {
            "status": "compared",
            "equivalent": comparison["equivalent"],
            **comparison,
        }

    ref_info = ref_sample["artifact"]
    candidate_info = candidate_sample["artifact"]
    ref_text = texts.get(reference)
    candidate_text = texts.get(candidate)
    similarity = holdout.text_similarity(ref_text, candidate_text)
    bag_similarity = character_bag_similarity(ref_text, candidate_text)
    expected_markers = case.get("expected_text_markers", [])
    missing_markers = [
        marker
        for marker in expected_markers
        if candidate_text is None or marker not in candidate_text
    ]
    result: dict[str, Any] = {
        "status": "compared",
        "reference_pages": ref_info.get("pages"),
        "candidate_pages": candidate_info.get("pages"),
        "page_match": ref_info.get("pages") == candidate_info.get("pages"),
        "text_similarity": similarity,
        "character_bag_similarity": bag_similarity,
        "missing_candidate_text_markers": missing_markers,
        "text_quality_no_worse": holdout.text_quality_no_worse(
            ref_info.get("text"), candidate_info.get("text")
        ),
    }
    if ref_text is None or candidate_text is None:
        return {
            **result,
            "status": "oracle-unavailable",
            "equivalent": None,
            "reason": "MuPDF text extraction is unavailable",
        }
    if rule["oracle"] == "cross-engine-pdf":
        # Different engines use different font metrics and may legitimately
        # paginate differently. Require readable, substantially matching text;
        # preserve page and font facts as diagnostics rather than false pixel
        # equivalence.
        result["equivalent"] = bool(
            bag_similarity is not None
            and bag_similarity >= 0.98
            and result["text_quality_no_worse"] is not False
            and not missing_markers
        )
        result["oracle_scope"] = (
            "valid PDF, normalized scalar coverage, and declared semantic markers; "
            "bidi sequence and geometry diagnostic only"
        )
        return result

    raster = None
    if oracle_level == "visual":
        required = ("mutool", "compare")
        if all(shutil.which(tool) for tool in required):
            base.mkdir(parents=True, exist_ok=True)
            raster = holdout.compare_pdf_rasters(ref_path, candidate_path, base)
            shutil.rmtree(base)
        else:
            return {
                **result,
                "status": "oracle-unavailable",
                "equivalent": None,
                "reason": "mutool and ImageMagick compare are required for visual PDF comparison",
            }
    result["raster"] = raster
    result["equivalent"] = bool(
        result["page_match"]
        and result["text_quality_no_worse"] is not False
        and (raster is None or raster.get("equivalent"))
    )
    result["oracle_scope"] = (
        "pages, text quality, and all-page raster"
        if raster is not None
        else "pages and text quality"
    )
    return result


def ratio(candidate: float | int | None, reference: float | int | None) -> float | None:
    if candidate is None or reference in (None, 0):
        return None
    return round(float(candidate) / float(reference), 6)


def character_bag_similarity(
    reference: str | None, candidate: str | None
) -> float | None:
    """Compare extracted scalar coverage without pretending bidi order agrees."""
    if reference is None or candidate is None:
        return None
    reference_counts = collections.Counter(reference)
    candidate_counts = collections.Counter(candidate)
    denominator = max(len(reference), len(candidate))
    if denominator == 0:
        return 1.0
    overlap = sum((reference_counts & candidate_counts).values())
    return round(overlap / denominator, 6)


def comparison_performance(
    reference: dict[str, Any], candidate: dict[str, Any]
) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for field in ("wall_seconds", "cpu_seconds", "max_rss_kib", "output_bytes"):
        ref = reference.get("summary", {}).get(field, {}).get("total")
        cand = candidate.get("summary", {}).get(field, {}).get("total")
        if ref is not None and cand is not None:
            result[field] = {
                "reference_total": ref,
                "candidate_total": cand,
                "ratio": ratio(cand, ref),
            }
    return result


def cell_successful(cell: dict[str, Any] | None) -> bool:
    return bool(
        cell
        and cell.get("status") == "measured"
        and cell["summary"]["successful"] == cell["summary"]["samples"]
    )


def paired_aggregates(entries: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    buckets: dict[str, dict[str, Any]] = {}
    for entry in entries:
        runs = entry["runs"]
        for comparison in entry["comparisons"]:
            bucket = buckets.setdefault(
                comparison["id"],
                {
                    "reference": comparison["reference"],
                    "candidate": comparison["candidate"],
                    "workflow": comparison["workflow"],
                    "route": comparison["route"],
                    "oracle": comparison["oracle"],
                    "cases": 0,
                    "equivalent": 0,
                    "failed": 0,
                    "unavailable": 0,
                    "successful_pairs": 0,
                    "paired_cases": 0,
                    "observations": 0,
                    "_values": {
                        field: {"reference": [], "candidate": []}
                        for field in (
                            "wall_seconds",
                            "cpu_seconds",
                            "max_rss_kib",
                            "output_bytes",
                            "tex_passes",
                            "semantic_tool_passes",
                        )
                    },
                    "_wins": {"wall_seconds": 0, "cpu_seconds": 0, "output_bytes": 0},
                    "_case_ratios": {
                        field: []
                        for field in (
                            "wall_seconds",
                            "cpu_seconds",
                            "max_rss_kib",
                            "output_bytes",
                        )
                    },
                },
            )
            bucket["cases"] += 1
            if comparison.get("equivalent") is True:
                bucket["equivalent"] += 1
            elif comparison.get("equivalent") is False:
                bucket["failed"] += 1
            else:
                bucket["unavailable"] += 1
            ref_key = f"{comparison['workflow']}.{comparison['route']}.{comparison['reference']}"
            candidate_key = f"{comparison['workflow']}.{comparison['route']}.{comparison['candidate']}"
            reference = runs.get(ref_key)
            candidate = runs.get(candidate_key)
            if not cell_successful(reference) or not cell_successful(candidate):
                continue
            bucket["successful_pairs"] += 1
            if comparison.get("equivalent") is not True:
                continue
            bucket["paired_cases"] += 1
            paired_samples = min(len(reference["samples"]), len(candidate["samples"]))
            bucket["observations"] += paired_samples
            for field, sides in bucket["_values"].items():
                sides["reference"].extend(
                    float(sample[field])
                    for sample in reference["samples"][:paired_samples]
                    if sample.get(field) is not None
                )
                sides["candidate"].extend(
                    float(sample[field])
                    for sample in candidate["samples"][:paired_samples]
                    if sample.get(field) is not None
                )
            for field in bucket["_case_ratios"]:
                ref_total = reference["summary"].get(field, {}).get("total")
                candidate_total = candidate["summary"].get(field, {}).get("total")
                if ref_total is not None and candidate_total is not None:
                    if field in bucket["_wins"] and candidate_total < ref_total:
                        bucket["_wins"][field] += 1
                    case_ratio = ratio(candidate_total, ref_total)
                    if case_ratio is not None:
                        bucket["_case_ratios"][field].append(case_ratio)

    result: dict[str, dict[str, Any]] = {}
    for rule_id, raw in buckets.items():
        values = raw.pop("_values")
        wins = raw.pop("_wins")
        case_ratios = raw.pop("_case_ratios")
        metrics = {}
        for field, sides in values.items():
            reference_values = sides["reference"]
            candidate_values = sides["candidate"]
            if not reference_values or not candidate_values:
                continue
            if field == "max_rss_kib":
                reference_value = percentile(reference_values, 0.95)
                candidate_value = percentile(candidate_values, 0.95)
                statistic = "p95"
            else:
                reference_value = sum(reference_values)
                candidate_value = sum(candidate_values)
                statistic = "total"
            metrics[field] = {
                "statistic": statistic,
                "reference": round(reference_value or 0.0, 6),
                "candidate": round(candidate_value or 0.0, 6),
                "ratio": ratio(candidate_value, reference_value),
            }
            if field in wins:
                metrics[field]["candidate_case_wins"] = wins[field]
            ratios = case_ratios.get(field, [])
            if ratios:
                metrics[field]["median_case_ratio"] = round(
                    percentile(ratios, 0.5) or 0.0, 6
                )
                if all(value > 0 for value in ratios):
                    metrics[field]["geomean_case_ratio"] = round(
                        math.exp(
                            sum(math.log(value) for value in ratios) / len(ratios)
                        ),
                        6,
                    )
        result[rule_id] = {**raw, "metrics": metrics}
    return result


def stratified_summary(entries: list[dict[str, Any]]) -> dict[str, Any]:
    dimensions = (
        "documentclasses",
        "packages",
        "fonts",
        "languages",
        "asset_extensions",
        "features",
    )
    result: dict[str, Any] = {}
    for dimension in dimensions:
        values = sorted(
            {
                value
                for entry in entries
                for value in entry["inventory"].get(dimension, ())
            }
        )
        result[dimension] = {}
        for value in values:
            selected = [
                entry
                for entry in entries
                if value in entry["inventory"].get(dimension, ())
            ]
            aggregates = paired_aggregates(selected)
            result[dimension][value] = {
                "cases": len(selected),
                "comparisons": {
                    rule_id: {
                        "paired_cases": aggregate["paired_cases"],
                        "equivalent": aggregate["equivalent"],
                        "failed": aggregate["failed"],
                        "cpu_ratio": aggregate.get("metrics", {})
                        .get("cpu_seconds", {})
                        .get("ratio"),
                        "wall_ratio": aggregate.get("metrics", {})
                        .get("wall_seconds", {})
                        .get("ratio"),
                        "output_size_ratio": aggregate.get("metrics", {})
                        .get("output_bytes", {})
                        .get("ratio"),
                    }
                    for rule_id, aggregate in aggregates.items()
                },
            }
    return result


def coverage_summary(entries: list[dict[str, Any]]) -> dict[str, Any]:
    dimensions = (
        "documentclasses",
        "packages",
        "fonts",
        "languages",
        "asset_extensions",
        "features",
    )
    coverage: dict[str, Any] = {"cases": len(entries)}
    for dimension in dimensions:
        counts: dict[str, int] = {}
        for entry in entries:
            for value in entry["inventory"].get(dimension, ()):
                counts[value] = counts.get(value, 0) + 1
        coverage[dimension] = [
            {"name": name, "cases": count}
            for name, count in sorted(
                counts.items(), key=lambda item: (-item[1], item[0])
            )
        ]
    return coverage


def overall_summary(entries: list[dict[str, Any]]) -> dict[str, Any]:
    cells = [cell for entry in entries for cell in entry["runs"].values()]
    comparisons = [
        comparison for entry in entries for comparison in entry["comparisons"]
    ]
    by_producer: dict[str, dict[str, Any]] = {}
    for cell in cells:
        producer = cell["producer"]
        bucket = by_producer.setdefault(
            producer,
            {
                "cells": 0,
                "successful_cells": 0,
                "samples": 0,
                "wall_seconds": 0.0,
                "cpu_seconds": 0.0,
            },
        )
        bucket["cells"] += 1
        if (
            cell["status"] == "measured"
            and cell["summary"]["successful"] == cell["summary"]["samples"]
        ):
            bucket["successful_cells"] += 1
        if cell["status"] == "measured":
            bucket["samples"] += cell["summary"]["samples"]
            bucket["wall_seconds"] += (
                cell["summary"].get("wall_seconds", {}).get("total", 0.0)
            )
            bucket["cpu_seconds"] += (
                cell["summary"].get("cpu_seconds", {}).get("total", 0.0)
            )
    for value in by_producer.values():
        value["wall_seconds"] = round(value["wall_seconds"], 6)
        value["cpu_seconds"] = round(value["cpu_seconds"], 6)
    return {
        "cases": len(entries),
        "cells": len(cells),
        "measured_cells": sum(cell["status"] == "measured" for cell in cells),
        "successful_cells": sum(
            cell["status"] == "measured"
            and cell["summary"]["successful"] == cell["summary"]["samples"]
            for cell in cells
        ),
        "comparisons": len(comparisons),
        "equivalent_comparisons": sum(
            item.get("equivalent") is True for item in comparisons
        ),
        "failed_comparisons": sum(
            item.get("equivalent") is False for item in comparisons
        ),
        "unavailable_comparisons": sum(
            item.get("equivalent") is None for item in comparisons
        ),
        "by_producer": by_producer,
        "by_comparison": paired_aggregates(entries),
    }


def machine_identity(
    binary: Path, format_path: Path, worker_budget: int
) -> dict[str, Any]:
    tools = {
        "aitex": [str(binary), "--version"],
        "tex": ["tex", "--version"],
        "latex": ["latex", "--version"],
        "pdflatex": ["pdflatex", "--version"],
        "latexmk": ["latexmk", "--version"],
        "xelatex": ["xelatex", "--version"],
        "lualatex": ["lualatex", "--version"],
        "bibtex": ["bibtex", "--version"],
        "biber": ["biber", "--version"],
        "makeindex": ["makeindex", "--version"],
        "ghostscript": ["gs", "--version"],
        "mutool": ["mutool", "-v"],
        "dvitype": ["dvitype", "--version"],
        "pdffonts": ["pdffonts", "-v"],
        "imagemagick": ["compare", "-version"],
    }
    git_commit = command_version(["git", "rev-parse", "HEAD"])
    dirty = (
        subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            text=True,
            check=True,
        ).stdout
        != ""
    )
    return {
        "platform": platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "logical_cpus": os.cpu_count(),
        "effective_cpus": effective_cpus(),
        "outer_jobs": 1,
        "worker_budget": worker_budget,
        "aitex_pdf_workers_per_process": worker_budget,
        "git_commit": git_commit,
        "git_dirty": dirty,
        "binary_sha256": file_sha256(binary),
        "format_sha256": file_sha256(format_path),
        "tools": {name: command_version(command) for name, command in tools.items()},
    }


def run_matrix(args: argparse.Namespace, manifest: dict[str, Any]) -> dict[str, Any]:
    binary = args.binary.resolve()
    format_path = args.format.resolve()
    if args.build:
        subprocess.run(
            ["cargo", "build", "--release", "--locked", "-p", "aitex-cli"],
            cwd=ROOT,
            check=True,
        )
        format_path.parent.mkdir(parents=True, exist_ok=True)
        format_env = dict(os.environ)
        format_env["AITEX_BINARY"] = str(binary)
        subprocess.run(
            [sys.executable, str(ROOT / "tools" / "mkfmt.py"), str(format_path.parent)],
            cwd=ROOT,
            env=format_env,
            check=True,
        )
    if not binary.is_file():
        raise MatrixError(f"aiTeX binary is absent: {binary}; pass --build")
    if not format_path.is_file():
        raise MatrixError(f"aiTeX format is absent: {format_path}")

    tier = manifest["tiers"][args.tier]
    samples = args.samples if args.samples is not None else tier["samples"]
    warmups = args.warmups if args.warmups is not None else tier["warmups"]
    timeout = (
        args.timeout
        if args.timeout is not None
        else manifest["defaults"]["timeout_seconds"]
    )
    requested_budget = (
        args.worker_budget
        if args.worker_budget is not None
        else manifest["defaults"]["worker_budget"]
    )
    worker_budget = max(1, min(requested_budget, effective_cpus()))
    oracle_level = args.oracle or manifest["defaults"]["oracle"]
    epoch = int(manifest["defaults"]["source_date_epoch"])
    producers = producer_map(manifest)
    comparison_rules = selected_comparison_rules(manifest, args.comparison)
    comparison_cells: dict[tuple[str, str, str], set[str]] = {}
    for rule in comparison_rules:
        comparison_cells.setdefault(
            (rule["family"], rule["workflow"], rule["route"]), set()
        ).update((rule["reference"], rule["candidate"]))
    cases = selected_cases(manifest, args.tier, args.corpus)
    if args.case:
        wanted = set(args.case)
        cases = [
            case
            for case in cases
            if case["id"] in wanted or case.get("corpus_id") in wanted
        ]
        unknown = (
            wanted
            - {case["id"] for case in cases}
            - {case.get("corpus_id") for case in cases if case.get("corpus_id")}
        )
        if unknown:
            raise MatrixError(
                "requested cases are outside the selected tier: "
                + ", ".join(sorted(unknown))
            )
    if args.limit is not None:
        cases = cases[: args.limit]
    if not cases:
        raise MatrixError("selected tier contains no cases")

    for case in cases:
        verify_case_source(case)
        case["inventory"] = discover_traits(
            Path(case["source_path"]), case.get("traits")
        )
    if args.dry_run:
        return {
            "schema": SCHEMA,
            "dry_run": True,
            "tier": args.tier,
            "cases": [
                {
                    "id": case["id"],
                    "family": case["family"],
                    "routes": case["routes"],
                    "workflows": case["workflows"],
                    "producer_profile": case["producer_profile"],
                    "inventory": case["inventory"],
                }
                for case in cases
            ],
            "coverage": coverage_summary(
                [{"inventory": case["inventory"]} for case in cases]
            ),
            "comparisons": [rule["id"] for rule in comparison_rules],
        }

    machine = machine_identity(binary, format_path, worker_budget)
    identity = {
        "schema": SCHEMA,
        "manifest_sha256": file_sha256(args.manifest.resolve()),
        "runner_sha256": file_sha256(Path(__file__)),
        "tier": args.tier,
        "cases": [
            {
                "id": case["id"],
                "tree_sha256": case["tree_sha256"],
                "root": case["root"],
                "producer_profile": case["producer_profile"],
                "workflows": case["workflows"],
                "routes": case["routes"],
            }
            for case in cases
        ],
        "samples": samples,
        "warmups": warmups,
        "timeout_seconds": timeout,
        "worker_budget": worker_budget,
        "oracle": oracle_level,
        "source_date_epoch": epoch,
        "comparisons": [rule["id"] for rule in comparison_rules],
        "binary_path": str(binary),
        "binary_sha256": machine["binary_sha256"],
        "format_path": str(format_path),
        "format_sha256": machine["format_sha256"],
        "machine": {key: value for key, value in machine.items() if key != "git_dirty"},
    }
    state_file = checkpoint_path(args)
    if state_file == args.output.resolve():
        raise MatrixError("checkpoint path and result path must differ")
    if args.fresh:
        state_file.unlink(missing_ok=True)

    started = time.monotonic()
    started_at = utc_now()
    prior_elapsed = 0.0
    entries: list[dict[str, Any]] = []
    if state_file.is_file():
        try:
            state = json.loads(state_file.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise MatrixError(
                f"cannot read checkpoint {state_file}: {error}; pass --fresh to replace it"
            ) from error
        if state.get("kind") != "compiler-build-matrix-checkpoint":
            raise MatrixError(
                f"invalid checkpoint {state_file}; pass --fresh to replace it"
            )
        if state.get("identity") != identity:
            raise MatrixError(
                f"checkpoint does not match this run: {state_file}; "
                "pass --fresh to replace it"
            )
        entries = state.get("entries", [])
        if not isinstance(entries, list):
            raise MatrixError(f"invalid checkpoint entries in {state_file}")
        completed = [entry.get("id") for entry in entries if isinstance(entry, dict)]
        expected = [case["id"] for case in cases[: len(entries)]]
        if len(completed) != len(entries) or completed != expected:
            raise MatrixError(
                f"checkpoint cases are not a valid completed prefix: {state_file}"
            )
        prior_elapsed = float(state.get("elapsed_seconds", 0.0))
        started_at = state.get("started_at", started_at)
        print(
            f"compiler/build matrix: resuming after {len(entries)}/{len(cases)} cases "
            f"from {state_file}",
            flush=True,
        )

    completed_cases = len(entries)
    with tempfile.TemporaryDirectory(prefix="aitex-compiler-matrix-") as raw:
        scratch = Path(raw)
        for case_number, case in enumerate(cases, start=1):
            if case_number <= completed_cases:
                continue
            runs: dict[str, dict[str, Any]] = {}
            final_paths: dict[tuple[str, str], dict[str, Path]] = {}
            final_texts: dict[tuple[str, str], dict[str, str | None]] = {}
            final_samples: dict[tuple[str, str], dict[str, dict[str, Any]]] = {}
            profile = manifest["producer_profiles"][case["producer_profile"]]
            print(f"[{case_number:3d}/{len(cases)}] {case['id']}", flush=True)
            for workflow in case["workflows"]:
                for route in case["routes"]:
                    allowed_producers = comparison_cells.get(
                        (case["family"], workflow, route)
                    )
                    if allowed_producers is None:
                        continue
                    target = f"{case['family']}:{route}"
                    applicable = [
                        producer_id
                        for producer_id in profile
                        if producer_id in allowed_producers
                        if producers[producer_id]["workflow"] == workflow
                        and target in producers[producer_id]["commands"]
                    ]
                    if not applicable:
                        continue
                    cell_samples: dict[str, list[dict[str, Any]]] = {
                        producer_id: [] for producer_id in applicable
                    }
                    final_paths[(workflow, route)] = {}
                    final_texts[(workflow, route)] = {}
                    final_samples[(workflow, route)] = {}
                    for warmup in range(warmups):
                        for producer_id in ordered_producers(
                            case["id"], workflow, route, -(warmup + 1), applicable
                        ):
                            run_producer(
                                scratch,
                                case,
                                producers[producer_id],
                                route,
                                f"{case_number}-{workflow}-{route}-{producer_id}-warm-{warmup}",
                                binary,
                                format_path,
                                timeout,
                                epoch,
                                worker_budget,
                                False,
                            )
                    for repetition in range(samples):
                        for producer_id in ordered_producers(
                            case["id"], workflow, route, repetition, applicable
                        ):
                            sample, artifact, text = run_producer(
                                scratch,
                                case,
                                producers[producer_id],
                                route,
                                f"{case_number}-{workflow}-{route}-{producer_id}-{repetition}",
                                binary,
                                format_path,
                                timeout,
                                epoch,
                                worker_budget,
                                repetition == samples - 1,
                            )
                            cell_samples[producer_id].append(sample)
                            if sample["status"] == "measured":
                                final_samples[(workflow, route)][producer_id] = sample
                                if artifact is not None:
                                    final_paths[(workflow, route)][
                                        producer_id
                                    ] = artifact
                                final_texts[(workflow, route)][producer_id] = text
                    for producer_id, values in cell_samples.items():
                        cell_id = f"{workflow}.{route}.{producer_id}"
                        if values and all(
                            value["status"] == "unavailable" for value in values
                        ):
                            runs[cell_id] = values[0]
                            runs[cell_id].update(
                                {
                                    "workflow": workflow,
                                    "route": route,
                                    "producer": producer_id,
                                }
                            )
                        else:
                            runs[cell_id] = {
                                "status": "measured",
                                "workflow": workflow,
                                "route": route,
                                "producer": producer_id,
                                "samples": values,
                                "summary": summarize(values),
                            }

            comparisons = []
            env = benchmark_env(worker_budget, epoch=epoch)
            for rule in comparison_rules:
                key = (rule["workflow"], rule["route"])
                if rule["family"] != case["family"] or key not in final_samples:
                    continue
                oracle = compare_outputs(
                    case,
                    rule,
                    final_paths[key],
                    final_texts[key],
                    final_samples[key],
                    scratch / f"oracle-{case_number}-{rule['id']}",
                    env,
                    oracle_level,
                )
                reference_cell = runs.get(
                    f"{rule['workflow']}.{rule['route']}.{rule['reference']}"
                )
                candidate_cell = runs.get(
                    f"{rule['workflow']}.{rule['route']}.{rule['candidate']}"
                )
                performance = (
                    comparison_performance(reference_cell, candidate_cell)
                    if oracle.get("equivalent") is True
                    and cell_successful(reference_cell)
                    and cell_successful(candidate_cell)
                    else {}
                )
                comparisons.append({**rule, **oracle, "performance": performance})
            retained_directories = {
                path.parent for paths in final_paths.values() for path in paths.values()
            }
            for directory in retained_directories:
                shutil.rmtree(directory)
            entries.append(
                {
                    "id": case["id"],
                    "corpus_id": case.get("corpus_id"),
                    "source_kind": case["source_kind"],
                    "source_tree_sha256": case["tree_sha256"],
                    "root": case["root"],
                    "family": case["family"],
                    "inventory": case["inventory"],
                    "runs": runs,
                    "comparisons": comparisons,
                }
            )
            atomic_write_json(
                state_file,
                {
                    "schema": SCHEMA,
                    "kind": "compiler-build-matrix-checkpoint",
                    "started_at": started_at,
                    "updated_at": utc_now(),
                    "elapsed_seconds": round(
                        prior_elapsed + time.monotonic() - started, 3
                    ),
                    "identity": identity,
                    "entries": entries,
                },
            )

    return {
        "schema": SCHEMA,
        "name": manifest["name"],
        "description": manifest["description"],
        "created_at": utc_now(),
        "tier": args.tier,
        "manifest_sha256": identity["manifest_sha256"],
        "runner_sha256": identity["runner_sha256"],
        "machine": machine,
        "protocol": {
            "measurement": "sequential latency; fresh project per producer and sample",
            "timing_scope": "complete producer process tree; staging and output oracles excluded",
            "system_cache": "one unmeasured warm-up per producer/case/cell when warmups > 0",
            "order": "deterministically counterbalanced by case/workflow/route/repetition/producer",
            "project_state": "clean source tree for both one-shot and complete-build cells",
            "one_shot": "exactly one compiler invocation; unresolved auxiliaries are expected",
            "full_build": "ordinary aitex or latexmk frontend through its declared fixed point",
            "pdf_policy": "producer defaults; CPU/wall/RSS and output bytes are reported jointly",
            "samples": samples,
            "warmups": warmups,
            "timeout_seconds": timeout,
            "oracle": oracle_level,
            "comparisons": [rule["id"] for rule in comparison_rules],
            "elapsed_seconds": round(prior_elapsed + time.monotonic() - started, 3),
        },
        "coverage": coverage_summary(entries),
        "strata": stratified_summary(entries),
        "summary": overall_summary(entries),
        "entries": entries,
    }


def markdown_report(result: dict[str, Any]) -> str:
    if result.get("dry_run"):
        return (
            f"# Compiler/build matrix plan\n\n"
            f"Tier `{result['tier']}` selects {len(result['cases'])} cases.\n"
        )
    summary = result["summary"]
    lines = [
        "# Compiler/build matrix",
        "",
        f"Tier: `{result['tier']}`. Cases: {summary['cases']}. "
        f"Measured cells: {summary['measured_cells']}/{summary['cells']}.",
        "",
        f"Correct comparisons: {summary['equivalent_comparisons']}; "
        f"failed: {summary['failed_comparisons']}; "
        f"unavailable/not comparable: {summary['unavailable_comparisons']}.",
        "",
        "| Producer | Successful cells | Samples | Wall (s) | CPU (s) |",
        "|---|---:|---:|---:|---:|",
    ]
    for producer, values in sorted(summary["by_producer"].items()):
        lines.append(
            f"| {producer} | {values['successful_cells']}/{values['cells']} | "
            f"{values['samples']} | {values['wall_seconds']:.3f} | {values['cpu_seconds']:.3f} |"
        )
    lines.extend(["", "## Aggregate paired comparisons", ""])
    lines.append(
        "| Comparison | Correct cases | Paired cases | CPU total | Wall total | Wall median case | Size total |"
    )
    lines.append("|---|---:|---:|---:|---:|---:|---:|")
    for rule_id, aggregate in sorted(summary["by_comparison"].items()):
        metrics = aggregate.get("metrics", {})

        def show(field: str) -> str:
            value = metrics.get(field, {}).get("ratio")
            return f"{value:.3f}" if value is not None else "—"

        wall_median = metrics.get("wall_seconds", {}).get("median_case_ratio")

        lines.append(
            f"| {rule_id} | {aggregate['equivalent']}/{aggregate['cases']} | "
            f"{aggregate['paired_cases']} | {show('cpu_seconds')} | "
            f"{show('wall_seconds')} | "
            f"{f'{wall_median:.3f}' if wall_median is not None else '—'} | "
            f"{show('output_bytes')} |"
        )
    lines.extend(["", "## Per-case comparisons", ""])
    lines.append(
        "| Case | Comparison | Correct | CPU ratio | Wall ratio | Size ratio |"
    )
    lines.append("|---|---|---:|---:|---:|---:|")
    for entry in result["entries"]:
        for comparison in entry["comparisons"]:
            perf = comparison.get("performance", {})

            def show(field: str) -> str:
                value = perf.get(field, {}).get("ratio")
                return f"{value:.3f}" if value is not None else "—"

            correct = (
                "yes"
                if comparison.get("equivalent") is True
                else "no" if comparison.get("equivalent") is False else "n/a"
            )
            lines.append(
                f"| {entry['id']} | {comparison['id']} | {correct} | "
                f"{show('cpu_seconds')} | {show('wall_seconds')} | {show('output_bytes')} |"
            )
    lines.append("")
    return "\n".join(lines)


def self_test() -> int:
    manifest = load_manifest(DEFAULT_MANIFEST)
    assert percentile([1.0, 3.0], 0.5) == 2.0
    assert ratio(2, 4) == 0.5
    assert ratio(2, 0) is None
    assert character_bag_similarity("abc", "cba") == 1.0
    ordered = ordered_producers("case", "one-shot", "pdf", 0, ["b", "a", "c"])
    assert sorted(ordered) == ["a", "b", "c"]
    assert ordered == ordered_producers("case", "one-shot", "pdf", 0, ["c", "b", "a"])
    selected = selected_comparison_rules(manifest, ["classic-one-shot-dvi"])
    assert [rule["id"] for rule in selected] == ["classic-one-shot-dvi"]
    try:
        selected_comparison_rules(manifest, ["missing-comparison"])
    except MatrixError:
        pass
    else:
        raise AssertionError("unknown comparison rule was accepted")

    def aggregate_cell(wall: float) -> dict[str, Any]:
        sample = {
            "wall_seconds": wall,
            "cpu_seconds": wall,
            "max_rss_kib": 100.0,
            "output_bytes": 1000.0,
            "tex_passes": 1.0,
            "semantic_tool_passes": 0.0,
        }
        return {
            "status": "measured",
            "samples": [sample],
            "summary": {
                "samples": 1,
                "successful": 1,
                **{field: {"total": value} for field, value in sample.items()},
            },
        }

    aggregate_entries = []
    for case_id, reference_wall, candidate_wall, equivalent in (
        ("faster", 1.0, 0.5, True),
        ("slower", 1.0, 2.0, True),
        ("incorrect", 1.0, 0.01, False),
    ):
        aggregate_entries.append(
            {
                "id": case_id,
                "runs": {
                    "one-shot.pdf.reference": aggregate_cell(reference_wall),
                    "one-shot.pdf.candidate": aggregate_cell(candidate_wall),
                },
                "comparisons": [
                    {
                        "id": "test-rule",
                        "reference": "reference",
                        "candidate": "candidate",
                        "workflow": "one-shot",
                        "route": "pdf",
                        "oracle": "classic-pdf",
                        "equivalent": equivalent,
                    }
                ],
            }
        )
    aggregate = paired_aggregates(aggregate_entries)["test-rule"]
    assert aggregate["successful_pairs"] == 3
    assert aggregate["paired_cases"] == 2
    assert aggregate["metrics"]["wall_seconds"]["ratio"] == 1.25
    assert aggregate["metrics"]["wall_seconds"]["median_case_ratio"] == 1.25
    assert aggregate["metrics"]["wall_seconds"]["geomean_case_ratio"] == 1.0

    with tempfile.TemporaryDirectory(prefix="aitex-matrix-self-test-") as raw:
        checkpoint = Path(raw) / "nested" / "state.json"
        atomic_write_json(checkpoint, {"complete": True})
        assert json.loads(checkpoint.read_text(encoding="utf-8")) == {"complete": True}
    smoke = selected_cases(manifest, "smoke", None)
    assert {case["id"] for case in smoke} == {
        "plain",
        "article-common",
        "report-tools",
        "unicode",
    }
    sample = selected_cases(manifest, "corpus-10", None)
    assert len(sample) == 19
    assert len({case["id"] for case in sample}) == 19
    assert {case["producer_profile"] for case in sample} == {"classic-all"}
    inventory = discover_traits(ROOT / "tests" / "build-matrix" / "article-common")
    assert inventory["documentclasses"] == ["article"]
    assert {"amsmath", "hyperref", "microtype"} <= set(inventory["packages"])
    assert {"cross-references", "math", "tables", "links"} <= set(inventory["features"])
    print(
        "compiler/build matrix self-test: manifest, locks, deterministic selection, "
        "counterbalancing, taxonomy, and aggregation pass"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument(
        "--tier",
        choices=("smoke", "curated", "corpus-10", "corpus-full", "all"),
        default="smoke",
    )
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--format", type=Path, default=DEFAULT_FORMAT)
    parser.add_argument(
        "--corpus", type=Path, help="materialized locked-corpus source root"
    )
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--report", type=Path)
    parser.add_argument(
        "--state",
        type=Path,
        help="checkpoint path (default: OUTPUT.state.json)",
    )
    parser.add_argument(
        "--fresh",
        action="store_true",
        help="discard a matching or stale checkpoint and start from the first case",
    )
    parser.add_argument("--samples", type=int)
    parser.add_argument("--warmups", type=int)
    parser.add_argument("--timeout", type=int)
    parser.add_argument("--worker-budget", type=int)
    parser.add_argument("--oracle", choices=("structural", "visual"))
    parser.add_argument(
        "--case", action="append", help="curated ID or locked corpus ID"
    )
    parser.add_argument(
        "--comparison",
        action="append",
        help="run only the named comparison rule and its two producer cells",
    )
    parser.add_argument("--limit", type=int)
    parser.add_argument("--build", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail when a scheduled candidate fails against a successful reference",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="also require every scheduled producer cell and comparison to succeed",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if args.samples is not None and args.samples < 1:
        parser.error("--samples must be positive")
    if args.warmups is not None and args.warmups < 0:
        parser.error("--warmups cannot be negative")
    if args.timeout is not None and args.timeout < 1:
        parser.error("--timeout must be positive")
    if args.worker_budget is not None and args.worker_budget < 1:
        parser.error("--worker-budget must be positive")
    if args.limit is not None and args.limit < 1:
        parser.error("--limit must be positive")
    try:
        manifest = load_manifest(args.manifest.resolve())
        result = run_matrix(args, manifest)
    except MatrixError as error:
        print(f"compiler/build matrix: FAIL: {error}", file=sys.stderr)
        return 1
    atomic_write_json(args.output, result)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(markdown_report(result), encoding="utf-8")
    if not result.get("dry_run"):
        checkpoint_path(args).unlink(missing_ok=True)
    if result.get("dry_run"):
        print(f"compiler/build matrix: plan contains {len(result['cases'])} cases")
    else:
        summary = result["summary"]
        print(
            f"compiler/build matrix: {summary['equivalent_comparisons']}/"
            f"{summary['comparisons']} comparisons pass "
            f"({summary['cases']} cases, {summary['measured_cells']} measured cells)"
        )
    print(f"results: {args.output} ({file_sha256(args.output)})")
    if not result.get("dry_run") and (args.check or args.strict):
        summary = result["summary"]
        if summary["failed_comparisons"]:
            return 1
        if args.strict and (
            summary["successful_cells"] != summary["cells"]
            or summary["equivalent_comparisons"] != summary["comparisons"]
        ):
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
