#!/usr/bin/env python3
"""Materialize the mutated source for each corpus entry, next to its log.

Structural rules match brackets in the *broken* text, so scoring them needs the
file as the engine saw it -- not the clean original.
"""
import json, os, shutil, sys, tempfile
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mutate import apply_patch

ROOT = Path(__file__).resolve().parent.parent
CORPUS_FILE = __import__("os").environ.get("CORPUS_FILE","broken_corpus.json")
MUT_FILE = __import__("os").environ.get("MUT_FILE","mutations_all.json")
OUT = ROOT / "corpus" / __import__("os").environ.get("SRCDIR","sources")

def one(rec):
    dest = OUT / f"{rec['id']}.tex"
    if dest.exists():
        return "cached"
    with tempfile.TemporaryDirectory(prefix="aitex-src-") as td:
        work = Path(td) / "w"
        try:
            shutil.copytree(rec["src_path"], work, symlinks=False,
                            ignore_dangling_symlinks=True)
        except Exception:
            return "copy-failed"
        if not apply_patch(work, rec):
            return "patch-failed"
        # The rules read the file the error points into, which is the mutated one.
        src = work / rec["file"]
        if not src.exists():
            return "missing"
        shutil.copy(src, dest)
        return "ok"

def main():
    OUT.mkdir(parents=True, exist_ok=True)
    corpus = json.load(open(ROOT / "corpus" / "meta" / CORPUS_FILE))
    muts = {m["id"]: m for m in json.load(open(ROOT / "corpus" / "meta" / MUT_FILE))}
    full = [muts[r["id"]] for r in corpus if r["id"] in muts]
    from collections import Counter
    c = Counter()
    with ProcessPoolExecutor(max_workers=max(1, (os.cpu_count() or 4) - 2)) as ex:
        for n, s in enumerate(ex.map(one, full), 1):
            c[s] += 1
            if n % 200 == 0:
                print(f"  [{n}/{len(full)}] {dict(c)}", flush=True)
    print(dict(c))

if __name__ == "__main__":
    main()
