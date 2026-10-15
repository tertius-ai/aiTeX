#!/usr/bin/env python3
"""Turn fetched arXiv projects into bench specs, ranked by preamble simplicity."""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
META = ROOT / "corpus" / "meta"
ARX = ROOT / "corpus" / "arxiv"

recs = json.load(open(META / "arxiv_raw.json"))
ok = [r for r in recs if r.get("status") == "ok" and r.get("root")]


def simplicity(r):
    """Lower is simpler. Heavy graphics packages dominate the ranking."""
    return (len(r.get("heavy") or []) * 100
            + r.get("n_pkgs", 99)
            + max(0, r.get("n_tex", 1) - 1) * 2)


ok.sort(key=simplicity)
specs = [{
    "name": r["id"].replace("/", "_"),
    "path": str(ARX / r["id"].replace("/", "_")),
    "root": r["root"],
    "section": r["section"],
    "n_pkgs": r.get("n_pkgs"),
    "heavy": r.get("heavy"),
    "simplicity": simplicity(r),
} for r in ok]

(META / "arxiv_specs.json").write_text(json.dumps(specs, indent=1))
print(f"{len(specs)} arXiv specs (sorted simplest-first)")
for s in specs[:8]:
    print(f"  {s['name']:16} {s['section']:16} pkgs={s['n_pkgs']:3} score={s['simplicity']}")
