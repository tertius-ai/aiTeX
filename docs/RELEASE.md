# Release qualification and procedure

Version 0.3.1 is the initial public source release. The source tag names the
exact commit on which every blocking gate below passed and the maintainers
approved publication.

## Public feature boundary

The public claim is layered:

- **Stable:** classic pdfTeX-compatible compilation, direct PDF and exact DVI,
  the native complete-build scheduler, exact converged-output cache, watch
  mode, compact shared diagnostics, and conservative artifact-backed lint.
- **Preview:** explicit, bounded, validation-gated AI explanation and repair.
  A successful private complete build is required before aiTeX offers an edit;
  this is not a guarantee of autonomous repair.
- **Experimental:** native Unicode/OpenType shaping, extended font and language
  compatibility, vertical/color-font work, and tagged-PDF foundations.

TeX Live remains required for the LaTeX kernel, packages, classic font
metrics/programs, and BibTeX/Biber/index/glossary tools. Graphics outside the
native lossless paths may additionally need MuPDF, Ghostscript, Netpbm, or
ImageMagick. Ordinary build, watch, pass, and lint operations are offline.
A provider is contacted only by an explicit explain/fix route after source
upload is authorized.

The first public distribution is the annotated source tag. GitHub exposes zip
and tar snapshots of that source; no additional release object or uploaded
asset is required. Native Linux/macOS bundles, Python wheels, package-registry
entries, containers, and hosted compiler services are deferred. A source build
requires a compatible local TeX Live or MacTeX installation and creates its
derived, engine-matched `latex.afmt` in the user's platform cache.

## Release contract

Every source release must:

- keep only redistributable product code, original fixtures, and offline
  qualification machinery in the public tree;
- state stable, preview, and experimental behavior consistently;
- require explicit source-upload consent for every live provider route while
  keeping ordinary compilation offline and preserving a no-AI build;
- enforce source, edit, time, token, build, and optional cost limits on repair;
- pass formatting, correctness clippy, all workspace tests, documentation
  links, dependency policy, repository hygiene, and offline repair tests;
- pass the locked 191-project holdout, exact DVI, PDF correctness and
  performance, cache, font, Unicode, package, and 10,000-page stress checks;
- run the complete `--tier all --oracle visual` compiler/build matrix on the
  pinned Ryzen qualification host;
- pass clean source builds on hosted Linux and macOS runners; and
- receive maintainer approval before its annotated source tag is created.

## Evidence

The v0.3.1 compiler qualification records:

| Surface | Passing result |
|---|---:|
| Syntax batteries | 1,057/1,057 forms |
| Box displays | 388/388 |
| Trace transcripts | 4,303/4,303 lines |
| Historical arXiv regression suite | 34/34 projects; 770 pages; 1,506,678 glyphs |
| Product qualification roots | 191/191 evaluated |
| PDF conditional correctness | 148/148 valid; 148/148 page counts |
| Comparable PDF rasters | 144/144 within locked tolerance |
| PDF text quality | 148/148 no more invalid code points than stock |
| DVI conditional correctness | 58/58 valid |
| Exact DVI drawing, specials, and combined stream | 58/58 |
| Aggregate cold PDF CPU | 0.977× stock pdfLaTeX |
| Aggregate cold DVI CPU | 1.289× stock LaTeX; release ceiling 1.30× |
| Cold PDF p95 RSS | 1.730× stock |
| Exact-output hits | 56/56 exact DVI; 0.01× stock LaTeX CPU |
| Corpus timeouts / memory aborts | 0 / 0 |
| Scale | 10,000/10,000 pages validate through DVI and PDF |
| TRIP | 16 pages; 220 DVI records; 9 `\showlists` blocks; 7,256 transcript lines |
| Package catalog | 50/50 installed packages; 3 locked absent |

The complete compiler/build matrix covers 199 cases and 1,378 measured cells.
All 168 available oracle comparisons pass; unavailable cells remain explicit
rather than being counted as successes. The locked holdout contains no timeout
or memory-limit abort.

`tools/done_check.sh` is the authority for the complete local product gate. A
gate that examines zero cases fails. The holdout checker rejects partial or
stale results and verifies source, environment, overlay, runner, format, and
release-binary identities.

## Candidate qualification

First build the exact candidate and generate its ignored holdout evidence:

```sh
export AITEX_BUILD_GIT_DESCRIBE=v0.3.1
cargo build --release --locked -p aitex-cli
python3 tools/arxiv_holdout.py run --jobs 3 --timeout 120 \
  --memory-limit-mib 2048 \
  --output target/qualification/holdout-200-current.results.json
python3 tools/arxiv_holdout.py report \
  --results target/qualification/holdout-200-current.results.json \
  --output target/qualification/HOLDOUT-200-CURRENT.md
python3 tools/holdout_check.py \
  --results target/qualification/holdout-200-current.results.json \
  --binary target/release/aitex
```

Then run the complete gate from that clean commit:

```sh
tools/release_check.sh 0.3.1
```

The explicit build identity makes the pre-tag executable report the same bare
version as the future exact tag, so tagging cannot silently change the bytes
that were qualified. The wrapper sets and verifies this identity itself for
its rebuild. It also checks the workspace version and tag state, enforces
formatting and correctness clippy lints, builds the optimized compiler and a
qualified local `latex.afmt`, reconstructs the pinned reference formats and
TeX Live overlay, validates the locked 191-project result, and runs the
complete done-check. It writes:

- `target/release/aitex`
- `target/release/latex.afmt`
- `target/release/qualification.txt`
- `target/release/holdout-200.results.json`
- `target/release/HOLDOUT-200.md`
- `target/release/SHA256SUMS`

The qualification gate may also exercise the unpublished native-bundle tooling:

```sh
make dist
python3 tools/bundle_check.py \
  dist/aitex-0.3.1-$(rustc -vV | sed -n 's/^host: //p').tar.gz
```

Complete release qualification and the visual compiler/build matrix run
locally on the pinned, pre-provisioned Ryzen host without installing or
modifying its TeX Live oracle. The public repository contains no workflow that
runs untrusted code on that self-hosted machine. Hosted CI verifies ordinary
builds and repository invariants; native-package tools remain local,
unpublished qualification surfaces.

Rust currently retains absolute checkout and Cargo source paths in the
executable, so hosted rebuilds cannot be required to share a raw binary hash
with a local rebuild. Hosted qualification waives only that hash comparison:
version, source commit, functional checks, corpus identity, output correctness,
memory, and performance remain strict. The local tag gate remains byte-strict.

## Tag and publication

Normal development occurs on `main`. Release qualification and tags always use
a clean exact `main` commit.

After hosted CI and local qualification pass:

```sh
VERSION=0.3.1
git switch main
git pull --ff-only
test "$(git rev-parse HEAD)" = "QUALIFIED_COMMIT"
git tag -a "v$VERSION" -m "aiTeX $VERSION"
git push origin "v$VERSION"
```

Do not run the tag or publication commands until the maintainer explicitly
approves the final candidate and verifies that the retained qualification names
the current commit. Creating the tag publishes source only; do not create a
GitHub Release or upload package assets for v0.3.1.
