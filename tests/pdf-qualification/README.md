# Synthetic PDF qualification sources

Every source in this directory was written for aiTeX qualification and is
distributed under the repository license. The prose, formulas, identifiers,
and document structure are synthetic; they are not excerpts or transformations
of a third-party document.

`synthetic-book.tex` is the repeatable medium-document size and rendering
workload. A small checked-in TeX loop creates its pages at compile time, so the
repository does not carry generated source expansions or PDF artifacts. It
exercises repeated page streams, section destinations and outlines, Type 1 font
families and styles, math, rules, colors, and cross-references. The fixed source
epoch used by the runner makes metadata deterministic.

`classic-fonts.tex` repeats an original matrix of classic encodings, math
families, sizes, styles, and transformed text. `annotation-stress.tex` makes
boxed internal and URI links, destinations, outlines, colors, and rules a
meaningful share of output. `graphics-stress.tex` reuses an original EPS and
the deterministic PNG matrix made by `tools/pdf_fixture_assets.py`: RGB,
partial alpha, one- and eight-bit grayscale, indexed color-key transparency,
and identical content under distinct names and transforms. Neither image
bytes nor prose come from an external publication.

`python3 tools/pdf_suite.py --quick` runs these cases alongside the existing
synthetic book, configurable 10,000-page source, Unicode CLI fixture, and a
three-page positioned-font producer. The full suite uses three samples for
moderate cases and one measured plus one deterministic probe for the scale and
positioned cases. Generated PDFs and rasters remain temporary.

The broader qualification command also reuses focused original fixtures under
`tests/pdf`, `tests/scale`, and `tests/unicode`. Locked OpenType test subsets are
covered by their OFL notice and provenance file in
`crates/aitex-fonts/tests/fonts`.

Private local documents may reveal useful debugging signals, but they are not
inputs, derived fixtures, or repository qualification dependencies. No release
claim depends on unavailable or non-redistributable content.
