# aiTeX roadmap

This roadmap describes the public compiler repository after the initial v0.3.1
source release. Detailed research campaigns and private licensed corpora live
outside this tree.

## Product direction

aiTeX is a fast TeX and LaTeX compiler that works with an installed TeX Live
tree. It aims to make the common PDF build path fast, deterministic, and
pleasant without asking users to abandon existing document classes, packages,
fonts, bibliography databases, or build artifacts.

The project has three deliberately different support levels:

| Level | Surface |
|---|---|
| Stable | Classic pdfTeX-compatible compilation, direct PDF and exact DVI, native fixed-point complete builds, exact output cache, watch mode, compact diagnostics, and conservative lint |
| Preview | Explicit, budgeted, validation-gated AI explanation and repair |
| Experimental | Native Unicode/OpenType shaping, extended font and language compatibility, color/vertical typography, and tagged-PDF foundations |

Ordinary build, watch, pass, and lint commands are local and do not contact a
model provider. TeX Live remains the package, classic-font, and semantic-tool
host for the source-built compiler.

## Current release: v0.3.1

Version 0.3.1 is the initial public source release. It establishes a clean
product boundary, source installation, documentation, hosted CI, and exact
release qualification.

Release gates:

- [x] Separate large-corpus research machinery and private data from the public
  compiler repository.
- [x] State the stable, preview, and experimental boundaries consistently.
- [x] Keep the small original repair lab and its offline product checks.
- [x] Pass the full workspace, differential, PDF, font, cache, scale, and
  191-project release qualification on the exact candidate commit.
- [x] Run the complete visual compiler/build matrix on the pinned Ryzen host.
- [x] Pass clean-checkout source builds on Linux and macOS.
- [x] Review license notices, dependency policy, SBOM, provider privacy, secret
  handling, offline behavior, and the no-AI build.
- [x] Obtain final approval before creating the annotated v0.3.1 source tag.

The first public distribution is the tagged source tree. Users build it against
a local TeX Live or MacTeX installation; aiTeX creates and validates
`latex.afmt` in the user's platform cache on first use. Native bundles, wheels,
package-registry entries, and hosted compiler services are deferred.

## Qualified compiler baseline

The public regression set is the locked 191-project qualification. It records
complete conditional correctness, exact DVI drawing and special streams,
bounded PDF raster differences, exact cache hits, no timeouts or memory
aborts, and a 10,000-page DVI/PDF stress pass. Release decisions are made by
the reproducible checks in this repository and the locked product set; this is
not a claim that every TeX document is supported.

## 0.3.x follow-ups

### Complete-build semantics

Replace the remaining host semantic programs with native implementations:

- BibTeX;
- Biber-compatible bibliography processing;
- MakeIndex and nomenclature processing;
- MakeGlossaries workflows.

Until then, aiTeX invokes the installed TeX Live tools only when their content
inputs change. TeX itself is never delegated to `latexmk`.

### Profiles and preloading

Design an explicit profile syntax for a document class plus a fixed, ordered
preamble palette. A profile must have a visible opening and closing marker,
participate completely in cache identity, and fail safely when project input
escapes the frozen boundary. Shipped profiles may be built against the detected
TeX Live installation; users must have an equally straightforward path to
build their own.

The engine, rather than a host lexer, must observe any reusable semantic
boundary. Literal `\begin{document}` matching is insufficient because TeX
expansion can create or hide that token sequence.

### Editor loop

Extend `aitex watch` with:

- SyncTeX emission and editor integration;
- more granular dependency explanations;
- persistent worker/checkpoint experiments where correctness can be proven;
- clear cache-hit, rebuild-cause, and published-output events for humans and
  machines.

### Performance

Continue measuring cold startup, cache misses, warm exact hits, incremental
edits, long documents, and semantic-tool workflows separately. Priorities are:

1. preserve the excellent ordinary direct-PDF path;
2. reduce cold DVI overhead below stock LaTeX;
3. remove repeated filesystem and kpathsea process work;
4. explore native filename-database lookup without breaking TeX Live policy;
5. reduce snapshot construction and clean-convergence overhead;
6. investigate persistent compiler services and safely reusable engine state.

Every performance result needs the same input, host, toolchain, affinity,
dependency boundary, and correctness oracle. A faster wrong page does not
count.

## AI-assisted repair

The preview product keeps a conservative transaction:

1. run deterministic local diagnosis and safe repair rules;
2. show the same compact source-mapped failure to the human and model;
3. request explicit consent before uploading selected source;
4. bound attempts, elapsed time, tokens, changed lines, deletions, and cost;
5. restrict the model to source reads and exact edits inside the project;
6. validate the proposed tree with a private complete build;
7. show the diff and diagnostic delta;
8. apply atomically only after acceptance, retaining a backup.

Near-term work:

- improve expansion-aware and cross-file source localization;
- improve successful-but-wrong output detection;
- make diagnostic packets more useful to humans and models together;
- evaluate model, prompt, tool, and error-reporting changes outside the product
  repository;
- publish a licensed benchmark separately when its provenance, splits,
  anti-gaming oracle, and maintenance model are ready;
- add local-model routes only when they meet the same safety and validation
  contract.

No model score is a stable product promise. Provider behavior, model identity,
price, latency, and quality can all change independently of aiTeX.

## Compatibility and output

### Classic TeX

Continue closing low-level pdfTeX/e-TeX compatibility gaps exposed by real
packages. Preserve exact DVI semantics and compare rendered PDF output as well
as process success. Avoid package-specific emulation where a general engine
rule exists.

### Unicode and fonts

The experimental native path already covers UTF-8 input, bidi/script
itemization, line breaking, deterministic font fallback, shaping, TrueType and
CFF subsetting, variable-font instancing, vertical glyph runs, and COLRv0
layers. Promotion requires broader coverage of:

- language-specific line breaking and hyphenation;
- vertical paragraph composition;
- OpenType MATH and a Unicode math-list builder;
- COLRv1, SVG, sbix, and CBDT/CBLC color glyphs;
- legacy encodings and mixed classic/native font use;
- platform font discovery and viewer interoperability.

### PDF

Keep direct PDF deterministic and compact while extending:

- SyncTeX;
- accessibility tagging and a credible PDF/UA conformance path;
- publication metadata and archival profiles;
- graphics-native coverage that reduces host-tool fallbacks;
- additional annotation, destination, color-space, and transparency behavior.

## Distribution and ecosystem

After the source-only v0.3.1 publication:

- add signed release artifacts and checksums;
- decide whether native wheels provide enough value over prefix bundles;
- test additional Linux distributions and macOS versions;
- add Windows support;
- make package-manager recipes only when their TeX Live dependency story is
  honest;
- explore collaboration or distribution with TeX Live rather than silently
  forking its package and font ecosystem;
- stabilize library interfaces only after the CLI and compiler contracts settle.

## Definition of done

A work item is complete only when:

- its user-visible contract is documented;
- positive, negative, and invalidation paths are tested;
- correctness is compared with the appropriate independent oracle;
- performance claims include time, memory, input, environment, and confidence;
- cache or snapshot state has a versioned identity and safe fallback;
- third-party inputs have recorded provenance and redistribution rights;
- ordinary offline commands remain offline;
- generated artifacts stay out of the source tree;
- release claims point to evidence that can be reproduced or clearly identify
  an archived external qualification.
