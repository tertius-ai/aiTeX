#!/usr/bin/env python3
"""Compile-verify and time a corpus.

Two measurements, per plan §10:
  * total compile time   -- latexmk to convergence, and single-pass pdflatex
  * time to first error  -- pdflatex -halt-on-error

Every project is copied to a scratch dir before compiling; source trees are
never written to. All invocations use the plan's mandated flags:
    max_print_line=1000 -file-line-error -interaction=nonstopmode -no-shell-escape
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
META = ROOT / "corpus" / "meta"

ENV = dict(os.environ)
ENV.update({
    "max_print_line": "1000",
    "error_line": "254",
    "half_error_line": "238",
    "TEXMFVAR": "",  # filled per-run for cold-cache isolation
    "openout_any": "p",
    "openin_any": "p",
    # Reproducible output: without these pdfTeX embeds /CreationDate and the
    # golden hash differs on every run, making it useless as an oracle.
    "SOURCE_DATE_EPOCH": "1451606400",  # 2016-01-01T00:00:00Z
    "FORCE_SOURCE_DATE": "1",
})

COMMON = ["-file-line-error", "-interaction=nonstopmode", "-no-shell-escape"]
TIMEOUT = 180


def run(cmd, cwd, env, timeout=TIMEOUT):
    t0 = time.perf_counter()
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, timeout=timeout,
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        return time.perf_counter() - t0, p.returncode, p.stdout.decode("utf-8", "replace")
    except subprocess.TimeoutExpired:
        return time.perf_counter() - t0, -9, "TIMEOUT"


def first_error(log_text):
    """Extract the first `file:line: message` the engine reports."""
    for line in log_text.splitlines():
        m = re.match(r"^(?:\./)?(\S+?):(\d+): (.*)$", line)
        if m and not m.group(3).startswith("=="):
            return {"file": m.group(1), "line": int(m.group(2)),
                    "msg": m.group(3)[:200]}
    for line in log_text.splitlines():
        if line.startswith("! "):
            return {"file": None, "line": None, "msg": line[2:222]}
    return None


def pdf_stats(pdf, log_text=""):
    """Hash the PDF; take the page count from the log.

    Counting /Type /Page in the raw bytes fails whenever the PDF uses object
    streams (which pdfTeX does by default). pdfTeX states the count plainly:
        Output written on foo.pdf (12 pages, 345678 bytes).
    """
    pages = None
    m = re.search(r"Output written on .*?\((\d+) pages?", log_text)
    if m:
        pages = int(m.group(1))
    if not pdf.exists():
        return None, pages
    return hashlib.sha256(pdf.read_bytes()).hexdigest()[:16], pages


def badboxes(log_text):
    return (len(re.findall(r"Overfull \\[hv]box", log_text)),
            len(re.findall(r"Underfull \\[hv]box", log_text)))


def bench_one(spec):
    """spec: dict(name, path, root, reps, mode)"""
    name, src, root = spec["name"], Path(spec["path"]), spec["root"]
    reps = spec.get("reps", 3)
    res = {"name": name, "root": root, "section": spec.get("section")}

    with tempfile.TemporaryDirectory(prefix="aitex-") as td:
        td = Path(td)
        work = td / "w"
        try:
            shutil.copytree(src, work, symlinks=False, ignore_dangling_symlinks=True)
        except Exception as e:
            res["status"] = f"copy-failed: {e}"
            return res
        # Strip stale build artifacts so timings are honest.
        for pat in ("*.aux", "*.log", "*.pdf", "*.out", "*.toc", "*.bbl", "*.blg",
                    "*.fls", "*.fdb_latexmk", "*.synctex.gz", "*.brf", "*.ind", "*.idx"):
            for f in work.rglob(pat):
                try:
                    f.unlink()
                except Exception:
                    pass

        rootpath = work / root
        if not rootpath.exists():
            res["status"] = "root-missing"
            return res
        jobdir, job = rootpath.parent, rootpath.stem

        env = dict(ENV)
        env["TEXMFVAR"] = str(td / "texmfvar")
        (td / "texmfvar").mkdir(exist_ok=True)

        # --- single-pass pdflatex, cold then warm reps ---
        times = []
        out = ""
        for i in range(reps):
            t, rc, out = run(["pdflatex"] + COMMON + [rootpath.name], jobdir, env)
            times.append(t)
        logf = jobdir / f"{job}.log"
        logtxt = logf.read_text("utf-8", "replace") if logf.exists() else out

        res["pdflatex_cold_s"] = round(times[0], 3)
        res["pdflatex_warm_s"] = round(statistics.median(times[1:] or times), 3)
        res["pdflatex_rc"] = rc
        res["first_error"] = first_error(logtxt)
        ov, un = badboxes(logtxt)
        res["overfull"], res["underfull"] = ov, un

        # --- time to first error (halt-on-error) ---
        t, rc_h, out_h = run(["pdflatex"] + COMMON + ["-halt-on-error", rootpath.name],
                             jobdir, env)
        res["time_to_error_s"] = round(t, 3)
        res["halt_rc"] = rc_h

        # --- latexmk to convergence ---
        for f in jobdir.glob(f"{job}.*"):
            if f.suffix in {".aux", ".log", ".pdf", ".out", ".toc", ".bbl", ".blg"}:
                try:
                    f.unlink()
                except Exception:
                    pass
        t, rc_m, out_m = run(
            ["latexmk", "-pdf", "-silent", "-f",
             f"-pdflatex=pdflatex {' '.join(COMMON)} %O %S", rootpath.name],
            jobdir, env, timeout=TIMEOUT * 2)
        res["latexmk_s"] = round(t, 3)
        res["latexmk_rc"] = rc_m

        mklog = jobdir / f"{job}.log"
        mktxt = mklog.read_text("utf-8", "replace") if mklog.exists() else out_m
        h, pages = pdf_stats(jobdir / f"{job}.pdf", mktxt)
        res["pdf_sha256_16"], res["pages"] = h, pages
        res["status"] = "clean" if (rc == 0 and h) else ("broken" if h else "failed")
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", required=True, help="json list of {name,path,root}")
    ap.add_argument("--output", required=True)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 4) - 2))
    a = ap.parse_args()

    specs = json.load(open(a.input))
    for s in specs:
        s["reps"] = a.reps
    print(f"[bench] {len(specs)} projects, {a.jobs} workers", flush=True)

    out = []
    with ProcessPoolExecutor(max_workers=a.jobs) as ex:
        for n, r in enumerate(ex.map(bench_one, specs), 1):
            out.append(r)
            print(f"[{n}/{len(specs)}] {r['name']:30} {r.get('status'):8} "
                  f"pdflatex={r.get('pdflatex_warm_s','-')}s "
                  f"latexmk={r.get('latexmk_s','-')}s pages={r.get('pages','-')}",
                  flush=True)
            Path(a.output).write_text(json.dumps(out, indent=1))
    print(f"[done] clean={sum(1 for r in out if r.get('status')=='clean')}", flush=True)


if __name__ == "__main__":
    main()
