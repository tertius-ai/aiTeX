# Test-suite guide

Most tests are differential fixtures compiled by the scripts in `tools/`, not
Rust integration-test binaries. The directories are organized by oracle:

- `battery/` — compact syntax and engine-state forms compared with TeX.
- `errors/` — error recovery, context, help text, and diagnostic ordering.
- `showbox/` — normalized `\showbox` and list representations.
- `trace/` — command, macro, page, paragraph, alignment, and output transcripts.
- `warn/` — bad-box and warning behavior.
- `pdf/` — PDF driver features, graphics, fonts, links, encodings, and imported
  pages.
- `pdf-qualification/` — original synthetic, generated-at-compile-time prose,
  classic-font, annotation, and raster/vector workloads for PDF size, timing,
  extraction, and rendering. The varied suite combines these with the scale,
  Unicode, and locked OFL font fixtures. No external publication source is
  included.
- `unicode/` — opt-in UTF-8, native OpenType, multilingual, and hybrid
  classic/native PDF fixtures; run `tools/unicode_check.sh` after `make`.
- `perf/` — deterministic root, include, bibliography, and image inputs used by
  the versioned edit-loop benchmark.
- `scale/` — large-document and page-count limits.
- `wheel/` — clean LaTeX smoke input compiled after installing a native wheel
  outside the checkout.
- `build/` — native fixed-point scheduling across TeX, BibTeX, Biber,
  MakeIndex, MakeGlossaries, and nomenclature, including default PDF output
  and an exact repeated-build cache hit.
- `lint/` — one successful document whose converged artifacts exercise the
  high-confidence undefined-reference, undefined-citation, and duplicate-label
  rules.

The umbrella command is `tools/done_check.sh`. Quick Rust checks are
`cargo test --workspace --locked`; release qualification adds the locked paper
corpora and environment through `tools/release_check.sh`.

New semantic fixtures should include a stock-TeX oracle or an independently
validated output property. A test that only asserts that aiTeX accepts its own
output is not sufficient for compatibility work.
