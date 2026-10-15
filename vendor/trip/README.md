# TRIP — Knuth's torture test

Fetched from CTAN `systems/knuth/dist/tex/` (not shipped in TeX Live's texmf-dist,
which carries only `tripman.pdf`).

| File | Role |
|---|---|
| `trip.tex` | the input |
| `tripin.log` | reference transcript of the INITEX/format-dump pass |
| `trip.log` | reference transcript of the main pass |
| `trip.fot` | reference abbreviated terminal output |
| `trip.typ` | reference `dvitype` output of `trip.dvi` |
| `trip.pl` | property list; `PLtoTF` it to build `trip.tfm` |

Per docs/history/PLAN.md §4, TRIP is a **late confidence check, not a gate** — XeTeX and
LuaTeX do not pass it either. Two things make it useful anyway:

* every artifact is a text file, so "% of `trip.log` matching" is a real metric;
* `tripin.log` is reachable at engine E1 — it exercises tokenizer, eqtb,
  primitive tables and dump/undump with no typesetting at all.

Legitimate differences are limited to dates, filenames, and glue-set values
subject to system-dependent rounding.
