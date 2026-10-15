#!/usr/bin/env python3
"""Materialize each broken-corpus mutant and keep its .log, for scoring the
Rust parser against real engine output rather than synthetic fixtures."""
import json
import os
import shutil
import subprocess
import sys
import tempfile
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mutate import apply_patch  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
CORPUS_FILE = __import__("os").environ.get("CORPUS_FILE","broken_corpus.json")
MUT_FILE = __import__("os").environ.get("MUT_FILE","mutations_all.json")
OUT = ROOT / "corpus" / __import__("os").environ.get("LOGDIR","logs")
ENV = dict(os.environ)
ENV.update({"max_print_line": "1000", "error_line": "254", "half_error_line": "238",
            "openout_any": "p", "openin_any": "p",
            "SOURCE_DATE_EPOCH": "1451606400", "FORCE_SOURCE_DATE": "1"})
FLAGS = ["-file-line-error", "-interaction=nonstopmode", "-no-shell-escape"]


def one(rec):
    dest = OUT / f"{rec['id']}.log"
    if dest.exists():
        return "cached"
    with tempfile.TemporaryDirectory(prefix="aitex-log-") as td:
        td = Path(td)
        work = td / "w"
        try:
            shutil.copytree(rec["src_path"], work, symlinks=False,
                            ignore_dangling_symlinks=True)
        except Exception:
            return "copy-failed"
        for pat in ("*.aux", "*.log", "*.pdf", "*.out", "*.fls", "*.fdb_latexmk"):
            for f in work.rglob(pat):
                try:
                    f.unlink()
                except Exception:
                    pass
        if not apply_patch(work, rec):
            return "patch-failed"
        root = work / rec["src_root"]
        env = dict(ENV)
        env["TEXMFVAR"] = str(td / "tv")
        (td / "tv").mkdir(exist_ok=True)
        try:
            subprocess.run(["pdflatex"] + FLAGS + [root.name], cwd=root.parent, env=env,
                           timeout=90, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            pass
        log = root.parent / f"{root.stem}.log"
        if log.exists():
            shutil.copy(log, dest)
            return "ok"
        return "no-log"


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    recs = json.load(open(ROOT / "corpus" / "meta" / CORPUS_FILE))
    # keep src_path/src_root, which the finalizer drops
    muts = {m["id"]: m for m in json.load(open(ROOT / "corpus" / "meta" / MUT_FILE))}
    full = [muts[r["id"]] for r in recs if r["id"] in muts]
    print(f"[logs] {len(full)} mutants", flush=True)
    from collections import Counter
    c = Counter()
    with ProcessPoolExecutor(max_workers=max(1, (os.cpu_count() or 4) - 2)) as ex:
        for n, s in enumerate(ex.map(one, full), 1):
            c[s] += 1
            if n % 100 == 0:
                print(f"  [{n}/{len(full)}] {dict(c)}", flush=True)
    print(f"[logs] {dict(c)}")


if __name__ == "__main__":
    main()
