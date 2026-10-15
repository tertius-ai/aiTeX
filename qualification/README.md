# Qualification environment

This directory contains product qualification manifests and derived evidence,
not third-party source trees. The larger private licensed-corpus and model
evaluation records were removed from the 0.3.1 compiler tree; their historical
state is preserved by the v0.3.0 tag. The archived compiler conclusion is
15,982/15,982 agreements on stock-clean roots, with zero aiTeX failures and
zero output mismatches under the recorded policy.

`pdf-corpus-v1.json` locks the 0.3 PDF/font/language inputs and provenance;
`pdf-suite-v1.json` defines the seven workload profiles and acceptance limits.
The entries are original aiTeX fixtures, deterministic generated assets, or
license-recorded OFL font fixtures. They forbid non-redistributable publication
sources, derived excerpts, and checked-in PDFs. Run
`python3 tools/pdf_suite.py --quick` after a release build for the development
matrix, or pass both `--baseline qualification/pdf-suite-v1.results.json` and
`--output qualification/pdf-suite-v1.results.json` without `--quick` when
refreshing the versioned result. The old file remains readable until the new
result is ready, so it can safely serve as its own baseline. All generated PDFs
remain temporary. The current 0.3.1 candidate result was produced from clean
commit `6ae3c6adfe1291a96b8a87761b736baec0089424`; its SHA-256 is
`a62500c55de1e6b1ecc56646def8911553853bd5745c4b1b8a74b6bdc945b46a`.

The full result passes all seven cases. The five comparable classic cases are
0.790x stock by unweighted geometric mean. Four are smaller than stock; the
graphics matrix is the 1.087x maximum, inside its 1.25x ceiling with a passing
whole-document raster comparison. Classic prose is 411,295 bytes versus
stock's 524,367, with a 65.8 ms two-pass assembly median. The 10,000-page case
is 1,904,568 bytes versus stock's 2,266,980.
`python3 tools/pdf_contract_check.py` cheaply revalidates this evidence, every
locked fixture digest, and the seven-case coverage contract without rebuilding
the host-dependent matrix. This result establishes the baseline for subsequent
same-input, same-environment performance comparisons.
`pdf_qualification.py` and `pdf-synthetic-v1.json` retain the focused
193-page trend line; that clean result remains tied to commit `6eaa547` with
SHA-256
`9780bee54a009d20ccef38d12baf1eac03e795572a2aaabf277fb57693996418`.

`compiler-build-matrix-v1.json` is the source and execution contract for the
systematic TeX-family comparison. It separates exactly one compiler pass from
a clean complete build, and separates DVI, direct PDF, and Unicode/OpenType
routes. The manifest locks eight original projects plus deterministic 10% and
full selections from the 191-root source lock. Results retain raw CPU, wall,
RSS, I/O, pass/tool counts, output bytes, correctness, toolchain identity, and
source taxonomy instead of collapsing unlike workflows into one ratio. See
[`../docs/COMPILER-BUILD-MATRIX.md`](../docs/COMPILER-BUILD-MATRIX.md).

`document-boundary-v1.results.json` is the sequential, counterbalanced v0.3
comparison of the engine-observed `\document` boundary with the immediately
preceding full-pass implementation. It records matching binary/format
identities, raw wall/CPU/RSS samples, the single bounded retained checkpoint,
and exact DVI identity for every pair. See
[`../docs/PERFORMANCE.md`](../docs/PERFORMANCE.md#v03-document-boundary-result).

aiTeX uses two complementary, immutable arXiv sets. They answer different
questions and neither may silently replace the other.

- `corpus.lock.json` pins the historical 34-project regression suite, including
  stock page and glyph counts. It is small enough for frequent exact-DVI runs.
- `holdout-200.sample.json` records a blind sample selected before source
  inspection. `holdout-200.sources.json` pins all downloaded responses and the
  191 automatically identified TeX roots; nine submissions contained no TeX
  source. This is the 0.1 product denominator. Because fixes were made after
  examining failures from this set, it is now a locked regression and release
  qualification corpus rather than an untouched generalization estimate.

`environment.lock.json` pins the classical TeX tools, kernel and package files.
Its two generated-format hashes preserve the original cold-run provenance, but
package-manager dumps contain installation time and are not portable between
otherwise identical machines. `reference-formats.lock.json` therefore pins
fixed-epoch reconstructions of `latex.fmt` and `pdflatex.fmt`. Its canonical
fingerprints require every uncompressed byte to match. A tracked
`pdftexconfig.tex` pins Letter paper and all low-level PDF defaults instead of
inheriting a distribution's A4/Letter choice. Locked exact-DVI and exact-PDF
probes then prove that each format loads and typesets identically. The release
and done-check wrappers build and select those formats for every current
classical LaTeX oracle.
`texlive-overlay.lock.json` pins extra TeX Live packages needed by the blind
holdout without modifying the host installation. An unavailable package or
encoding is recorded as absent; it is never dropped from a denominator by an
empty test loop.
`texlive-overlay.pk.json` pins the 144 fixed-epoch Metafont bitmaps materialized
during the indexed release holdout. Overlay setup regenerates every bitmap in
an isolated PK cache and content-checks it before creating the locale-stable
filename database, so warm developer caches and fresh release hosts see the
same classic-font surface.

`performance-baseline-v1.json` is the machine-qualified P1 edit-loop baseline.
It contains raw samples, p50/p95 summaries, internal compiler phases, machine
and toolchain identity, and a correctness result for every timed workload. See
[`../docs/PERFORMANCE.md`](../docs/PERFORMANCE.md) for its contract and the
numeric v0.2 targets.

`performance-p2.json` is the clean five-sample P2 cache and persistent-service
qualification. It adds class-only, portable watch, and Linux copy-on-write
worker modes, with an output oracle for every timing. See
[`../docs/PERFORMANCE.md`](../docs/PERFORMANCE.md) for the result summary and
the targets that remain open.

`performance-p3.json` is the clean five-sample, 1,200-paragraph comparison of
the P2 portable-watch control with the opt-in P3 body-checkpoint path. It
records no-op and late-paragraph edits through DVI and PDF, including exact
checkpoint/artifact-reuse diagnostics, raw phase metrics, and a stock semantic
oracle for every timing. See
[`../docs/PERFORMANCE.md`](../docs/PERFORMANCE.md) for the result summary.

`repair-lab-luna-2026-09-03.jsonl` is the first live controlled-repair matrix:
six model-route fixtures by three bounded context profiles, using Codex with
the explicitly requested and returned `gpt-5.6-luna` model. It records every
raw answer, any separately audited transport normalization, token usage,
latency, strict patch acceptance, isolated complete-build result, and exact
oracle comparison. Compact context built 5/6 repairs and expanded context 6/6;
diagnostic-only context built none. Its SHA-256 is
`f4ea806297f72b86708c3d66de9265bf8f66060436e0812771626977cc947f46`. See
[`../docs/AI-REPAIR-LAB.md`](../docs/AI-REPAIR-LAB.md).

`repair-pilot-v1.json` is the aggregate, privacy-preserving A0 protocol
qualification. It records the 72-response OpenRouter finalist screen, all 72
blinded semantic labels, the zero-provider-call replay through the final v1
normalizer/compiler, model/provider correctness-cost-speed rows, the compact
profile decision, and hashes of every private source artifact and evaluator
component. Raw prompts, model responses, and per-answer labels remain in the
ignored private campaign tree. See
[`../docs/AI-REPAIR-PROTOCOL-V1.md`](../docs/AI-REPAIR-PROTOCOL-V1.md).

`repair-pilot-v1.1.json` qualifies the additive shared-diagnostic protocol. It
records the 20/20 offline oracle/build gate and a matched 18-attempt v1 versus
v1.1 comparison using the same compact cases, repeats, seeds, frozen catalog,
Mistral Small 24B model, and served DeepInfra route. Both protocols achieved
15/18 complete builds and 15/18 blinded semantic successes; v1.1 added about
55 provider-counted input tokens and $0.000003 per attempt. The record hashes
the private raw campaign, labels, zero-call content-addressed replay, protocol
sources, and evaluator binaries. See
[`../docs/AI-REPAIR-PROTOCOL-V1.1.md`](../docs/AI-REPAIR-PROTOCOL-V1.1.md).

`repair-pilot-v1.2.json` is the first fully accounted active-protocol model
check. It records 36/36 provider-pinned responses over six compact fixtures and
three repeats, complete cost/provider/token/timing facts, and blinded Codex
semantic review. Mistral Small 24B produced 14/18 semantic repairs for
$0.000565 total; Nemotron 3.5 Lightning produced 5/18 for $0.001339. It is a
controlled-fixture pilot, not a representative model benchmark or independent
human evaluation. Raw prompts, responses, patches, and labels remain private;
the aggregate hashes each source artifact.

`performance-v02-review.json` is the post-P3 five-sample review of every
versioned compiler workflow: format startup, cache miss and edit paths, class
snapshots, portable and forked services, long documents, raw LaTeX loading,
and clean or bibliography convergence. Every timing carries its output oracle;
the result records `git_dirty: false` at commit `3c55e20`. It predates the
final host-tool, save-position, and uncached-bookkeeping changes and remains
the path-by-path review; the indexed cold and warm results below are the final
release-binary measurements. Its SHA-256 is
`77ca857c93a93d590f17a87c648a4c434329646fac76863de6970e0b1a468ae7`.

`holdout-200-p3.incremental.json` selects the 56 roots that passed the P2 DVI
gate and applies the P3 admission policy mechanically. All 53 roots with bodies
of at least 8 KiB produce exact incremental-versus-clean DVI after a late inert
edit; three roots are explicitly recorded as small-body ineligible. The runner,
binary, format, sources, and P2-selection identities are stored with every
per-root result.

`holdout-200-v03.results.json` is the archived schema-11 compatibility
qualification immediately after the systematic large-corpus failure/mismatch fixes. All
191 roots are evaluated. Conditional stock-success correctness is 148/148 PDF
and 58/58 DVI; all 58 comparable DVI drawing and special streams are exact;
all 144 raster-comparable PDF projects match; page count and text quality are
148/148. Its 1.11x PDF and 1.34x DVI CPU ratios capture the post-correctness,
pre-performance state and do not pass the release policy. The current v0.3.1
candidate is generated into ignored `target/qualification` outputs: its latest
precommit run retains the same correctness and records 0.860x PDF CPU, 1.297x
DVI CPU, and 1.732x PDF p95 RSS, passing all three resource gates. The archived
result SHA-256 is
`dd28c4f9944046da9d58053d88e32848e7aa98681a2d94b78140150e9d1f1768`.
See [`../docs/HOLDOUT-200.md`](../docs/HOLDOUT-200.md).

`holdout-200-v02.results.json` is the final v0.2 indexed schema-11 cold
qualification. It evaluates all 191 roots with database-only lookup in the
locked TeX Live overlay. Correctness passes completely. Aggregate PDF CPU is
0.930x stock and PDF p95 RSS is 1.778x stock, passing those release thresholds;
aggregate DVI CPU is 1.283x stock, passing the explicit 1.30x v0.2 ceiling
while remaining a below-stock optimization target.
The result SHA-256 is
`2d99d043a4a30e90ef7a660f53622c3ac9b1e29a0838126c80f378dfd40014ab`.
See [`../docs/HOLDOUT-200-V02.md`](../docs/HOLDOUT-200-V02.md).

`holdout-200-v02.warm.json` selects the 56 stock-DVI successes from that exact
cold result and qualifies conservative converged-output reuse. All 56 hits
match stock DVI exactly; aggregate hit CPU is 0.01x stock and median RSS is
20.2 MiB versus 47.2 MiB. The schema-5 run also proves that the disabled
class/project snapshot layers are not published. Its SHA-256 is
`0c34eb5a9bf8cb9f1076602b879f74f09d35cf6ad4c4393b3b85dc5ee1b296b7`. See
[`../docs/HOLDOUT-200-V02-WARM.md`](../docs/HOLDOUT-200-V02-WARM.md).

`holdout-200.warm.json` applies the project-preamble cache experiment to all 56
locked roots whose stock DVI route succeeds. It records exact-DVI checks for
both the initial snapshot-writing pass and the snapshot hit, plus source,
runner, compiler, format, and cold-selection hashes. See
[`../docs/HOLDOUT-200-WARM.md`](../docs/HOLDOUT-200-WARM.md).

`holdout-200-p2.warm.json` is the corrected layered-cache qualification over
the same 56 roots. It checks the layered miss, project hit, and class-only hit
or safe fallback against stock DVI, using an indexed immutable TeX overlay.
See [`../docs/HOLDOUT-200-P2-WARM.md`](../docs/HOLDOUT-200-P2-WARM.md).

`holdout-200.classes.json` is the reproducible P2 class/option census over all
191 roots. It is generated by `tools/class_frequency.py` and summarized in
[`../docs/CLASS-FREQUENCY.md`](../docs/CLASS-FREQUENCY.md).

## Blind holdout artifacts

The holdout artifacts preserve the separation between selection, source
inspection, compilation, and reporting:

- `holdout-200.sample.json`: selected IDs and sampling provenance.
- `holdout-200.sources.json`: immutable URLs, response hashes, roots, and tree
  hashes.
- `holdout-200.results.json`: compiler/environment/runner hashes, every route,
  resource measurements, and final-artifact comparisons.
- `holdout-200-v03.results.json`: archived post-correctness, pre-performance
  qualification after the full failure/output-mismatch pass.
- `target/qualification/holdout-200-current.results.json`: fresh ignored
  release-candidate result, regenerated for the exact candidate commit.
- `holdout-200-v02.results.json`: final v0.2 indexed cold qualification with the
  TeX filename-database hash and schema-11 resource measurements.
- `holdout-200-v02.warm.json`: exact converged-output cache qualification over
  the 56 roots selected by the current cold result.
- `holdout-200.warm.json`: stabilized stock, cache-miss, and snapshot-hit
  measurements for the 56 stock-DVI-eligible roots.
- `holdout-200-p2.warm.json`: indexed-overlay layered miss, project hit,
  class hit, and safe-fallback qualification over those 56 roots.
- `holdout-200-p3.incremental.json`: P3 admission and exact incremental DVI
  qualification over the same 56-root P2 selection.
- `performance-v02-review.json`: final five-sample, all-path v0.2 performance
  review with a correctness oracle on every timed compiler sample.
- `holdout-200.classes.json`: literal class and option frequency for prewarm
  policy.
- `../docs/HOLDOUT-200.md`: generated human-readable report.
- `../docs/HOLDOUT-200-WARM.md`: generated warm-cache report.
- `../docs/HOLDOUT-200-P2-WARM.md`: generated P2 layered-cache report.
- `../docs/HOLDOUT-200-V02.md`: final v0.2 indexed cold report.
- `../docs/HOLDOUT-200-V02-WARM.md`: current converged-output report.

The blocking checker requires all 191 roots to be evaluated. Conditional on
stock success, aiTeX must compile, preserve page count, match positioned DVI
glyph/rule semantics and normalized driver specials, render every PDF page
within the locked tolerance, and introduce no additional replacement, control,
or private-use Unicode code points. A raster pair is non-comparable only when
the stock PDF contains an extractable EPS filename that is also named by the
exact DVI `PSfile` stream: pdfLaTeX's draft-box fallback with shell escape
disabled. aiTeX must render that EPS, not reproduce the placeholder. Raw
extracted-text similarity remains in the report as a diagnostic because
repairing stock's invalid mappings correctly lowers it. The checker also
requires aggregate PDF CPU strictly below stock, aggregate DVI CPU no greater
than 1.30x stock, and PDF p95 RSS no greater than twice stock.
DVI's semantic gate remains exact; only its bounded performance policy differs
from the primary PDF route.

Local release qualification also requires the measured compiler executable's
SHA-256 exactly. A hosted source rebuild may pass
`--allow-rebuilt-binary` because Rust embeds machine-specific absolute source
paths; that option waives only the raw executable hash. The checker requires
the recorded compiler commit to be an ancestor of `HEAD`, rejects any committed
or working-tree difference—including untracked inputs—in the workspace
manifests, toolchain pin, or four compiler crates, and still runs every output,
corpus, resource, and performance gate. Tags are created only after the default
byte-strict local release wrapper also passes.

```sh
python3 tools/holdout_check.py \
  --results target/qualification/holdout-200-current.results.json \
  --binary target/release/aitex
```

The special-stream normalization is deliberately narrow: positions may differ
by one DVI unit, a pop at page-base depth is inert, an adjacent color push/pop
with no intervening semantic event is inert, and a top-level `gray 0` push is
the page's existing default black. Text, order, nontrivial colors, and any pair
enclosing drawing remain strict. `tools/holdout_selftest.py` pins those rules.

## Reproduction

Validate the historical environment and populate its corpus with:

```sh
python3 tools/reference_formats.py
export TEXFORMATS="$PWD/target/qualification/reference-formats:"
python3 tools/qualification_env.py --require-corpus
python3 tools/fetch_locked_corpus.py
python3 tools/fetch_locked_corpus.py --offline
```

Build the isolated package overlay, verify/fetch the blind sources, and produce
a fresh result and report with:

```sh
python3 tools/texlive_overlay.py
python3 tools/arxiv_holdout.py fetch --offline
cargo build --release --locked -p aitex-cli
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
python3 tools/holdout_check.py
```

The fetchers never overwrite a mismatching tree. A partial or stale run cannot
pass the checker: source, environment, overlay, runner, format, and release
binary hashes are part of the result. Only hosted source rebuilds use the
documented executable-hash exception above.
