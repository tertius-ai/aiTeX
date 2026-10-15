# Documentation guide

The active project direction lives in [`ROADMAP.md`](../ROADMAP.md). The files
in this directory describe the current compiler and the evidence behind its
released behavior:

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — compiler structure, performance model,
  serialization, and safe checkpoint constraints.
- [`FAST-PATH.md`](FAST-PATH.md) — recommended release-format, cache, direct
  PDF, watch, and CI commands for the lowest latency.
- [`DISTRIBUTION.md`](DISTRIBUTION.md) — tagged source builds, prefix installs,
  and unpublished package checks for the engine-matched format.
- [`PRIVACY.md`](PRIVACY.md) — offline commands, explicit source upload,
  credentials, local repair records, cost boundaries, and host tools.
- [`RELEASE.md`](RELEASE.md) — qualification gates, hashes, the current
  first-public-source-release boundary, and tag procedure.
- [`HOLDOUT-200.md`](HOLDOUT-200.md) — human-readable report for the locked
  191-root paper qualification.
- [`HOLDOUT-200-WARM.md`](HOLDOUT-200-WARM.md) — exact-DVI warm project-cache
  results for every stock-DVI-eligible holdout root.
- [`PERFORMANCE.md`](PERFORMANCE.md) — versioned benchmark contract, raw
  evidence summaries, and current performance debt.
- [`COMPILER-BUILD-MATRIX.md`](COMPILER-BUILD-MATRIX.md) — systematic
  one-shot versus complete-build comparisons across aiTeX, TeX/pdfTeX,
  latexmk, XeTeX, and LuaTeX on curated and locked real-world tiers.
- [`PDF-SIZE.md`](PDF-SIZE.md) — compact-PDF architecture plus the focused and
  seven-case redistributable size, correctness, and cost qualifications.
- [`PDF-IMAGES.md`](PDF-IMAGES.md) — lossless image admission, native format
  preservation, bounded fallbacks, resource sharing, and graphics accounting.
- [`FONTS.md`](FONTS.md) — the shared classic/OpenType boundary, shaping and
  subsetting contracts, embedding policy, and upstream ecosystem relationship.
- [`CACHE.md`](CACHE.md) — P2 layered snapshot identity, validation, recovery,
  bounds, and operational commands.
- [`CHECKPOINTS.md`](CHECKPOINTS.md) — P3 live-job boundary, payload,
  provenance, refusal rules, and differential gate.
- [`AI-REPAIR-LAB.md`](AI-REPAIR-LAB.md) — controlled 0.3 diagnostic-to-model
  protocol, `aitex explain`, validation-gated `aitex fix`, small multi-tool
  corpus, prompt profiles, and privacy boundary.
- [`AI-REPAIR-PROTOCOL-V1.2.md`](AI-REPAIR-PROTOCOL-V1.2.md) — active compact
  repair/model boundary, including typed source-mapped diagnostic provenance.
- [`AI-REPAIR-PROTOCOL-V1.1.md`](AI-REPAIR-PROTOCOL-V1.1.md) — immutable shared
  human/model diagnostic-display comparison baseline.
- [`AI-REPAIR-PROTOCOL-V1.md`](AI-REPAIR-PROTOCOL-V1.md) — immutable original
  repair protocol and comparison baseline.
- [`LINT.md`](LINT.md) — conservative design contract for detecting successful
  but suspicious semantic changes without fabricating compiler errors.
- [`CLASS-FREQUENCY.md`](CLASS-FREQUENCY.md) — measured document-class and
  option distribution in the locked 191-root corpus.
- [`history/`](history/) — superseded plans, old measurements, and engine
  investigation notes retained for provenance rather than active direction.

Other contributor entry points:

- [`../CHANGELOG.md`](../CHANGELOG.md) — user-visible changes and the planned
  v0.3.1 public-source-release positioning.
- [`../CONTRIBUTING.md`](../CONTRIBUTING.md) — development, evidence, license,
  privacy, and pull-request expectations.
- [`../SECURITY.md`](../SECURITY.md) — private reporting and supported security
  boundary.
- [`../tools/README.md`](../tools/README.md) — commands and script taxonomy.
- [`../tests/README.md`](../tests/README.md) — differential and integration test
  suites.
- [`../qualification/README.md`](../qualification/README.md) — locked corpus and
  environment artifacts.
