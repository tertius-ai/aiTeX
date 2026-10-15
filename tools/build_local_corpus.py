#!/usr/bin/env python3
"""Select PUBLISHED papers from ~/Dropbox/research for the local timing corpus.

Published = title matches an arXiv entry authored by Antieau. Everything
grant/job/tenure/referee/letter related is excluded by name before we look at
content at all, so confidential third-party material never enters the pipeline.
"""
import difflib
import json
import re
import time
import urllib.parse
import urllib.request
from pathlib import Path

UA = "aiTeX-corpus-builder/0.1 (research benchmark; benjamin.antieau@gmail.com)"
RESEARCH = Path.home() / "Dropbox" / "research"
META = Path(__file__).resolve().parent.parent / "corpus" / "meta"

# Hard exclusions: never enters the corpus regardless of content.
EXCLUDE = re.compile(
    r"nsf|job|grant|letter|referee|refereework|review|tenure|promot|salary"
    r"|personal|report|eval|recommend|cv|teaching|seminar|talk|slides|notes",
    re.I,
)


def norm(s):
    s = re.sub(r"\\[a-zA-Z]+\s*", " ", s or "")
    s = re.sub(r"[^a-z0-9 ]+", " ", s.lower())
    return re.sub(r"\s+", " ", s).strip()


def arxiv_titles(author="Antieau", want=400):
    out, start, step = [], 0, 100
    while start < want:
        q = urllib.parse.urlencode({
            "search_query": f'au:"{author}"',
            "start": start, "max_results": step,
        })
        req = urllib.request.Request(
            f"http://export.arxiv.org/api/query?{q}", headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=60) as r:
            xml = r.read().decode("utf-8", "replace")
        entries = re.findall(r"<entry>(.*?)</entry>", xml, re.S)
        if not entries:
            break
        for e in entries:
            t = re.search(r"<title>(.*?)</title>", e, re.S)
            i = re.search(r"<id>http://arxiv\.org/abs/([^<]+)</id>", e)
            if t and i:
                out.append({"title": re.sub(r"\s+", " ", t.group(1)).strip(),
                            "id": i.group(1).strip()})
        start += step
        time.sleep(3.0)
    return out


def tex_title(src):
    m = re.search(r"\\title\s*\{", src)
    if not m:
        return None
    i, depth, buf = m.end(), 1, []
    while i < len(src) and depth:
        c = src[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if not depth:
                break
        buf.append(c)
        i += 1
    return "".join(buf)


def candidate_roots(d):
    """Prefer a .tex named after the directory; else the largest root file."""
    roots = []
    for t in d.rglob("*.tex"):
        if any(part in {"drafts", "old", "archive", ".git"} for part in t.parts):
            continue
        try:
            s = t.read_text("utf-8", "replace")
        except Exception:
            continue
        if "\\documentclass" in s:
            roots.append((t, s))
    if not roots:
        return None
    stem = re.sub(r"^\d+-", "", d.name)
    for t, s in roots:
        if t.stem == stem:
            return t, s
    return max(roots, key=lambda p: len(p[1]))


def main():
    META.mkdir(parents=True, exist_ok=True)
    print("[arxiv] fetching Antieau titles...", flush=True)
    pubs = arxiv_titles()
    print(f"[arxiv] {len(pubs)} entries", flush=True)
    (META / "arxiv_author.json").write_text(json.dumps(pubs, indent=1))
    pub_norm = [(norm(p["title"]), p) for p in pubs]

    selected, skipped = [], []
    for d in sorted(RESEARCH.iterdir()):
        if not d.is_dir():
            continue
        if EXCLUDE.search(d.name):
            skipped.append({"dir": d.name, "why": "excluded-by-name"})
            continue
        cr = candidate_roots(d)
        if not cr:
            skipped.append({"dir": d.name, "why": "no-documentclass"})
            continue
        root, src = cr
        t = tex_title(src)
        if not t:
            skipped.append({"dir": d.name, "why": "no-title"})
            continue
        tn = norm(t)
        best, score = None, 0.0
        for pn, p in pub_norm:
            r = difflib.SequenceMatcher(None, tn, pn).ratio()
            if r > score:
                best, score = p, r
        if score < 0.75:
            skipped.append({"dir": d.name, "why": f"unmatched({score:.2f})",
                            "title": t[:80]})
            continue
        selected.append({
            "dir": d.name,
            "path": str(d),
            "root": str(root.relative_to(d)),
            "title": re.sub(r"\s+", " ", t).strip()[:120],
            "arxiv_id": best["id"],
            "match": round(score, 3),
            "n_tex": len(list(d.rglob("*.tex"))),
            "has_bib": bool(list(d.rglob("*.bib"))),
        })

    (META / "local_selected.json").write_text(json.dumps(selected, indent=1))
    (META / "local_skipped.json").write_text(json.dumps(skipped, indent=1))
    print(f"[select] {len(selected)} published papers, {len(skipped)} skipped", flush=True)
    for s in selected[:15]:
        print(f"  {s['dir']:28} {s['match']:.2f}  {s['title'][:56]}", flush=True)


if __name__ == "__main__":
    main()
