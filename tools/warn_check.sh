#!/bin/bash
# Differential box warnings: §660 and §675, ours against real TeX's.
#
# `Overfull \hbox`, `Underfull \vbox` and their relatives are the messages a
# typesetter is judged by in practice — they are what a user sees when a line
# does not fit, and the only report TeX makes about a box that is *wrong* but
# still typeset. This engine emitted none of them at all: `\hbadness`,
# `\vbadness` and `\vfuzz` were registered parameters with nothing reading them.
#
# What is compared: every `Overfull`/`Underfull`/`Loose`/`Tight` line, verbatim
# and in order. That covers the four verdicts, the two axes, the amount (scaled
# points for an overfull box, a badness otherwise) and — the part a wrong
# recovery shows up in — the `at lines N--M` attribution.
#
# What is NOT compared: the box display that follows each one. It is emitted,
# and tools/showbox_check.sh already holds that format to TeX's.
#
#   tools/warn_check.sh [name]
set -u
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
BIN=$ROOT/target/release/aitex
ONLY=${1:-}

command -v tex >/dev/null || { echo "no tex(1) — nothing to compare against"; exit 2; }
TMP=$(mktemp -d); [ -n "${KEEP:-}" ] || trap 'rm -rf "$TMP"' EXIT
[ -n "${KEEP:-}" ] && echo "working in $TMP"

# The warning line, and §663's `short_display` of the box under it.
#
# That second line is what says *which* line the warning is about, and it is
# the half a reader actually uses. It is only an hbox warning that has one —
# §675's vertical twin has no short form for a vertical list — so it is taken
# by position: the line after, when there is one that is not blank.
extract() {
  awk '
    tail { tail = 0; if (NF) { print; next } }
    /^(Overfull|Underfull|Loose|Tight) \\[hv]box / { print; tail = 1 }
  ' "$1"
}

FAILED=0; CASES=0; TOTAL=0
for f in tests/warn/*.tex; do
  name=$(basename "$f" .tex)
  [ -n "$ONLY" ] && [ "$name" != "$ONLY" ] && continue
  CASES=$((CASES + 1))
  cp "$f" "$TMP/b.tex"
  ( cd "$TMP" && max_print_line=1000 timeout 120 tex -ini \
      -interaction=nonstopmode b.tex >/dev/null 2>&1 )
  extract "$TMP/b.log" > "$TMP/ref.seq"
  if [ ! -s "$TMP/ref.seq" ]; then
    printf '  SKIP  %-22s reference warned about nothing — the case is malformed\n' "$name"
    FAILED=1; continue
  fi
  # Node building is off unless the engine is asked for output, and a box that
  # is never packed is never reported.
  ( cd "$TMP" && max_print_line=1000 AITEX_MAX_STEPS=400000000 timeout 120 "$BIN" pass b.tex --ini -o b.dvi >ours.out 2>&1 )
  extract "$TMP/ours.out" > "$TMP/our.seq"
  n=$(wc -l < "$TMP/ref.seq" | tr -d ' ')
  TOTAL=$((TOTAL + n))
  if diff -q "$TMP/ref.seq" "$TMP/our.seq" >/dev/null; then
    printf '  ok    %-22s %s warnings\n' "$name" "$n"
  else
    d=$(diff "$TMP/ref.seq" "$TMP/our.seq" | grep -c '^[<>]')
    printf '  FAIL  %-22s %s warnings, %s differ\n' "$name" "$n" "$d"
    diff "$TMP/ref.seq" "$TMP/our.seq" | head -"${DIFFLINES:-20}" | sed 's/^/          /'
    FAILED=1
  fi
done
echo
[ "$CASES" = 0 ] && { echo "no warn cases matched"; exit 2; }
if [ "$FAILED" = 0 ]; then echo "box warnings match TeX's — $TOTAL warnings"
else echo "box warnings differ from TeX's — see above"; fi
exit "$FAILED"
