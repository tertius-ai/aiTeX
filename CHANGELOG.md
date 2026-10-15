# Changelog

## 0.3.1 — 2026-10-15

Initial public source release.

- Provide a classic pdfTeX-compatible compiler with direct PDF, exact DVI,
  native fixed-point complete builds, exact output reuse, watch mode, compact
  diagnostics, and conservative artifact-backed lint.
- Ship the tagged source tree as the public distribution. TeX Live or MacTeX
  remains a documented build and runtime requirement; native bundles, wheels,
  package-registry entries, and hosted compiler services are not published.
- Keep locally built executables source-only. The first document build, or an
  explicit `aitex format`, derives and validates an engine-matched
  `latex.afmt` from the user's TeX installation and caches it locally.
- Make the stable compiler, preview AI repair, and experimental
  Unicode/OpenType boundaries explicit. Ordinary compilation, watch, pass,
  and lint commands remain deterministic, local, and offline.
- Run push and pull-request checks only on GitHub-hosted runners, pin workflow
  actions to exact revisions, and keep package workflows unpublished.
- Include contribution, security, privacy, installation, dependency, corpus,
  and release-qualification guidance suitable for a public academic project.
- Qualify the release on all 191 locked source roots: 148/148 conditional PDF
  builds, 144/144 comparable raster matches, 58/58 exact DVI drawing and
  special streams, and no additional invalid extracted Unicode code points.

Detailed evidence and current policy live in
[`docs/RELEASE.md`](docs/RELEASE.md), [`ROADMAP.md`](ROADMAP.md), and
[`qualification/README.md`](qualification/README.md).
