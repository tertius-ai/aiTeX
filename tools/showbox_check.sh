#!/bin/bash
# Differential box displays: `\showbox` and `\showlists`, ours against real TeX.
#
# The box display is the instrument, not the product. Nothing a page-breaking
# or insertion question needs is visible in a DVI file: whether the glue above
# a line came from `\baselineskip` or `\lineskip`, whether an insertion on the
# page is the one that was contributed or a remainder left by a split, how tall
# the page thinks it is. All of it is in `\showlists`, and until this file
# existed we had no way to know our version of it was telling the truth.
#
# It was not. Ours printed `Discriminant(10)` where TeX prints an insertion
# line, put the `[]` that marks an elided list on a line of its own instead of
# at the end of the line that introduced it, treated a negative `\showboxdepth`
# as "no limit" when TeX treats it as "show nothing", and named no glue at all
# — so every `\glue 3.0` could have come from any of eighteen parameters. Three
# of those made our trip.tex transcript disagree with TeX's in ways that looked
# like page-builder faults and were not.
#
# What is compared: every `> \box…`, `### …` and `Completed box being shipped
# out …` block in both logs, verbatim. The last is §638's `\tracingoutput`,
# which is the only record of what actually left the engine.
#
# What is NOT compared, and why: TeX follows each of these with `! OK.` and an
# error context (`l.4 \showbox0`). Our diagnostics are deliberately a different
# shape — they carry the step number the fault happened at, which is what makes
# a divergence findable — so the `!` lines are dropped from both sides here.
# Matching TeX's error format byte-for-byte is a separate job; TRIP will
# eventually want it, since a third of trip.log is error contexts.
#
#   tools/showbox_check.sh [name]
set -u
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
BIN=${AITEX_BIN:-$ROOT/target/release/aitex}
ONLY=${1:-}

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

# The blocks, from either engine's log. A display runs from its `>` or `###`
# header to the first line that is blank or belongs to something else.
extract() {
  awk '
    /^> .?box[0-9]+=/ || /^### / || /^Completed box being shipped out/ { inblk = 1 }
    inblk && (/^$/ || /^!/ || /^init=/ || /^l\.[0-9]/ || /^\(/ ) { inblk = 0 }
    inblk { print }
  ' "$1"
}

FAILED=0
TOTAL=0
CASES=0

for f in tests/showbox/*.tex; do
  name=$(basename "$f" .tex)
  [ -n "$ONLY" ] && [ "$name" != "$ONLY" ] && continue
  CASES=$((CASES + 1))
  cp "$f" "$TMP/b.tex"
  # A case may bring a purpose-built TFM as a human-readable PL file. This is
  # how rare ligature operators can be tested without committing opaque binary
  # font data or depending on a system font that happens to use them.
  pl="tests/showbox/$name.pl"
  # A reduction may use a checked-in canonical PL file without duplicating
  # hundreds of lines beside the small TeX input. The directive is deliberately
  # a repository-relative path and is only read from our own test fixtures.
  declared_pl=$(sed -n 's/^% TFM-PL: //p' "$f" | head -1)
  [ -n "$declared_pl" ] && pl="$declared_pl"
  if [ -f "$pl" ]; then
    command -v pltotf >/dev/null || {
      printf '  SKIP  %-20s pltotf is required for its PL font\n' "$name"
      FAILED=1
      continue
    }
    pl_name=$(basename "$pl")
    tfm_name=${pl_name%.pl}
    cp "$pl" "$TMP/$pl_name"
    ( cd "$TMP" && pltotf "$pl_name" "$tfm_name.tfm" >/dev/null 2>&1 ) || {
      printf '  SKIP  %-20s its PL font is malformed\n' "$name"
      FAILED=1
      continue
    }
  fi

  # Knuth's own binary wherever the case will run under it, which keeps a
  # superset's thirty years of separate maintenance out of the oracle — and
  # `pdftex -ini -etex` only where it will not. A case exercising `\lpcode` or
  # `\pdfprotrudechars` cannot run under TeX82 at all: the names do not exist,
  # the reference comes out with no protrusion in it, and the comparison is
  # against an engine that was never asked the question. `tools/battery.sh` has
  # had this fallback since it was written; this file did not, and the first
  # case that needed it looked like an engine bug for a while.
  ( cd "$TMP" && max_print_line=1000 timeout 120 tex -ini \
      -interaction=nonstopmode b.tex >/dev/null 2>&1 )
  ref_engine=tex
  if grep -qE '^! (Undefined control sequence|You can.t use)' "$TMP/b.log" 2>/dev/null; then
    ( cd "$TMP" && max_print_line=1000 timeout 120 pdftex -ini -etex \
        -interaction=nonstopmode b.tex >/dev/null 2>&1 )
    ref_engine=etex
  fi
  # `\showbox` reports `! OK.`, so an error count cannot tell a malformed case
  # from a working one here. What can: a reference that produced no display at
  # all means the case never ran.
  extract "$TMP/b.log" > "$TMP/ref.seq"
  if [ ! -s "$TMP/ref.seq" ]; then
    printf '  SKIP  %-20s reference shows no box at all — the case is malformed\n' "$name"
    FAILED=1
    continue
  fi

  # Node building is off unless the engine is asked for output, and a box
  # display of nothing is not a comparison.
  ( cd "$TMP" && max_print_line=1000 AITEX_MAX_STEPS=400000000 timeout 120 "$BIN" pass b.tex --ini -o b.dvi >ours.out 2>&1 )
  extract "$TMP/ours.out" > "$TMP/our.seq"

  n=$(grep -cE '^(> .?box[0-9]+=|### |Completed box)' "$TMP/ref.seq")
  TOTAL=$((TOTAL + n))
  if diff -q "$TMP/ref.seq" "$TMP/our.seq" >/dev/null; then
    printf '  ok    %-20s %s displays\n' "$name" "$n"
  else
    printf '  FAIL  %-20s %s displays\n' "$name" "$n"
    diff "$TMP/ref.seq" "$TMP/our.seq" | head -"${DIFFLINES:-20}" | sed 's/^/          /'
    FAILED=1
  fi
done

echo
if [ "$CASES" = 0 ]; then
  echo "no showbox cases matched"
  exit 2
fi
if [ "$FAILED" = 0 ]; then
  echo "box displays match TeX's — $TOTAL displays"
else
  echo "box displays differ from TeX's — see above"
fi
exit "$FAILED"
