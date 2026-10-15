#!/usr/bin/env python3
"""Materialize, compile and label each candidate mutation.

For every mutant we record whether it actually breaks the build (many mutations
are no-ops and must be discarded), where pdflatex *claims* the error is, and the
localization gap between claim and truth. Also captures time-to-first-error,
which is one of the two baseline numbers in plan §10.
"""
import argparse
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mutate import apply_patch  # noqa: E402

ENV = dict(os.environ)
ENV.update({
    "max_print_line": "1000", "error_line": "254", "half_error_line": "238",
    "openout_any": "p", "openin_any": "p",
    "SOURCE_DATE_EPOCH": "1451606400", "FORCE_SOURCE_DATE": "1",
})
COMMON = ["-file-line-error", "-interaction=nonstopmode", "-no-shell-escape"]
TIMEOUT = 90

ERR_RE = re.compile(r"^(?:\./)?(\S+?):(\d+): (.*)$")


def first_error(text):
    for line in text.splitlines():
        m = ERR_RE.match(line)
        if m:
            return {"file": m.group(1), "line": int(m.group(2)), "msg": m.group(3)[:200]}
    for line in text.splitlines():
        if line.startswith("! "):
            return {"file": None, "line": None, "msg": line[2:222]}
    return None


def one(rec):
    out = {k: rec[k] for k in
           ("id", "project", "file", "line", "family", "expected_gap", "note")}
    with tempfile.TemporaryDirectory(prefix="aitex-mut-") as td:
        td = Path(td)
        work = td / "w"
        try:
            shutil.copytree(rec["src_path"], work, symlinks=False,
                            ignore_dangling_symlinks=True)
        except Exception as e:
            out["status"] = f"copy-failed: {e}"
            return out
        for pat in ("*.aux", "*.log", "*.pdf", "*.out", "*.toc", "*.fls",
                    "*.fdb_latexmk", "*.synctex.gz"):
            for f in work.rglob(pat):
                try:
                    f.unlink()
                except Exception:
                    pass

        if not apply_patch(work, rec):
            out["status"] = "patch-did-not-apply"
            return out

        root = work / rec["src_root"]
        env = dict(ENV)
        env["TEXMFVAR"] = str(td / "tv")
        (td / "tv").mkdir(exist_ok=True)

        t0 = time.perf_counter()
        try:
            p = subprocess.run(["pdflatex"] + COMMON + ["-halt-on-error", root.name],
                               cwd=root.parent, env=env, timeout=TIMEOUT,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            rc, txt = p.returncode, p.stdout.decode("utf-8", "replace")
        except subprocess.TimeoutExpired:
            rc, txt = -9, "TIMEOUT"
        out["time_to_error_s"] = round(time.perf_counter() - t0, 3)

        logf = root.parent / f"{root.stem}.log"
        if logf.exists():
            txt = logf.read_text("utf-8", "replace")

        fe = first_error(txt)
        out["rc"] = rc
        out["reported"] = fe

        if rc == 0 and "TIMEOUT" not in txt:
            out["status"] = "no-op"           # still compiles: discard
            return out
        if not fe:
            out["status"] = "broken-unparsed"
            out["gap"] = None
            return out

        out["status"] = "broken"
        if fe["line"] is None:
            out["gap"] = None
        elif fe["file"] and Path(fe["file"]).name != Path(rec["file"]).name:
            out["gap"] = math.inf                # different file entirely
        else:
            out["gap"] = abs(fe["line"] - rec["line"])
        return out


def bucket(g):
    if g is None:
        return "unknown"
    if g == math.inf:
        return "other-file"
    if g == 0:
        return "gap=0"
    if g <= 50:
        return "0<gap<=50"
    return "gap>50"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mutations", required=True)
    ap.add_argument("--output", required=True)
    ap.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 4) - 2))
    a = ap.parse_args()

    recs = json.load(open(a.mutations))
    print(f"[verify] {len(recs)} mutants, {a.jobs} workers", flush=True)
    out = []
    with ProcessPoolExecutor(max_workers=a.jobs) as ex:
        for n, r in enumerate(ex.map(one, recs), 1):
            out.append(r)
            if n % 20 == 0 or n == len(recs):
                print(f"  [{n}/{len(recs)}]", flush=True)
                Path(a.output).write_text(
                    json.dumps(out, indent=1, default=lambda o: "inf"))
    Path(a.output).write_text(json.dumps(out, indent=1, default=lambda o: "inf"))

    from collections import Counter
    st = Counter(r["status"] for r in out)
    bk = Counter(bucket(r.get("gap")) for r in out if r["status"] == "broken")
    print("\n=== status ===")
    for k, v in st.most_common():
        print(f"  {v:5}  {k}")
    print("=== gap buckets (broken only) ===")
    for k, v in bk.most_common():
        print(f"  {v:5}  {k}")
    print("=== gap by family ===")
    fam = {}
    for r in out:
        if r["status"] == "broken":
            fam.setdefault(r["family"], []).append(bucket(r.get("gap")))
    for f in sorted(fam):
        c = Counter(fam[f])
        print(f"  {f:20} n={len(fam[f]):4}  " +
              " ".join(f"{k}={v}" for k, v in c.most_common()))


if __name__ == "__main__":
    main()
