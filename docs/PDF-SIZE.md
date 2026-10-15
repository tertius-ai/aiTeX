# PDF size qualification

This document defines the reproducible v0.3 PDF compaction gate and records
the architecture behind it. Release claims come from checked-in synthetic or
clearly redistributable inputs, never from a private working document.

## Benchmark contract

The medium workload is
[`tests/pdf-qualification/synthetic-book.tex`](../tests/pdf-qualification/synthetic-book.tex).
It contains original test prose and uses a small TeX loop to generate a cover
plus 192 distinct content pages. The source exercises repeated page streams,
destinations, outlines, internal and URI links, the standard boxed-link style,
Type 1 families and styles, math, rules, colors, references, and changing page
content. Expanded source and generated PDFs are not stored.

[`qualification/pdf-corpus-v1.json`](../qualification/pdf-corpus-v1.json)
locks this workload and the focused PDF, scale, Unicode, and OFL font fixtures
used by the wider gate. Its policy excludes non-redistributable publication
sources, excerpts or reconstructed derivatives, and generated PDFs.

Run the benchmark after building a release executable and its format:

```sh
make build format
python3 tools/pdf_qualification.py \
  --output qualification/pdf-synthetic-v1.json
```

The distribution gate is the seven-case manifest in
[`qualification/pdf-suite-v1.json`](../qualification/pdf-suite-v1.json). Build
its small native positioned-font producer once, then run either the fast
development matrix or the complete qualification:

```sh
cargo build --release -p aitex-driver --example pdf_qualification
python3 tools/pdf_suite.py --quick
python3 tools/pdf_suite.py \
  --baseline qualification/pdf-suite-v1.results.json \
  --output qualification/pdf-suite-v1.results.json
```

The suite adds classic-font, annotation, raster/vector graphics, 10,000-page,
Unicode CLI, and native positioned-font workloads to the focused prose case.
Classic inputs use pdfTeX when direct-PDF input contracts agree; the graphics
case uses `latex` plus dvipdfmx. Native Unicode output is checked against its
own deterministic, extraction, font, tagging, and rendering oracles rather
than treated as comparable to an eight-bit pdfTeX document.

Each candidate starts in a fresh temporary directory and receives a fixed
source epoch, locale, time zone, two-billion-command engine budgets, four PDF
compression workers, and no output cache. The runner takes three clean samples
from aiTeX and three from `latexmk -pdf`, retains raw wall/CPU/RSS and internal
PDF-stage metrics, and writes the requested machine result. It also makes one
classic-xref diagnostic build from each producer so payload categories can be
compared without object-stream accounting obscuring them. PDFs are deleted
with the temporary worktree. Without `--output`, the result goes under
`target/qualification/`; `--quick` uses eight generated sections, while
`--preview-dir DIR` copies only first, middle, and last page rasters for human
review.

Matching-environment performance checks use both relative and absolute
thresholds. Total PDF time must regress by more than 10% and 40 ms to fail;
PDF assembly must regress by more than 10% and 2 ms. The absolute total-stage
allowance reflects measured run-to-run drift in unchanged font and image
preparation, while the tighter assembly gate protects serializer work directly.
An unchanged Batch 9 control measured 23.7 ms above its own locked graphics
baseline during Batch 10 qualification; the paired Batch 10 binary was 7.7 ms
faster than that control. Subsequent concurrent-load measurements reached a
33.6 ms total-stage delta without a corresponding assembly delta, so the
three-sample absolute guard is 40 ms. The one-sample 10,000-page scale case
uses 50 ms total and 30 ms assembly guards: an exact Batch 9 control drifted by
29.5 ms and 20.4 ms respectively, while its paired Batch 10 measurement was
within 1.5 ms on both measures. RSS likewise requires both a 15% regression
and a 16 MiB increase, widened to 48 MiB for that single-sample scale case;
the unchanged control itself moved by 34.7 MiB while Batch 10 was 6.6 MiB above
the paired control. The relative half of each gate still applies.

## Focused synthetic trend result

The Batch 7 v1 run on Apple arm64 at commit `6eaa547` produced:

| Measure | aiTeX | pdfTeX/latexmk | aiTeX / stock |
|---|---:|---:|---:|
| PDF bytes | 492,186 | 524,361 | 0.939x |
| Median complete wall time | 4.304 s | 1.546 s | 2.78x |
| Median complete CPU time | 4.290 s | 1.520 s | 2.82x |
| Median peak RSS | 204.7 MiB | 63.6 MiB | 3.22x |
| Median aiTeX PDF stage | 124.9 ms | — | — |
| Median aiTeX PDF assembly | 72.7 ms | — | — |

All three clean aiTeX PDFs were byte-identical. Fractional classic font widths,
residual-aware positioning, compact generated numbers, and used-code width
ranges reduced production output from the preceding 584,888-byte result to
492,186 bytes, a further 15.8% reduction. It is 47.7% smaller than the first
940,636-byte v1 result. Median PDF-stage time moved from 145.1 ms to 124.9 ms,
and assembly from 92.4 ms to 72.7 ms. The compact PDF 1.5 layout is 34.5%
smaller than aiTeX's 751,790-byte compressed classic-xref diagnostic form.

The matched classic-xref builds make the remaining difference concrete. aiTeX
is 751,790 bytes versus stock's 884,312: stored font programs are 82,700 versus
181,762 bytes, ToUnicode streams are 4,220 versus 9,583, and non-payload
structure is 335,718 versus 430,263. Page streams remain the one positive delta
at 329,152 versus 262,704 bytes, down from 420,492 bytes in Batch 6. Their
decoded representation fell from 3,710,776 to 1,917,847 bytes.

aiTeX is therefore smaller than stock on the locked medium document. The varied
suite below tests whether that result generalizes. Complete compilation of this
deliberately expansion-heavy loop also remains substantially slower and more
memory-hungry than stock. The PDF stage itself is only 2.9% of aiTeX's complete
wall time, so compiler profiling is the route to the larger end-to-end speed
gap.

## Varied suite result

The 0.3.1 candidate run from clean commit
`6ae3c6adfe1291a96b8a87761b736baec0089424` produced the following result. Bytes
are deterministic production PDFs; time and RSS are medians of three clean
samples except that the 10,000-page and positioned-font cases use one measured
sample plus a separately recorded deterministic rebuild.

This result establishes the post-image/font-work baseline. Future refreshes
use the checked-in file as `--baseline`; unchanged inputs on the same measured
environment then enforce the relative-plus-absolute PDF-stage, assembly, RSS,
and two-percent output-size regression limits in the manifest.

| Workload | Pages | aiTeX bytes | Stock bytes | Ratio | aiTeX wall | PDF stage | Peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|
| Classic prose | 193 | 411,295 | 524,367 | 0.784x | 1.520 s | 96.6 ms | 149.4 MiB |
| Classic fonts | 12 | 142,868 | 302,854 | 0.472x | 0.233 s | 19.8 ms | 84.2 MiB |
| Annotations | 64 | 87,629 | 96,178 | 0.911x | 0.462 s | 15.1 ms | 85.1 MiB |
| Raster/vector graphics | 12 | 542,313 | 498,987 | 1.087x | 0.273 s | 19.9 ms | 83.9 MiB |
| High page count | 10,000 | 1,904,568 | 2,266,980 | 0.840x | 3.976 s | 150.1 ms | 154.7 MiB |
| Native Unicode CLI | 1 | 90,277 | — | — | 0.200 s | 18.1 ms | 86.5 MiB |
| Native positioned fonts | 3 | 68,076 | — | — | 0.007 s | 4.2 ms | 6.5 MiB |

All five meaningful stock comparisons are below the per-case 1.25x ceiling.
Their unweighted geometric-mean ratio is 0.790x, and the maximum is 1.087x.
The graphics result retains six native image objects and one shared vector
form; its modest positive byte delta is accepted only because the complete
12-page Poppler comparison remains within the locked tolerance.

Every case records complete object/stream/font accounting from a classic-xref
diagnostic representation, clean byte determinism, logical extraction, font
embedding, Ghostscript validation, available independent-reader checks, and
Poppler rendering. Moderate comparable cases render every page. The scale case
renders pages 1, 5,000, and 10,000 while Ghostscript traverses the entire PDF.
The annotation workload uses a narrowly documented 0.004 normalized-MAE limit
because its deliberately dense link borders amplify harmless producer-specific
rectangle rasterization; this is still five times tighter than the 0.02 product
corpus tolerance, and link counts, targets, familiar red/cyan boxes, and
extraction are checked separately.

Cold profiling of the preceding 10,000-page result put 95.7% of samples in the
linear scan used by the exact page cache. Batch 9 replaces that scan with a
hash-indexed exact-byte lookup and shared-key FIFO eviction under the same
32 MiB bound. It also emits a balanced page tree with at most 256 children per
node instead of putting every page under one root. Assembly fell from 3,737.3
ms to 182.4 ms (20.5x), total PDF-stage time from 3,765.0 ms to 203.9 ms, and
complete wall time from 7.985 s to 3.820 s; the matched stock run took 4.836 s.
The production PDF is also 3.2% smaller than the preceding result and 7.1%
smaller than stock. Independent traversal found 41 `/Pages` nodes, 10,000
leaves in order, no node over the fanout bound, and no parent or count errors.

The native rows deliberately have no synthetic stock ratio: variable TrueType,
CFF1, instantiated CFF2, bidi scripts, vertical CJK, COLRv0, and tagged
positioned text are qualified through the native contract.

Batch 10 keeps a classic PDF text object open across adjacent glyph runs,
reuses exact font state, and uses a relative `Td` only when its encoded decimal
move reconstructs the already encoded absolute position. Rules, graphics,
specials, and transformations remain hard text boundaries. Exact redundant
standard-color pushes and pops no longer emit operators, while raw PDF literals
and unmatched graphics-state boundaries conservatively invalidate tracked
color state. The stream-format cache tag changed, so no older serialized page
or output graph can be restored under the new representation.

On classic prose this reduces decoded page content from 1,917,847 to 1,479,483
bytes and stored page content from 329,152 to 279,369 bytes. The production PDF
falls from 490,123 to 440,279 bytes (10.2%) and is 16.0% smaller than stock.
In the readable stream, `BT`/`ET` pairs and absolute `Tm` operators fall from
18,249 each to 1,347 each; 16,902 relative `Td` moves preserve the run
boundaries, and color operators fall from 1,544 to 386. Clean prose assembly is
64.8 ms versus Batch 9's locked 68.4 ms, with total PDF time essentially flat
at 116.2 versus 117.1 ms. A same-session paired control also measured Batch 10
assembly at 65.8 ms versus 74.6 ms for the exact Batch 9 binary.

## Classic face-unification result

The Batch 11 candidate groups Type 1 uses by complete physical-face identity
and prepares one union closure. Against the checked Batch 10 result on the
same Apple arm64 host, the full 12-page classic-font case drops from 19 to 17
embedded programs, 123,749 to 111,915 stored program bytes, and 157,398 to
145,184 production PDF bytes (7.8%). The 193-page prose case drops from 12 to
11 programs and 440,279 to 433,874 bytes (1.5%).

Three clean samples remain inside the existing no-material-regression gate:
classic-font PDF time is 64.0 ms versus 58.8 ms and assembly is 9.3 ms versus
9.0 ms; prose PDF time is 125.7 ms versus 116.2 ms and assembly is 65.7 ms
versus 64.8 ms. Peak RSS is 113.7 MiB versus 108.9 MiB for classic fonts and
211.4 MiB versus 206.1 MiB for prose. The complete two-case qualification
passes determinism, extraction, font embedding, Ghostscript, Quartz, and PDF
size/RSS/time gates. The focused face-sharing fixture additionally matches
pdfLaTeX's raster at normalized MAE 0.00000001 with no pixels beyond 5% gray.

## Correctness gates

A size or speed result is accepted only when all of these hold:

- repeated clean aiTeX outputs are byte-identical;
- Poppler reports the intended page count and all fonts embedded and subset;
- extracted section markers are complete and remain in logical order, including
  contextual scripts represented through `/ActualText` where a glyph-level
  `/ToUnicode` map would be ambiguous;
- aiTeX and pdfTeX expose the same URI annotation inventory;
- the readable classic-xref representation contains the expected red internal
  link boxes and cyan URI boxes;
- comparable moderate cases render every page with Poppler at 72 dpi; the
  default normalized-MAE limit is 0.0015 with at most 0.5% of pixels outside
  5% grayscale fuzz, while the documented annotation profile uses 0.004 and
  1%; the scale profile compares its first, middle, and last pages;
- Ghostscript accepts both outputs, and Quartz, MuPDF, or qpdf are recorded and
  enforced whenever available.

The synthetic gate is intentionally stricter than the heterogeneous product
corpus raster tolerance. The source contains familiar math operators, so the
result also records whether extraction maps sum, square-root, and integral
symbols rather than hiding semantic improvements behind raw byte equality.
The v1 run maps all three through aiTeX; stock pdfTeX's extracted text maps none
of them, so raw extracted text is deliberately not required to be byte-equal.

## 0.3 qualification closure and scope

The checked-in result closes the 0.3 foundation against this matrix:

| Required surface | Admitted cases and evidence |
|---|---|
| Classic Type 1 prose, math, repeated/transformed faces | `classic-prose`, `classic-fonts`; stock size and raster comparisons, exact DVI gates, font and stream accounting |
| Links, color, rules, rotations, vector/raster imports | `annotations`, `graphics`; annotation inventories and boxed-link checks, image/form accounting, whole-case stock rasters |
| Many pages and objects | `high-page-count`; 10,000 pages, whole-file Ghostscript traversal, first/middle/last Poppler rasters |
| Latin, Arabic, Hebrew, Devanagari, CJK, Thai, Lao | `native-unicode-cli`, `native-positioned-fonts`; logical extraction markers, embedded font inventory, deterministic Poppler and Quartz renders |
| Static TrueType, CFF1, variable TrueType, CFF2 | locked OFL fixtures plus `native-positioned-fonts`; static or instantiated subset embedding and extraction |
| Horizontal, bidi, shaped vertical output | both native cases; Identity-H and Identity-V font inventories and direction-specific extraction tests |
| Tagged positioned-text foundation | `native-positioned-fonts`; three-page tagged structure, languages, roles, MCIDs, parent tree, and `MarkInfo` |
| Malformed and bounded-resource hardening | unit/integration tests, explicit parser bounds, and full-output validator passes |

`python3 tools/pdf_contract_check.py` is the fast, host-independent audit. It
hashes every admitted source and font, rejects private-document paths, verifies
that all seven rows and five meaningful stock comparisons remain present, and
checks the recorded measurement and correctness fields. The full
`tools/pdf_suite.py` run remains the authority that creates those fields.

Two boundaries are explicit rather than implicit omissions. First, the native
positioned producer is a single public writer call, so its measured
`pdf_total_ms` is the complete font-preparation-and-assembly boundary; its
separate `pdf_assembly_ms` field is recorded as unavailable. The replacement
acceptance criterion is that total, CPU, RSS, deterministic bytes, object/font
breakdown, extraction, tagging, and independent readers all pass. The ordinary
CLI cases continue to report font, assembly, and total PDF subphases
separately. Second, MuPDF and qpdf are enforced when installed and recorded as
unavailable otherwise; Ghostscript and Poppler are mandatory, and Quartz is
enforced on macOS.

This result does not claim vertical paragraph composition, broader script or
viewer coverage, Unicode/OpenType math construction, PDF/UA, PDF/A, PDF/X, or
unsupported color-font formats. Those remain explicitly rescoped, ordered
follow-on work. Malformed inputs discovered during development are retained as
small reviewed regression cases when their redistribution terms permit it.

## Stream compression

Production output uses deterministic adaptive zlib-rs level-6 compression.
The serializer compares complete encoded and plain representations and keeps a
stream plain when compression would make its object larger. JPEG, filtered PNG,
and imported filtered streams remain opaque. Independent page streams are
compressed in bounded batches.

Four workers are the default and maximum, or fewer when the machine exposes
fewer CPUs. `AITEX_PDF_COMPRESSION_WORKERS=N` assigns a smaller per-document
budget; an outer runner compiling four documents at once should normally assign
one worker to each. Within one process, a FIFO permit pool grants a document's
requested group atomically, avoiding accidental processor subdivision between
simultaneous documents. Worker count is an execution choice and never changes
serialized bytes or cache identity.

`AITEX_PDF_UNCOMPRESSED=1` selects the readable diagnostic representation. The
stream-format tag is part of persistent and in-process cache keys, so changing
the representation cannot restore stale output.

## Font and object compaction

The classic path subsets conventional Type 1 PFB programs deterministically.
It preserves `.notdef`, private-dictionary machinery, hinting, encodings, and
statically readable `seac` components. A bounded abstract interpreter follows
direct and conventional Adobe dynamic subroutine calls transitively. It keeps
the original subroutine indices, retains the small standard flex helpers 0–4,
and removes only definitions proved unreachable. Unknown dynamic dispatch
keeps the complete subroutine table; an unsafe glyph-table rewrite keeps the
complete prepared font. Exact resource dictionaries and ToUnicode maps are
interned.

Classic resolution has two explicit layers. A physical-face identity contains
the source digest, PostScript name, exact slant/extension bits, embedding
policy, and preparation version. A TeX use retains its encoding, ToUnicode
map, TFM widths, point size, and exact DVI advance. The writer unions glyph
names across compatible uses before preparing Type 1 once, then shares the
program and descriptor while interning use dictionaries only when all of
their observable semantics agree. Prepared successes and failures live in a
bounded 32 MiB/128-entry cache; PK remains device- and size-specific.

`tools/pdf_size.py` reports classic physical faces, embedded programs,
descriptors, encodings, and PDF font dictionaries separately. The
`classic-face-sharing.tex` fixture selects Latin Modern Roman at two sizes and
through two encodings, and requires one face/program/descriptor but two PDF
encodings and three use dictionaries (the scaled-point width rounding differs
between sizes), matching extracted words against pdfLaTeX, and Ghostscript
acceptance.

Classic `/Widths` entries are deterministic fractional values derived from the
exact TFM advance and are limited to the used code range. The page writer keeps
the exact DVI pen and the represented PDF pen separately: adjacent glyphs use
one string, while decimal residual is absorbed into the next genuine space or
kern. It never hides real movement behind a tolerance threshold. Generated
classic text matrices, font sizes, colors, rules, and `TJ` operands use compact
decimal spelling; raw package/user PDF literals remain opaque.

Production PDFs leave content and other streams direct, pack eligible small
non-stream objects into deterministic groups of at most 100 objects or 64 KiB,
and emit a PDF 1.5 cross-reference stream. Page dictionaries name only the
resources they use. `AITEX_PDF_CLASSIC_XREF=1` retains compressed content while
emitting ordinary objects and a classic xref table for inspection by
`tools/pdf_size.py`. Object layout, Type 1 policy, and page-operator format each
have distinct cache identities.

The native OpenType path embeds shaped static TrueType and CFF1 subsets as
Type 0/CID fonts. Variable TrueType and CFF2 faces are first materialized as
static TrueType instances. Logical clusters drive ToUnicode and ActualText;
visual glyph order is never reused as reading order.

## Direct positioned-page handoff

Normal PDF builds do not write, reread, or parse a temporary DVI. The engine's
shipout walk emits the shared `aitex-page` event document directly to the PDF
driver. Exact private DVI remains in memory as an independent migration oracle;
`AITEX_PDF_DIFFERENTIAL=1` reparses and compares it, while
`AITEX_PDF_DVI_COMPAT=1` forces the compatibility route.

On the original 10,000-page scale fixture, five alternating direct and
compatibility samples measured a 26.4 ms median DVI parse and a 7.2 ms event-
capture cost, removing about 19 ms of deterministic work before compression.
Median PDF-stage time was 3411.9 ms direct and 3434.6 ms compatible, with
effectively unchanged 164.9 MB peak RSS. All ten PDFs were byte-identical.
Those measurements predate Batch 9's indexed page cache; they remain the
isolated evidence for the handoff itself, not the current PDF-stage baseline.
DVI-only builds do not construct the event document and retain their dedicated
zero-capture path.

## Remaining size work

The varied suite is now the release distribution and the focused medium book
remains its stable trend case. Matched classic-xref accounting still identifies
page content as the only positive prose delta, but production aiTeX is smaller
overall because its fonts, ToUnicode maps, and non-payload structure are
smaller. The high-page result now proves that assembly remains bounded at
10,000 pages rather than extrapolating from that prose generator.

The next optimizations should therefore be measured on this suite:
format-specific image handling, shared classic font-face identities, and
remaining graphics-state opportunities. Other follow-ons include compact
number spelling for native positioned text and cross-document scheduling
measurements. Each change must retain the determinism, extraction, annotation,
raster, validator, memory, and cache gates above.
