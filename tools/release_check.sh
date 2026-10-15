#!/usr/bin/env bash
# Build a tag-ready compiler and run every differential qualification gate.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-}"
HOLDOUT_RESULTS=${AITEX_HOLDOUT_RESULTS:-target/qualification/holdout-200-current.results.json}
HOLDOUT_REPORT=${AITEX_HOLDOUT_REPORT:-target/qualification/HOLDOUT-200-CURRENT.md}
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]]; then
  echo "usage: tools/release_check.sh <major.minor.patch[-prerelease]>" >&2
  exit 2
fi

WORKSPACE_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
if [ "$VERSION" != "$WORKSPACE_VERSION" ]; then
  echo "release version $VERSION does not match workspace version $WORKSPACE_VERSION" >&2
  exit 1
fi

if [ ! -s "$HOLDOUT_RESULTS" ] || [ ! -s "$HOLDOUT_REPORT" ]; then
  cat >&2 <<EOF
release qualification requires a fresh locked-holdout result and report:
  $HOLDOUT_RESULTS
  $HOLDOUT_REPORT
Generate them with the commands in docs/RELEASE.md, then rerun this command.
EOF
  exit 1
fi

# Keep the result selected here and the result graded inside done_check.sh
# identical. Environment variables supplied by hosted qualification remain
# authoritative; the local defaults above are intentionally ignored outputs.
export AITEX_HOLDOUT_RESULTS="$HOLDOUT_RESULTS"
export AITEX_HOLDOUT_REPORT="$HOLDOUT_REPORT"

# Untracked files cannot change a commit or tag. Tracked changes can, and mean
# the result would qualify bytes other than the candidate commit.
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "release qualification requires a clean tracked worktree" >&2
  git status --short --untracked-files=no >&2
  exit 1
fi

TAG="v$VERSION"
TAG_COMMIT=$(git rev-list -n 1 "$TAG" 2>/dev/null || true)
if [ -n "$TAG_COMMIT" ] && [ "$TAG_COMMIT" != "$(git rev-parse HEAD)" ]; then
  echo "$TAG already names $TAG_COMMIT, not the candidate $(git rev-parse HEAD)" >&2
  exit 1
fi

# Build the pre-tag candidate with the same public version identity it will
# have at the exact annotated tag. Otherwise `git describe` would make the
# qualified binary differ from the subsequently tagged release binary.
export AITEX_BUILD_GIT_DESCRIBE="$TAG"

echo "release qualification: $TAG at $(git rev-parse HEAD)"
echo "rust: $(rustc --version)"
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -A warnings -D clippy::correctness
cargo build --release --locked -p aitex-cli
if [ "$(target/release/aitex --version)" != "aitex $VERSION" ]; then
  echo "candidate binary does not report the exact release version" >&2
  target/release/aitex --version >&2
  exit 1
fi
python3 tools/fetch_locked_corpus.py
REFERENCE_FORMATS="$ROOT/target/qualification/reference-formats"
python3 tools/reference_formats.py --output "$REFERENCE_FORMATS"
export AITEX_REFERENCE_FORMATS="$REFERENCE_FORMATS"
export TEXFORMATS="$REFERENCE_FORMATS:${TEXFORMATS:-}"
python3 tools/qualification_env.py --require-corpus
python3 tools/texlive_overlay.py

# Source builds prewarm a format against the qualification host's locked TeX
# tree. Public native bundles contain only the executable; users generate this
# derived cache against their own TeX Live or MacTeX installation.
make bundle
cmp -s target/release/aitex bin/aitex || {
  echo "bundled executable differs from the qualified release binary" >&2
  exit 1
}
cp .aitex/format/fmt.afmt target/release/latex.afmt

mkdir -p target/release
REPORT=$(mktemp)
trap 'rm -f "$REPORT"' EXIT
tools/done_check.sh 2>&1 | tee "$REPORT"
grep -qx 'DONE — every gate passes.' "$REPORT"
cp "$REPORT" target/release/qualification.txt
cp "$HOLDOUT_RESULTS" target/release/holdout-200.results.json
cp "$HOLDOUT_REPORT" target/release/HOLDOUT-200.md
(cd target/release && sha256sum aitex latex.afmt qualification.txt \
  holdout-200.results.json HOLDOUT-200.md > SHA256SUMS)
python3 tools/sbom.py --output "dist/aitex-$VERSION.spdx.json"
python3 tools/package_release.py all --output-dir dist
HOST_TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
python3 tools/bundle_check.py "dist/aitex-$VERSION-$HOST_TRIPLE.tar.gz"

echo "READY $TAG $(git rev-parse HEAD)"
cat target/release/SHA256SUMS
