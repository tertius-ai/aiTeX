# Compiler/build matrix fixtures

These small projects were written for aiTeX qualification and are distributed
under the repository license. They are not excerpts from publications. The
matrix deliberately keeps them readable: each project isolates a document
shape that also occurs in the locked real-world corpus.

- `plain` exercises the plain-TeX format and DVI output.
- `article-common` combines common mathematical, table, color, link, and
  microtypography packages with cross-references and a table of contents.
- `book` exercises chapters, running heads, two-sided output, a table of
  contents, and cross-references.
- `amsart` concentrates theorem and display-math work.
- `report-tools` requires bibliography and index processors to reach its fixed
  point.
- `beamer` covers a slide class and its usual color/link/PGF stack.
- `graphics` consumes the PDF suite's deterministically generated RGB, alpha,
  grayscale, indexed, and duplicate-content PNG assets.
- `unicode` uses one source with project-local OFL font fixtures under aiTeX,
  XeLaTeX, and LuaLaTeX. It is a semantic cross-engine comparison, not an
  assertion that distinct layout engines must place every glyph identically.

Generated artifacts remain in temporary benchmark directories. The versioned
matrix manifest locks every input tree by content hash.
