# Performance benchmark

> **v0.2.0 note:** class/project/body snapshot measurements below are retained
> as experiment history, not enabled release paths. The v0.2 release
> retains preloaded formats, full-pass cold measurements, and exact converged-
> output hits; prefix reuse awaits an engine-observed semantic boundary.

The versioned benchmark measures the workflows aiTeX intends to accelerate,
not just one isolated parser loop. Its command, schema, fixture, raw samples,
and correctness decisions are versioned in the repository.

For user-facing commands and mode selection, see
[`FAST-PATH.md`](FAST-PATH.md). This document is the measurement contract and
evidence record.

## Commands

```sh
# Pure-Python schema/fixture self-test used by ordinary CI.
python3 tools/perf_baseline.py --self-test

# Small real-compiler suite used by release CI.
cargo build --release -p aitex-cli --locked
python3 tools/perf_baseline.py --smoke

# Machine-qualified P1 baseline. Five samples is the repository default.
python3 tools/perf_baseline.py --samples 5 \
  --output qualification/performance-baseline-v1.json

# Machine-qualified P2 cache/service result.
python3 tools/perf_baseline.py --samples 5 \
  --output qualification/performance-p2.json

# Focused P3 long-body control/incremental comparison.
python3 tools/perf_baseline.py --incremental --samples 5 \
  --output qualification/performance-p3.json

# Current indexed cold and converged-output qualifications.
python3 tools/texlive_overlay.py
python3 tools/arxiv_holdout.py run --corpus corpus/holdout-200 --jobs 3 \
  --timeout 120 --memory-limit-mib 2048 \
  --output qualification/holdout-200-v02.results.json
python3 tools/arxiv_holdout.py report \
  --results qualification/holdout-200-v02.results.json \
  --output docs/HOLDOUT-200-V02.md
python3 tools/latex_warm.py \
  --cold qualification/holdout-200-v02.results.json \
  --format corpus/holdout-200/format/fmt.afmt --jobs 3 \
  --output qualification/holdout-200-v02.warm.json \
  --report docs/HOLDOUT-200-V02-WARM.md

# Post-P3 review of every versioned compiler workflow.
python3 tools/perf_baseline.py --samples 5 \
  --output qualification/performance-v02-review.json

# Focused v0.3 document-boundary comparison. Each binary needs its matching
# format; the fallback is the parent implementation before issue #4.
python3 tools/document_boundary_benchmark.py \
  --candidate-binary /path/to/candidate/aitex \
  --candidate-format /path/to/candidate/fmt.afmt \
  --candidate-commit CANDIDATE_COMMIT \
  --fallback-binary /path/to/fallback/aitex \
  --fallback-format /path/to/fallback/fmt.afmt \
  --fallback-commit FALLBACK_COMMIT \
  --output qualification/document-boundary-v1.results.json
```

The runner builds a fresh `.afmt`, stages every project in a temporary
directory, fixes the source date and locale, and excludes staging and output
validation from the timed child process. The release binary, format, fixture,
Git revision, machine, memory, kernel, and tool versions are recorded with the
raw samples.

## Workloads and cache conditions

- `default_build_clean`, `default_build_unchanged`, `default_build_source`,
  `default_build_bibliography`, and `default_build_index` measure the actual
  PDF-first user command, `aitex main.tex`, with its implicit platform cache.
  The stock side runs pdfLaTeX plus content-required BibTeX and MakeIndex steps
  to the same fixed point. Every sample checks PDF text, pages, and raster
  equivalence. The edit cases begin from identical converged projects and then
  change respectively nothing, document text, the `.bib`, or an index term.
- `cold_source` starts INITEX and interprets the LaTeX kernel as source. The
  current compiler supports this comparison through DVI; its final-PDF package
  prelude requires a preloaded LaTeX format and is reported as unsupported.
- `format` starts both implementations from their preloaded LaTeX formats.
- `cache_miss` supplies an empty aiTeX project-preamble cache and includes
  snapshot serialization and writing in the measurement.
- `edit_no_op`, `edit_paragraph`, `edit_included_file`, and `edit_image` begin
  from a valid project-preamble snapshot. `edit_preamble` deliberately changes
  its key and must report a miss.
- `convergence_clean` starts without auxiliary files. `edit_bibliography`
  begins converged, changes the bibliography, runs BibTeX, and continues until
  the auxiliary state stabilizes. The deterministic loop compares directly
  with `latex` or `pdflatex`; when `latexmk` is present, one real frontend
  process is also measured from the identical starting state.
- `long_document_1000` typesets 1,000 sections and 6,000 paragraphs through DVI;
  the separate scale gate retains the 10,000-page DVI and PDF stress cases.
- The current locked-corpus warm run selects all 56 of 191 roots for which
  stock LaTeX produces DVI, stabilizes auxiliary state outside the timed
  region, then measures stock LaTeX, an aiTeX layered/output-cache population,
  its exact converged-output hit, and a class-only fallback control.
- Shared document-class snapshots are measured with the project layer disabled
  and a forced-warm snapshot produced by a separate seed project. This mode
  uses the installed, corpus-representative `scrartcl` class; the ordinary
  `article` load is intentionally below cache admission and is not presented
  as a class-cache win.
- `watch_no_op`, `watch_paragraph`, and `watch_image` keep one compiler process,
  snapshot payload cache, path resolver, and PDF asset caches alive. The timed
  request receives the edit after two warm frames; the benchmark accounts for
  the process tree and checks the resulting artifact against a fresh stock
  build.
- On Linux, `fork_watch_no_op` measures the same request through a
  single-threaded copy-on-write parent. Its framed protocol reports completed
  child CPU, peak RSS, and block I/O, which are added to the live parent's
  counters rather than disappearing when the short-lived child exits.
- The focused P3 suite expands the fixture to 1,200 paragraphs and compares
  ordinary portable watch mode with `AITEX_INCREMENTAL=1` for both a no-op
  frame and an edit in paragraph 1,100. The incremental cases must report a
  body-checkpoint hit; every DVI and PDF artifact still passes the stock
  semantic oracle.

Both DVI and final-PDF routes are measured wherever the workflow is supported.
Stock DVI uses `latex`; stock PDF uses `pdflatex`.

## Metrics and correctness

Schema 1 stores raw samples and linearly interpolated p50/p95 summaries for
wall time, user-plus-system CPU, maximum RSS, and filesystem bytes read and
written. GNU `time -v` supplies process metrics; Linux block-I/O counters are
converted from their specified 512-byte units.

With `AITEX_METRICS=1`, the compiler emits one schema-3 JSON object covering source
read, format or snapshot restore, cache validation/load, setup, TeX execution,
snapshot write, shipout, DVI finalization, and PDF parse/font/image/assembly/
write time. It also records local resolver hits, persistent-kpathsea startup
and queries, one-shot fallbacks, PK generation, and `extractbb` fallbacks with
separate counts and elapsed times. Shipout is nested inside TeX execution; the
PDF subphases partition PDF conversion. External wall time minus the compiler's
internal interval is reported as process startup/exit overhead.

No successful timing exists without an oracle result in the same sample. DVI
uses the locked normalized positioned-drawing and special-stream comparison.
PDF requires valid files, equal page counts, equal normalized extracted text,
and the same raster tolerances as the 191-paper qualification.

## v0.3 document-boundary result

The focused closure fixture runs three complete TeX passes from a clean
project. Its first two passes deliberately change `main.aux` and a body-read
`late.toc`, proving that the preamble checkpoint misses when observed state
changes. Only the third pass may reuse the boundary. A quarter-million-step
original, deterministic preamble calculation makes that saved work measurable
without an extra package dependency while retaining only a small final state.

The counterbalanced result compares the v0.3 candidate with the immediately
preceding full-pass implementation, using a matching binary and format for
each. Persistent caches are disabled, one unmeasured warm-up precedes seven
samples, and execution is sequential with a one-worker budget. Every pair
must converge in three passes and emit byte-identical DVI. The candidate must
report exactly one engine-observed boundary hit; its retained-boundary limit is
one. Raw wall, CPU, RSS, per-pass engine metrics, binary/format hashes, and the
fixture hash are stored in
[`../qualification/document-boundary-v1.results.json`](../qualification/document-boundary-v1.results.json).

The recorded median ratios are machine-qualified evidence for the complete-
build optimization, not a portable speed guarantee. General semantic body/page
reconvergence remains separately bounded by the conservative P3 contract
rather than being implied by this preamble measurement.

### TeX Live resolver review

The 2026-09-02 cold-path review separates kpathsea work from TeX execution.
On the small DVI fixture, the default TeX Live tree took 7.5 ms to start the
persistent helper and 6.0 ms for 72 framed queries. Across five successful
corpus PDFs with the 2 GiB supplemental tree correctly indexed, ordinary jobs
took about 26 ms to initialize the filename databases and 9--16 ms for
117--208 queries. One bitmap-font job needed a second resolver and a one-shot
PK lookup, for 89 ms of resolver work.

The same large tree configured as recursive `TEXMFHOME` without its `ls-R`
database took 1.2--2.2 seconds in query time. Marking it database-only with
`!!` and adding it to `TEXMFDBS` removes that accidental slow path. A native C
probe linked directly to the installed kpathsea library and a batched
`kpsewhich` process both took about 30 ms for 180 indexed lookups: database
initialization, not process IPC, dominates their cold time. aiTeX therefore
keeps the portable persistent helper for v0.2. A future in-process index
must preserve kpathsea precedence and invalidation and beat tens of
milliseconds, not merely replace the executable boundary.

## P3 body-checkpoint qualification

The final clean five-sample focused P3 run was recorded on 2026-08-31 from
commit `a2088a0700efbead945162be7d7d6d86f65dd641`. Its result SHA-256 is
`dc419eb5965887bc78cd25c4f164820b50a6708a56b87fe380e93370d328541c`.
The artifact records `git_dirty: false`. All 40 paired measurements (80 engine
samples) passed their DVI or PDF semantic oracle.

| Route and edit | Mode | p50 wall | p95 wall | p50 CPU | p95 RSS |
|---|---|---:|---:|---:|---:|
| DVI no-op | Project control | 64.85 ms | 66.93 ms | 60 ms | 26.56 MiB |
| DVI no-op | Proven artifact | 4.77 ms | 5.02 ms | 0 ms | 74.90 MiB |
| DVI late paragraph | Project control | 64.99 ms | 65.67 ms | 60 ms | 26.60 MiB |
| DVI late paragraph | Body checkpoint | 16.43 ms | 16.65 ms | 20 ms | 40.04 MiB |
| PDF no-op | Project control | 67.14 ms | 68.86 ms | 60 ms | 29.56 MiB |
| PDF no-op | Proven artifact | 6.47 ms | 6.78 ms | 10 ms | 82.88 MiB |
| PDF late paragraph | Project control | 80.65 ms | 80.99 ms | 80 ms | 35.33 MiB |
| PDF late paragraph | Body checkpoint | 31.94 ms | 32.93 ms | 30 ms | 54.71 MiB |

An exact no-op validates source, dependencies, clock, output path, and all
final/log/auxiliary digests, then reuses the converged artifacts without
reconstructing an engine. Its p95 wall time is about 0.08x the project-snapshot
control for DVI and 0.10x for PDF, meeting the absolute service target by a wide
margin. The reported 0 ms p50 DVI CPU is GNU time's 10 ms measurement
granularity, not a claim that validation is free.

A late paragraph edit resumes the prepared tail checkpoint. Its p95 wall time
is 0.25x the DVI control and 0.41x the PDF control. The actual remaining TeX
body is 6.98 ms DVI and 6.10 ms PDF at p50; applying the prepared checkpoint is
below 0.5 ms p95. Exact unchanged-page streams and immutable font resources
hold the edited PDF phase to 14.98 ms p95.

The speed path deliberately trades bounded resident memory for latency. A
fully warmed no-op host retains compressed checkpoints, up to three decoded
payloads, one prepared engine, DVI prefixes, and PDF caches, reaching 74.90 MiB
DVI and 82.88 MiB PDF at p95. After a changed checkpoint is consumed, the
measured edit frames are 40.04 MiB DVI and 54.71 MiB PDF. The checkpoint/DVI
budget is 64 MiB, complete PDF graphs 64 MiB, and page/font/file/asset layers
are separately bounded; oversized entries fall back instead of entering a
cache.

Against stock on this 1,200-paragraph fixture, late-edit wall time is 0.10x for
DVI and 0.18x for PDF. Both late-edit results pass the absolute 20/35 ms v0.2
service targets. Raw samples and phase metrics are in
[`../qualification/performance-p3.json`](../qualification/performance-p3.json).

The locked-corpus companion gate selects the 56 roots that passed the P2 DVI
qualification. Fifty-three have an eligible body of at least 8 KiB; every one
serves an exact no-op artifact, compiles a late inert edit, and matches a clean
aiTeX DVI rebuild. The other three are mechanically reported as small-body
ineligible rather than silently removed. The result is
[`../qualification/holdout-200-p3.incremental.json`](../qualification/holdout-200-p3.incremental.json),
SHA-256 `31d223e0ed86c31dfd4c1d117aaa77ee7de1064ad2e1a5fd001fc8f531623bba`.

## Current v0.2 corpus qualification

The indexed schema-11 cold run was recorded on 2026-09-02 from compiler commit
`39ee4a502fcd9c84d82e007a21d7213c3c28832e`. Its 191 roots completed without a
timeout or memory-limit abort. Conditional on stock success, aiTeX passes
143/143 PDF builds and 56/56 DVI builds; all 140 comparable PDF raster pairs
pass, and all 56 DVI drawing and driver-special streams are exact. aiTeX also
produces three PDFs where stock's direct-PDF route fails. The result SHA-256 is
`2d99d043a4a30e90ef7a660f53622c3ac9b1e29a0838126c80f378dfd40014ab`.

| Cold route | Paired roots | aiTeX CPU | Stock CPU | CPU ratio | aiTeX median RSS | Stock median RSS |
|---|---:|---:|---:|---:|---:|---:|
| PDF end-to-end | 143 | 268.67 s | 288.99 s | 0.930x | 44.3 MiB | 49.8 MiB |
| DVI compiler | 56 | 81.52 s | 63.52 s | 1.283x | 42.4 MiB | 47.4 MiB |

PDF CPU remains below stock and passes the primary release threshold. DVI's
1.283x ratio passes the bounded 1.30x v0.2 ceiling but remains post-v0.2
optimization debt with a below-stock goal. Memory medians are lower than
stock. PDF p95 is 106.2 MiB versus 59.7 MiB, a passing 1.778x ratio; the
maximum is 147.8 MiB versus 88.3 MiB. The full report is
[`HOLDOUT-200-V02.md`](HOLDOUT-200-V02.md).

The improvement came from four measured changes: dependency manifests are not
built for a true no-cache one-shot; common PNG/JPEG/PDF bounding boxes and
classic single-page PDF imports avoid helper processes; eight-bit RGBA PNGs
are normalized into filtered RGB/alpha zlib streams in-process; and the engine
reuses macro-scanning buffers while keeping ordinary alignment tokens on the
compact scanner path. On the dominant long DVI paper, the last two engine
changes reduced one pass from about 11.0 to 9.15 seconds. Instrumented
allocation counts fell from about 48.6 million to 19.2 million and allocation
traffic from 3.62 GiB to 2.01 GiB on that paper. The final release pass also
removed cache-only clock-provenance checks from uncached compilation and fused
the deferred-write/save-position page scan; cached builds retain the complete
dependency checks.

The companion unchanged-converged run uses a conservative persistent output
artifact. A hit first validates the root source, every transitive input and
resolution, every non-primary side output, and PDF font/asset dependencies;
only then may it atomically restore the requested DVI. All 56 eligible roots
hit and exactly match the fresh stock DVI. The result SHA-256 is
`0c34eb5a9bf8cb9f1076602b879f74f09d35cf6ad4c4393b3b85dc5ee1b296b7`.
No class or project snapshot is published.

| Stabilized pass | Aggregate CPU | Median RSS |
|---|---:|---:|
| Stock LaTeX | 32.06 s | 47.2 MiB |
| aiTeX output-cache population | 41.84 s | 58.9 MiB |
| aiTeX exact output-cache hit | 0.47 s | 20.2 MiB |

The output hit is 0.01x stock aggregate CPU, passing the 0.25x locked-corpus
target. This is an unchanged-build claim, kept separate from the cold
one-shot result and from P3's qualified late-edit checkpoint path. The
generated report is
[`HOLDOUT-200-V02-WARM.md`](HOLDOUT-200-V02-WARM.md).

## 2026-09-25 v0.3 cold-path and RSS review

The v0.3 review profiled cold startup and the high-RSS PDF tail before changing
code. A no-cache pass was hashing the 18 MiB base format even though no cache
key or retained decoded template consumed that identity. Build/watch paths had
already computed the same identity before format restore. The corrected path
now computes an identity only when a cache or retained template needs it and
passes the existing digest into restore. PDF input files now use one shared
`Arc<[u8]>` from the bounded file cache through direct font and graphic
embedding instead of cloning the full bytes at each boundary.

A streaming postcard restore was also implemented and measured, then rejected:
it reduced a tiny cold pass from roughly 46 MiB to 28 MiB RSS but increased
format initialization from about 41 ms to 76 ms. That is not an acceptable
default for a compiler whose primary promise is speed. The retained changes
improve CPU or memory without that tradeoff.

Focused alternating-process A/B checks used the same source, environment, and
engine-matched format. Every DVI/PDF pair was byte-identical:

| Cold workload | Before | After | Result |
|---|---:|---:|---|
| Tiny DVI, format initialization | 40.9 ms median | 27.1 ms median | 0.66x |
| Tiny DVI, peak RSS | 46,416 KiB median | 44,032 KiB median | 0.95x |
| 16.6 MiB image paper, peak RSS | 124,356 KiB | 116,100 KiB | 0.93x |
| 41.4 MiB image paper, peak RSS | 245,260 KiB | 242,580 KiB | 0.99x |

The five-sample correctness-gated matrix then exercised complete builds,
uncached passes, exact-output hits, watch/fork-watch, a 1,000-section document,
raw INITEX loading, and auxiliary convergence. The ratios below are aiTeX over
the corresponding indexed TeX Live frontend; every timed sample passed its
DVI drawing/special or PDF page/text/raster oracle:

| Workload | CPU ratio (unless noted) | RSS ratio | Boundary |
|---|---:|---:|---|
| Format-only DVI / PDF | 1.00x / 1.00x | 1.12x / 1.09x | cold compiler parity |
| Cache-miss DVI / PDF | 1.17x / 1.15x | 1.59x / 1.56x | remains debt |
| Proven no-op DVI / PDF | 0.14x / 0.13x wall | 0.56x / 0.54x | exact output hit |
| 1,000-section DVI | 1.23x | 1.14x | remains execution debt |
| Clean convergence DVI / PDF | 1.14x / 1.15x | 1.60x / 1.55x | remains debt |
| Bibliography edit DVI / PDF | 0.38x / 0.41x | 1.59x / 1.54x | CPU win, RSS debt |
| Raw LaTeX from INITEX | 1.17x | 2.26x | explicit future work |

`*` No-op CPU rounds below GNU time's sample resolution, so that row reports
the stable end-to-end wall ratio instead.

The full artifact is
[`../qualification/performance-v03-review.json`](../qualification/performance-v03-review.json).
It records clean commit `fc0e104503cd2e6dd5a0ad802d3127ab93f45e16`,
binary SHA-256
`7e2f7c394362bd1647b9fd0cf89fd1b3a30b698d30359ad38dbd6a5cc9ed6e42`,
benchmark-format SHA-256
`bfa550ab51779570a94f83fd6107bdbf7e2b287009ab885a2d8154edcc43c745`,
and artifact SHA-256
`82f0ebe75cea5bef30c05f6736d7bf42437b01e3fe6174efb4dc104e55e59e56`.
The benchmark now explicitly passes `--ini` for its raw-kernel control; the
old implicit behavior depended on whether the selected binary happened to sit
beside an installed format.

Profiling also keeps the remaining long-tail work concrete. One generated
TikZ-heavy paper spent 99.7% of its engine time in expansion: about 832 million
expansion steps, 19 million allocations, and 2.1 GB of cumulative allocation
for one DVI pass. That is an execution-model problem, not startup I/O, and is
left visible rather than being conflated with the safe cold-path fixes above.

## 2026-09-01 all-path performance review

The earlier post-P3 five-sample matrix was recorded from clean commit
`3c55e20`. It predates the final host-tool, save-position, and uncached
bookkeeping changes, so it remains a path-by-path performance review rather
than the release binary's cold authority; the indexed results above are the
final release measurements. Its SHA-256 is
`77ca857c93a93d590f17a87c648a4c434329646fac76863de6970e0b1a468ae7`;
every timed engine sample passed its DVI or PDF output oracle.

| Workflow | Route | aiTeX p95 wall | Stock p95 wall | p50 CPU ratio | p95 RSS ratio |
|---|---|---:|---:|---:|---:|
| Format run | DVI | 135.39 ms | 144.17 ms | 0.92x | 1.09x |
| Format run | PDF | 134.57 ms | 155.37 ms | 0.86x | 1.05x |
| Project-cache miss | DVI | 172.53 ms | 143.36 ms | 1.23x | 1.54x |
| Project-cache miss | PDF | 175.80 ms | 157.20 ms | 1.14x | 1.48x |
| Proven no-op output | DVI | 34.05 ms | 143.25 ms | 0.15x | 0.53x |
| Proven no-op output | PDF | 35.48 ms | 150.50 ms | 0.15x | 0.51x |
| Project-snapshot paragraph edit | DVI | 58.42 ms | 143.52 ms | 0.38x | 0.60x |
| Project-snapshot paragraph edit | PDF | 61.80 ms | 149.77 ms | 0.38x | 0.59x |
| 1,000-section full pass | DVI | 428.84 ms | 286.57 ms | 1.11x | 1.11x |
| Raw LaTeX source load | DVI | 4832.75 ms | 4575.84 ms | 1.10x | 2.21x |
| Clean convergence | DVI | 610.21 ms | 460.19 ms | 1.29x | 1.71x |
| Clean convergence | PDF | 653.34 ms | 530.62 ms | 1.20x | 1.64x |
| Bibliography edit convergence | DVI | 162.28 ms | 598.26 ms | 0.22x | 0.61x |
| Bibliography edit convergence | PDF | 172.36 ms | 579.31 ms | 0.23x | 0.60x |

The p95 wall column is intentionally shown even where five samples contain a
host-scheduling outlier; CPU and RSS decide the corresponding resource target.
Format startup is now faster than stock on the deterministic fixture. Exact
no-op reuse passes its latency, CPU, and memory limits. A class snapshot takes
233.34 ms DVI and 242.52 ms PDF at p95 versus 506.26/563.49 ms for aiTeX's own
uncached class baseline. Portable watch no-op and paragraph requests pass at
17.81/19.71 ms DVI and 28.64/28.39 ms PDF; PDF image invalidation also passes
at 29.57 ms, while DVI image invalidation remains above its 20 ms target at
30.13 ms. Fork-watch no-op is 15.22 ms DVI and 19.24 ms PDF at p95.

The remaining costs separate into two groups. Snapshot construction and clean
convergence pay serialization, dependency, and auxiliary-artifact costs and
remain above their CPU/RSS limits. One-shot changed project hits also remain
above their absolute wall/CPU limits despite being substantially faster than
stock; P3's qualified persistent incremental path is a separate faster mode.
Raw kernel loading and the long full pass spend almost all measured time in
TeX execution, consistent with the cold DVI corpus gap and requiring a more
compact token/meaning execution model. The raw samples and internal phase
summaries are in
[`../qualification/performance-v02-review.json`](../qualification/performance-v02-review.json).

## P2 cache and service qualification

The clean five-sample P2 run was recorded on 2026-08-31 from commit
`a8365da2141324a7f875e730c0b171bc3a82b665` on the same benchmark host and
toolchain as P1. Its result SHA-256 is
`cdea8ac9c19df449e3be65569cf832500b64aa01248c65711208d2297b344600`.
Every timed sample passed its DVI or PDF oracle.

| Route and mode | p50 wall | p95 wall | p50 CPU | p95 RSS |
|---|---:|---:|---:|---:|
| DVI format load | 137.70 ms | 138.24 ms | 120 ms | 41.50 MiB |
| DVI project hit, no-op | 57.70 ms | 58.66 ms | 50 ms | 23.01 MiB |
| PDF format load | 138.76 ms | 140.16 ms | 130 ms | 41.20 MiB |
| PDF project hit, no-op | 58.91 ms | 59.79 ms | 50 ms | 23.09 MiB |
| DVI class baseline / hit | 496.29 / 238.13 ms | 504.60 / 240.02 ms | — | — |
| PDF class baseline / hit | 499.28 / 235.68 ms | 812.69 / 396.28 ms | — | — |
| DVI watch, no-op | 18.28 ms | 19.61 ms | 10 ms | 23.80 MiB |
| PDF watch, no-op | 22.14 ms | 22.44 ms | 20 ms | 17.41 MiB |
| DVI fork worker, no-op | 15.40 ms | 15.61 ms | 20.78 ms | 51.38 MiB |
| PDF fork worker, no-op | 17.04 ms | 18.12 ms | 21.57 ms | 51.30 MiB |

Project hits now use about 0.42x the format-run p95 wall time on both routes,
and class hits use about 0.48x their uncached baseline. The portable watch
service meets the P1 limits of 20 ms DVI and 35 ms PDF at p95 for no-op,
paragraph, and image edits; all six hot-service modes remain below 24 MiB RSS.
The cache is bounded to 1 GiB by default. The Linux fork worker is an explicit
latency-for-memory option, not the portable default.

The run also preserves the targets that P2 has not yet reached:

- One-shot project hits remain around 58–63 ms and 50 ms CPU, above the
  40/50 ms p95 and 20 ms CPU gates, although their RSS is below 24 MiB.
- Cache misses meet the 200 ms wall gate but still exceed stock CPU and the
  1.10x stock RSS limit.
- The long-document RSS limit passes narrowly, but CPU remains slower than
  stock (270 ms versus 240 ms at p50).
- Raw kernel startup remains slower and above the 2.00x RSS limit.
- Clean DVI convergence matches stock CPU and clean PDF does not; both routes
  remain above the clean-convergence RSS limit.

These are open optimization targets, not exceptions to the benchmark. The raw
samples and internal phase summaries are in
[`../qualification/performance-p2.json`](../qualification/performance-p2.json).

### Indexed locked-corpus result

The P2 corpus runner extends the historical project-cache experiment with a
forced-admission class-only pass. It also indexes the immutable 2 GiB TeX Live
overlay with kpathsea's `ls-R` database and uses database-only lookup. This
preserves the locked search result while avoiding a recursive 106,000-file
tree walk for every dependency validation.

All 56 stock-DVI-eligible roots pass the exact DVI oracle on the layered miss,
project hit, and class-only path. Fifty-five safely admit and hit a class
snapshot. One `revtex4-2` prefix does not reach the required serializable
zero-depth boundary, so aiTeX declines that snapshot and the exact full
fallback passes.

| Stabilized pass | Aggregate CPU | CPU p50 | CPU p95 | RSS p95 |
|---|---:|---:|---:|---:|
| Stock LaTeX | 33.04 s | 390 ms | 1.175 s | 48.38 MiB |
| aiTeX layered miss | 51.50 s | 585 ms | 1.657 s | 104.08 MiB |
| aiTeX project hit | 32.97 s | 235 ms | 1.115 s | 69.15 MiB |
| aiTeX class hit (55 roots) | 43.87 s | 470 ms | 1.510 s | 80.38 MiB |

The project hit improves median wall time to 0.41x the aiTeX miss, p95 wall
time to 0.67x, and aggregate CPU to 0.64x. Against indexed stock, the project
hit is 1.00x aggregate CPU and 1.43x p95 RSS. Exactness and the 1.50x RSS
target pass; the 0.25x CPU target does not.

The raw result is
[`../qualification/holdout-200-p2.warm.json`](../qualification/holdout-200-p2.warm.json)
with SHA-256
`8e2526f36798422a36b81292da2c7333db4e6b4d3f5b732eaee32459d85ec275`;
its report is [`HOLDOUT-200-P2-WARM.md`](HOLDOUT-200-P2-WARM.md).

## Baseline and v0.2 targets

The five-sample baseline was recorded on 2026-08-30 from clean commit
`14b60d1a85cc73be3b3312281fd52fb821af3b72`, using the AMD Ryzen 9 7950X3D
host, TeX Live 2023, and Latexmk 4.83 identified in the result. The result
SHA-256 is
`88af3a9ea775ff66225de75d98c45c51aa0c351df84d00394f73d50f424f0c86`.

| Workload | aiTeX p50 wall | Wall vs stock | CPU vs stock | RSS vs stock |
|---|---:|---:|---:|---:|
| Preloaded format, DVI | 157 ms | 1.08× | 1.15× | 1.07× |
| Preloaded format, PDF | 162 ms | 1.07× | 1.07× | 1.03× |
| Project cache hit, paragraph edit, DVI | 53 ms | 0.38× | 0.31× | 0.56× |
| Project cache hit, paragraph edit, PDF | 63 ms | 0.43× | 0.36× | 0.57× |
| Project cache miss, PDF | 182 ms | 1.23× | 1.21× | 1.03× |
| Bibliography edit to convergence, PDF | 197 ms | 0.43× | 0.37× | 0.56× |
| 1,000 sections / 6,000 paragraphs, DVI | 304 ms | 1.19× | 1.21× | 1.10× |
| Raw LaTeX kernel from INITEX, DVI | 2.99 s | 1.16× | 1.16× | 2.21× |

The real-frontend comparison includes all child processes. Values below are
p50 CPU for complete auxiliary convergence:

| Workload | Direct stock | aiTeX loop | Latexmk | aiTeX vs Latexmk |
|---|---:|---:|---:|---:|
| Clean DVI | 380 ms | 480 ms | 590 ms | 0.81× |
| Clean PDF | 410 ms | 500 ms | 620 ms | 0.81× |
| Bibliography edit, DVI | 390 ms | 120 ms | 260 ms | 0.46× |
| Bibliography edit, PDF | 410 ms | 150 ms | 270 ms | 0.56× |

The separate locked-corpus result has 56/56 exact cache-miss matches and 56/56
exact snapshot-hit matches. The miss uses 147.89 seconds aggregate CPU
(0.58× stock); the hit uses 77.89 seconds (0.30× stock). Per-paper CPU and RSS
distributions are:

| Stabilized pass | CPU p50 | CPU p95 | RSS p50 | RSS p95 |
|---|---:|---:|---:|---:|
| Stock LaTeX | 4.29 s | 9.49 s | 39.8 MiB | 40.8 MiB |
| aiTeX, snapshot-writing miss | 2.36 s | 5.40 s | 58.6 MiB | 96.7 MiB |
| aiTeX, snapshot hit | 1.10 s | 2.91 s | 41.9 MiB | 73.2 MiB |

That result is in
[`../qualification/holdout-200.warm.json`](../qualification/holdout-200.warm.json)
with SHA-256
`816bd4f41115b92ea61c470529327241904824ca096ff952a55cf9b6384ab80a`;
its concise report is [`HOLDOUT-200-WARM.md`](HOLDOUT-200-WARM.md).

This historical P1 result used the same overlay as an ordinary recursive
`TEXMFHOME`, without a kpathsea filename database. The corrected P2 run shows
that this penalized stock LaTeX much more heavily than aiTeX: indexed stock
aggregate CPU is 33.04 seconds rather than roughly 260 seconds. The P1 artifact
remains valid correctness evidence and historical measurement evidence, but
its 0.30x-stock cache-hit ratio is not a fair compiler-speed claim. The current
indexed rerun above replaces it for cold DVI/PDF speed claims.

The indexed corpus and small edit-loop fixture expose different regimes. The
fixture makes fixed process and snapshot costs visible; substantial papers are
increasingly dominated by body execution, where the long-document marginal
CPU remains slower than stock. The compiler explicitly reaps its kpathsea
helper, so external CPU includes path-resolution work rather than silently
orphaning it. Filesystem-read counters are often zero on this host because the
kernel page cache is warm; they are retained as raw observations, not described
as physical-device traffic.

The complete raw samples and internal phase summaries are in
[`../qualification/performance-baseline-v1.json`](../qualification/performance-baseline-v1.json).

### Numeric v0.2 gate

These targets apply on the locked benchmark host/toolchain, or on a newly
committed baseline when that host is retired:

- Project-snapshot no-op, paragraph, included-file, and image edits: p95 wall
  at most 40 ms for DVI and 50 ms for PDF; p50 CPU at most 20 ms; p95 RSS at
  most 24 MiB.
- Project-cache miss and preamble edit: p95 wall at most 200 ms, CPU no greater
  than stock, and RSS no greater than 1.10× stock.
- Persistent-service no-op or paragraph edit once P2 lands: p95 request
  latency at most 20 ms for DVI and 35 ms for PDF.
- The 1,000-section DVI case: p50 CPU no greater than stock and p50 RSS no
  greater than 1.10× stock.
- Raw LaTeX kernel load: p50 CPU no greater than stock and p50 RSS no greater
  than 2.00× stock.
- Clean auxiliary convergence: aggregate CPU no greater than stock on either
  output route, no greater than Latexmk, and peak RSS no greater than 1.10×
  stock.
- Locked-corpus converged-output hits: 100% exact-DVI equivalence, aggregate
  CPU no greater than 0.25× stock, and p95 RSS no greater than 1.50× stock.

Correctness remains a prerequisite, not a target that speed may trade away.
