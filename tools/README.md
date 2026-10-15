# Tool guide

Repository scripts stay in one directory because release gates call each other
by stable paths. They are grouped here by purpose; moving them into nested
directories would currently create more CI and shell-path churn than clarity.

All validation scripts should use `mktemp` or an explicit output directory and
must leave the repository root clean. `repo_check.sh` enforces that contract.
Compiler oracles use `aitex pass` when they intend to compare exactly one TeX
pass; bare `aitex main.tex` is reserved for complete-build tests and benchmarks.

## Release and repository gates

- `done_check.sh` — complete compiler correctness/performance gate.
- `release_check.sh` — clean, versioned release wrapper and evidence hashes.
- `package_release.py` — reproducible source and native prefix archives with
  normalized metadata, internal payload hashes, and notices.
- `bundle_check.py` — safe structural extraction audit plus optional clean PDF
  smoke build against the host TeX Live/MacTeX tree.
- `sbom.py` — deterministic SPDX 2.3 workspace and transitive dependency
  inventory generated from locked Cargo metadata.
- `ecosystem_sweep.py` — manifest-driven umbrella over generated scanner,
  historical syntax, LaTeX layout, package, PDF, and locked-holdout
  differentials. `--pr` is the curated fast gate; `--full` runs every layer
  and writes hashed, nonzero-denominator evidence under `target/qualification/`.
- `scanner_matrix.py` — mechanically generates the token-identity versus
  resolved-command scanner combinations implicated by issues #26 and #32 and
  compares every probe with TeX.
- `reference_formats.py` — reproduce canonically locked stock LaTeX formats
  without package-install timestamps and verify their DVI/PDF probes.
- `repo_check.sh` — generated-artifact and Git cleanliness check, plus the
  hosted-CI and source-only-publication workflow policy.
- `docs_check.py` — tracked Markdown relative-link invariant.
- `check_zero_deps.py` — engine dependency-policy invariant.

## TeX semantic differential checks

- `battery.sh`, `typeset_check.sh`, `plain_check.sh`, `primitives.sh`
- `showbox_check.sh`, `trace_check.sh`, `error_check.sh`, `warn_check.sh`
- `trip.sh`, `trip_lists.sh`
- `dvi_semantic.sh`, `dvi_semantic.awk`, `dvidiff.sh`

## LaTeX, PDF, fonts, and scale

- `latex_check.sh`, `packages.sh`, `mkfmt.sh`, `mkfmt.py` — LaTeX checks plus
  compatibility wrappers around the native `aitex format` command.
- `wheel_check.py` — verifies native wheel tags, executable permissions,
  and third-party notices.
- `pdf_check.sh`, `encoding_check.sh`, `font_matrix_check.sh`,
  `unicode_check.sh` — the PDF and Unicode checks also compare direct page
  events against the independent DVI oracle; the Unicode check locks exact
  direct/compatibility PDF identity.
- `pdf_size.py` — deterministic object/stream, page-content, font-program,
  ToUnicode, Type 3, form, and per-image color-space/bit-depth/filter/mask byte
  accounting for classic-xref PDFs,
  including distinct face/program/descriptor/encoding/PDF-font counts.
  Normalize object-stream PDFs with `mutool clean` before comparing them.
- `pdf_qualification.py` — builds the locked original synthetic book with
  aiTeX and pdfTeX, records size/time/RSS and PDF-stage metrics, checks clean
  byte determinism, fonts, text, boxed links, independent validators, and
  whole-document Poppler rasters. One classic-xref diagnostic build from each
  producer supplies a matched object/stream category comparison. Generated
  PDFs live only in a temporary directory; `--preview-dir` exports three raster
  images for visual review.
- `pdf_suite.py` — runs the seven-case varied PDF qualification: classic prose,
  classic fonts, annotations, generated raster/vector graphics, 10,000-page
  scale, the Unicode CLI, and the native positioned-font writer. It enforces a
  per-case 1.25x stock size ceiling where comparison is meaningful and records
  determinism, extraction, fonts, object categories, timing, RSS, validators,
  and complete or sampled Poppler raster checks. `--quick` is the development
  gate; full results remain temporary unless `--output` names a tracked result.
  When refreshing that result, also pass its existing path as `--baseline` to
  enforce same-input, same-environment performance regression limits.
- `pdf_contract_check.py` — fast offline audit of the checked-in varied-suite
  evidence. It rehashes every redistributable fixture and verifies coverage,
  measurements, determinism, extraction, font status, validators, rasters, and
  the five-case 1.25x stock size ceiling without compiling documents.
- `pdf_fixture_assets.py` — standard-library-only deterministic generator for
  the RGB, alpha, grayscale, indexed, low-bit, and duplicate-content PNG
  inputs consumed by the graphics workload.
- `scale_check.sh`
- `incremental_check.py` — P3 no-op, reconvergence, body/include edit,
  changed-output repair, and preamble invalidation gate against clean DVI/PDF.
- `incremental_adversarial.py` — hostile state/invalidation suite covering
  output routines, cross-file definitions, auxiliary protocols, generated
  files, graphics, randomness, shell-escape refusal, and huge paragraphs.
- `output_cache_check.py` — persistent converged DVI/PDF restoration and exact
  source, auxiliary, custom-output, transcript, and driver invalidation gate.

## Performance and profiling

- `bench.py`, `bench_expand.sh`, `bench_typeset.sh`
- `perf_baseline.py` — versioned P1/P2 benchmark, CI smoke fixture, and focused
  P3 long-body control/incremental comparison.
- `document_boundary_benchmark.py` — counterbalanced comparison of v0.3's
  engine-observed document-boundary reuse with its full-pass parent binary;
  every sample performs three complete-build passes and requires exact DVI.
- `compiler_build_matrix.py` — manifest-locked, sequential and counterbalanced
  comparison of one compiler pass versus a complete clean build across
  aiTeX, TeX/pdfTeX, latexmk, XeTeX, and LuaTeX. It combines original curated
  fixtures with deterministic 10% and complete 191-root corpus tiers and keeps
  correctness, source taxonomy, time, memory, pass count, and output size in
  every result.
- `perf_check.sh`, `cpu_time.py`
- `latex_warm.py` — locked-corpus exact-output-cache population and hit
  qualification with exact-DVI validation and a guard against publishing the
  disabled class/project snapshot layers.
- `incremental_corpus.py` — mechanically select all P3-eligible locked P2 DVI
  roots and compare an incremental edit with a clean exact-DVI rebuild.
- `class_frequency.py` — reproducible locked-corpus class/option census and
  P2 prewarm-order artifact.

## Locked corpus and qualification

- `fetch_locked_corpus.py`, `corpus_lock.py`, `corpus_check.sh`
- `../qualification/pdf-corpus-v1.json` — provenance and hashes for the
  synthetic PDF/font/language qualification inputs.
- `qualification_env.py`, `texlive_overlay.py` — validate the host and build
  the fixed-epoch PK inventory and locale-stable, locked TeX filename database.
  `texlive_overlay.py --refresh-pk-lock` is the explicit maintenance path for
  rewriting bitmap hashes from fresh isolated-cache reproductions; normal runs
  regenerate and reject divergence without modifying the lock.
- `arxiv_holdout.py`, `holdout_check.py`, `holdout_selftest.py` — run and grade
  the 191-root product denominator. `holdout_check.py` is byte-strict by
  default; its rebuilt-binary exception is reserved for the hosted workflow.
- `paper.sh`, `classify_corpus.sh`

## Corpus and AI development tools

- `fetch_arxiv.py`, `make_arxiv_specs.py`, `build_local_corpus.py`
- `mutate.py`, `verify_mutants.py`, `finalize_corpus.py`
- `dump_logs.py`, `dump_sources.py`
- `export_repairs.py` — explicitly export private `.aitex/repairs/` session
  records as stable attempt-level JSONL; full-fidelity by default, with a
  `--metadata-only` privacy mode.
- `model_campaign.py` — create frozen, resumable OpenRouter campaigns with
  request-level reservations, native token/cost/timing facts, semantic review,
  confidence intervals, and correctness/cost/speed Pareto reports; an explicit
  fully reviewed qualification export contains aggregates and private-artifact
  hashes, never raw prompts or source. Raw runs default to ignored
  `.aitex/benchmarks/` directories.

## Debugging, comparison, and reduction

- `difftrace.sh`, `logdiff.sh`, `ltx_coverage.sh`
- `dvi_reduce.py`, `parreduce.sh`

The active compiler and repair-product priorities are in
[`ROADMAP.md`](../ROADMAP.md). Large-corpus research tooling and data live in a
separate workspace.
