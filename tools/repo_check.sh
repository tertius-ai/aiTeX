#!/usr/bin/env bash
# Keep generated output out of the source tree.
#
# The default mode also requires a clean Git worktree and is intended for CI or
# a clean checkout. `--artifacts-only` is useful while developing a change: it
# checks filesystem hygiene without objecting to the change itself.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

case "${1:-}" in
  "") CHECK_GIT=1 ;;
  --artifacts-only) CHECK_GIT=0 ;;
  *) echo "usage: tools/repo_check.sh [--artifacts-only]" >&2; exit 2 ;;
esac

FAILED=0

# Public repositories must not execute untrusted pull-request code on private
# runners or publish packages from repository workflows.  The first public
# release is a source tag; manual packaging workflows may retain short-lived
# qualification artifacts, but they are not publication channels.
if grep -R -n --fixed-strings 'self-hosted' .github/workflows; then
  echo "repository policy: public workflows must use GitHub-hosted runners" >&2
  FAILED=1
fi

UNPINNED_ACTIONS=$(
  grep -R -n -E 'uses:[[:space:]]+[^[:space:]]+@' .github/workflows \
    | grep -Ev '@[0-9a-f]{40}([[:space:]]|$)' || true
)
if [ -n "$UNPINNED_ACTIONS" ]; then
  echo "repository policy: workflow actions must be pinned to commits:" >&2
  printf '%s\n' "$UNPINNED_ACTIONS" >&2
  FAILED=1
fi

if grep -R -n -E \
    '(^|[[:space:]])(gh release|uv publish|cargo publish|docker push|twine upload|maturin publish)([[:space:]]|$)' \
    .github/workflows; then
  echo "repository policy: package publication commands are forbidden" >&2
  FAILED=1
fi

# Inspect event keys in the block form so push branch filters are supported.
CI_EVENTS=$(awk '
  /^on:$/ { in_events=1; next }
  in_events && /^[^[:space:]#]/ { exit }
  in_events { print }
' .github/workflows/ci.yml)
for event in push pull_request; do
  if ! printf '%s\n' "$CI_EVENTS" | grep -Eq "^  ${event}:([[:space:]]|$)"; then
    echo "repository policy: hosted CI must run on main pushes and pull requests" >&2
    FAILED=1
  fi
done

if ! grep -Fq 'tools/package_release.py source' \
    .github/workflows/release-artifacts.yml; then
  echo "repository policy: source snapshot check must build the source archive" >&2
  FAILED=1
fi

if grep -n -E 'package_release\.py (bundle|wheel|all)' \
    .github/workflows/release-artifacts.yml; then
  echo "repository policy: the source snapshot check must stay source-only" >&2
  FAILED=1
fi

generated_files() {
  find . \
    -path './.git' -prune -o \
    -path './target' -prune -o \
    -path './corpus' -prune -o \
    -path './.aitex' -prune -o \
    -path './.aitex-cache' -prune -o \
    -path './build' -prune -o \
    -path './dist' -prune -o \
    -type f \( \
    -name '*.aux' -o -name '*.log' -o -name '*.out' -o -name '*.err' -o \
    -name '*.dvi' -o -name '*.xdv' -o -name '*.pdf' -o -name '*.fmt' -o \
    -name '*.dvitype' -o -name '*.semantic' -o -name '*.synctex.gz' -o \
    -name '*.fls' -o -name '*.fdb_latexmk' -o -name '*.toc' -o \
    -name '*.bbl' -o -name '*.blg' -o -name '*.idx' -o -name '*.ind' -o \
    -name '*.ilg' -o -name '*.lof' -o -name '*.lot' -o -name '*.nav' -o \
    -name '*.snm' -o -name '*.vrb' \
    \) -print | while IFS= read -r path; do
      relative=${path#./}
      if ! git ls-files --error-unmatch "$relative" >/dev/null 2>&1; then
        printf '%s\n' "$path"
      fi
    done
}

GENERATED=$(generated_files)
if [ -n "$GENERATED" ]; then
  echo "repository hygiene: untracked generated files outside target/corpus:" >&2
  printf '%s\n' "$GENERATED" >&2
  FAILED=1
fi

if find . \
    -path './.git' -prune -o \
    -path './target' -prune -o \
    -path './corpus' -prune -o \
    -path './.aitex' -prune -o \
    -path './.aitex-cache' -prune -o \
    -path './build' -prune -o \
    -path './dist' -prune -o \
    \( -type d -name __pycache__ -o -type f \( -name '*.pyc' -o -name '*.pyo' \) \) \
    -print -quit | grep -q .; then
  echo "repository hygiene: Python bytecode outside ignored build/corpus trees:" >&2
  find . \
    -path './.git' -prune -o \
    -path './target' -prune -o \
    -path './corpus' -prune -o \
    -path './.aitex' -prune -o \
    -path './.aitex-cache' -prune -o \
    -path './build' -prune -o \
    -path './dist' -prune -o \
    \( -type d -name __pycache__ -o -type f \( -name '*.pyc' -o -name '*.pyo' \) \) \
    -print >&2
  FAILED=1
fi

if [ "$CHECK_GIT" = 1 ]; then
  if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "repository hygiene: tracked worktree changes remain:" >&2
    git status --short >&2
    FAILED=1
  elif git ls-files --others --exclude-standard | grep -q .; then
    echo "repository hygiene: untracked, non-ignored files remain:" >&2
    git ls-files --others --exclude-standard >&2
    FAILED=1
  fi
fi

if [ "$FAILED" = 0 ]; then
  echo "repository hygiene: clean"
fi
exit "$FAILED"
