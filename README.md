# aiTeX

aiTeX is a TeX and LaTeX compiler written in Rust. It interprets TeX, builds
pages, and writes PDF with embedded fonts, color, hyperlinks, destinations,
outlines, metadata, and vector or raster EPS graphics. Classic TeX material
and experimental native Unicode/OpenType text share one positioned-page
contract consumed directly by the in-process PDF writer. An exact private DVI
is retained in memory as a compatibility and differential oracle, not reparsed
on the normal PDF path. Its engine state can be dumped as a preloaded format,
and an exact converged DVI or PDF can be restored after every input and side
artifact has been revalidated.

aiTeX is an academic project by Benjamin Antieau and Deven Manam.

## Current development status

`v0.3.1` is the initial public source release. The distribution is the tagged
source tree: no native bundle,
wheel, package-registry entry, or hosted compiler service is published. The
stable public surface is the
classic pdfTeX-compatible compiler, native complete-build scheduler, direct
PDF/DVI output, exact cache, watch mode, and conservative lint command. The
validation-gated AI repair workflow is an opt-in preview, and native
Unicode/OpenType typesetting remains experimental. Ordinary build, watch, and
lint operations are deterministic, local, and never contact a model provider.

The bounded repair agent receives the same source-mapped causal diagnostics
shown to humans. Conservative artifact-backed lint checks report unresolved
references, unresolved citations, and duplicate labels. Repair remains a
preview: every proposed edit is bounded and privately build-validated, but
arbitrary documents are not guaranteed to be repaired autonomously. See the
[`CHANGELOG`](CHANGELOG.md), [release boundary](docs/RELEASE.md), and
[roadmap](ROADMAP.md) for the release scope and planned follow-ups.

The v0.3.1 qualification evaluates all 191 locked source roots. aiTeX succeeds
on all 148 papers where stock pdfLaTeX produces a PDF and all 58 where stock
LaTeX produces DVI. All 58 DVI drawing and special streams match exactly, all
144 comparable PDF raster pairs are within the locked tolerance, and aiTeX
introduces no additional invalid Unicode code points. Aggregate CPU is 0.977×
stock on the PDF route and 1.289× stock on the DVI route. Median RSS is below
stock on both paired routes; PDF p95 RSS is 1.730× stock. On an unchanged converged 56-document
workload, exact output-cache hits use 0.01× stock LaTeX CPU and 20.2 MiB median
RSS versus 47.2 MiB. The 10,000-page stress document validates through both
routes. On the v0.3 PDF suite, its in-process two-pass PDF stage takes 226.7 ms
and complete aiTeX wall time is 4.244 s versus 5.265 s for pdfTeX/latexmk. The
release policy makes PDF the primary blocking performance route:
PDF must remain faster than stock and below 2× stock p95 RSS. DVI correctness
remains exact, with a bounded 1.30× cold-CPU ceiling; getting DVI
below stock remains explicit performance debt.

## How aiTeX differs from RaTeX / TeXres

[RaTeX](https://leoliu0.github.io/blog-ratex.html) has been renamed
[TeXres](https://github.com/leoliu0/texres/tree/v0.7.0). Both projects are
modern Rust implementations that accept existing LaTeX documents, provide a
single complete-build command, cache validated work, emit PDF directly, and
replace traditional TeX terminal failures with structured diagnostics. Their
current product boundaries are nevertheless quite different:

| Area | aiTeX 0.3.1 | TeXres 0.7.0 (formerly RaTeX) |
|---|---|---|
| Distribution model | Uses the installed TeX Live packages, classic fonts, and semantic tools | Self-contained executable with embedded formats, packages, fonts, BibTeX, and Biber |
| Stable engine scope | Classic pdfTeX-compatible TeX/LaTeX; native Unicode/OpenType is experimental | pdfLaTeX-, XeLaTeX-, and LuaLaTeX-compatible personalities |
| Output and editor loop | Direct PDF, exact DVI, exact output cache, and watch mode; SyncTeX is not emitted yet | Direct PDF, validated incremental complete builds, and SyncTeX |
| Compatibility strategy | Reuses the host TeX Live ecosystem and qualifies output against stock TeX/LaTeX | Ships a hermetic TeX distribution and compares its bundled engines with TeX Live |
| Diagnostics and assistance | Shared causal diagnostics, explicit artifact-backed lint, and validation-gated `explain`/`fix` preview | Structured source diagnostics with excerpts, expansion/include context, and hints |
| Embedding and platforms | Source-built CLI on Linux and macOS | Linux, macOS, and Windows packages plus C and WebAssembly library interfaces |

Choose aiTeX when integration with an existing TeX Live tree, exact DVI,
conservative lint, or validation-gated repair matters most. Choose TeXres when
a self-contained distribution, XeLaTeX/LuaLaTeX personalities, SyncTeX,
Windows packages, or in-memory C/WebAssembly embedding is the priority.

The projects' published speed numbers are not directly comparable: they use
different versions, corpora, machines, dependency boundaries, and cold/warm
definitions. aiTeX therefore makes no claim to be faster or slower than
TeXres yet. A useful head-to-head result must run both released binaries on
the same licensed inputs and host, distinguish one-pass from complete builds,
and grade output correctness before timing it. TeXres's maintained
[performance methodology](https://github.com/leoliu0/texres/blob/v0.7.0/PERFORMANCE.md)
and aiTeX's [performance contract](docs/PERFORMANCE.md) document the separate
measurements available today.

## Build and use

The repository pins Rust 1.90.0. A TeX Live installation supplies the LaTeX
kernel, packages, TFM metrics, Type 1 fonts, and the `extractbb` graphics-
dimension fallback. Ordinary text, grayscale/RGB JPEG, low-bit grayscale,
indexed/color-key/RGB PNG, common RGBA PNG, simple EPS,
and directly importable PDF graphics stay inside aiTeX. More difficult assets
use host tools on demand: MuPDF normalizes complex PDF pages and is the first
raster fallback; Ghostscript is the final fallback, especially for procedure-
heavy EPS; Netpbm performs low-memory PNG/alpha normalization; and ImageMagick
is its compatibility fallback. Documents that do not need those paths do not
launch those programs. The complete qualification environment requires all
four and also uses Ghostscript, MuPDF, and ImageMagick as independent PDF
validity, text, and raster-comparison oracles.

Image handling is lossless in production. Set `AITEX_IMAGE_TRACE=1` to report
the selected import route, source/retained bytes, reuse, bounded transient-byte
estimate, and asset time for qualification or troubleshooting; there is no
implicit JPEG recompression, downsampling, or color-profile stripping.

```sh
git clone --branch v0.3.1 --depth 1 https://github.com/tertius-ai/aiTeX.git
cd aiTeX
make
bin/aitex --help
bin/aitex document.tex
```

`make` creates `bin/aitex` and prewarms an engine- and TeX-matched LaTeX format
in the local build cache. Source-built executables create the same
format automatically on their first document build, under the platform cache
directory; it is not shipped as a TeX-derived release payload. Run
`aitex format` to prewarm it explicitly.
For a system command available from every directory, build as your normal user
and install with `sudo make install`. For an unprivileged installation, use
`make install-user`; if needed, add `$HOME/.local/bin` to `PATH` once. macOS
with MacTeX and Linux with TeX Live are supported. See
[docs/DISTRIBUTION.md](docs/DISTRIBUTION.md) for source builds and staged local
installs. `make dist-source` verifies a clean source snapshot; native bundle
and wheel targets remain unpublished packaging tools. Interactive terminal
runs use a colored aiTeX wordmark followed by
one compact line per compile phase. The full TeX transcript is still written
to the `.log` file; use `-v`/`--verbose` (or `AITEX_VERBOSE=1`) to also show it
in the terminal. Redirected runs retain the traditional unstyled output for
scripts and differential tools. Use `--no-banner` or `AITEX_NO_BANNER=1` to
suppress the wordmark, and `NO_COLOR=1` to keep the display monochrome.

`aitex document.tex` is the normal complete build (`aitex build document.tex`
is an explicit alias). It defaults to `document.pdf`, owns a bounded fixed-point
loop, and invokes
BibTeX, Biber, MakeIndex, or MakeGlossaries only when their content inputs have
changed. Those semantic processors remain TeX Live dependencies in v0.3; they
are not used to schedule TeX passes. Use `aitex pass document.tex` when exactly
one TeX pass is intentional.

`aitex lint document.tex` performs that same cached complete build, then checks
only evidence observed in its converged artifacts. The initial high-confidence
rules report unresolved references, unresolved citations, and duplicate
labels. Findings are warnings by default; use `--deny warnings` in CI or
`--json` for the versioned `aitex-lint-report-v1` record. Lint is model-free,
works in `make no-ai` builds, and adds no work to ordinary `aitex document.tex`
compilation.

`aitex watch document.tex` runs that same complete build immediately, then
rebuilds when an observed project input changes. The dependency set includes
TeX inputs, fonts and graphics, and semantic-tool inputs such as bibliography
databases; rapid and atomic editor saves are coalesced. A failed rebuild keeps
the service alive and leaves the previous complete PDF in place, while each
successful PDF is published with one same-directory rename. Editors that
already schedule saves can use `aitex watch --stdin document.tex`: this retains
the earlier protocol of one TeX pass per input line and `quit` to stop.

Use `-o document.dvi` to keep DVI instead. PDF output consumes the canonical
`aitex-page` events from the engine's shipout walk directly; it no longer writes
a temporary DVI file or reparses DVI. During this migration the same walk still
builds exact private DVI bytes in memory as an independent oracle. Set
`AITEX_PDF_DIFFERENTIAL=1` to reparse and compare that oracle, or
`AITEX_PDF_DVI_COMPAT=1` to force the former parser route. These are developer
qualification switches and do not alter PDF or cache identity. DVI-only builds
keep their specialized zero-capture path and do not measure every glyph merely
to satisfy the PDF contract. In experimental `--unicode` mode, native shaped
runs occupy the same page stream without passing through DVI. Production PDFs
use deterministic adaptive zlib-rs level-6 stream compression with a bounded
four-worker budget. Small non-stream objects are collected into bounded PDF
1.5 object streams, and the cross-reference data is itself a stream;
deterministic shared resource dictionaries avoid repeating identical page
resources. Developers can set
`AITEX_PDF_COMPRESSION_WORKERS=1` when an outer runner already parallelizes
documents, or `AITEX_PDF_UNCOMPRESSED=1` when readable PDF operators are more
useful than compact output. `AITEX_PDF_CLASSIC_XREF=1` retains compressed
content but emits ordinary objects and a classic cross-reference table for
compatibility investigations. Both choices have distinct output-cache
identities. The process exits nonzero after TeX errors and
reports which auxiliary files changed when another pass is needed. Complete
builds cache by default under `$XDG_CACHE_HOME/aitex` (normally
`~/.cache/aitex`) on Unix/Linux
and `~/Library/Caches/aiTeX` on macOS. An explicit `--cache` overrides that
location and `--no-cache` disables it. The first converged run can publish a
complete-output artifact. A later unchanged run validates every dependency
and side output before atomically restoring the requested DVI or PDF.

aiTeX 0.3.1 deliberately keeps pdfTeX-compatible, byte-faithful input and the
classic TFM/Type 1 font stack as its stable path. An opt-in
native path accepts UTF-8 source, performs per-line bidi and script itemization,
discovers and deterministically falls back through system fonts, shapes with
HarfRust, and embeds static TrueType or CFF1 subsets as PDF Type 0/CID fonts:

```sh
aitex --unicode document.tex
aitex --unicode --font "Noto Serif" --language en document.tex
```

Raw UTF-8 needs no LaTeX package. `--font`, `--language`, `--features`,
`--variations`, and `--direction` select native defaults; `--font-path` adds
project font directories and may be repeated. The equivalent checked-in
configuration is:

```toml
[unicode]
font = "Noto Serif"
font-paths = ["fonts"]
language = "en"
features = "+liga,-kern"
variations = "wght=450"
direction = "auto"
```

The UTF-8 contract covers the root and included `.tex`, `.bbl`, and other TeX
inputs. A legacy `\usepackage[utf8]{inputenc}` declaration is accepted as an
inert compatibility spelling; legacy encodings such as `latin1` are rejected
in `--unicode` mode rather than silently reinterpreting decoded scalars. Use
the classic mode for byte-encoded source trees.

aiTeX finds the nearest `.aitex.toml` between the input and its repository
root. CLI values override scalar project settings; project and CLI font paths
precede `AITEX_FONT_PATH`, then system fonts. Resolved font files and normalized
options participate in cache identity. System-font indexing is lazy: a fully
resolved project-font document never pays for an ambient system scan.
`\aitexnativefont{Family}` selects a
native font for ordinary text; `\aitexfont`, `\aitexlanguage`,
`\aitexfeatures`, `\aitexvariations`, and `\aitexdirection` are group-local
low-level controls, while `\aitextext{...}` remains useful for an explicitly
bounded mixed-direction span.

Common `\setmainfont`, `\setsansfont`, `\setmonofont`, `\fontspec`,
`\newfontfamily`, and language-selection forms have a deliberately bounded
[fontspec](https://ctan.org/pkg/fontspec)/[babel](https://ctan.org/pkg/babel)/
[polyglossia](https://ctan.org/pkg/polyglossia) compatibility layer. It accepts
native feature tags plus a documented small set of familiar feature values and diagnoses
unsupported controls; it does not claim XeTeX/LuaTeX primitive compatibility.
Locked Arabic, Hebrew, Devanagari, CJK, Thai, Lao, variable-font, and ordinary
vertical-run fixtures exercise shaping, line breaking, subsetting, rendering,
and extraction. Concrete variable-font
instances, including CFF2 sources, are materialized as static TrueType subsets
before embedding. Already-shaped vertical runs now have a PDF Identity-V path
with OpenType vertical metrics. Opaque COLRv0/CPAL glyphs are painted as
ordered vector layers using palette zero from the CLI, with logical text kept
in `ActualText`; ordinary monochrome runs retain their compact `TJ` path. The
font boundary also identifies COLRv1, SVG, sbix, CBDT, and the first
layout-critical OpenType MATH constants. Partial alpha, COLRv1 gradients and
compositing, SVG/bitmap glyphs, vertical paragraph composition, a Unicode
math-list builder, and complete language/viewer coverage remain explicit work.
An opt-in library API
can emit a real tagged-PDF structure and parent tree for positioned text; this
foundation is not yet advertised as PDF/UA conformance.

The preview feature set includes a controlled repair lab, a read-only
`aitex explain` command, and validation-gated `aitex fix`. They use the same
versioned diagnostic packet, three bounded context profiles, and zero-token
deterministic routing.
The complete-build scheduler now structures failures from TeX, BibTeX, Biber,
MakeIndex, MakeGlossaries, and nomenclature—including MakeIndex rejection
messages emitted with a successful process status.

AI repair support is included in the default binary, but ordinary
compilation remains deterministic and offline: it never constructs the repair
layer or contacts a provider. Build, explain, or preview a repair with:

```sh
make
bin/aitex explain document.tex
bin/aitex fix document.tex --dry-run
```

For the smallest compiler-only binary, `make no-ai` omits the repair harness,
HTTP client, TLS stack, and provider entirely. The Cargo equivalent is
`cargo build --release -p aitex-cli --no-default-features`.

Local rules run before any model. Bare `aitex fix document.tex` starts a guided
session on a terminal. It first shows the same compact `Read`, cache/prepare,
typeset, render, and semantic-tool phases as an ordinary build, then presents
the diagnosis and repair routes; `--verbose` is not required for this phase
view. The default model route is now a bounded agent: it starts from that
diagnostic, searches and reads only relevant source ranges, makes exact edits,
and uses complete cached builds as feedback. It has no shell or arbitrary file
access. The compact and expanded one-shot routes remain available as cheaper
baselines. Inspect the validated multi-file diff, then accept or decline it.
The real source changes only after the complete private chain produces a
structurally valid requested PDF or DVI.

`fix --dry-run` performs the same bounded chain noninteractively, leaves source
unchanged, and records an acceptance command containing its session ID.
`fix --session ID --apply` replays and revalidates that exact chain without a
second model call, then applies all author-file changes atomically, rebuilds the
real project, and retains originals under `.aitex/backups/`. Every attempted
route—including rejected model output—is recorded under `.aitex/repairs/` with
the diagnostic, bounded prompt envelope, raw response, normalized patch,
digests, token/timing facts, safety result, validation artifact size, and user
decision. Agent sessions additionally retain every bounded tool action,
diagnostic delta, aggregate token/cost facts, and per-file transaction.
Generated products and out-of-project paths are never patch targets.

The complete credential, upload, local-record, and host-tool boundary is in
the [privacy guide](docs/PRIVACY.md).

Use `--json` for versioned JSONL events, or `--offline` to forbid model routes.
Attempt, elapsed-time, estimated-input, output-token, uploaded-source,
changed-line, and deleted-line budgets are configurable; a requested cost cap
refuses providers that cannot supply authoritative cost and stops subsequent
requests once reported spend reaches the cap. A provider-side key limit is the
hard backstop before the first request. Live requests
require explicit source-upload consent. `explain --prompt-only` prints the
compact packet without network access. See
[the repair workflow](docs/AI-REPAIR-LAB.md) and the active
[v1.3 protocol](docs/AI-REPAIR-PROTOCOL-V1.3.md), including exact structured
source edits plus typed expansion, include, generated-artifact, semantic-tool,
and build-pass provenance. Non-local and compound investigation uses the
[agentic repair v1.1 protocol](docs/AI-AGENTIC-REPAIR-V1.md), including a
bounded causal source preview and generated-artifact/spelling evidence. The
[v1.2 protocol](docs/AI-REPAIR-PROTOCOL-V1.2.md),
[v1.1 protocol](docs/AI-REPAIR-PROTOCOL-V1.1.md), and original
[v1 protocol](docs/AI-REPAIR-PROTOCOL-V1.md) remain immutable comparison
evidence.

OpenRouter is the direct multi-model route. It requests zero-data-retention,
denies provider data collection, and reports the served provider, fresh/cache/
reasoning tokens, elapsed time, and authoritative charged cost in the same
compact CLI result:

```sh
bin/aitex explain document.tex \
  --provider openrouter --model MODEL --allow-source-upload
bin/aitex fix document.tex --dry-run \
  --provider openrouter --model MODEL --max-cost-usd 0.10 \
  --allow-source-upload
bin/aitex fix document.tex --dry-run --route ai-agent \
  --provider openrouter --model MODEL --allow-source-upload
```

An existing `OPENROUTER_API_KEY` environment variable wins. Otherwise, only
when an OpenRouter operation is explicitly requested, aiTeX reads
`$XDG_CONFIG_HOME/aitex/env` or `~/.config/aitex/env`. The file may contain
`OPENROUTER_API_KEY=...` or `export OPENROUTER_API_KEY='...'`; it is parsed as
data, never executed as shell code, and group/world-accessible permissions are
rejected on Unix. Ordinary compilation does not inspect credentials.

Add `--openrouter-provider PROVIDER` to pin one provider slug and disable
fallbacks; omit it for OpenRouter's normal routed behavior. OpenRouter does not
currently identify the exact physical serving endpoint in this response path.

For an inexpensive experiment through an already authenticated Codex CLI, use
the cost-sensitive Luna model explicitly:

```sh
bin/aitex explain document.tex --provider codex \
  --model gpt-5.6-luna --allow-source-upload
bin/aitex fix document.tex --dry-run --provider codex \
  --model gpt-5.6-luna --allow-source-upload
```

This route needs `codex` on `PATH` (or `AITEX_CODEX` set to its path), but no
additional aiTeX API-key setting. aiTeX runs each model turn ephemerally with
structured output in an empty read-only workspace; the model never receives
filesystem access to the TeX project. It reports the Codex JSONL usage while
retaining the same local patch checks and complete-build validation.

## Qualification

The quick developer checks are ordinary Cargo commands:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -A warnings -D clippy::correctness
cargo test --workspace --locked
```

The 0.3 PDF/font/language gate uses seven original synthetic or explicitly OFL
workloads; it does not copy, derive from, or require an external publication
source. After `make build format` and building the positioned qualification
example, run `python3 tools/pdf_suite.py --quick` for the development matrix or
the same command without `--quick` for the versioned result. Comparable classic
cases use pdfTeX or `latex` plus dvipdfmx; Poppler supplies extraction, font, and
raster checks, while Ghostscript and available MuPDF, qpdf, and macOS Quartz
readers independently validate the output. The gate records size, time, memory,
determinism, font embedding, annotations, matched classic-xref categories, and
complete or sampled rasters. The focused `tools/pdf_qualification.py` command
retains the 193-page trend line. Malformed-input unit and integration tests
cover the font and PDF boundaries. `python3 tools/pdf_contract_check.py`
provides a fast offline audit of the checked-in full-suite evidence and every
locked fixture digest.

For compiler performance across document shapes, use the versioned
[one-shot and complete-build matrix](docs/COMPILER-BUILD-MATRIX.md). It keeps
single `tex`/`latex`/`pdflatex`/XeLaTeX/LuaLaTeX invocations separate from
ordinary aiTeX and `latexmk` fixed-point builds, with curated, deterministic
10%, and complete 191-root tiers.

The eight original matrix projects are redistributable; the 191 real arXiv
projects are not cleared for redistribution as a collection. The repository
retains only their identifiers, versioned source URLs, response and tree
hashes, and derived qualification evidence.
Their source trees are fetched onto a qualification machine, verified against
the lock, and remain ignored by Git. Public availability or
[arXiv's default nonexclusive distribution license](https://info.arxiv.org/help/bulk_data_s3.html)
is not permission to redistribute a paper.
New checked-in corpus material therefore requires an explicit compatible
license and retained attribution; prefer original deterministic surrogates
when permission is absent or unclear.

Use the deterministic 10% tier for routine development. Before a release, run
the complete `--tier all --oracle visual` matrix on the pinned Ryzen 9 7950X3D
qualification host. Keep latency runs sequential, retain the single four-worker
budget, pin the toolchain and CPU affinity, and compare performance only with a
baseline from the same host and environment. Correctness oracles run outside
the timed interval and must pass before a cell contributes a speed ratio.

```sh
make build format
cargo build --release -p aitex-driver --example pdf_qualification
python3 tools/pdf_suite.py --quick
```

The complete gate verifies the pinned environment and corpora, tests the
workspace, compares TeX syntax, boxes, traces, formats and snapshots, runs
TRIP, exercises packages, encodings, fonts, PDF features, performance and
scale, compares the historical 34-paper regression suite, and checks the
locked 191-root product qualification:

```sh
python3 tools/fetch_locked_corpus.py
python3 tools/texlive_overlay.py
tools/done_check.sh
```

For a clean, versioned release candidate, run:

```sh
# Generate target/qualification/holdout-200-current.results.json and its
# report first; docs/RELEASE.md contains the exact commands.
export AITEX_BUILD_GIT_DESCRIBE=v0.3.1
tools/release_check.sh 0.3.1
```

Success ends with `READY v0.3.1 <commit>`. Run it locally on the pinned
qualification host; public GitHub workflows never execute on that machine. See
[ROADMAP.md](ROADMAP.md) for the active post-v0.1 development plan,
[qualification/README.md](qualification/README.md) for the locked denominator,
[docs/RELEASE.md](docs/RELEASE.md) for the victory evidence and tag procedure,
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the implementation model, and
[docs/FAST-PATH.md](docs/FAST-PATH.md) for the fastest production and edit-loop
commands. [docs/PERFORMANCE.md](docs/PERFORMANCE.md) contains their versioned
measurement evidence.

## Workspace

- `aitex-engine`: the `no_std` interpreter, typesetter, page builder, DVI
  writer, TFM support, and serialized engine state. Its only external direct
  dependencies are `serde` and `postcard`; it also uses the dependency-free
  first-party `aitex-page` vocabulary.
- `aitex-page`: a small `no_std`, dependency-free vocabulary for final page
  geometry, classic glyphs, shaped glyph runs, and logical text clusters.
- `aitex-driver`: a small DVI reader and PDF writer with pure-Rust stream
  compression. The writer consumes shared classic and shaped page events;
  static TrueType and CFF faces become horizontal or vertical PDF Type 0/CID
  fonts without routing shaped text through DVI. Classic text uses fractional
  TFM widths and a separate represented-PDF pen so decimal residual does not
  become a per-glyph positioning operator. Its positioned API also owns the
  first optional tagged-PDF structure-tree adapter. Classic Type 1 faces are
  resolved independently of their sized TeX encodings, so compatible uses
  receive one union subset and share its program and descriptor.
- `aitex-fonts`: the bytes-in/bytes-out font boundary: classic Type 1
  preparation and conservative subroutine closure, OpenType inspection and
  embedding policy, HarfRust shaping, and Skera subsetting. Its public model is
  owned by aiTeX so the engine and PDF representation do not depend on an
  upstream crate's API.
- `aitex-unicode`: the host-side Unicode service. It owns bidi and script
  itemization, UAX #14 line-break opportunities, system-font indexing,
  deterministic fallback, and the adapter from shaped output to native engine
  nodes. Heavy Unicode/font dependencies do not enter the serializable engine.
- `aitex-cli`: filesystem and kpathsea integration plus the `aitex` binary.
- `aitex-core`: shared source-span and diagnostic types.
- `aitex-harness`: the optional diagnostic, deterministic-rule, prompt, and
  provider layer used by the validation-gated repair CLI.

### Open-source typography ecosystem

The modern font path is intentionally built as part of the existing Rust font
ecosystem. [Fontations](https://github.com/googlefonts/fontations) provides
`read-fonts` and `skrifa` for OpenType data and metadata;
[HarfRust](https://github.com/harfbuzz/harfrust) provides shaping; and
[Skera](https://github.com/googlefonts/fontations/tree/main/skera) provides
subsetting. Skrifa's color-paint traversal resolves COLRv0 glyphs into
aiTeX-owned vector layers before Skera retains their outline closure.
[ICU4X](https://github.com/unicode-org/icu4x) supplies Unicode
script properties and UAX #14 segmentation;
[`unicode-bidi`](https://github.com/servo/unicode-bidi) implements the Unicode
bidi algorithm; and [`fontdb`](https://github.com/RazrFalcon/fontdb) indexes
platform fonts while aiTeX retains ownership of fallback policy and coverage
checks. aiTeX aims to contribute fixes, reduced test cases, and useful API work
upstream when production documents expose shared problems. Locked shaping
fixtures use small OFL subsets of [Noto](https://github.com/notofonts/noto-fonts),
[Amiri](https://github.com/aliftype/amiri), and
[Junicode](https://github.com/psb1558/Junicode-font), plus CFF coverage from
[Source Serif](https://github.com/adobe-fonts/source-serif). See
[docs/FONTS.md](docs/FONTS.md) for the boundary and current integration limits.

## Repository guide

- [Active roadmap](ROADMAP.md)
- [Best-performance guide](docs/FAST-PATH.md)
- [Source builds and local packaging checks](docs/DISTRIBUTION.md)
- [Privacy and network behavior](docs/PRIVACY.md)
- [Documentation index](docs/README.md)
- [Tool index](tools/README.md)
- [Test-suite index](tests/README.md)
- [Qualification artifacts](qualification/README.md)
- [Historical plans and investigations](docs/history/README.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Citation metadata](CITATION.cff)

Licensed under MIT or Apache-2.0.
