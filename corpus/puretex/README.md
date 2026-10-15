# Pure plain-TeX corpus

Real-world plain-TeX documents (no `\documentclass`), used as a differential
test set for the engine **before** `latex.ltx` fully loads. Every LaTeX corpus
document needs the format; these exercise the tokenizer / gullet / stomach /
typesetting path on genuine documents our engine can attempt today.

`DOCUMENTS` lists which files here are documents rather than libraries a
document `\input`s. `tools/done_check.sh` reads it, wraps each with `\input
plain` / `\bye` where the file does not supply them, and requires the run to set
the same characters, in the same order, as `tex -ini` on the same source.

## Contents (provenance + license)

All harvested from the TeX Live 2024 distribution (`texmf-dist`), which vets
its contents for redistribution. Each file keeps its original header.

| File | Source | License | Notes |
|---|---|---|---|
| `story.tex` | `tex/plain/knuth-lib/` | Knuth (free for use/testing) | The canonical plain-TeX example (D. E. Knuth). Accents, `\centerline`, `\vfill\eject`. |
| `pliptest.tex` + `plipsum.tex` | `doc/plain/plipsum/`, `tex/plain/plipsum/` | LPPL 1.3 (© 2013 Sergio Spina) | Lorem-ipsum generator for plain TeX; multi-page filler text. |

## Current status vs `tex -ini` (2026-08-19)

| File | tex | ours | |
|---|---|---|---|
| `story.tex` | 1 page, 203 chars | 1 page, 203 chars | identical sequence |
| `pliptest.tex` | 5 pages, 20714 chars | 5 pages, 20714 chars | identical sequence |

Both were failing when this file was first written — `story.tex` shipped no
pages at all for want of `\accent` (§1123), and `pliptest.tex` errored. The last
divergence to go was `story.tex`'s `Drofnats---or`, where §1035's explicit-hyphen
discretionary was appended as each hyphen arrived instead of at the end of the
ligature run, so the em-dash ligature could never form.

The comparison is the *character sequence*, not `dvitype`'s `[...]` text runs:
those group by movement command, and TeX reuses its `w/x/y/z` registers more
aggressively than we do, which differs without anything being wrong.

Add more as they are found; prefer documents that (a) use only the plain format,
(b) are freely licensed, and (c) `tex -ini` compiles cleanly, so any divergence
is ours. Anything added must be listed in `DOCUMENTS` to be run.
