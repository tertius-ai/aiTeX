#!/bin/bash
# Differential error sequences: which errors an engine raises, in what order.
#
# Every other differential tool here drops the `!` lines. tools/showbox_check.sh
# says why, and says what this file is for:
#
#   "Matching TeX's error format byte-for-byte is a separate job; TRIP will
#    eventually want it, since a third of trip.log is error contexts."
#
# This is the first half of that job — *which* errors, in *what order*, not yet
# the context lines under them. Both halves matter and the order is the harder
# one to get right by accident: it is a statement about when during scanning a
# fault is noticed, so it pins control flow that nothing else here observes.
# `\count0=\undefinedthing` is the case that started this file. TeX reports the
# undefined control sequence and then the missing number, because §370 makes an
# undefined name expandable and the number scanner trips over it from inside its
# own `get_x_token`. We reported them the other way round, which meant our scan
# had given up a token earlier than TeX's — invisible in every gate we had,
# because none of them looked at an error.
#
# What is compared: the `! …` lines of both logs, verbatim and in order, *and*
# every `PROBE:` marker a case emits with `\message`.
#
# The markers are not decoration. Every one of these errors has a recovery — a
# value TeX uses in place of the bad one, a register it does or does not empty —
# and a right message with a wrong recovery is barely better than no message at
# all: it just moves the next error somewhere else. §977 is the case in point.
# We neither reported that `\vsplit` had been handed an `\hbox` nor left the
# register alone as TeX does, and only the second of those two faults can lose
# a document its box. tools/battery.sh cannot cover this ground — it refuses any
# case whose reference raises an error, which is every case here.
#
# The context lines under each error ARE compared, as of §311's `show_context`:
# the whole input stack, each level named and split into what it has read and
# what it has left. That was excluded here for as long as ours was a file line
# and TeX's was a stack; it is not any more.
#
# What is still NOT compared: the help text under an error, which is verbatim
# from probes and would only restate ErrorKind::help.
#
#   tools/error_check.sh [name]
set -u
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
BIN=$ROOT/target/release/aitex
ONLY=${1:-}

command -v tex >/dev/null || { echo "no tex(1) — nothing to compare against"; exit 2; }

TMP=$(mktemp -d); [ -n "${KEEP:-}" ] || trap 'rm -rf "$TMP"' EXIT
[ -n "${KEEP:-}" ] && echo "working in $TMP"

# The `!` lines and the `PROBE:` markers, in the order they were written. A
# `PROBE:` may share a line with other text, so it is pulled out rather than
# matched whole — and `-a` because trip-flavoured tests can put a NUL in a log,
# which makes grep call the whole file binary and print nothing at all.
#
# The context lines under an error are dropped first, because they *quote the
# source* — and the source of a case here is full of `PROBE:` markers. Ours
# quotes a different span of it than TeX's does (§311's context is an input
# stack; ours is a file line), so leaving them in would compare the one thing
# this gate has deliberately excluded. A context is an `l.N` or `<…>` head plus
# the padded second half that always follows it.
# A context is *two* lines — the head plus what has not been read yet, padded
# under it — and §317 bounds both. Taking only the first would leave the second
# bound untested, so the line after a head goes through verbatim, spaces and
# all. It is matched by position rather than by pattern because that is what it
# is: whatever followed the head, however it was cut.
extract() {
  awk '
    # Several markers can share a line: with max_print_line wide, consecutive
    # \message texts run together, and TeX writes `PROBE:a=1  PROBE:b=2`.
    function markers(s,   n) { n = 0
      while (match(s, /PROBE:[^ ]*/)) {
        print substr(s, RSTART, RLENGTH); s = substr(s, RSTART + RLENGTH); n++ }
      return n }
    # ...unless it is a source echo carrying a PROBE marker, in which case the
    # marker is what this file has always compared and the padding around it is
    # noise. A case whose long line needs checking simply keeps its \message on
    # a line of its own.
    tail { tail = 0; if (!markers($0)) print; next }
    /^! / { print; next }
    /^l\.[0-9]+ / || /^<[a-z ]+> / || /^\\[A-Za-z@]+ / { print; tail = 1; next }
    # §992 `box_error`: what an error threw away, and its top line. Recovery is
    # half of what this file compares — a right message with a wrong recovery
    # just moves the next error — and losing a box is the recovery a document
    # can least afford to have wrong.
    /^The following box has been deleted:/ { print; tail = 1; next }
    /^\.\.\.$/ { print; next }
    # §1005-§1010, when a case turns `\tracingpages` on. Kept because *where*
    # an error falls among these lines is the thing under test for anything the
    # page builder raises: §1009 freezes the page before §1010 complains about
    # the register, so the complaint belongs under the `%% goal height` line.
    /^%%/ || /^% t=/ || /^% split/ { print; next }
    { markers($0) }
  ' "$1"
}

FAILED=0
CASES=0
TOTAL=0

for f in tests/errors/*.tex; do
  name=$(basename "$f" .tex)
  [ -n "$ONLY" ] && [ "$name" != "$ONLY" ] && continue
  CASES=$((CASES + 1))
  cp "$f" "$TMP/b.tex"

  # A case may name its reference engine on its first line. Register *range* is
  # the one place the two disagree by design: TeX82 has 256 of each register and
  # eTeX has 32768, so `\count300` is an error in one and ordinary in the other.
  # This engine is eTeX-ranged — expl3 allocates far past 256 — so a case about
  # the range has to be judged against eTeX or it would be measuring our own
  # deliberate decision. Everything else is the same in both.
  if head -1 "$f" | grep -q '^%% reference: etex'; then
    ( cd "$TMP" && max_print_line=1000 timeout 120 pdftex -ini -etex \
        -interaction=nonstopmode b.tex >/dev/null 2>&1 )
  else
    ( cd "$TMP" && max_print_line=1000 timeout 120 tex -ini \
        -interaction=nonstopmode b.tex >/dev/null 2>&1 )
  fi
  extract "$TMP/b.log" > "$TMP/ref.seq"
  if [ ! -s "$TMP/ref.seq" ]; then
    printf '  SKIP  %-22s reference raised no error — the case is malformed\n' "$name"
    FAILED=1
    continue
  fi

  # Node building is off unless the engine is asked for output, and the errors
  # a run raises depend on how far it gets.
  ( cd "$TMP" && max_print_line=1000 AITEX_MAX_STEPS=400000000 timeout 120 "$BIN" pass b.tex --ini -o b.dvi >ours.out 2>&1 )
  extract "$TMP/ours.out" > "$TMP/our.seq"

  n=$(wc -l < "$TMP/ref.seq" | tr -d ' ')
  TOTAL=$((TOTAL + n))
  if diff -q "$TMP/ref.seq" "$TMP/our.seq" >/dev/null; then
    printf '  ok    %-22s %s errors\n' "$name" "$n"
  else
    d=$(diff "$TMP/ref.seq" "$TMP/our.seq" | grep -c '^[<>]')
    printf '  FAIL  %-22s %s errors, %s differ\n' "$name" "$n" "$d"
    diff "$TMP/ref.seq" "$TMP/our.seq" | head -"${DIFFLINES:-20}" | sed 's/^/          /'
    FAILED=1
  fi
done

echo
if [ "$CASES" = 0 ]; then
  echo "no error cases matched"
  exit 2
fi
if [ "$FAILED" = 0 ]; then
  echo "error sequences match TeX's — $TOTAL errors"
else
  echo "error sequences differ from TeX's — see above"
fi
exit "$FAILED"
