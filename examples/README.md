# Running the examples

aiTeX compiles both plain TeX and LaTeX to DVI or PDF. PDF is the normal user
route; DVI remains the byte-level and semantic comparison oracle used by the
qualification suite.

Build the optimized compiler once:

```sh
cargo build --release --locked -p aitex-cli
```

The checked-in examples are plain TeX and load `plain.tex` themselves, so they
can be run directly:

```sh
target/release/aitex examples/hello.tex -o examples/hello.pdf
target/release/aitex examples/engine.tex -o examples/engine.dvi
target/release/aitex examples/manypages.tex -o examples/manypages.pdf
```

- `hello.tex` is a short typesetting smoke test.
- `engine.tex` exercises definitions, expansion, registers, conditionals, and
  terminal messages.
- `manypages.tex` exercises page breaking and streaming output over a longer
  document.

For LaTeX projects, aiTeX prepares the local format automatically and reuses a
stable cache directory:

```sh
target/release/aitex document.tex \
  --cache .aitex/cache \
  -o document.pdf
```

The `.pdf` route is one command and needs no external DVI converter. Internally
the qualified PDF driver consumes aiTeX's private DVI representation. To keep
the DVI instead, request an output ending in `.dvi`; standard tools such as
`dvitype`, `dvipdfmx`, and `dvips` can inspect or convert it.

The compiler writes a TeX-style transcript and returns a nonzero status after
TeX errors or host write failures. LaTeX auxiliary files are ordinary project
outputs; the normal command reruns to a fixed point, while `aitex pass` reports
when another pass is needed. See
[`docs/FAST-PATH.md`](../docs/FAST-PATH.md) for format preloading, cache warming,
and persistent edit-loop commands.
