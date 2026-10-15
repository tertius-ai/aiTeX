# Compiler and complete-build matrix

The compiler/build matrix answers a narrower and more reproducible question
than the compatibility sweep: how do aiTeX and the principal TeX-family
producers behave on the same documents when asked to perform either exactly
one compiler pass or a complete clean build?

The versioned contract is
[`../qualification/compiler-build-matrix-v1.json`](../qualification/compiler-build-matrix-v1.json).
The runner is
[`../tools/compiler_build_matrix.py`](../tools/compiler_build_matrix.py).

## Terminology

`one-shot` means one compiler process and one TeX pass. It intentionally does
not resolve a newly written table of contents, reference, citation, index, or
glossary. The classical comparisons are:

| Source/route | Reference | Candidate |
|---|---|---|
| Plain format loaded from source, DVI | `tex -ini` | `aitex pass --ini` |
| LaTeX format, DVI | `latex` | `aitex pass -o …dvi` |
| LaTeX format, direct PDF | `pdflatex` | `aitex pass -o …pdf` |

`full-build` means a clean project driven to the producer's fixed point. The
entire frontend process tree is timed, including repeated TeX passes and any
BibTeX, Biber, MakeIndex, or MakeGlossaries children:

| Route | Reference | Candidate |
|---|---|---|
| DVI | `latexmk -dvi` | ordinary `aitex … -o …dvi` |
| direct PDF | `latexmk -pdf` | ordinary `aitex … -o …pdf` |

XeLaTeX and LuaLaTeX are additional direct-PDF producers in both modes. Their
complete-build cells use `latexmk -xelatex` and `latexmk -lualatex`.
Tectonic is deliberately absent until its bundle can be pinned and made fully
offline; otherwise the comparison would silently include network and mutable
package state. pTeX/upTeX and ConTeXt are outside the current product target.

## Corpus layers

The suite combines two kinds of evidence:

- Eight small, original, redistributable projects cover plain TeX, `article`,
  `book`, `report`, `amsart`, and `beamer`; common math, theorem, table, link,
  microtype, bibliography, index, PGF/TikZ, and generated PNG work; classic
  Type 1 fonts; and a same-font-file Arabic, Hebrew, Devanagari, CJK, Thai, and
  Lao source for aiTeX, XeLaTeX, and LuaLaTeX.
- The locked 191-root corpus supplies real project trees, document classes,
  packages, font choices, graphics, and auxiliary protocols. Its sources are
  materialized from their immutable response and tree hashes and are not
  redistributed by this repository.

The manifest content-locks every curated source tree, the complete shared
OpenType fixture tree, and the deterministic image generator. Thus the
same-font-file claim cannot drift when an unrelated font test fixture changes.

Every result inventories document classes, packages, explicit fonts,
languages, graphic formats, build features, source files, and source bytes.
Raw cells remain attached to those traits, so a regression can be grouped by
class, package, font, language, asset type, workflow, route, or producer rather
than disappearing into one corpus total.

The deterministic tiers are:

| Tier | Content | Default repetitions |
|---|---|---:|
| `smoke` | plain, common article, bibliography/index report, Unicode | 1 |
| `curated` | all eight original projects | 3 after one warm-up |
| `corpus-10` | hash-selected 19/191 real roots, including XeTeX/LuaTeX | 3 after one warm-up |
| `corpus-full` | all 191 real roots, classic core producers | 1 after one warm-up |
| `all` | eight curated plus all 191 roots and all producers | 1 after one warm-up |

The 10% selection is a stable hash ranking with the seed stored in the
manifest. It cannot drift toward favorable documents as the compiler changes.

## Timing protocol

Latency runs are sequential by construction. Each measured producer receives
a new source-tree copy with a fixed clock and fixed mtimes. When enabled, one
unmeasured run of the same producer/case/cell warms executable, package, font,
and filesystem pages. Measured producer order is then deterministically
counterbalanced for every repetition.

The timed interval contains only the complete producer process tree. Source
copying, tree hashing, PDF/DVI inspection, text extraction, rasterization, and
differential comparison occur outside it. Wall time, child CPU, peak RSS,
filesystem I/O where the host exposes it, TeX-pass count, semantic-tool count,
output bytes, pages, font embedding, and output hashes are retained per
sample. Machine, effective CPU affinity, exact tool versions, compiler/format
hashes, worker budget, Git commit, and dirty state are retained with the run.
Aggregate tables report both the ratio of total work across paired,
correctness-equivalent cases and the median and geometric mean of per-case
ratios. The former weights large projects by their actual cost; the latter
keeps one large project from standing in for the typical document. Failed,
timed-out, oracle-unavailable, or semantically different cells remain visible
as compatibility evidence but never contribute misleading speed ratios.

Long runs checkpoint atomically after every complete project. The default
checkpoint is `OUTPUT.state.json`; rerunning the identical command resumes the
completed prefix. Its identity includes the manifest and runner hashes, case
locks, aiTeX binary and format hashes, machine/toolchain, oracle, worker budget,
and sampling protocol, so changed experiments cannot silently mix results.
`--fresh` deliberately discards a checkpoint. A successfully finalized JSON
and optional Markdown report make the checkpoint redundant, so it is removed.

The runner has one explicit worker budget. It defaults to four but is capped
by the process's effective CPU affinity and is passed to aiTeX as its PDF
worker allowance. There is no hidden outer parallelism in the latency
protocol. A future throughput experiment must be a separately labeled mode
that divides this same global budget rather than nesting independent pools.

## Correctness and comparability

Classic DVI pairs use the exact normalized positioned drawing and driver-
special oracle. Classic PDF pairs require valid outputs, matching page counts,
and no text-mapping quality regression. `--oracle visual` additionally applies
the all-page raster tolerance outside the timed interval.

XeTeX and LuaTeX do not promise pdfTeX geometry or pagination, even with the
same source and font files. Their cross-engine oracle therefore requires a
valid PDF, no worse invalid-codepoint count, at least 98% normalized character
multiset coverage, and every case-declared semantic marker. Logical sequence,
page count, fonts, and geometry remain visible diagnostics. This catches
missing or duplicated script text without pretending that different layout
engines should produce the same pixels.

A reference failure is recorded as `reference-failed` and never turned into a
candidate failure. If the reference succeeds and aiTeX fails, the comparison
fails. No unavailable command or zero denominator is silently dropped.

The default aiTeX PDF and stock PDF do not always perform identical image
work: aiTeX's lossless normalization may spend more CPU to produce a much
smaller artifact. Consequently PDF CPU, wall, RSS, and bytes must be read
together. The matrix preserves this product-default comparison. A future
same-image-work lane requires an explicit, supported fast-output profile; the
benchmark will not invent an undocumented environment switch or loosen a CPU
gate to hide that tradeoff.

## Commands

```sh
# Offline manifest, source-lock, taxonomy, and scheduling checks.
python3 tools/compiler_build_matrix.py --self-test

# Inspect a tier without invoking a compiler.
python3 tools/compiler_build_matrix.py --tier corpus-10 --dry-run \
  --output target/qualification/compiler-matrix-plan.json

# Rebuild an engine-matched aiTeX format and run the small matrix.
python3 tools/compiler_build_matrix.py --tier smoke --build \
  --output target/qualification/compiler-matrix-smoke.json \
  --report target/qualification/COMPILER-MATRIX-SMOKE.md

# Stable development sample from the materialized locked corpus.
python3 tools/compiler_build_matrix.py --tier corpus-10 --oracle visual \
  --output target/qualification/compiler-matrix-corpus-10.json \
  --report target/qualification/COMPILER-MATRIX-CORPUS-10.md

# Focused lane: run only the named comparison and its two producer cells.
python3 tools/compiler_build_matrix.py --tier corpus-10 --oracle visual \
  --comparison classic-one-shot-dvi \
  --output target/qualification/compiler-matrix-one-shot-dvi.json \
  --report target/qualification/COMPILER-MATRIX-ONE-SHOT-DVI.md

# Full machine-qualified matrix. This is intentionally a manual long run.
python3 tools/compiler_build_matrix.py --tier all --oracle visual \
  --output target/qualification/compiler-build-matrix-v1.results.json \
  --report target/qualification/COMPILER-BUILD-MATRIX-V1.md
```

`--check` returns failure when aiTeX fails against a successful reference.
`--strict` additionally requires every scheduled producer and comparison to
succeed; it is appropriate for the curated tier, not a heterogeneous corpus
where a classical or cousin engine may legitimately reject some projects.
Use `--fresh` to restart a partially completed run instead of resuming its
automatically validated checkpoint.
