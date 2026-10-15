# Repository agent discipline

These rules apply to every automated or human-assisted change in this
repository. The detailed compiler benchmark contract lives in
`docs/COMPILER-BUILD-MATRIX.md`; the corpus licensing and provenance policy
lives below and in `qualification/README.md`.

## Corpus licensing

- Never commit the materialized 191-project arXiv holdout, its PDFs, or excerpts
  derived from those sources. Keep only identifiers, versioned URLs, hashes,
  extraction instructions, metadata, and derived qualification results.
- Do not treat public availability, successful download, or arXiv's default
  nonexclusive distribution license as permission to redistribute.
- Before adding any third-party source, font, image, or fixture to Git, record
  its explicit redistribution license, provenance, stable upstream URL, and
  required attribution. If those rights are absent or unclear, use an original
  deterministic surrogate or fetch-and-verify the input outside Git.
- The default redistributable publication-source allowlist is CC0, CC BY, and
  CC BY-SA. Preserve attribution and ShareAlike obligations. Broader or
  ambiguous cases require an explicit repository policy decision before use.
- Inspect staged files before committing so an ignored or locally materialized
  corpus cannot enter the repository accidentally.

## Performance and release qualification

- Use `python3 tools/compiler_build_matrix.py --tier corpus-10 --oracle visual`
  for routine broad performance work. Use smaller tiers only for focused
  iteration and label their results accordingly.
- Before tagging a release, run the complete `--tier all --oracle visual`
  matrix on the pinned AMD Ryzen 9 7950X3D qualification host. Preserve its
  result and report as release evidence.
- Keep latency measurements sequential. The matrix's worker allowance is one
  global budget, currently four, passed to aiTeX's PDF work; do not add hidden
  outer parallelism or nested worker pools. A throughput experiment must be a
  separately named mode with an explicit shared budget.
- Pin and record the compiler commit, binary and format hashes, source locks,
  TeX/tool versions, worker budget, clock/mtime policy, and effective CPU
  affinity. Keep affinity stable on the Ryzen host.
- Compare performance only against a baseline from the same machine, affinity,
  toolchain, corpus, sampling protocol, and oracle. Results from a developer
  Mac or hosted CI are useful signals, not substitutes for the Ryzen release
  baseline.
- Never weaken a correctness oracle, omit failures, or mix one-shot and
  complete-build results to improve a speed number. Only correctness-equivalent
  cells contribute performance ratios; rasterization and other oracle work
  stays outside the timed process tree.
