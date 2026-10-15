#!/bin/bash
# Differential batteries: every syntactic form, ours against real TeX.
#
# Each file in tests/battery/ exercises one area of TeX's syntax — integers,
# dimensions, glue, parameter texts, `\meaning`, the expansion primitives — and
# prints every result with `\message` so both engines can be compared on
# *output* rather than on whether they crashed.
#
# This exists because a corpus that passes completely can still leave whole
# syntactic forms untested. plain.tex loads 1241 of 1241 lines and never once
# writes a literal dimension in an `\ifdim`, copies one glue register to
# another, or coerces a count — so `\ifdim 1pt>0pt` was broken outright,
# `\skip0=\skip5` silently dropped its stretch, and `\count5\p@` came out 65536
# times too small, all while every gate was green. Twelve faults in one day
# came out of these files; none of them would have surfaced from the corpus.
#
# The rule for adding a case: the expectation is whatever the reference engine
# prints, and its log must contain no errors. Twice now I have taken TeX's
# echo of an error context for TeX's output and written a test asserting the
# opposite of the truth, so the error count is checked, not assumed.
#
# The oracle is `tex` — TeX82 itself — for every battery that TeX82 will run,
# and `pdftex -ini -etex` only for those that need the e-TeX primitives.
#
#   tools/battery.sh [name]     — all batteries, or just tests/battery/<name>.tex
set -u
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
BIN=$ROOT/target/release/aitex
ONLY=${1:-}

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
cp vendor/plain/plain.tex "$TMP/" 2>/dev/null

FAILED=0
TOTAL=0

for f in tests/battery/*.tex; do
  name=$(basename "$f" .tex)
  [ -n "$ONLY" ] && [ "$name" != "$ONLY" ] && continue
  cp "$f" "$TMP/b.tex"

  # Reference: real TeX82 where the battery will run under it, and only where
  # it will not — the e-TeX forms, which are most of the point of these files —
  # `pdftex -ini -etex`. Comparing against Knuth's own binary wherever possible
  # keeps a superset's thirty years of separate maintenance out of the oracle.
  ( cd "$TMP" && max_print_line=1000 timeout 120 tex -ini \
      -interaction=nonstopmode b.tex >ref.out 2>&1 )
  ref_engine=tex
  if grep -qE '^!' "$TMP/ref.out"; then
    ( cd "$TMP" && max_print_line=1000 timeout 120 pdftex -ini -etex \
        -interaction=nonstopmode b.tex >ref.out 2>&1 )
    ref_engine=etex
  fi
  errs=$(grep -cE '^!' "$TMP/ref.out")
  if [ "$errs" != 0 ]; then
    printf '  SKIP  %-18s reference has %s error(s) — the battery is malformed\n' \
      "$name" "$errs"
    FAILED=1
    continue
  fi

  ( cd "$TMP" && max_print_line=1000 timeout 120 "$BIN" pass b.tex --ini -o b.dvi >ours.out 2>&1 )

  # Compare only the `<...>` probes each battery prints, so banners, allocation
  # logs and TeX's own `[1]` page numbers do not drown the signal. Every
  # battery must wrap each result in angle brackets and nothing else may.
  pat='<[^>]*>'
  grep -oE "$pat" "$TMP/ref.out"  > "$TMP/ref.seq"
  grep -oE "$pat" "$TMP/ours.out" > "$TMP/our.seq"

  n=$(wc -l < "$TMP/ref.seq" | tr -d ' ')
  TOTAL=$((TOTAL + n))
  # The rule `tools/done_check.sh` enforces on itself: a case that examined
  # nothing FAILS. A battery whose probes are not wrapped in `<...>` — the
  # brackets are easy to forget, and `[...]` reads just as well to a human —
  # produces an empty sequence, matches an empty sequence, and reports `ok  0
  # forms`. That is a green line for a file that tested nothing.
  if [ "$n" = 0 ]; then
    printf '  FAIL  %-18s examined NOTHING — probes must be wrapped in <...>\n' "$name"
    FAILED=1
    continue
  fi
  if diff -q "$TMP/ref.seq" "$TMP/our.seq" >/dev/null; then
    printf '  ok    %-18s %s forms (%s)\n' "$name" "$n" "$ref_engine"
  else
    printf '  FAIL  %-18s %s forms\n' "$name" "$n"
    diff "$TMP/ref.seq" "$TMP/our.seq" | head -12 | sed 's/^/          /'
    FAILED=1
  fi
done

echo
if [ "$FAILED" = 0 ]; then
  echo "all batteries agree with TeX — $TOTAL forms"
else
  echo "batteries disagree with TeX — see above"
fi
exit "$FAILED"
