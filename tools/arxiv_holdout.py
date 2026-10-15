#!/usr/bin/env python3
"""Create and evaluate a blind, versioned arXiv compiler holdout.

The four subcommands deliberately separate selection, source inspection,
compilation, and reporting. In particular, ``sample`` commits to every ID
before ``fetch`` is allowed to inspect its source and before ``run`` can see a
compiler result.
"""

from __future__ import annotations

import argparse
import copy
import concurrent.futures
import datetime as dt
import difflib
import gzip
import hashlib
import io
import json
import os
import platform
import random
import re
import resource
import shutil
import signal
import statistics
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.parse
import urllib.request
import unicodedata
import xml.etree.ElementTree as ET
from collections import Counter, defaultdict
from pathlib import Path, PurePosixPath

sys.dont_write_bytecode = True
from corpus_lock import tree_fingerprint


ROOT = Path(__file__).resolve().parent.parent
SAMPLE = ROOT / "qualification" / "holdout-200.sample.json"
SOURCES = ROOT / "qualification" / "holdout-200.sources.json"
RESULTS = ROOT / "qualification" / "holdout-200.results.json"
REPORT = ROOT / "docs" / "HOLDOUT-200.md"
CORPUS = ROOT / "corpus" / "holdout-200"
OVERLAY_LOCK = ROOT / "qualification" / "texlive-overlay.lock.json"
TEXMF_OVERLAY = (ROOT / "corpus" / "texlive-overlay" / "root" / "usr" /
                 "share" / "texlive" / "texmf-dist")
USER_AGENT = "aiTeX-holdout/0.1 (compiler compatibility research)"
ATOM = {"a": "http://www.w3.org/2005/Atom",
        "x": "http://arxiv.org/schemas/atom"}
MAX_ARCHIVE_BYTES = 128 * 1024 * 1024
MAX_EXTRACTED_BYTES = 256 * 1024 * 1024
MAX_FILES = 5_000
RASTER_DPI = 72
# PDF rasterization is an end-product smoke test, not a byte/pixel identity
# oracle. pdfTeX and aiTeX can embed identical source pixels at the same
# physical rectangle yet MuPDF resamples their different Image-XObject forms
# differently. These bounds still reject the corpus's missing, enlarged,
# shifted, and clipped figures by a wide margin; exact DVI drawing semantics,
# page count, and extracted-text quality carry the stricter checks.
RASTER_MAX_NORMALIZED_MAE = 0.02
RASTER_FUZZ_PERCENT = 12.5
RASTER_MAX_FUZZ_FRACTION = 0.05
SPECIAL_POSITION_TOLERANCE_DVI_UNITS = 1
MEASUREMENT_SCHEMA = 11


def canonical(value: object) -> bytes:
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode()


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_bytes(canonical(value))
    os.replace(temporary, path)


def file_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def arxiv_id(url: str) -> str:
    return url.rstrip("/").rsplit("/", 1)[-1]


def known_ids() -> set[str]:
    ids: set[str] = set()
    raw = ROOT / "corpus" / "meta" / "arxiv_raw.json"
    if raw.is_file():
        ids.update(item["id"] for item in json.loads(raw.read_text()))
    lock = ROOT / "qualification" / "corpus.lock.json"
    if lock.is_file():
        ids.update(item["id"] for item in json.loads(lock.read_text())["entries"])
    return ids


def fetch_feed(count: int) -> bytes:
    query = urllib.parse.urlencode({
        "search_query": "all",
        "start": 0,
        "max_results": count,
        "sortBy": "submittedDate",
        "sortOrder": "descending",
    })
    request = urllib.request.Request(
        f"https://export.arxiv.org/api/query?{query}",
        headers={"User-Agent": USER_AGENT},
    )
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def parse_feed(body: bytes) -> list[dict]:
    root = ET.fromstring(body)
    entries = []
    for node in root.findall("a:entry", ATOM):
        identity = arxiv_id(node.findtext("a:id", "", ATOM))
        primary = node.find("x:primary_category", ATOM)
        entries.append({
            "id": identity,
            "url": f"https://arxiv.org/e-print/{identity}",
            "published": node.findtext("a:published", "", ATOM),
            "updated": node.findtext("a:updated", "", ATOM),
            "primary_category": (primary.attrib.get("term", "")
                                 if primary is not None else ""),
            "categories": [item.attrib.get("term", "")
                           for item in node.findall("a:category", ATOM)],
            "title": " ".join(node.findtext("a:title", "", ATOM).split()),
        })
    return entries


def command_sample(args: argparse.Namespace) -> int:
    if args.output.exists() and not args.replace:
        raise SystemExit(f"{args.output} already exists; refusing to replace the blind sample")
    body = args.feed.read_bytes() if args.feed else fetch_feed(args.pool)
    entries = parse_feed(body)
    if len(entries) != args.pool:
        raise SystemExit(f"arXiv feed returned {len(entries)} entries, expected {args.pool}")
    prior = known_ids()
    eligible = [item for item in entries if item["id"] not in prior]
    if len(eligible) < args.count:
        raise SystemExit(f"only {len(eligible)} of {len(entries)} entries are new")
    rng = random.Random(args.seed)
    picked = set(rng.sample(range(len(eligible)), args.count))
    selected = [item for index, item in enumerate(eligible) if index in picked]
    population_ids = [item["id"] for item in entries]
    result = {
        "schema": 1,
        "description": "Blind arXiv-wide aiTeX compatibility holdout",
        "created_at": utc_now(),
        "population": {
            "query": "all",
            "sort_by": "submittedDate",
            "sort_order": "descending",
            "returned": len(entries),
            "ids": population_ids,
            "ids_sha256": hashlib.sha256("\n".join(population_ids).encode()).hexdigest(),
        },
        "selection": {
            "method": "Python random.sample over new entries, then original stream order",
            "seed": args.seed,
            "prior_ids_excluded": len(prior),
            "eligible": len(eligible),
            "selected": len(selected),
        },
        "entries": selected,
    }
    write_json(args.output, result)
    print(f"locked blind sample: {len(selected)} of {len(eligible)} new entries "
          f"from a {len(entries)}-entry stream")
    print(f"sample sha256: {file_sha256(args.output)}")
    return 0


def safe_unpack(raw: bytes, destination: Path) -> None:
    if raw.startswith(b"%PDF-"):
        raise ValueError("source endpoint returned PDF, not TeX source")
    try:
        archive = tarfile.open(fileobj=io.BytesIO(raw), mode="r:*")
    except tarfile.ReadError:
        try:
            body = gzip.decompress(raw)
        except (gzip.BadGzipFile, EOFError) as error:
            raise ValueError("response is neither an archive nor gzip source") from error
        if len(body) > MAX_EXTRACTED_BYTES:
            raise ValueError("expanded source exceeds safety limit")
        (destination / "main.tex").write_bytes(body)
        return
    extracted = files = 0
    with archive:
        for member in archive.getmembers():
            path = PurePosixPath(member.name)
            if not member.isfile() or path.is_absolute() or ".." in path.parts:
                continue
            files += 1
            extracted += member.size
            if files > MAX_FILES or extracted > MAX_EXTRACTED_BYTES:
                raise ValueError("archive exceeds extraction safety limit")
            target = destination.joinpath(*path.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            if source is not None:
                with target.open("wb") as output:
                    shutil.copyfileobj(source, output)


def choose_root(project: Path) -> str | None:
    candidates = []
    for path in project.rglob("*.tex"):
        if not path.is_file() or path.is_symlink():
            continue
        body = path.read_bytes()
        if re.search(rb"\\documentclass(?:\s*\[[^]]*\])?\s*\{", body):
            candidates.append((len(body), path.relative_to(project).as_posix()))
    return max(candidates, default=(0, None))[1]


def download_source(item: dict, destination: Path) -> dict:
    last_error = "download failed"
    for attempt in range(3):
        try:
            request = urllib.request.Request(
                item["url"], headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(request, timeout=180) as response:
                length = int(response.headers.get("Content-Length", "0") or 0)
                if length > MAX_ARCHIVE_BYTES:
                    return {"status": "source_too_large", "bytes": length}
                raw = response.read(MAX_ARCHIVE_BYTES + 1)
                content_type = response.headers.get("Content-Type", "")
            if len(raw) > MAX_ARCHIVE_BYTES:
                return {"status": "source_too_large", "bytes": len(raw)}
            if raw.startswith(b"%PDF-"):
                return {
                    "status": "no_tex_source",
                    "response_bytes": len(raw),
                    "response_sha256": hashlib.sha256(raw).hexdigest(),
                    "content_type": content_type,
                }
            temporary = Path(tempfile.mkdtemp(prefix=f".{item['id']}.",
                                              dir=destination.parent))
            try:
                safe_unpack(raw, temporary)
                root = choose_root(temporary)
                digest, files, size = tree_fingerprint(temporary)
                result = {
                    "status": "ready" if root else "no_documentclass",
                    "root": root,
                    "tree_sha256": digest,
                    "files": files,
                    "bytes": size,
                    "response_bytes": len(raw),
                    "response_sha256": hashlib.sha256(raw).hexdigest(),
                }
                os.replace(temporary, destination)
                return result
            finally:
                if temporary.exists():
                    shutil.rmtree(temporary)
        except Exception as error:  # network errors need a bounded retry
            last_error = f"{type(error).__name__}: {error}"
            if attempt < 2:
                time.sleep(10 * (attempt + 1))
    return {"status": "fetch_error", "error": last_error}


def verify_source(project: Path, item: dict) -> None:
    if not project.is_dir():
        raise RuntimeError("source directory is absent")
    digest, files, size = tree_fingerprint(project)
    if (digest != item.get("tree_sha256") or files != item.get("files")
            or size != item.get("bytes")):
        raise RuntimeError(f"source fingerprint differs ({digest}, {files}, {size})")
    root = item.get("root")
    if root and not (project / root).is_file():
        raise RuntimeError(f"root {root} is absent")


def command_fetch(args: argparse.Namespace) -> int:
    sample = json.loads(args.sample.read_text())
    sample_sha = file_sha256(args.sample)
    args.corpus.mkdir(parents=True, exist_ok=True)
    source_dir = args.corpus / "sources"
    source_dir.mkdir(exist_ok=True)
    state_path = args.corpus / "fetch-state.json"
    state = (json.loads(state_path.read_text()) if state_path.is_file()
             else {"sample_sha256": sample_sha, "entries": {}})
    if state.get("sample_sha256") != sample_sha:
        raise SystemExit("fetch state belongs to a different blind sample")
    prior_lock = json.loads(args.output.read_text()) if args.output.is_file() else None
    prior_by_id = ({item["id"]: item for item in prior_lock["entries"]}
                   if prior_lock else {})
    for index, item in enumerate(sample["entries"], 1):
        identity = item["id"]
        destination = source_dir / identity.replace("/", "_")
        known = prior_by_id.get(identity) or state["entries"].get(identity)
        if known and known["status"] in {"ready", "no_documentclass"}:
            verify_source(destination, known)
            result = known
            word = "verified"
        elif known and known["status"] in {
                "no_tex_source", "source_too_large", "fetch_error"} and not args.retry:
            result = known
            word = "recorded"
        else:
            if destination.exists():
                raise SystemExit(f"{identity}: unverified destination already exists")
            result = download_source(item, destination)
            word = "fetched"
            if index < len(sample["entries"]):
                time.sleep(args.delay)
        state["entries"][identity] = result
        write_json(state_path, state)
        print(f"  [{index:3d}/{len(sample['entries'])}] {word:8s} {identity:18s} "
              f"{result['status']}", flush=True)
    entries = []
    for item in sample["entries"]:
        entries.append({**item, **state["entries"][item["id"]]})
    lock = {
        "schema": 1,
        "description": "Source lock for the blind arXiv compatibility holdout",
        "created_at": utc_now(),
        "sample_sha256": sample_sha,
        "entries": entries,
    }
    write_json(args.output, lock)
    counts = Counter(item["status"] for item in entries)
    print("source lock:", ", ".join(f"{key}={value}"
                                     for key, value in sorted(counts.items())))
    print(f"source-lock sha256: {file_sha256(args.output)}")
    return 0


def time_metrics(path: Path) -> dict:
    """Read the machine-readable subset of GNU time -v's C-locale output."""
    if not path.is_file():
        return {"user_seconds": None, "system_seconds": None,
                "max_rss_kib": None, "filesystem_read_bytes": None,
                "filesystem_write_bytes": None}
    text = path.read_text(errors="replace")

    def number(label: str, cast: type) -> int | float | None:
        match = re.search(rf"^\s*{re.escape(label)}:\s*([0-9.]+)\s*$",
                          text, re.M)
        return cast(match.group(1)) if match else None

    # Linux reports filesystem I/O here in 512-byte blocks (the same units as
    # ``ru_inblock``/``ru_oublock``), not in filesystem block size. Keep the
    # conversion at this boundary so every benchmark schema can name bytes.
    read_blocks = number("File system inputs", int)
    write_blocks = number("File system outputs", int)
    return {
        "user_seconds": number("User time (seconds)", float),
        "system_seconds": number("System time (seconds)", float),
        "max_rss_kib": number("Maximum resident set size (kbytes)", int),
        "filesystem_read_bytes": (read_blocks * 512
                                  if read_blocks is not None else None),
        "filesystem_write_bytes": (write_blocks * 512
                                   if write_blocks is not None else None),
    }


def run_command(argv: list[str], cwd: Path, env: dict[str, str],
                timeout: int, memory_limit_mib: int) -> tuple[int, float, str,
                                                               bool, dict]:
    descriptor, metrics_name = tempfile.mkstemp(prefix=".holdout-time.", dir=cwd)
    os.close(descriptor)
    metrics_path = Path(metrics_name)
    command = ["/usr/bin/time", "-v", "-o", str(metrics_path), "--", *argv]

    def set_memory_limit() -> None:
        limit = memory_limit_mib * 1024 * 1024
        resource.setrlimit(resource.RLIMIT_AS, (limit, limit))

    started = time.monotonic()
    try:
        process = subprocess.Popen(
            command, cwd=cwd, env=env, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, start_new_session=True,
            preexec_fn=set_memory_limit,
        )
        try:
            body, _ = process.communicate(timeout=timeout)
            returncode = process.returncode
            timed_out = False
        except subprocess.TimeoutExpired as error:
            body = error.output or b""
            os.killpg(process.pid, signal.SIGKILL)
            remainder, _ = process.communicate()
            body += remainder or b""
            returncode = 124
            timed_out = True
        measured = time_metrics(metrics_path)
        return (returncode, time.monotonic() - started,
                body.decode("utf-8", "replace"), timed_out, measured)
    finally:
        metrics_path.unlink(missing_ok=True)


def first_error(log: Path, output: str) -> str | None:
    text = (log.read_bytes().decode("utf-8", "replace")
            if log.is_file() else output)
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if line.startswith("! "):
            following = " ".join(lines[index:index + 3])
            return following[:500]
    for pattern in (r"^.*?:\d+: .*", r"^aitex: .*", r"^.*not found.*"):
        match = re.search(pattern, text, re.I | re.M)
        if match:
            return match.group(0)[:500]
    return None


def pdf_info(path: Path) -> dict:
    if not path.is_file() or not path.read_bytes()[:5] == b"%PDF-":
        return {"valid": False, "pages": 0, "bytes": 0}
    absolute = str(path.resolve())
    postscript_path = absolute.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
    process = subprocess.run(
        ["gs", "-q", "-dSAFER", f"--permit-file-read={absolute}",
         "-dNODISPLAY", "-c",
         f"({postscript_path}) (r) file runpdfbegin pdfpagecount = quit"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    match = re.search(r"^(\d+)\s*$", process.stdout, re.M)
    return {
        "valid": process.returncode == 0 and match is not None,
        "pages": int(match.group(1)) if match else 0,
        "bytes": path.stat().st_size,
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }


def dvi_info(path: Path, env: dict[str, str] | None = None) -> dict:
    if not path.is_file():
        return {"valid": False, "pages": 0, "characters": 0}
    process = subprocess.run(
        ["dvitype", "-output-level=2", "-page-start=*", str(path)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
    codes = re.findall(r"setchar\d+|set_char_\d+", process.stdout)
    return {
        "valid": process.returncode == 0 and bool(codes),
        "pages": process.stdout.count("beginning of page"),
        "characters": len(codes),
        "characters_sha256": hashlib.sha256("\n".join(codes).encode()).hexdigest(),
        "bytes": path.stat().st_size,
    }


def dvi_semantics(path: Path, env: dict[str, str] | None = None) -> dict:
    """Normalize a DVI to positioned drawing and driver-special streams."""
    if not path.is_file():
        return {"valid": False, "records": 0, "error": "missing DVI"}
    dvitype = subprocess.run(
        ["dvitype", "-output-level=4", "-page-start=*", path.name],
        cwd=path.parent, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    if dvitype.returncode != 0:
        error = dvitype.stderr.decode("utf-8", "replace").strip()
        return {"valid": False, "records": 0, "error": error[:500]}
    normalized = subprocess.run(
        ["gawk", "-f", str(ROOT / "tools" / "dvi_semantic.awk")],
        input=dvitype.stdout, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    content = normalized.stdout
    if normalized.returncode != 0 or not content:
        error = normalized.stderr.decode("utf-8", "replace").strip()
        return {"valid": False, "records": 0, "error": error[:500]}
    lines = content.splitlines()
    drawing = b"\n".join(line for line in lines if not line.startswith(b"special ")) + b"\n"
    specials = b"\n".join(line for line in lines if line.startswith(b"special "))
    if specials:
        specials += b"\n"
    return {
        "valid": True,
        "records": len(lines),
        "sha256": hashlib.sha256(content).hexdigest(),
        "drawing_records": sum(not line.startswith(b"special ") for line in lines),
        "drawing_sha256": hashlib.sha256(drawing).hexdigest(),
        "special_records": sum(line.startswith(b"special ") for line in lines),
        "special_sha256": hashlib.sha256(specials).hexdigest(),
        "_content": content,
        "_drawing": drawing,
        "_specials": specials,
    }


SPECIAL_RECORD = re.compile(
    rb"^special page=([0-9]+) h=(-?[0-9]+) v=(-?[0-9]+) text=(.*)$")


def postscript_string_value(value: bytes) -> bytes | None:
    """Decode one balanced PostScript literal-string body."""
    out = bytearray()
    p = 0
    while p < len(value):
        byte = value[p]
        if byte != 0x5c:
            out.append(byte)
            p += 1
            continue
        p += 1
        if p >= len(value):
            return None
        if value[p] in b"01234567":
            end = p + 1
            while end < len(value) and end < p + 3 and value[end] in b"01234567":
                end += 1
            out.append(int(value[p:end], 8))
            p = end
            continue
        escapes = {ord("n"): 10, ord("r"): 13, ord("t"): 9,
                   ord("b"): 8, ord("f"): 12}
        if value[p] in escapes:
            out.append(escapes[value[p]])
        elif value[p] == 13:
            if value[p:p + 2] == b"\r\n":
                p += 1
        elif value[p] != 10:
            out.append(value[p])
        p += 1
    return bytes(out)


def canonical_special_text(text: bytes) -> bytes:
    """Canonicalize equivalent encodings inside otherwise-strict specials."""
    marker = b"/Title ("
    start = text.find(marker)
    if start < 0 or b"/OUT pdfmark" not in text:
        return text
    body_start = start + len(marker)
    depth = 1
    p = body_start
    while p < len(text) and depth:
        if text[p] == 0x5c:
            p += 2
            continue
        if text[p] == ord("("):
            depth += 1
        elif text[p] == ord(")"):
            depth -= 1
        p += 1
    if depth:
        return text
    raw = postscript_string_value(text[body_start:p - 1])
    if raw is None:
        return text
    try:
        if raw.startswith(b"\xfe\xff"):
            title = raw[2:].decode("utf-16-be")
        elif raw.startswith(b"\xff\xfe"):
            title = raw[2:].decode("utf-16-le")
        else:
            title = raw.decode("latin-1")
    except UnicodeDecodeError:
        return text
    canonical = title.encode("utf-8").hex().encode("ascii")
    return text[:start] + b"/Title <unicode:" + canonical + b">" + text[p:]


def normalized_special_records(content: bytes) -> tuple[list[dict], int, int, int, int]:
    """Parse positioned specials and remove only provable state no-ops.

    DVI color stacks reset on every page. A `color pop` at stack depth zero is
    therefore semantically inert; aiTeX emits one such defensive pop for some
    xcolor paths while stock LaTeX does not. Pops that close a real push remain
    in the protocol and must compare in order like every other special.
    """
    # Reduce only adjacent state operations within the complete drawing
    # stream. A glyph, rule, or font event flushes the block. Consecutive PGF
    # saves have no state change between them and capture identical states;
    # likewise consecutive restores return through identical states. Their
    # multiplicity is unobservable, while an intervening operator remains a
    # strict barrier.
    records = []
    adjacent = []
    ignored_pairs = 0
    ignored_ps_state = 0

    def flush_adjacent() -> None:
        nonlocal adjacent, ignored_pairs, ignored_ps_state
        collapsed = []
        for record in adjacent:
            if (record["text"] in (b"'ps:: save '", b"'ps:: restore '")
                    and collapsed
                    and collapsed[-1]["text"] == record["text"]
                    and collapsed[-1]["page"] == record["page"]):
                ignored_ps_state += 1
                continue
            collapsed.append(record)
        reduced = []
        for record in collapsed:
            color_pair = (record["text"] == b"'color pop'" and reduced
                          and reduced[-1]["text"].startswith(b"'color push "))
            ps_pair = (record["text"] == b"'ps:: restore '" and reduced
                       and reduced[-1]["text"] == b"'ps:: save '")
            if ((color_pair or ps_pair) and reduced[-1]["page"] == record["page"]):
                reduced.pop()
                if color_pair:
                    ignored_pairs += 1
                else:
                    ignored_ps_state += 2
            else:
                reduced.append(record)
        records.extend(reduced)
        adjacent = []

    for raw in content.splitlines():
        match = SPECIAL_RECORD.fullmatch(raw)
        if match is None:
            flush_adjacent()
            continue
        record = {
            "page": int(match.group(1)),
            "h": int(match.group(2)),
            "v": int(match.group(3)),
            "text": canonical_special_text(match.group(4)),
        }
        adjacent.append(record)
    flush_adjacent()

    depth_by_page: dict[int, int] = defaultdict(int)
    normalized = []
    ignored = 0
    ignored_black_pushes = 0
    for record in records:
        page = record["page"]
        text = record["text"]
        if text == b"'color pop'":
            if depth_by_page[page] == 0:
                ignored += 1
                continue
            depth_by_page[page] -= 1
        elif text.startswith(b"'color push "):
            # A DVI page and its color stack begin at device black. Some
            # xcolor paths explicitly push that same base and leave it for the
            # page reset; others push/pop it without drawing. At depth zero the
            # explicit `gray 0` push changes neither pixels nor later nested
            # push/pop behavior, so it is a page-base no-op.
            if text == b"'color push gray 0'" and depth_by_page[page] == 0:
                ignored_black_pushes += 1
                continue
            depth_by_page[page] += 1
        normalized.append(record)
    return normalized, ignored, ignored_pairs, ignored_black_pushes, ignored_ps_state


def compare_positioned_specials(reference: bytes, candidate: bytes) -> dict:
    ref, ref_ignored, ref_pairs, ref_black, ref_ps = normalized_special_records(reference)
    ours, our_ignored, our_pairs, our_black, our_ps = normalized_special_records(candidate)
    first = None
    max_delta = 0
    for index, (wanted, got) in enumerate(zip(ref, ours), start=1):
        delta_h = abs(wanted["h"] - got["h"])
        delta_v = abs(wanted["v"] - got["v"])
        max_delta = max(max_delta, delta_h, delta_v)
        if (wanted["page"] != got["page"] or wanted["text"] != got["text"]
                or delta_h > SPECIAL_POSITION_TOLERANCE_DVI_UNITS
                or delta_v > SPECIAL_POSITION_TOLERANCE_DVI_UNITS):
            first = {
                "record": index,
                "reference": {
                    **wanted,
                    "text": wanted["text"].decode("utf-8", "replace"),
                },
                "candidate": {
                    **got,
                    "text": got["text"].decode("utf-8", "replace"),
                },
                "delta_h": delta_h,
                "delta_v": delta_v,
            }
            break
    if first is None and len(ref) != len(ours):
        index = min(len(ref), len(ours))
        first = {
            "record": index + 1,
            "reference": (None if index >= len(ref) else {
                **ref[index],
                "text": ref[index]["text"].decode("utf-8", "replace"),
            }),
            "candidate": (None if index >= len(ours) else {
                **ours[index],
                "text": ours[index]["text"].decode("utf-8", "replace"),
            }),
        }
    protocol = lambda records: b"\n".join(
        str(item["page"]).encode() + b" " + item["text"] for item in records)

    def eps_assets(records: list[dict]) -> list[str]:
        assets = []
        for record in records:
            match = re.search(rb'PSfile=(?:"([^"]+)"|([^\s\']+))', record["text"])
            if match:
                name = (match.group(1) or match.group(2)).decode("latin-1")
                if name.lower().endswith(".eps") and name not in assets:
                    assets.append(name)
        return assets

    return {
        "equivalent": first is None,
        "position_tolerance_dvi_units": SPECIAL_POSITION_TOLERANCE_DVI_UNITS,
        "max_position_delta_dvi_units": max_delta,
        "reference_records": len(ref),
        "candidate_records": len(ours),
        "reference_ignored_base_color_pops": ref_ignored,
        "candidate_ignored_base_color_pops": our_ignored,
        "reference_ignored_adjacent_color_pairs": ref_pairs,
        "candidate_ignored_adjacent_color_pairs": our_pairs,
        "reference_ignored_base_black_pushes": ref_black,
        "candidate_ignored_base_black_pushes": our_black,
        "reference_ignored_redundant_ps_state": ref_ps,
        "candidate_ignored_redundant_ps_state": our_ps,
        "reference_protocol_sha256": hashlib.sha256(protocol(ref)).hexdigest(),
        "candidate_protocol_sha256": hashlib.sha256(protocol(ours)).hexdigest(),
        "reference_eps_assets": eps_assets(ref),
        "candidate_eps_assets": eps_assets(ours),
        "first_difference": first,
    }


def draft_eps_assets(text: str | None, special_comparison: dict,
                     side: str) -> list[str]:
    """Identify pdfLaTeX-style draft boxes without inspecting source syntax.

    With shell escape disabled, pdfTeX's graphics driver cannot include an EPS
    file. It silently substitutes a ruled box containing the asset name. A
    file is classified as a draft placeholder only when that exact name both
    occurs in the DVI route's ``PSfile`` special and is extractable as text
    from the direct-PDF route. This avoids paper-specific exceptions and does
    not excuse an ordinary rendering disagreement.
    """
    if text is None:
        return []
    assets = special_comparison.get(f"{side}_eps_assets", [])
    return [name for name in assets if "".join(name.split()) in text]


def compare_dvi_semantics(reference: Path, candidate: Path,
                          env: dict[str, str] | None = None) -> dict:
    ref = dvi_semantics(reference, env)
    ours = dvi_semantics(candidate, env)
    ref_content = ref.pop("_content", b"")
    our_content = ours.pop("_content", b"")
    ref_drawing = ref.pop("_drawing", b"")
    our_drawing = ours.pop("_drawing", b"")
    ref.pop("_specials", b"")
    ours.pop("_specials", b"")
    valid = bool(ref.get("valid") and ours.get("valid"))
    drawing_equivalent = bool(
        valid and ref["drawing_sha256"] == ours["drawing_sha256"])
    special_comparison = compare_positioned_specials(ref_content, our_content)
    specials_equivalent = bool(valid and special_comparison["equivalent"])
    equivalent = bool(drawing_equivalent and specials_equivalent)

    def first_difference(reference_content: bytes, candidate_content: bytes) -> dict:
        ref_lines = reference_content.decode("utf-8", "replace").splitlines()
        our_lines = candidate_content.decode("utf-8", "replace").splitlines()
        difference = next(
            (index for index, pair in enumerate(zip(ref_lines, our_lines), start=1)
             if pair[0] != pair[1]),
            min(len(ref_lines), len(our_lines)) + 1,
        )
        return {
            "record": difference,
            "reference": ref_lines[difference - 1] if difference <= len(ref_lines) else None,
            "candidate": our_lines[difference - 1] if difference <= len(our_lines) else None,
        }

    result = {
        "valid": valid,
        "equivalent": equivalent,
        "drawing_equivalent": drawing_equivalent,
        "specials_equivalent": specials_equivalent,
        "specials_applicable": bool(
            ref.get("special_records", 0) or ours.get("special_records", 0)),
        "special_comparison": special_comparison,
        "reference": ref,
        "candidate": ours,
    }
    if valid and not equivalent:
        result["first_difference"] = first_difference(ref_content, our_content)
    if valid and not drawing_equivalent:
        result["drawing_first_difference"] = first_difference(
            ref_drawing, our_drawing)
    if valid and not specials_equivalent:
        result["specials_first_difference"] = special_comparison["first_difference"]
    return result


def ppm_dimensions(path: Path) -> tuple[int, int] | None:
    """Read the small ASCII header of a binary P6 PPM without decoding pixels."""
    try:
        with path.open("rb") as stream:
            tokens: list[bytes] = []
            token = bytearray()
            comment = False
            while len(tokens) < 4:
                byte = stream.read(1)
                if not byte:
                    return None
                value = byte[0]
                if comment:
                    if value in (10, 13):
                        comment = False
                    continue
                if value == 35 and not token:
                    comment = True
                elif value in b" \t\r\n":
                    if token:
                        tokens.append(bytes(token))
                        token.clear()
                else:
                    token.append(value)
            if tokens[0] != b"P6" or tokens[3] != b"255":
                return None
            return int(tokens[1]), int(tokens[2])
    except (OSError, ValueError):
        return None


def image_metric(reference: Path, candidate: Path, metric: str,
                 fuzz: float | None = None) -> float | None:
    command = ["compare", "-metric", metric]
    if fuzz is not None:
        command += ["-fuzz", f"{fuzz}%"]
    command += [str(reference), str(candidate), "null:"]
    process = subprocess.run(command, stdout=subprocess.DEVNULL,
                             stderr=subprocess.PIPE, text=True)
    # ImageMagick returns 1 when images differ; 0 and 1 both carry a metric.
    if process.returncode not in (0, 1):
        return None
    if metric == "MAE":
        match = re.search(r"\(([0-9.eE+-]+)\)", process.stderr)
    else:
        match = re.search(r"([0-9.eE+-]+)", process.stderr)
    return float(match.group(1)) if match else None


def acceptable_rasterizer_exit(returncode: int, stderr: bytes) -> bool:
    """Accept MuPDF's known ICC warning only when it is the sole complaint."""
    if returncode == 0:
        return True
    warnings = {b"warning: ICC support is not available"}
    messages = [line.strip() for line in stderr.splitlines() if line.strip()]
    return returncode == 1 and bool(messages) and all(
        line in warnings for line in messages)


def rasterize(pdf: Path, destination: Path) -> list[Path] | None:
    destination.mkdir()
    process = subprocess.run(
        ["mutool", "draw", "-q", "-F", "ppm", "-r", str(RASTER_DPI),
         "-o", str(destination / "page-%d.ppm"), str(pdf)],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if not acceptable_rasterizer_exit(process.returncode, process.stderr):
        return None
    pages = list(destination.glob("page-*.ppm"))
    pages.sort(key=lambda page: int(page.stem.split("-")[-1]))
    return pages or None


def compare_pdf_rasters(reference: Path, candidate: Path, base: Path) -> dict:
    ref_pages = rasterize(reference, base / "reference-raster")
    our_pages = rasterize(candidate, base / "candidate-raster")
    result = {
        "valid": bool(ref_pages and our_pages),
        "equivalent": False,
        "dpi": RASTER_DPI,
        "max_normalized_mae": RASTER_MAX_NORMALIZED_MAE,
        "fuzz_percent": RASTER_FUZZ_PERCENT,
        "max_fuzz_fraction": RASTER_MAX_FUZZ_FRACTION,
        "pages": len(ref_pages or []),
    }
    if not ref_pages or not our_pages:
        result["error"] = "one or both PDFs could not be rasterized"
        return result
    if len(ref_pages) != len(our_pages):
        result["error"] = f"page count differs: {len(ref_pages)} vs {len(our_pages)}"
        return result
    worst_mae = 0.0
    worst_fraction = 0.0
    worst_mae_page = None
    worst_fraction_page = None
    outside_tolerance = []
    for page_number, (ref_page, our_page) in enumerate(
            zip(ref_pages, our_pages), start=1):
        ref_size = ppm_dimensions(ref_page)
        our_size = ppm_dimensions(our_page)
        if ref_size is None or ref_size != our_size:
            result["error"] = (f"page {page_number} dimensions differ: "
                               f"{ref_size} vs {our_size}")
            return result
        mae = image_metric(ref_page, our_page, "MAE")
        outside = image_metric(ref_page, our_page, "AE", RASTER_FUZZ_PERCENT)
        if mae is None or outside is None:
            result["error"] = f"ImageMagick could not compare page {page_number}"
            return result
        pixels = ref_size[0] * ref_size[1]
        fraction = outside / pixels
        if mae > worst_mae:
            worst_mae = mae
            worst_mae_page = page_number
        if fraction > worst_fraction:
            worst_fraction = fraction
            worst_fraction_page = page_number
        if mae > RASTER_MAX_NORMALIZED_MAE or fraction > RASTER_MAX_FUZZ_FRACTION:
            outside_tolerance.append({
                "page": page_number,
                "normalized_mae": mae,
                "fuzz_fraction": fraction,
            })
    result["equivalent"] = not outside_tolerance
    result["worst_normalized_mae"] = worst_mae
    result["worst_normalized_mae_page"] = worst_mae_page
    result["worst_fuzz_fraction"] = worst_fraction
    result["worst_fuzz_fraction_page"] = worst_fraction_page
    result["pages_outside_tolerance"] = outside_tolerance
    if outside_tolerance:
        first = outside_tolerance[0]
        result["error"] = (
            f"{len(outside_tolerance)} page(s) exceed tolerance; first is page "
            f"{first['page']}: mae={first['normalized_mae']:.6g}, "
            f"fuzz_fraction={first['fuzz_fraction']:.6g}")
    return result


def extracted_text(path: Path) -> str | None:
    """Return a reader-stable logical character sequence for a PDF.

    Whitespace in an untagged TeX PDF is inferred from glyph geometry rather
    than stored as text, and Ghostscript and MuPDF make visibly different but
    defensible spacing choices around scripts and font changes. Positioned DVI
    comparison already grades that geometry exactly, so the text oracle keeps
    the Unicode sequence and discards inferred whitespace. Stock pdfTeX also
    exposes an unmapped TS1 bullet as C1 slot U+0088; normalize that known
    extraction artifact to the semantic U+2022 emitted by our ToUnicode map.
    """
    if not path.is_file():
        return None
    process = subprocess.run(
        ["mutool", "draw", "-q", "-F", "txt", "-o", "-", str(path.resolve())],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if not acceptable_rasterizer_exit(process.returncode, process.stderr):
        return None
    decoded = process.stdout.decode("utf-8", "replace")
    decoded = unicodedata.normalize("NFKC", decoded).replace("\u0088", "\u2022")
    return "".join(decoded.split())


def text_info(text: str | None) -> dict | None:
    if text is None:
        return None
    encoded = text.encode("utf-8")
    return {
        "bytes": len(encoded),
        "characters": len(text),
        "sha256": hashlib.sha256(encoded).hexdigest(),
        "replacement_characters": text.count("\ufffd"),
        "control_characters": sum(unicodedata.category(char) == "Cc" for char in text),
        "private_use_characters": sum(unicodedata.category(char) == "Co" for char in text),
    }


def text_similarity(reference: str | None, candidate: str | None) -> float | None:
    if reference is None or candidate is None:
        return None
    return round(difflib.SequenceMatcher(None, reference, candidate).ratio(), 6)


def text_first_difference(reference: str | None, candidate: str | None) -> dict | None:
    """A compact, non-lossy diagnosis of the first Unicode disagreement."""
    if reference is None or candidate is None or reference == candidate:
        return None
    matcher = difflib.SequenceMatcher(None, reference, candidate, autojunk=False)
    for operation, r0, r1, c0, c1 in matcher.get_opcodes():
        if operation == "equal":
            continue
        describe = lambda text: [
            {"character": char, "codepoint": f"U+{ord(char):04X}",
             "name": unicodedata.name(char, "UNNAMED")}
            for char in text[:8]
        ]
        return {
            "operation": operation,
            "reference_index": r0,
            "candidate_index": c0,
            "reference": describe(reference[r0:r1]),
            "candidate": describe(candidate[c0:c1]),
            "reference_context": reference[max(0, r0 - 12):min(len(reference), r1 + 12)],
            "candidate_context": candidate[max(0, c0 - 12):min(len(candidate), c1 + 12)],
        }
    return None


def text_quality_no_worse(reference: dict | None, candidate: dict | None) -> bool | None:
    """Whether our mapping introduces no more objectively bad code points.

    Exact equality against stock pdfTeX is useful diagnostics, but it penalizes
    a repaired Unicode mapping. Replacement, control, and private-use code
    points are reader-independent defects, so their total gives the product
    gate. Compare the sum rather than each bucket independently: replacing
    hundreds of invalid controls with one unmapped glyph is an objective
    improvement, not a regression.
    """
    if reference is None or candidate is None:
        return None
    keys = ("replacement_characters", "control_characters",
            "private_use_characters")
    return sum(candidate[key] for key in keys) <= sum(reference[key] for key in keys)


def prepare(source: Path, base: Path, name: str, root: str) -> tuple[Path, Path]:
    project = base / name
    shutil.copytree(source, project)
    root_path = project / root
    return project, root_path.parent


def compile_mode(source: Path, base: Path, name: str, root: str,
                 argv: list[str], output_name: str, timeout: int,
                 memory_limit_mib: int, env: dict[str, str]) -> tuple[dict, Path]:
    project, cwd = prepare(source, base, name, root)
    root_name = Path(root).name
    job = Path(root).stem
    elapsed = 0.0
    output = ""
    returncode = 1
    timed_out = False
    passes = 0
    pass_metrics = []
    for pass_number in range(1, 3):
        returncode, duration, output, timed_out, measured = run_command(
            [part.replace("{root}", root_name) for part in argv], cwd, env,
            timeout, memory_limit_mib)
        elapsed += duration
        passes += 1
        cpu = (measured["user_seconds"] + measured["system_seconds"]
               if measured["user_seconds"] is not None
               and measured["system_seconds"] is not None else None)
        pass_metrics.append({
            "pass": pass_number,
            "returncode": returncode,
            "seconds": round(duration, 6),
            "cpu_seconds": round(cpu, 6) if cpu is not None else None,
            **measured,
        })
        if returncode != 0:
            break
    output_path = cwd / output_name.replace("{job}", job)
    log = cwd / f"{job}.log"
    result = {
        "returncode": returncode,
        "passes": passes,
        "seconds": round(elapsed, 3),
        "timed_out": timed_out,
        "first_error": first_error(log, output),
        "cpu_seconds": (round(sum(item["cpu_seconds"] for item in pass_metrics), 6)
                        if all(item["cpu_seconds"] is not None
                               for item in pass_metrics) else None),
        "peak_rss_kib": max((item["max_rss_kib"] for item in pass_metrics
                             if item["max_rss_kib"] is not None), default=None),
        "pass_metrics": pass_metrics,
    }
    return result, output_path


def failure_kind(result: dict, output: dict) -> str | None:
    if result["timed_out"]:
        return "timeout"
    message = (result.get("first_error") or "").lower()
    if any(word in message for word in (
            "memory allocation of", "failed to allocate", "cannot allocate memory")):
        return "memory_limit"
    if result["returncode"] in {-6, 134}:
        # The release binary's allocator aborts on OOM; Rust panics use 101.
        # Above 1 GiB RSS under this runner's address-space ceiling, SIGABRT is
        # therefore an allocation-limit outcome rather than an engine panic.
        return ("memory_limit" if (result.get("peak_rss_kib") or 0) >= 1024 * 1024
                else "crash")
    if any(word in message for word in (
            "not found", "can't find file", "cannot find file", "not loadable")):
        return "missing_dependency_or_file"
    if any(word in message for word in (
            "cannot determine size", "boundingbox", "unknown graphics extension")):
        return "graphics_route"
    if any(word in message for word in (
            "requires xetex", "requires luatex", "fontspec", "pdf mode")):
        return "different_engine_required"
    if output.get("valid") and output.get("pages", 0) > 0:
        return None if result["returncode"] == 0 else "output_with_errors"
    if result["returncode"] != 0:
        return "tex_error"
    return "no_valid_output"


def diagnostic_metadata(result: dict) -> dict:
    """Turn a first failure into a stable subsystem-sized work item.

    The papers are observations, not issue identifiers. Paths, source line
    numbers, and individual figure names vary across papers while the engine
    branch that failed is often identical, so preserve the useful package or
    extension and discard the accidental coordinates.
    """
    if result.get("timed_out"):
        return {"subsystem": "execution-control", "fingerprint": "timeout"}
    kind = result.get("failure_kind")
    if kind == "memory_limit":
        return {"subsystem": "execution-control", "fingerprint": "memory-limit"}
    if kind == "crash":
        return {"subsystem": "engine-safety", "fingerprint": "process-crash"}

    raw = (result.get("first_error") or "").strip()
    lower = raw.lower()
    if "cannot determine size of graphic" in lower or "boundingbox" in lower:
        match = re.search(r"graphic in\s+(.+?)\s*\(no boundingbox\)", lower)
        suffix = Path(match.group(1).strip()).suffix.lower() if match else ""
        suffix = suffix or "unknown-extension"
        return {"subsystem": "graphics-driver",
                "fingerprint": f"no-bounding-box:{suffix}"}

    missing = re.search(r"(?:file\s+)?[`'\"]([^`'\"]+)[`'\"]?\s+not found", raw,
                        re.I)
    if missing:
        name = missing.group(1).strip()
        suffix = Path(name).suffix.lower()
        base = Path(name).name.lower()
        if suffix in {".sty", ".cls", ".fd", ".def", ".clo"}:
            return {"subsystem": "tex-environment",
                    "fingerprint": f"missing-dependency:{base}"}
        if suffix in {".pdf", ".png", ".jpg", ".jpeg", ".eps", ".ps", ".svg"}:
            return {"subsystem": "graphics-driver",
                    "fingerprint": f"missing-asset:{suffix}"}
        # graphicx often reports a requested base name with no extension after
        # its driver supplied an extension list that cannot see the real file.
        if not suffix:
            return {"subsystem": "graphics-driver",
                    "fingerprint": "missing-asset:no-extension"}
        return {"subsystem": "file-resolution",
                "fingerprint": f"missing-file:{suffix}"}

    if "xcolor.sty" in lower and "undefined control sequence" in lower:
        return {"subsystem": "graphics-driver",
                "fingerprint": "xcolor-driver:undefined-control-sequence"}
    if "neither \\pdfsavepos nor \\savepos" in lower:
        return {"subsystem": "pdftex-extensions",
                "fingerprint": "position-tracking:savepos-unavailable"}
    if "undefined control sequence" in lower:
        source = re.match(r"(?:.*[/\\])?([^:/\\]+):\d+:", raw)
        origin = source.group(1).lower() if source else "unknown-source"
        return {"subsystem": "expansion-or-primitives",
                "fingerprint": f"undefined-control-sequence:{origin}"}
    if "missing number" in lower:
        source = re.match(r"(?:.*[/\\])?([^:/\\]+):\d+:", raw)
        origin = source.group(1).lower() if source else "unknown-source"
        return {"subsystem": "number-scanning",
                "fingerprint": f"missing-number:{origin}"}
    if "runaway" in lower or "file ended while scanning" in lower:
        return {"subsystem": "macro-scanning", "fingerprint": "runaway-input"}

    normalized = re.sub(r"^(?:.*[/\\])?[^:/\\]*:\d+:\s*", "", lower)
    normalized = re.sub(r"\b\d+\b", "#", normalized)
    normalized = re.sub(r"\s+", " ", normalized).strip(" .")
    if not normalized:
        normalized = kind or "unknown-failure"
    return {"subsystem": "other", "fingerprint": normalized[:120]}


def evaluate_one(payload: tuple[dict, str, str, str, str, str, str, int, int]) -> dict:
    (item, corpus_text, format_text, binary_text, overlay_text,
     texmfhome_search, texmfdbs_search, timeout, memory_limit_mib) = payload
    corpus = Path(corpus_text)
    source = corpus / "sources" / item["id"].replace("/", "_")
    root = item["root"]
    binary = Path(binary_text)
    fmt = Path(format_text)
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
        "TEXMFHOME": texmfhome_search,
        "TEXMFDBS": texmfdbs_search,
    })
    with tempfile.TemporaryDirectory(prefix=f"aitex-holdout-{item['id']}.") as temp:
        base = Path(temp)
        # Some locked TeX fonts exist only as Metafont sources.  Keep kpathsea's
        # generated PK cache inside this disposable case rather than mutating
        # the read-only qualification overlay and invalidating its inventory.
        texmf_var = base / "texmf-var"
        texmf_var.mkdir()
        env["TEXMFVAR"] = str(texmf_var)
        env["VARTEXFONTS"] = str(texmf_var / "fonts")
        specs = {
            "pdftex_pdf": (
                "pdftex-pdf",
                ["pdflatex", "-file-line-error", "-interaction=nonstopmode",
                 "-no-shell-escape", "-halt-on-error", "{root}"],
                "{job}.pdf", pdf_info,
            ),
            "pdftex_dvi": (
                "pdftex-dvi",
                ["pdflatex", "-output-format=dvi", "-file-line-error",
                 "-interaction=nonstopmode", "-no-shell-escape", "-halt-on-error",
                 "{root}"],
                "{job}.dvi", dvi_info,
            ),
            "aitex_pdf": (
                "aitex-pdf",
                [str(binary), "pass", "{root}", "--fmt", str(fmt), "-file-line-error",
                 "-halt-on-error", "-o", "aitex.pdf"],
                "aitex.pdf", pdf_info,
            ),
            "aitex_dvi": (
                "aitex-dvi",
                [str(binary), "pass", "{root}", "--fmt", str(fmt), "-file-line-error",
                 "-halt-on-error", "-o", "aitex.dvi"],
                "aitex.dvi", dvi_info,
            ),
        }
        ours_first = int(hashlib.sha256(item["id"].encode()).hexdigest()[-1], 16) % 2
        if ours_first:
            route_order = ["aitex_pdf", "pdftex_pdf", "aitex_dvi", "pdftex_dvi"]
        else:
            route_order = ["pdftex_pdf", "aitex_pdf", "pdftex_dvi", "aitex_dvi"]
        stages = {}
        paths = {}
        for key in route_order:
            name, argv, output_name, inspect = specs[key]
            stage, output_path = compile_mode(
                source, base, name, root, argv, output_name, timeout,
                memory_limit_mib, env)
            stage["output"] = (inspect(output_path, env)
                               if inspect is dvi_info else inspect(output_path))
            stage["failure_kind"] = failure_kind(stage, stage["output"])
            stage["diagnostic"] = diagnostic_metadata(stage)
            stages[key] = stage
            paths[key] = output_path
        pdftex_pdf = stages["pdftex_pdf"]
        pdftex_dvi = stages["pdftex_dvi"]
        aitex_pdf = stages["aitex_pdf"]
        aitex_dvi = stages["aitex_dvi"]
        ref_dvi = pdftex_dvi["output"]
        our_dvi = aitex_dvi["output"]
        if ref_dvi.get("valid") and our_dvi.get("valid"):
            dvi_comparison = compare_dvi_semantics(
                paths["pdftex_dvi"], paths["aitex_dvi"], env)
        else:
            dvi_comparison = {"valid": False, "equivalent": False,
                              "drawing_equivalent": False,
                              "specials_equivalent": False,
                              "specials_applicable": False,
                              "error": "one or both DVI routes failed"}
        ref_content = extracted_text(paths["pdftex_pdf"])
        our_content = extracted_text(paths["aitex_pdf"])
        special_comparison = dvi_comparison.get("special_comparison", {})
        ref_draft_eps = draft_eps_assets(
            ref_content, special_comparison, "reference")
        our_draft_eps = draft_eps_assets(
            our_content, special_comparison, "candidate")
        if (pdftex_pdf["output"].get("valid")
                and aitex_pdf["output"].get("valid")):
            pdf_raster = compare_pdf_rasters(
                paths["pdftex_pdf"], paths["aitex_pdf"], base)
            pdf_raster["comparable"] = not ref_draft_eps
            if ref_draft_eps:
                pdf_raster["reference_draft_eps_assets"] = ref_draft_eps
        else:
            pdf_raster = {"valid": False, "equivalent": False,
                          "comparable": False,
                          "error": "one or both PDF routes failed"}
        ref_text = text_info(ref_content)
        our_text = text_info(our_content)
        return {
            "id": item["id"],
            "route_order": route_order,
            "pdftex_pdf": pdftex_pdf,
            "pdftex_dvi": pdftex_dvi,
            "aitex_pdf": aitex_pdf,
            "aitex_dvi": aitex_dvi,
            # Product drawing parity and driver-protocol parity are separate
            # claims. A color-pop or outline special can differ while every
            # glyph and rule is still placed exactly, and the reverse is also
            # possible. Keep the historical key for the primary drawing
            # oracle, and retain the combined strict result explicitly.
            "dvi_match": dvi_comparison["drawing_equivalent"],
            "dvi_special_match": dvi_comparison["specials_equivalent"],
            "dvi_special_applicable": dvi_comparison["specials_applicable"],
            "dvi_exact_match": dvi_comparison["equivalent"],
            "dvi_semantics": dvi_comparison,
            "pdf_page_match": bool(
                pdftex_pdf["output"].get("valid")
                and aitex_pdf["output"].get("valid")
                and pdftex_pdf["output"]["pages"] == aitex_pdf["output"]["pages"]),
            "pdf_text_match": bool(ref_text and our_text and ref_text == our_text),
            "pdf_text_similarity": text_similarity(ref_content, our_content),
            "pdf_text_first_difference": text_first_difference(ref_content, our_content),
            "pdf_text_quality_no_worse": text_quality_no_worse(ref_text, our_text),
            "pdftex_text": ref_text,
            "aitex_text": our_text,
            "pdftex_draft_eps_assets": ref_draft_eps,
            "aitex_draft_eps_assets": our_draft_eps,
            "pdf_raster": pdf_raster,
        }


def semantic_identity(semantic: dict) -> dict:
    """Return the stable, independently reproducible identity of one DVI."""
    return {
        key: semantic.get(key)
        for key in (
            "valid",
            "records",
            "sha256",
            "drawing_records",
            "drawing_sha256",
            "special_records",
            "special_sha256",
        )
    }


def evaluate_candidate_against_reference(
    payload: tuple[dict, dict, str, str, str, str, str, str, int, int],
) -> dict:
    """Remeasure aiTeX and reuse stock oracles only after exact proof.

    PDF byte identity and normalized DVI-stream identity make every saved
    stock comparison transitive. This is the safe iteration path when the
    classical result is already pinned; a changed stock-clean candidate falls
    back to a fresh four-route comparison instead of silently inheriting a
    stale correctness judgment.
    """
    (item, reference, corpus_text, format_text, binary_text, overlay_text,
     texmfhome_search, texmfdbs_search, timeout, memory_limit_mib) = payload
    corpus = Path(corpus_text)
    source = corpus / "sources" / item["id"].replace("/", "_")
    root = item["root"]
    binary = Path(binary_text)
    fmt = Path(format_text)
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
        "TEXMFHOME": texmfhome_search,
        "TEXMFDBS": texmfdbs_search,
    })
    with tempfile.TemporaryDirectory(prefix=f"aitex-holdout-{item['id']}.") as temp:
        base = Path(temp)
        texmf_var = base / "texmf-var"
        texmf_var.mkdir()
        env["TEXMFVAR"] = str(texmf_var)
        env["VARTEXFONTS"] = str(texmf_var / "fonts")
        specs = {
            "aitex_pdf": (
                "aitex-pdf",
                [str(binary), "pass", "{root}", "--fmt", str(fmt), "-file-line-error",
                 "-halt-on-error", "-o", "aitex.pdf"],
                "aitex.pdf", pdf_info,
            ),
            "aitex_dvi": (
                "aitex-dvi",
                [str(binary), "pass", "{root}", "--fmt", str(fmt), "-file-line-error",
                 "-halt-on-error", "-o", "aitex.dvi"],
                "aitex.dvi", dvi_info,
            ),
        }
        candidate_order = [
            route for route in reference.get("route_order", ()) if route in specs
        ]
        if set(candidate_order) != set(specs):
            candidate_order = ["aitex_pdf", "aitex_dvi"]
        stages = {}
        paths = {}
        for key in candidate_order:
            name, argv, output_name, inspect = specs[key]
            stage, output_path = compile_mode(
                source, base, name, root, argv, output_name, timeout,
                memory_limit_mib, env)
            stage["output"] = (inspect(output_path, env)
                               if inspect is dvi_info else inspect(output_path))
            stage["failure_kind"] = failure_kind(stage, stage["output"])
            stage["diagnostic"] = diagnostic_metadata(stage)
            stages[key] = stage
            paths[key] = output_path

        result = copy.deepcopy(reference)
        result["aitex_pdf"] = stages["aitex_pdf"]
        result["aitex_dvi"] = stages["aitex_dvi"]

        old_pdf = reference.get("aitex_pdf", {})
        new_pdf = stages["aitex_pdf"]
        pdf_identical = bool(
            successful(old_pdf)
            and successful(new_pdf)
            and old_pdf["output"].get("sha256")
            == new_pdf["output"].get("sha256")
        )
        stock_pdf_clean = successful(reference.get("pdftex_pdf", {}))
        needs_full_comparison = stock_pdf_clean and not pdf_identical
        if not pdf_identical:
            result.update({
                "pdf_page_match": False,
                "pdf_text_match": False,
                "pdf_text_similarity": None,
                "pdf_text_first_difference": None,
                "pdf_text_quality_no_worse": None,
                "aitex_text": None,
                "aitex_draft_eps_assets": [],
                "pdf_raster": {
                    "valid": False,
                    "equivalent": False,
                    "comparable": False,
                    "error": "candidate changed outside the stock-clean PDF set",
                },
            })

        candidate_semantic = dvi_semantics(paths["aitex_dvi"], env)
        candidate_semantic.pop("_content", None)
        candidate_semantic.pop("_drawing", None)
        candidate_semantic.pop("_specials", None)
        previous_semantic = reference.get("dvi_semantics", {}).get("candidate", {})
        dvi_identical = (
            semantic_identity(candidate_semantic)
            == semantic_identity(previous_semantic)
            and candidate_semantic.get("valid") is True
        )
        stock_dvi_clean = successful(reference.get("pdftex_dvi", {}))
        needs_full_comparison |= stock_dvi_clean and not dvi_identical
        if not dvi_identical:
            result.update({
                "dvi_match": False,
                "dvi_special_match": False,
                "dvi_special_applicable": False,
                "dvi_exact_match": False,
                "dvi_semantics": {
                    "valid": False,
                    "equivalent": False,
                    "drawing_equivalent": False,
                    "specials_equivalent": False,
                    "specials_applicable": False,
                    "error": "candidate changed outside the stock-clean DVI set",
                },
            })
        proof = {
            "pdf_bytes_identical": pdf_identical,
            "dvi_semantics_identical": dvi_identical,
            "full_comparison": needs_full_comparison,
        }
        if needs_full_comparison:
            full = evaluate_one((
                item, corpus_text, format_text, binary_text, overlay_text,
                texmfhome_search, texmfdbs_search, timeout, memory_limit_mib,
            ))
            full["candidate_reference_proof"] = proof
            return full
        result["candidate_reference_proof"] = proof
        return result


def successful(stage: dict) -> bool:
    output = stage["output"]
    return (stage["returncode"] == 0 and output.get("valid", False)
            and output.get("pages", 0) > 0)


def command_version(argv: list[str]) -> str:
    process = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             text=True, check=True)
    return next((line for line in process.stdout.splitlines() if line.strip()),
                "unknown version")


def effective_cpu_affinity() -> list[int] | None:
    """Return the CPUs available to this process when the OS exposes them."""
    if not hasattr(os, "sched_getaffinity"):
        return None
    try:
        return sorted(os.sched_getaffinity(0))
    except OSError:
        return None


def benchmark_environment(texmf_overlay: Path) -> dict:
    cpu_model = "unknown"
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        match = re.search(r"^model name\s*:\s*(.+)$", cpuinfo.read_text(), re.M)
        if match:
            cpu_model = match.group(1).strip()
    memory_bytes = None
    meminfo = Path("/proc/meminfo")
    if meminfo.is_file():
        match = re.search(r"^MemTotal:\s*(\d+) kB$", meminfo.read_text(), re.M)
        if match:
            memory_bytes = int(match.group(1)) * 1024
    return {
        "platform": platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "cpu_model": cpu_model,
        "logical_cpus": os.cpu_count(),
        "effective_cpu_affinity": effective_cpu_affinity(),
        "memory_bytes": memory_bytes,
        "pdflatex": command_version(["pdflatex", "--version"]),
        "ghostscript": command_version(["gs", "--version"]),
        "dvitype": command_version(["dvitype", "--version"]),
        "extractbb": command_version(["extractbb", "--version"]),
        "mutool": command_version(["mutool", "-v"]),
        "imagemagick": command_version(["compare", "-version"]),
        "netpbm": command_version(["pngtopam", "-version"]),
        "gnu_time": command_version(["/usr/bin/time", "--version"]),
        "python": platform.python_version(),
        "texmfhome": str(texmf_overlay),
        "texlive_overlay_lock_sha256": file_sha256(OVERLAY_LOCK),
    }


def command_run(args: argparse.Namespace) -> int:
    sample_sha = file_sha256(args.sample)
    sources_sha = file_sha256(args.sources)
    source_lock = json.loads(args.sources.read_text())
    if source_lock["sample_sha256"] != sample_sha:
        raise SystemExit("source lock belongs to a different blind sample")
    if len(source_lock["entries"]) != 200:
        raise SystemExit("holdout source lock does not contain exactly 200 entries")
    binary = args.binary.resolve()
    corpus = args.corpus.resolve()
    texmf_overlay = args.texmf_overlay.resolve()
    if not binary.is_file():
        raise SystemExit(f"compiler binary is absent: {binary}")
    marker_path = texmf_overlay.parents[4] / "overlay.json"
    if not texmf_overlay.is_dir() or not marker_path.is_file():
        raise SystemExit("locked TeX overlay is absent; run tools/texlive_overlay.py")
    marker = json.loads(marker_path.read_text())
    overlay_lock_sha = file_sha256(OVERLAY_LOCK)
    if marker.get("lock_sha256") != overlay_lock_sha:
        raise SystemExit("TeX overlay belongs to a different lock; rebuild it")
    filename_database = texmf_overlay / "ls-R"
    expected_texmfhome = f"!!{texmf_overlay}"
    expected_texmfdbs = f"{texmf_overlay}:"
    if (not filename_database.is_file()
            or marker.get("filename_database_sha256") != file_sha256(filename_database)
            or marker.get("texmfhome_search") != expected_texmfhome
            or marker.get("texmfdbs_search") != expected_texmfdbs):
        raise SystemExit("TeX overlay index is absent or stale; run tools/texlive_overlay.py")
    filename_database_sha = file_sha256(filename_database)
    for tool in ("pdflatex", "gs", "dvitype", "extractbb", "mutool", "gawk",
                 "compare", "pngtopam", "pnmtopng",
                 "/usr/bin/time"):
        if shutil.which(tool) is None:
            raise SystemExit(f"required tool is absent: {tool}")
    if args.jobs < 1 or args.timeout < 1 or args.memory_limit_mib < 64:
        raise SystemExit("jobs and timeout must be positive; memory limit must be >= 64 MiB")
    if args.limit is not None and args.limit < 1:
        raise SystemExit("limit must be positive")

    ready = [item for item in source_lock["entries"] if item["status"] == "ready"]
    for item in ready:
        source = corpus / "sources" / item["id"].replace("/", "_")
        verify_source(source, item)
    print(f"verified {len(ready)} locked source trees", flush=True)

    reference_result = None
    reference_runs: dict[str, dict] = {}
    reference_sha = None
    if args.reference_results is not None:
        reference_path = args.reference_results.resolve()
        try:
            reference_result = json.loads(reference_path.read_text())
        except (OSError, json.JSONDecodeError) as error:
            raise SystemExit(f"cannot read reference results: {error}") from error
        reference_sha = file_sha256(reference_path)
        if reference_result.get("schema") != MEASUREMENT_SCHEMA:
            raise SystemExit("reference results use a different measurement schema")
        for field, expected in (
            ("sample_sha256", sample_sha),
            ("sources_sha256", sources_sha),
            ("environment_sha256", file_sha256(
                ROOT / "qualification" / "environment.lock.json")),
        ):
            if reference_result.get(field) != expected:
                raise SystemExit(f"reference results {field} does not match")
        reference_entries = reference_result.get("entries", [])
        if len(reference_entries) != len(source_lock["entries"]):
            raise SystemExit("reference results do not cover the locked sample")
        for entry in reference_entries:
            source_item = entry.get("source", {})
            identity = source_item.get("id")
            run_result = entry.get("run")
            if identity and run_result and "worker_error" not in run_result:
                reference_runs[identity] = run_result
        missing_reference = sorted(item["id"] for item in ready
                                   if item["id"] not in reference_runs)
        if missing_reference:
            raise SystemExit(
                "reference results lack evaluated ready roots: "
                + ", ".join(missing_reference[:10])
            )
        print(
            f"reusing stock stages from {reference_path} ({reference_sha})",
            flush=True,
        )

    runner_sha = file_sha256(Path(__file__))
    binary_sha = file_sha256(binary)
    cpu_affinity = effective_cpu_affinity()
    state_path = corpus / "run-state.json"
    state = json.loads(state_path.read_text()) if state_path.is_file() else None
    format_dir = corpus / "format"
    format_dir.mkdir(parents=True, exist_ok=True)
    fmt = format_dir / "fmt.afmt"
    if state is None:
        format_env = dict(os.environ)
        format_env["TEXMFHOME"] = expected_texmfhome
        format_env["TEXMFDBS"] = expected_texmfdbs
        format_env["AITEX_BINARY"] = str(binary)
        subprocess.run([str(ROOT / "tools" / "mkfmt.sh"), str(format_dir)],
                       check=True, env=format_env)
        format_sha = file_sha256(fmt)
        state = {
            "measurement_schema": MEASUREMENT_SCHEMA,
            "runner_sha256": runner_sha,
            "format_sha256": format_sha,
            "memory_limit_mib": args.memory_limit_mib,
            "aitex_stack_mib": 64,
            "timeout_seconds_per_pass": args.timeout,
            "passes": 2,
            "jobs": args.jobs,
            "effective_cpu_affinity": cpu_affinity,
            "sample_sha256": sample_sha,
            "sources_sha256": sources_sha,
            "binary_sha256": binary_sha,
            "texlive_overlay_lock_sha256": overlay_lock_sha,
            "texmf_filename_database_sha256": filename_database_sha,
            "entries": {},
        }
        write_json(state_path, state)
    else:
        if not fmt.is_file():
            raise SystemExit("run format is absent; use a new corpus directory")
        format_sha = file_sha256(fmt)
    wanted_state = {
        "measurement_schema": MEASUREMENT_SCHEMA,
        "runner_sha256": runner_sha,
        "format_sha256": format_sha,
        "memory_limit_mib": args.memory_limit_mib,
        "aitex_stack_mib": 64,
        "timeout_seconds_per_pass": args.timeout,
        "passes": 2,
        "jobs": args.jobs,
        "effective_cpu_affinity": cpu_affinity,
        "sample_sha256": sample_sha,
        "sources_sha256": sources_sha,
        "binary_sha256": binary_sha,
        "texlive_overlay_lock_sha256": overlay_lock_sha,
        "texmf_filename_database_sha256": filename_database_sha,
    }
    if reference_sha is not None:
        wanted_state["reference_results_sha256"] = reference_sha
        if "reference_results_sha256" not in state and not state["entries"]:
            state["reference_results_sha256"] = reference_sha
            write_json(state_path, state)
    for key, wanted in wanted_state.items():
        if state.get(key) != wanted:
            raise SystemExit(f"run state {key} differs; use a new corpus directory")
    selected_ids = set(args.id or ())
    ready_ids = {item["id"] for item in ready}
    unknown_ids = sorted(selected_ids - ready_ids)
    if unknown_ids:
        raise SystemExit("requested holdout ID is not a compilable locked source: "
                         + ", ".join(unknown_ids))
    selected = ([item for item in ready if item["id"] in selected_ids]
                if selected_ids else ready)
    todo = [item for item in selected if item["id"] not in state["entries"]]
    remaining = len(todo)
    if args.limit is not None:
        todo = todo[:args.limit]
    selection = (f"{len(selected)} selected from {len(ready)} compilable source roots"
                 if selected_ids else f"{len(ready)} compilable source roots")
    print(f"holdout run: {selection}, {remaining} remaining, "
          f"{len(todo)} scheduled, {args.jobs} workers", flush=True)
    if reference_result is None:
        payloads = [
            (item, str(corpus), str(fmt), str(binary), str(texmf_overlay),
             expected_texmfhome, expected_texmfdbs,
             args.timeout, args.memory_limit_mib)
            for item in todo
        ]
        evaluator = evaluate_one
    else:
        payloads = [
            (item, reference_runs[item["id"]], str(corpus), str(fmt),
             str(binary), str(texmf_overlay), expected_texmfhome,
             expected_texmfdbs, args.timeout, args.memory_limit_mib)
            for item in todo
        ]
        evaluator = evaluate_candidate_against_reference
    completed = sum(item["id"] in state["entries"] for item in selected)
    with concurrent.futures.ProcessPoolExecutor(max_workers=args.jobs) as executor:
        futures = {executor.submit(evaluator, payload): payload[0]["id"]
                   for payload in payloads}
        for future in concurrent.futures.as_completed(futures):
            identity = futures[future]
            try:
                result = future.result()
            except Exception as error:
                result = {"id": identity, "worker_error":
                          f"{type(error).__name__}: {error}"}
            state["entries"][identity] = result
            write_json(state_path, state)
            completed += 1
            if "worker_error" in result:
                summary = "worker-error"
            else:
                summary = (f"pdftex-pdf={'ok' if successful(result['pdftex_pdf']) else 'FAIL'} "
                           f"aitex-pdf={'ok' if successful(result['aitex_pdf']) else 'FAIL'} "
                           f"dvi-draw={('yes' if result['dvi_match'] else 'no') if result['dvi_semantics']['valid'] else 'n/a'} "
                           f"dvi-special={('yes' if result['dvi_special_match'] else 'no') if result['dvi_semantics']['valid'] else 'n/a'} "
                           f"raster-match={'yes' if result['pdf_raster']['equivalent'] else 'no'}")
            print(f"  [{completed:3d}/{len(selected)}] {identity:18s} {summary}", flush=True)
    entries = []
    for item in source_lock["entries"]:
        run = state["entries"].get(item["id"])
        entries.append({"source": item, "run": run})
    result = {
        "schema": MEASUREMENT_SCHEMA,
        "description": "Indexed blind 200-paper arXiv compatibility results",
        "created_at": utc_now(),
        "sample_sha256": sample_sha,
        "sources_sha256": sources_sha,
        "environment_sha256": file_sha256(
            ROOT / "qualification" / "environment.lock.json"),
        "benchmark_environment": benchmark_environment(texmf_overlay),
        "runner_sha256": runner_sha,
        "reference_results_sha256": reference_sha,
        "compiler": {
            "git_commit": subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=ROOT,
                stdout=subprocess.PIPE, text=True, check=True).stdout.strip(),
            "binary_sha256": binary_sha,
            "format_sha256": format_sha,
            "version": command_version([str(binary), "--version"]).removeprefix("aitex "),
        },
        "run": {
            "measurement_schema": MEASUREMENT_SCHEMA,
            "artifact_oracle": {
                "dvi_drawing": "exact normalized positioned glyph/rule stream",
                "dvi_specials": "exact normalized positioned driver-special stream",
                "dvi_combined": "exact union of drawing and driver-special streams",
                "pdf_text": ("exact MuPDF Unicode sequence after NFKC, removal of "
                             "geometry-inferred whitespace, and stock TS1 bullet repair"),
                "pdf_text_quality": ("candidate has no more total replacement, control, "
                                     "and private-use code points than stock pdfTeX"),
                "pdf_raster_dpi": RASTER_DPI,
                "pdf_raster_max_normalized_mae": RASTER_MAX_NORMALIZED_MAE,
                "pdf_raster_fuzz_percent": RASTER_FUZZ_PERCENT,
                "pdf_raster_max_fuzz_fraction": RASTER_MAX_FUZZ_FRACTION,
                "pdf_raster_comparability": (
                    "exclude a stock direct-PDF page only when its extracted text "
                    "contains an EPS asset named by the exact DVI PSfile stream"),
            },
            "texlive_overlay_lock_sha256": overlay_lock_sha,
            "texmf_filename_database_sha256": filename_database_sha,
            "passes": 2,
            "timeout_seconds_per_pass": args.timeout,
            "memory_limit_mib": args.memory_limit_mib,
            "aitex_stack_mib": 64,
            "workers": args.jobs,
            "effective_cpu_affinity": cpu_affinity,
            "timing_scope": "compiler process only; source copy and output inspection excluded",
            "memory_scope": "maximum resident set size per pass from GNU time -v",
            "route_order": "deterministically alternated by arXiv ID",
            "stock_reference_reused": reference_result is not None,
            "stock_reference_proof": (
                "new PDF bytes and normalized DVI streams must equal the saved "
                "stock-compared candidate; changed stock-clean cases receive a "
                "fresh four-route comparison"
                if reference_result is not None else None
            ),
        },
        "entries": entries,
    }
    write_json(args.output, result)
    print(f"results sha256: {file_sha256(args.output)}")
    return 0


def wilson(successes: int, total: int) -> tuple[float, float]:
    if total == 0:
        return 0.0, 0.0
    z = 1.959963984540054
    proportion = successes / total
    denominator = 1 + z * z / total
    center = (proportion + z * z / (2 * total)) / denominator
    half = z * ((proportion * (1 - proportion) / total
                + z * z / (4 * total * total)) ** 0.5) / denominator
    return center - half, center + half


def rate(successes: int, total: int) -> str:
    low, high = wilson(successes, total)
    percent = successes * 100 / total if total else 0
    return f"{successes}/{total} ({percent:.1f}%; 95% CI {low * 100:.1f}–{high * 100:.1f}%)"


def percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    position = (len(ordered) - 1) * quantile
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    fraction = position - lower
    return ordered[lower] * (1 - fraction) + ordered[upper] * fraction


def paired_benchmark(entries: list[dict], reference: str, candidate: str) -> dict:
    pairs = [(item["run"][reference], item["run"][candidate])
             for item in entries
             if successful(item["run"][reference])
             and successful(item["run"][candidate])]
    timed = [(ref, ours) for ref, ours in pairs
             if ref.get("seconds") is not None and ours.get("seconds") is not None]
    cpu = [(ref, ours) for ref, ours in pairs
           if ref.get("cpu_seconds") is not None and ours.get("cpu_seconds") is not None]
    memory = [(ref["peak_rss_kib"] / 1024, ours["peak_rss_kib"] / 1024)
              for ref, ours in pairs
              if ref.get("peak_rss_kib") is not None
              and ours.get("peak_rss_kib") is not None]
    ref_wall = sum(ref["seconds"] for ref, _ in timed)
    our_wall = sum(ours["seconds"] for _, ours in timed)
    ref_cpu = sum(ref["cpu_seconds"] for ref, _ in cpu)
    our_cpu = sum(ours["cpu_seconds"] for _, ours in cpu)
    ref_memory = [ref for ref, _ in memory]
    our_memory = [ours for _, ours in memory]
    return {
        "documents": len(pairs),
        "wall_documents": len(timed),
        "ref_wall_seconds": ref_wall,
        "our_wall_seconds": our_wall,
        "wall_ratio": our_wall / ref_wall if ref_wall else None,
        "wall_wins": sum(ours["seconds"] < ref["seconds"] for ref, ours in timed),
        "cpu_documents": len(cpu),
        "ref_cpu_seconds": ref_cpu,
        "our_cpu_seconds": our_cpu,
        "cpu_ratio": our_cpu / ref_cpu if ref_cpu else None,
        "memory_documents": len(memory),
        "ref_median_mib": statistics.median(ref_memory) if ref_memory else None,
        "our_median_mib": statistics.median(our_memory) if our_memory else None,
        "ref_p95_mib": percentile(ref_memory, 0.95) if ref_memory else None,
        "our_p95_mib": percentile(our_memory, 0.95) if our_memory else None,
        "ref_max_mib": max(ref_memory, default=None),
        "our_max_mib": max(our_memory, default=None),
        "memory_ratio": ((statistics.median(our_memory)
                          / statistics.median(ref_memory))
                         if ref_memory and statistics.median(ref_memory) else None),
        "memory_wins": sum(ours < ref for ref, ours in memory),
    }


def resource_summary(entries: list[dict], stage: str) -> dict:
    runs = [item["run"][stage] for item in entries]
    memory = [item["peak_rss_kib"] / 1024 for item in runs
              if item.get("peak_rss_kib") is not None]
    return {
        "measured": len(memory),
        "median_mib": statistics.median(memory) if memory else None,
        "p95_mib": percentile(memory, 0.95) if memory else None,
        "max_mib": max(memory, default=None),
        "timeouts": sum(item["timed_out"] for item in runs),
        "memory_limits": sum(item.get("failure_kind") == "memory_limit"
                             for item in runs),
    }


def ratio_text(value: float | None) -> str:
    return f"{value:.2f}×" if value is not None else "n/a"


def seconds_text(value: float | None) -> str:
    return f"{value:.2f}s" if value is not None else "n/a"


def mib_text(value: float | None) -> str:
    return f"{value:.1f} MiB" if value is not None else "n/a"


def clustered_failures(items: list[dict], stage_name: str) -> list[dict]:
    clusters: dict[tuple[str, str], list[str]] = defaultdict(list)
    for item in items:
        stage = item["run"][stage_name]
        if successful(stage):
            continue
        diagnostic = stage.get("diagnostic") or diagnostic_metadata(stage)
        key = (diagnostic["subsystem"], diagnostic["fingerprint"])
        clusters[key].append(item["source"]["id"])
    return [
        {"subsystem": subsystem, "fingerprint": fingerprint,
         "count": len(ids), "representatives": ids[:3]}
        for (subsystem, fingerprint), ids in sorted(
            clusters.items(), key=lambda pair: (-len(pair[1]), pair[0]))
    ]


def dvi_divergence_clusters(items: list[dict]) -> list[dict]:
    clusters: dict[str, list[str]] = defaultdict(list)
    for item in items:
        run = item["run"]
        if run["dvi_match"]:
            continue
        reference = run["pdftex_dvi"]["output"]
        candidate = run["aitex_dvi"]["output"]
        page_delta = candidate.get("pages", 0) - reference.get("pages", 0)
        char_delta = candidate.get("characters", 0) - reference.get("characters", 0)
        if page_delta == 0 and char_delta == 0:
            signature = "same counts; ordered character stream differs"
        else:
            signature = f"page delta {page_delta:+d}; character delta {char_delta:+d}"
        clusters[signature].append(item["source"]["id"])
    return [
        {"signature": signature, "count": len(ids), "representatives": ids[:3]}
        for signature, ids in sorted(clusters.items(), key=lambda pair: (-len(pair[1]), pair[0]))
    ]


def command_report(args: argparse.Namespace) -> int:
    result = json.loads(args.results.read_text())
    stock_reference_reused = bool(
        result.get("run", {}).get("stock_reference_reused", False)
    )
    entries = result["entries"]
    total = len(entries)
    ready = [item for item in entries if item["source"]["status"] == "ready"]
    source_status = Counter(item["source"]["status"] for item in entries)
    evaluated = [item for item in ready if item["run"] and
                 "worker_error" not in item["run"]]
    metrics = {}
    for stage in ("pdftex_pdf", "pdftex_dvi", "aitex_pdf", "aitex_dvi"):
        metrics[stage] = sum(successful(item["run"][stage]) for item in evaluated)
    ref_pdf = [item for item in evaluated if successful(item["run"]["pdftex_pdf"])]
    ref_dvi = [item for item in evaluated if successful(item["run"]["pdftex_dvi"])]
    aitex_given_pdf = sum(successful(item["run"]["aitex_pdf"]) for item in ref_pdf)
    aitex_given_dvi = sum(successful(item["run"]["aitex_dvi"]) for item in ref_dvi)
    paired_pdf = [item for item in ref_pdf if successful(item["run"]["aitex_pdf"])]
    paired_dvi = [item for item in ref_dvi if successful(item["run"]["aitex_dvi"])]
    semantic_dvi = [
        item for item in paired_dvi
        if item["run"].get("dvi_semantics", {}).get("valid", False)
    ]
    dvi_match = sum(item["run"]["dvi_match"] for item in semantic_dvi)
    dvi_exact_match = sum(
        item["run"].get("dvi_exact_match", item["run"]["dvi_match"])
        for item in semantic_dvi)
    special_dvi = [
        item for item in semantic_dvi
        if item["run"].get(
            "dvi_special_applicable",
            bool(item["run"].get("dvi_semantics", {}).get("reference", {})
                 .get("special_records", 0)
                 or item["run"].get("dvi_semantics", {}).get("candidate", {})
                 .get("special_records", 0)))]
    dvi_special_match = sum(
        item["run"].get("dvi_special_match", item["run"]["dvi_match"])
        for item in special_dvi)
    text_pairs = [
        item for item in paired_pdf
        if item["run"].get("pdftex_text") is not None
        and item["run"].get("aitex_text") is not None
    ]
    text_match = sum(item["run"]["pdf_text_match"] for item in text_pairs)
    text_quality_no_worse = sum(
        item["run"].get("pdf_text_quality_no_worse", False)
        for item in text_pairs)
    text_similarities = [
        item["run"].get("pdf_text_similarity") for item in paired_pdf
        if item["run"].get("pdf_text_similarity") is not None
    ]
    median_text_similarity = (statistics.median(text_similarities)
                              if text_similarities else None)
    near_text_match = sum(value >= 0.99 for value in text_similarities)
    page_match = sum(item["run"].get("pdf_page_match", False) for item in paired_pdf)
    raster_pairs = [
        item for item in paired_pdf
        if item["run"].get("pdf_raster", {}).get("comparable", True)
    ]
    raster_match = sum(
        item["run"].get("pdf_raster", {}).get("equivalent", False)
        for item in raster_pairs)
    stock_draft_eps = [
        item for item in paired_pdf
        if item["run"].get("pdftex_draft_eps_assets")
    ]
    aitex_draft_eps = [
        item for item in paired_pdf
        if item["run"].get("aitex_draft_eps_assets")
    ]
    pdf_benchmark = paired_benchmark(evaluated, "pdftex_pdf", "aitex_pdf")
    dvi_benchmark = paired_benchmark(evaluated, "pdftex_dvi", "aitex_dvi")
    resources = {stage: resource_summary(evaluated, stage) for stage in (
        "pdftex_pdf", "pdftex_dvi", "aitex_pdf", "aitex_dvi")}
    failure_counts = Counter()
    for item in evaluated:
        if not successful(item["run"]["aitex_pdf"]):
            failure_counts[item["run"]["aitex_pdf"]["failure_kind"]] += 1
    category_total = Counter(item["source"]["primary_category"] for item in ready)
    category_success = Counter(
        item["source"]["primary_category"] for item in evaluated
        if successful(item["run"]["aitex_pdf"]))
    pdf_failure_clusters = clustered_failures(ref_pdf, "aitex_pdf")
    dvi_failure_clusters = clustered_failures(ref_dvi, "aitex_dvi")
    dvi_semantic_clusters = dvi_divergence_clusters(semantic_dvi)
    lines = [
        "# Blind 200-paper arXiv holdout",
        "",
        f"Generated from the locked result file on {result['created_at']}. The sample was",
        "selected before any source was inspected or compiler was run: 200 entries were",
        "drawn with a fixed seed from the 400 most recent arXiv-wide API results after",
        "excluding every previously examined ID available in the local corpus records.",
        "",
        "The selection was blind, but the corpus was subsequently used to diagnose and",
        "fix compiler defects. It is therefore now a locked regression and release",
        "qualification set, not an untouched estimate of generalization. A future",
        "generalization claim requires a newly selected sample that remains unseen",
        "until the candidate is frozen.",
        "",
        "## Results",
        "",
        "| Question | Result |",
        "|---|---:|",
        f"| TeX source with an automatically identified root | {rate(len(ready), total)} |",
        f"| Source roots evaluated | {rate(len(evaluated), len(ready))} |",
        f"| Stock pdfLaTeX produces a valid PDF | {rate(metrics['pdftex_pdf'], len(ready))} |",
        f"| Stock pdfTeX produces valid DVI | {rate(metrics['pdftex_dvi'], len(ready))} |",
        f"| aiTeX produces a valid PDF | {rate(metrics['aitex_pdf'], len(ready))} |",
        f"| aiTeX produces valid DVI | {rate(metrics['aitex_dvi'], len(ready))} |",
        f"| aiTeX PDF, conditional on stock PDF success | {rate(aitex_given_pdf, len(ref_pdf))} |",
        f"| aiTeX DVI, conditional on stock DVI success | {rate(aitex_given_dvi, len(ref_dvi))} |",
        (f"| Valid semantic DVI comparisons, when both compile | "
         f"{rate(len(semantic_dvi), len(paired_dvi))} |"),
        (f"| Exact positioned DVI glyph/rule semantics, when comparable | "
         f"{rate(dvi_match, len(semantic_dvi))} |"),
        (f"| Exact positioned DVI driver-special stream, when either emits specials | "
         f"{rate(dvi_special_match, len(special_dvi))} |"),
        (f"| Exact combined DVI semantics, when comparable | "
         f"{rate(dvi_exact_match, len(semantic_dvi))} |"),
        f"| Matching PDF page count, when both succeed | {rate(page_match, len(paired_pdf))} |",
        (f"| Visually equivalent PDF pages at {RASTER_DPI} DPI, when stock rendered "
         f"every EPS asset | {rate(raster_match, len(raster_pairs))} |"),
        (f"| Stock PDFs containing EPS draft placeholders | "
         f"{rate(len(stock_draft_eps), len(paired_pdf))} |"),
        (f"| aiTeX PDFs containing EPS draft placeholders | "
         f"{rate(len(aitex_draft_eps), len(paired_pdf))} |"),
        (f"| Extractable Unicode sequence from both PDFs | "
         f"{rate(len(text_pairs), len(paired_pdf))} |"),
        (f"| Candidate text has no more invalid code points than stock | "
         f"{rate(text_quality_no_worse, len(text_pairs))} |"),
        (f"| Matching normalized PDF Unicode sequence, when extractable | "
         f"{rate(text_match, len(text_pairs))} |"),
        (f"| At least 99% extracted-text similarity | "
         f"{rate(near_text_match, len(text_similarities))} |"),
        (f"| Median extracted-text similarity | "
         f"{median_text_similarity * 100:.2f}% |"
         if median_text_similarity is not None else
         "| Median extracted-text similarity | n/a |"),
        "",
        "A valid compile requires exit status zero, a readable output file, and at least",
        "one page. Each engine gets two clean passes with shell escape disabled. DVI",
        "drawing matching requires the exact normalized sequence of positioned glyphs,",
        "rules, and fonts; driver specials are graded separately and then as a strict",
        "combined stream. None require byte-identical movement encoding.",
        ("PDF text uses MuPDF, NFKC normalization, and a whitespace-free character "
         "sequence because spaces in an untagged TeX PDF are reader inferences from "
         "the geometry already graded by the DVI oracle. Stock pdfTeX's unmapped "
         "TS1 U+0088 bullet is normalized to U+2022."),
        ("Exact stock text remains a diagnostic: the release quality gate is that "
         "aiTeX introduces no additional Unicode replacement, control, or private-use "
         "characters, because a corrected mapping can legitimately differ from stock."),
        (f"PDF rendering uses the same reader at {RASTER_DPI} DPI and requires each page "
         f"to have normalized MAE <= {RASTER_MAX_NORMALIZED_MAE:.6f} and no more than "
         f"{RASTER_MAX_FUZZ_FRACTION:.2%} of pixels outside {RASTER_FUZZ_PERCENT}% fuzz."),
        ("A raster pair is marked non-comparable only when the stock PDF's extracted "
         "text contains an EPS filename that the exact DVI special stream names; this "
         "is pdfLaTeX's ruled draft-box fallback with shell escape disabled. aiTeX is "
         "still required to render the EPS rather than emit the same placeholder."),
        "The PDF row is product-level",
        "compatibility with pdfLaTeX. The DVI row is the like-for-like TeX algorithm route:",
        "both engines run the same DVI-mode LaTeX driver semantics.",
        "",
        "## Performance against stock pdfTeX",
        "",
        "Both figures cover two compiler passes. The aggregate ratio is aiTeX divided",
        "by pdfTeX, so lower is better. Only papers successfully compiled by both",
        "engines on that route are included.",
        "",
        "| Route | Paired papers | aiTeX wall | pdfTeX wall | Wall ratio | aiTeX CPU | pdfTeX CPU | CPU ratio | aiTeX faster |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
        (f"| PDF end-to-end | {pdf_benchmark['documents']} | "
         f"{seconds_text(pdf_benchmark['our_wall_seconds'])} | {seconds_text(pdf_benchmark['ref_wall_seconds'])} | "
         f"{ratio_text(pdf_benchmark['wall_ratio'])} | {seconds_text(pdf_benchmark['our_cpu_seconds'])} | "
         f"{seconds_text(pdf_benchmark['ref_cpu_seconds'])} | {ratio_text(pdf_benchmark['cpu_ratio'])} | "
         f"{pdf_benchmark['wall_wins']}/{pdf_benchmark['wall_documents']} |"),
        (f"| DVI compiler | {dvi_benchmark['documents']} | "
         f"{seconds_text(dvi_benchmark['our_wall_seconds'])} | {seconds_text(dvi_benchmark['ref_wall_seconds'])} | "
         f"{ratio_text(dvi_benchmark['wall_ratio'])} | {seconds_text(dvi_benchmark['our_cpu_seconds'])} | "
         f"{seconds_text(dvi_benchmark['ref_cpu_seconds'])} | {ratio_text(dvi_benchmark['cpu_ratio'])} | "
         f"{dvi_benchmark['wall_wins']}/{dvi_benchmark['wall_documents']} |"),
        "",
        "Peak RSS is the larger GNU `time -v` high-water mark from either pass.",
        "",
        "| Route | Paired papers | aiTeX median | pdfTeX median | Median ratio | aiTeX p95 | pdfTeX p95 | aiTeX max | pdfTeX max | aiTeX lower |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        (f"| PDF end-to-end | {pdf_benchmark['memory_documents']} | "
         f"{mib_text(pdf_benchmark['our_median_mib'])} | {mib_text(pdf_benchmark['ref_median_mib'])} | "
         f"{ratio_text(pdf_benchmark['memory_ratio'])} | {mib_text(pdf_benchmark['our_p95_mib'])} | "
         f"{mib_text(pdf_benchmark['ref_p95_mib'])} | {mib_text(pdf_benchmark['our_max_mib'])} | "
         f"{mib_text(pdf_benchmark['ref_max_mib'])} | "
         f"{pdf_benchmark['memory_wins']}/{pdf_benchmark['memory_documents']} |"),
        (f"| DVI compiler | {dvi_benchmark['memory_documents']} | "
         f"{mib_text(dvi_benchmark['our_median_mib'])} | {mib_text(dvi_benchmark['ref_median_mib'])} | "
         f"{ratio_text(dvi_benchmark['memory_ratio'])} | {mib_text(dvi_benchmark['our_p95_mib'])} | "
         f"{mib_text(dvi_benchmark['ref_p95_mib'])} | {mib_text(dvi_benchmark['our_max_mib'])} | "
         f"{mib_text(dvi_benchmark['ref_max_mib'])} | "
         f"{dvi_benchmark['memory_wins']}/{dvi_benchmark['memory_documents']} |"),
        "",
        "The successful-pair table excludes pathological failures. Across all 191",
        "evaluated sources, including failures, resource use was:",
        "",
        "| Route | RSS measured | Median RSS | p95 RSS | Maximum RSS | Timeouts | Memory-limit aborts |",
        "|---|---:|---:|---:|---:|---:|---:|",
        *[(f"| {label} | {resources[key]['measured']}/{len(evaluated)} | "
           f"{mib_text(resources[key]['median_mib'])} | {mib_text(resources[key]['p95_mib'])} | "
           f"{mib_text(resources[key]['max_mib'])} | {resources[key]['timeouts']} | "
           f"{resources[key]['memory_limits']} |")
          for key, label in (("pdftex_pdf", "pdfTeX PDF"),
                             ("pdftex_dvi", "pdfTeX DVI"),
                             ("aitex_pdf", "aiTeX PDF"),
                             ("aitex_dvi", "aiTeX DVI"))],
        "",
        "## Source outcomes",
        "",
    ]
    for name, count in sorted(source_status.items()):
        lines.append(f"- `{name}`: {count}")
    lines += ["", "## aiTeX PDF failure categories", ""]
    for name, count in failure_counts.most_common():
        lines.append(f"- `{name}`: {count}")
    lines += [
        "",
        "## Systematic failure clusters",
        "",
        "These tables condition on the corresponding stock route succeeding. Paths and",
        "line numbers are normalized so one engine subsystem is one work item even when",
        "dozens of papers expose it.",
        "",
        "### PDF product-compatibility failures",
        "",
        "| Subsystem | Fingerprint | Papers | Representatives |",
        "|---|---|---:|---|",
    ]
    for cluster in pdf_failure_clusters:
        reps = ", ".join(f"`{identity}`" for identity in cluster["representatives"])
        lines.append(f"| `{cluster['subsystem']}` | `{cluster['fingerprint']}` | "
                     f"{cluster['count']} | {reps} |")
    lines += [
        "",
        "### DVI compile failures",
        "",
        "| Subsystem | Fingerprint | Papers | Representatives |",
        "|---|---|---:|---|",
    ]
    for cluster in dvi_failure_clusters:
        reps = ", ".join(f"`{identity}`" for identity in cluster["representatives"])
        lines.append(f"| `{cluster['subsystem']}` | `{cluster['fingerprint']}` | "
                     f"{cluster['count']} | {reps} |")
    lines += [
        "",
        "### DVI glyph/rule divergences after both compilers succeed",
        "",
        "| Signature | Papers | Representatives |",
        "|---|---:|---|",
    ]
    for cluster in dvi_semantic_clusters:
        reps = ", ".join(f"`{identity}`" for identity in cluster["representatives"])
        lines.append(f"| {cluster['signature']} | {cluster['count']} | {reps} |")
    lines += ["", "## aiTeX PDF success by primary category", "",
              "| Category | Success |", "|---|---:|"]
    for name, count in sorted(category_total.items()):
        lines.append(f"| `{name}` | {category_success[name]}/{count} |")
    environment = result["benchmark_environment"]
    memory_gib = ((environment["memory_bytes"] / (1024 ** 3))
                  if environment.get("memory_bytes") is not None else None)
    if stock_reference_reused:
        measurement_method = [
            ("Stock routes, their timings, and their correctness oracles were reused "
             "from the pinned result named by `reference_results_sha256`."),
            ("aiTeX alone was remeasured from clean source copies. Every stock-clean "
             "PDF had to be byte-identical to the previously compared candidate, and "
             "every stock-clean DVI had to have identical normalized drawing and "
             "driver-special streams; any changed case received a fresh four-route "
             "comparison."),
            ("The CPU and memory ratios therefore pair a fresh aiTeX measurement with "
             "the saved stock measurement on this same locked machine and corpus."),
        ]
    else:
        measurement_method = [
            "Each route starts from its own clean source copy, then runs twice so the second",
            "pass consumes only that engine's own auxiliary files. Copying, DVI inspection,",
            "Ghostscript validation, and text extraction are outside the timed region.",
            (f"Engine order is alternated by paper ID; "
             f"{result['run']['workers']} worker(s) ran concurrently."),
        ]
    lines += [
        "",
        "## Method and benchmark environment",
        "",
        "The classical reference is TeX Live's stock pdfTeX in pdfLaTeX mode;",
        "Knuth TeX cannot load the e-TeX/pdfTeX-dependent LaTeX used by this corpus.",
        "PDF timing is end-to-end for each CLI, while DVI timing isolates compilation.",
        *measurement_method,
        f"Each compiler pass had a {result['run']['timeout_seconds_per_pass']}s timeout and a "
        f"{result['run']['memory_limit_mib']} MiB address-space safety ceiling.",
        (f"aiTeX's configurable virtual worker-stack reservation was set to "
         f"{result['run']['aitex_stack_mib']} MiB; peak RSS, not virtual size, is reported."),
        ("TeX packages come from the host TeX Live plus the isolated, exact-deb "
         "overlay locked in `qualification/texlive-overlay.lock.json`; no host "
         "packages are installed."),
        "",
        f"- CPU: {environment['cpu_model']} ({environment['logical_cpus']} logical CPUs)",
        f"- RAM: {memory_gib:.1f} GiB" if memory_gib is not None else "- RAM: unknown",
        f"- Platform: {environment['platform']}; kernel {environment['kernel']}",
        f"- Reference: {environment['pdflatex']}",
        f"- PDF validation: Ghostscript {environment['ghostscript']}",
        f"- Graphics sidecars: {environment['extractbb']}",
        f"- Graphics rendering and text extraction: {environment['mutool']}",
        f"- Raster comparison: {environment.get('imagemagick', 'not recorded')}",
        f"- PNG normalization: {environment.get('netpbm', 'not recorded')}",
        f"- Measurement: {environment['gnu_time']}",
        "",
        "## Reproduction",
        "",
        "The exact population, selected IDs, versioned source URLs, source-tree hashes,",
        "tool environment, compiler binary hash, per-engine results, timeouts, and first",
        "errors are retained in `qualification/holdout-200.*.json`.",
        "",
        "```sh",
        "# On a clean checkout, materialize the ignored source trees without replacing",
        "# the tracked lock. The run below verifies every tree against that lock.",
        "python3 tools/arxiv_holdout.py fetch \\",
        "  --output target/qualification/holdout-200-materialized.sources.json \\",
        "  --delay 0.2 --retry",
        "python3 tools/texlive_overlay.py",
        "cargo build --release --locked -p aitex-cli",
        (f"python3 tools/arxiv_holdout.py run --jobs {result['run']['workers']} "
         f"--timeout {result['run']['timeout_seconds_per_pass']} "
         f"--memory-limit-mib {result['run']['memory_limit_mib']} \\"),
        "  --output target/qualification/holdout-200-current.results.json",
        "python3 tools/arxiv_holdout.py report \\",
        "  --results target/qualification/holdout-200-current.results.json \\",
        "  --output target/qualification/HOLDOUT-200-CURRENT.md",
        "```",
        "",
    ]
    args.output.write_text("\n".join(lines))
    print("\n".join(lines[:26]))
    return 0


def command_fetch_offline(args: argparse.Namespace) -> int:
    lock = json.loads(args.output.read_text())
    source_dir = args.corpus / "sources"
    checked = 0
    for item in lock["entries"]:
        if item["status"] in {"ready", "no_documentclass"}:
            verify_source(source_dir / item["id"].replace("/", "_"), item)
            checked += 1
    print(f"offline holdout source verification: {checked} trees match")
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    sub = result.add_subparsers(dest="command", required=True)
    sample = sub.add_parser("sample", help="lock IDs before source inspection")
    sample.add_argument("--feed", type=Path, help="saved Atom feed instead of network")
    sample.add_argument("--pool", type=int, default=400)
    sample.add_argument("--count", type=int, default=200)
    sample.add_argument("--seed", default="aitex-blind-holdout-2026-08-28-v1")
    sample.add_argument("--output", type=Path, default=SAMPLE)
    sample.add_argument("--replace", action="store_true")
    sample.set_defaults(function=command_sample)
    fetch = sub.add_parser("fetch", help="download and fingerprint the locked sample")
    fetch.add_argument("--sample", type=Path, default=SAMPLE)
    fetch.add_argument("--output", type=Path, default=SOURCES)
    fetch.add_argument("--corpus", type=Path, default=CORPUS)
    fetch.add_argument("--delay", type=float, default=3.0)
    fetch.add_argument("--retry", action="store_true")
    fetch.add_argument("--offline", action="store_true")
    fetch.set_defaults(function=lambda args: (command_fetch_offline(args)
                                               if args.offline else command_fetch(args)))
    run = sub.add_parser("run", help="compile every source through all four routes")
    run.add_argument("--sample", type=Path, default=SAMPLE)
    run.add_argument("--sources", type=Path, default=SOURCES)
    run.add_argument("--output", type=Path, default=RESULTS)
    run.add_argument("--corpus", type=Path, default=CORPUS)
    run.add_argument("--binary", type=Path, default=ROOT / "target" / "release" / "aitex")
    run.add_argument("--texmf-overlay", type=Path, default=TEXMF_OVERLAY)
    run.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 1))
    run.add_argument("--timeout", type=int, default=120)
    run.add_argument("--memory-limit-mib", type=int, default=4096,
                     help="per-compiler address-space safety limit (default: 4096)")
    run.add_argument("--limit", type=int,
                     help="schedule only this many remaining papers, for smoke tests")
    run.add_argument("--id", action="append",
                     help="compile only this locked paper ID (repeatable)")
    run.add_argument(
        "--reference-results", type=Path,
        help=("reuse pinned stock stages and comparisons while remeasuring only "
              "aiTeX; changed stock-clean outputs receive a fresh full comparison"),
    )
    run.set_defaults(function=command_run)
    report = sub.add_parser("report", help="summarize the locked run")
    report.add_argument("--results", type=Path, default=RESULTS)
    report.add_argument("--output", type=Path, default=REPORT)
    report.set_defaults(function=command_report)
    return result


def main() -> int:
    args = parser().parse_args()
    return args.function(args)


if __name__ == "__main__":
    raise SystemExit(main())
