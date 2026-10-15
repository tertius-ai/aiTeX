# Getting the best performance

aiTeX is fastest when it can prove that earlier work is still valid. Always
use a release build and a preloaded LaTeX format. Then choose the reuse mode
that matches the workflow: no persistent cache for a genuinely one-off build,
validated output reuse for repeated builds, or a persistent incremental
process while editing.

PDF is the recommended output route. `aitex main.tex` produces
`main.pdf`, drives the document to a fixed point, and is the primary normal
command; `aitex build main.tex` is an explicit alias. The PDF driver consumes
aiTeX's private DVI representation internally; no external DVI tool or
intermediate file is required.

## One-time setup

Build the optimized compiler and create a local format once:

```sh
make
```

This creates `bin/aitex` and prewarms `.aitex/format/fmt.afmt` against the
installed TeX Live or MacTeX tree. A binary installed from a native bundle
instead creates `$XDG_CACHE_HOME/aitex/formats/latex.afmt` (or the platform
fallback below) on its first document build. `aitex format` performs that
one-time work explicitly. aiTeX validates the engine identity, checksum,
relevant TeX environment, and every kernel/configuration file read while the
format was built; it regenerates the cache after either aiTeX or TeX changes.
There is normally no reason to pass `--fmt`.

## Complete builds, repeated builds, and CI

The native scheduler retains aiTeX's hot resolver and asset caches across TeX
passes. It fingerprints the semantic inputs and outputs of BibTeX, Biber,
MakeIndex, and MakeGlossaries, invokes only processors whose inputs changed,
and stops when TeX-written inputs and semantic products are both stable. It
does not use `latexmk` to schedule compilation.

The standard command defaults to PDF and the platform cache directory:

```sh
bin/aitex main.tex
```

On Unix/Linux the cache is `$XDG_CACHE_HOME/aitex`, falling back to
`~/.cache/aitex`. On macOS it is `~/Library/Caches/aiTeX` unless
`XDG_CACHE_HOME` is explicitly set. aiTeX falls back to `.aitex-cache` beside
the input only when it cannot determine a home directory.

Name a stable cache explicitly when CI restores it at another path:

```sh
bin/aitex main.tex --cache .aitex/cache
```

On a later identical invocation aiTeX validates the root,
transitive inputs, path resolutions, clock-sensitive state, auxiliary files,
fonts, graphics, and requested output before restoring the PDF. A dependency
change or damaged cache object is a safe miss and runs the native scheduler.

`aitex cache warm` remains a lower-level exact-output operation for projects
whose bibliography/index/glossary products are already current. It converges
TeX-written auxiliary files but deliberately does not own semantic processors;
ordinary users should prefer bare `aitex main.tex`.

Keep `.aitex/cache` between CI jobs when the workspace and TeX installation are
stable. In an ephemeral job that will compile only once, skip snapshot-writing
overhead:

```sh
bin/aitex main.tex --no-cache
```

LaTeX3 initializes minute/date constants in every job, and PGF initializes a
default random seed while loading. The standard `latex.afmt` records provenance
for that narrowly identified LaTeX initialization; aiTeX defers its clock reads
until a memoized constant is actually consumed. Custom formats remain
conservative. PGF's shortcut likewise applies only to reviewed official package
bodies; an unknown version or project-local shadow remains clock-sensitive.
An ordinary document therefore survives minute and date changes; `\today`
expires at midnight, and a document using PGF's default random state retains its
`\time`/`\year` dependency. Arbitrary clock reads, including user `\everyjob`
hooks, remain immediate dependencies. Reproducible CI can pin the complete
TeX-visible clock when desired, as pdfTeX does:

```sh
FORCE_SOURCE_DATE=1 SOURCE_DATE_EPOCH=1788292800 \
  bin/aitex main.tex --cache .aitex/cache
```

## Interactive editing

For repeated editor builds, keep the compiler alive:

```sh
bin/aitex watch main.tex --cache .aitex/cache -o main.pdf
```

The initial build starts immediately. A new build starts automatically when
any observed TeX, bibliography, font, or graphics input changes. Rapid saves
are coalesced, and aiTeX reports an
`AITEX_WATCH ready` frame when the complete artifact is published. A failed
edit keeps the process and previous good PDF alive.

An editor integration that already owns file watching can instead run
`aitex watch --stdin ...`, save all changed project files, and send one newline
per requested TeX pass; `quit` exits that request-driven protocol.

An exact no-op can reuse the proven final artifact. Any edit runs a full pass
from the preloaded format. Class/project/body snapshots remain opt-in while
their engine-observed semantic-boundary work is qualified; uncertainty always
falls back to a full pass.

`fork-watch` remains available as an experimental Linux process wrapper:

```sh
bin/aitex fork-watch main.tex --cache .aitex/cache -o main.pdf
```

It uses more process-tree memory and does not bypass v0.2's semantic-boundary
restriction. Prefer portable `watch` unless measuring the process model itself.

## Measured guideposts

The final 2026-09-02 qualification measured these representative results:

| Workflow | Result |
|---|---:|
| Cold PDF over 143 paired papers | 0.930× stock pdfLaTeX aggregate CPU |
| Preloaded-format PDF fixture | 134.57 ms p95 vs 155.37 ms stock |
| Exact PDF output hit | 35.48 ms p95; 0.15× stock CPU |
| Exact output hits over 56 converged papers | 0.01× stock LaTeX CPU |

These are distinct cache conditions, not interchangeable speed claims. The
full raw samples, host identity, memory results, and correctness oracles are in
[`PERFORMANCE.md`](PERFORMANCE.md).

## Avoiding accidental slow paths

- Use the release binary, not `cargo run` or a debug build.
- Use `aitex pass main.tex` only for tooling that deliberately wants one TeX
  pass; it is not the normal document-build command.
- Reuse one `.afmt`; do not load `latex.ltx` from INITEX on every invocation.
- Reuse one cache directory instead of creating an empty cache for each build.
- Do not use `--cache` for a truly one-shot ephemeral compile: a cache miss
  includes dependency collection, serialization, and publication work.
- Keep the cache and outputs on a local filesystem when latency matters.
- Prefer direct PDF for normal use. DVI remains fully correctness-qualified,
  but its cold compiler CPU is currently slower than stock LaTeX.
- If `TEXMFHOME` points at a large package tree, build its `ls-R` with
  `mktexlsr`, include the tree in `TEXMFDBS`, and use kpathsea's `!!` prefix.
  An unindexed multi-gigabyte tree can turn each lookup into a recursive walk.

For example, a read-only supplemental tree can be configured as:

```sh
mktexlsr /opt/texmf-extra
export TEXMFHOME='!!/opt/texmf-extra'
export TEXMFDBS="/opt/texmf-extra:${TEXMFDBS:-}"
```

The interactive startup display labels an explicitly `!!`-qualified
`TEXMFHOME` as an indexed tree. `AITEX_METRICS=1` reports resolver startup,
query, local-hit, one-shot fallback, PK-generation, and `extractbb` counts and
time when investigating a slow document.

Persistent cache storage is bounded to 1 GiB by default. Watch and incremental
modes intentionally retain verified state and immutable PDF assets in memory
to reduce latency. See [`CACHE.md`](CACHE.md) and
[`CHECKPOINTS.md`](CHECKPOINTS.md) for identity, invalidation, budgets, and
fallback rules.
