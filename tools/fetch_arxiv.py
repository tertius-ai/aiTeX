#!/usr/bin/env python3
"""Fetch a stratified arXiv corpus: IDs via the public API, sources via /e-print.

Polite by construction: one request per REQ_DELAY seconds, descriptive User-Agent.
We want breadth across sections and *simple preambles* -- heavy TikZ/pgfplots
documents measure package execution, not engine speed.
"""
import gzip
import io
import json
import os
import re
import shutil
import sys
import tarfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

UA = "aiTeX-corpus-builder/0.1 (research benchmark; benjamin.antieau@gmail.com)"
REQ_DELAY = 3.0  # arXiv asks ~1 req / 3s
OUT = Path(__file__).resolve().parent.parent / "corpus" / "arxiv"
META = Path(__file__).resolve().parent.parent / "corpus" / "meta"

# Spread across all of arXiv, not just math.
SECTIONS = [
    "math.AG", "math.AT", "math.NT", "math.CO",
    "cs.LG", "cs.DS", "cs.CL",
    "physics.optics", "cond-mat.str-el", "astro-ph.GA", "hep-th", "quant-ph",
    "q-bio.PE", "econ.EM", "stat.ME", "eess.SP",
]
PER_SECTION = 14


def get(url, binary=False):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read() if binary else r.read().decode("utf-8", "replace")


def discover(section, n):
    """Query the arXiv API for recent IDs in a section."""
    q = urllib.parse.urlencode({
        "search_query": f"cat:{section}",
        "start": 0,
        "max_results": n,
        "sortBy": "submittedDate",
        "sortOrder": "descending",
    })
    xml = get(f"http://export.arxiv.org/api/query?{q}")
    ids = re.findall(r"<id>http://arxiv\.org/abs/([^<]+)</id>", xml)
    return [i.strip() for i in ids]


def fetch_source(aid):
    """Download and unpack one e-print into corpus/arxiv/<safe_id>/."""
    safe = aid.replace("/", "_")
    dest = OUT / safe
    if dest.exists():
        return dest, "cached"
    raw = get(f"https://arxiv.org/e-print/{aid}", binary=True)
    dest.mkdir(parents=True, exist_ok=True)
    try:
        # Usually a gzipped tar; sometimes a bare gzipped .tex.
        try:
            with tarfile.open(fileobj=io.BytesIO(raw), mode="r:*") as tf:
                members = [m for m in tf.getmembers()
                           if m.isreg() and not m.name.startswith("/")
                           and ".." not in m.name]
                for m in members:
                    tf.extract(m, dest, filter="data")
        except tarfile.ReadError:
            body = gzip.decompress(raw)
            (dest / "main.tex").write_bytes(body)
        return dest, "ok"
    except Exception as e:
        shutil.rmtree(dest, ignore_errors=True)
        return None, f"unpack-failed: {e}"


def profile(projdir):
    """Cheap structural profile used for the simple-preamble filter."""
    texs = list(projdir.rglob("*.tex"))
    roots = []
    for t in texs:
        try:
            s = t.read_text("utf-8", "replace")
        except Exception:
            continue
        if "\\documentclass" in s:
            roots.append((t, s))
    if not roots:
        return None
    # Root = the \documentclass file with the most content.
    root, src = max(roots, key=lambda p: len(p[1]))
    pkgs = re.findall(r"\\usepackage(?:\[[^\]]*\])?\{([^}]*)\}", src)
    pkgs = sorted({p.strip() for grp in pkgs for p in grp.split(",") if p.strip()})
    heavy = {"tikz", "pgfplots", "pstricks", "animate", "movie15", "media9",
             "circuitikz", "chemfig", "forest", "tikz-cd", "asymptote"}
    return {
        "root": str(root.relative_to(projdir)),
        "n_tex": len(texs),
        "n_pkgs": len(pkgs),
        "packages": pkgs,
        "heavy": sorted(set(pkgs) & heavy),
        "has_bib": bool(list(projdir.rglob("*.bib"))),
        "bytes": len(src),
    }


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    META.mkdir(parents=True, exist_ok=True)

    ids = []
    for sec in SECTIONS:
        try:
            got = discover(sec, PER_SECTION)
            ids += [(sec, i) for i in got]
            print(f"[ids] {sec}: {len(got)}", flush=True)
        except Exception as e:
            print(f"[ids] {sec}: FAILED {e}", flush=True)
        time.sleep(REQ_DELAY)

    print(f"[ids] total {len(ids)}", flush=True)
    records = []
    for n, (sec, aid) in enumerate(ids, 1):
        try:
            d, status = fetch_source(aid)
        except Exception as e:
            d, status = None, f"http-failed: {e}"
        rec = {"id": aid, "section": sec, "status": status}
        if d:
            p = profile(d)
            if p:
                rec.update(p)
            else:
                rec["status"] = "no-documentclass"
        records.append(rec)
        print(f"[{n}/{len(ids)}] {aid} {sec} {rec['status']} "
              f"pkgs={rec.get('n_pkgs','-')} heavy={rec.get('heavy','-')}", flush=True)
        (META / "arxiv_raw.json").write_text(json.dumps(records, indent=1))
        if status != "cached":
            time.sleep(REQ_DELAY)

    print(f"[done] {sum(1 for r in records if r['status']=='ok')} fetched", flush=True)


if __name__ == "__main__":
    main()
