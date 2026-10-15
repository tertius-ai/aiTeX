#!/bin/bash
# Differential transcripts: what the `\tracing*` parameters say, ours against
# real TeX's.
#
# TeX's seven tracing parameters are how an engine narrates itself, and between
# them they are most of what a TRIP transcript is made of — `\tracingcommands`
# alone accounts for 1,988 of trip.log's 7,220 lines. They are also the only
# way to see *what the engine did* rather than what it produced: a DVI file
# records the answer, a trace records the working.
#
# All seven were registered in this engine as parameters with nothing reading
# them. This tool is where each one is pinned as it lands.
#
# What is compared: every line of both transcripts, less the parts that cannot
# agree — banners, file names, the `!` error messages and their `l.N` contexts
# (our diagnostics are deliberately a different shape; see the note in
# tools/showbox_check.sh), each engine's own closing summary, and §638's bare
# `\tracingstats`'s figures — §639 and §1334 count TeX's `mem[]` and we have no
# such array (docs/history/PLAN.md decision 2), so ours are our own under our own header and
# there is nothing to diff — and §638's bare
# `[0]` page echo — which TeX writes against the current column, joining
# whatever line is in progress. We have no column, only whole messages, so that
# one print waits on giving the transcript a real character stream.
#
#   tools/trace_check.sh [name]
set -u
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
BIN=$ROOT/target/release/aitex
ONLY=${1:-}

command -v tex >/dev/null || { echo "no tex(1) — nothing to compare against"; exit 2; }

TMP=$(mktemp -d); [ -n "${KEEP:-}" ] || trap 'rm -rf "$TMP"' EXIT
[ -n "${KEEP:-}" ] && echo "working in $TMP"

# Everything both engines should agree on, and nothing else.
filter() {
  # §1327's string-pool, mem[], hash, flattened-font and packed-trie sizes
  # describe TeX's representation, not document semantics. The dump trace
  # keeps the exact identity/font/exception/no-page lines around them.
  # §537's `(./file.tex`, its `)` and §638's `[N]` are compared as written:
  # every printer that writes against a column has one now, so a diagnostic
  # TeX glues onto an open line is glued onto ours.
  #
  # A trailing `)` is stripped, for one reason: §639's `Memory usage` block —
  # which this engine cannot produce, its node model being different — prints
  # *between* the page echo and `final_cleanup`, so with `\tracingstats` on
  # TeX's `)` lands on a line of its own and ours lands on the `[N]`. The echo
  # is emitted and its position is right wherever §639 does not intervene.
  # An error context is a head — `l.N`, or `<output>`/`<inserted text>`/`<to be
  # read again>` — plus the padded second half that always follows it, echoing
  # the source at the break. Both halves go: the heads were already dropped, and
  # leaving the tails in compared a source echo, which is the one thing this
  # tool excludes on purpose (ours is a file line, TeX's is an input stack).
  # A `\showthe` inside `\output` produces one of these after every value, and
  # a context deeper than `\errorcontextlines` is elided to a bare `...`, which
  # goes with it.
  # §638 asks `(term_offset>0)or(file_offset>0)` before it writes ` [N]`, and
  # TeX has two streams: with `\tracingonline` at zero a diagnostic goes to the
  # log only, so the *terminal* still has `(./file.tex` on its line and the
  # space is printed although the log's line is empty. This engine has one
  # stream — the terminal is the log — so it cannot tell the two apart, and a
  # page echo after a log-only diagnostic comes out one space short. Only the
  # leading space of such a line is dropped; the echo itself is compared.
  # File-open echoes begin `(<path>` and are representation noise, but §1335's
  # `(\end occurred …)` is semantic final-cleanup output and must survive.
  sed 's/^ \(\[[0-9]\)/\1/' "$1" \
    | sed 's/ *)$//' \
    | awk '/^l\.[0-9]/ || /^<[a-z ]*>/ { skip = 1; next }
           skip { skip = 0; next }
           { print }' \
    | grep -vE '^(This is |\*\*|entering extended mode|\*\\end|\*$)' \
    | grep -vE '^(!|Output written|init=|aitex build: TeX )' \
    | grep -vE '^\.\.\.$' \
    | grep -vE '^\([^\\]|^ *\)' \
    | grep -vE '^(converged:|wrote |\([0-9]+ errors?\)$)' \
    | grep -vE '^ +[0-9]+  \\' \
    | grep -vE '^[0-9]+ (strings of total length|memory locations dumped|multiletter control sequences|words of font info)' \
    | grep -vE '^Hyphenation trie of length |^  [0-9]+ for language ' \
    | grep -vE '^--- [a-z]+ ---$' \
    | grep -vE '^(Here is how much|Memory usage| [0-9]+i,)' \
    | grep -vE '^ [0-9]+ (control sequence|word|parameter-stack|font|string|multiletter|hyphenation)' \
    | sed 's/[[:space:]]*$//' \
    | awk 'NF || seen { print; if (NF) seen = 1 }'
}

FAILED=0
CASES=0
TOTAL=0

for f in tests/trace/*.tex; do
  name=$(basename "$f" .tex)
  [ -n "$ONLY" ] && [ "$name" != "$ONLY" ] && continue
  CASES=$((CASES + 1))
  cp "$f" "$TMP/b.tex"

  # Rare TFM ligature programs are supplied as readable PL fixtures, by the
  # same repository-relative directive used by tools/showbox_check.sh.
  declared_pl=$(sed -n 's/^% TFM-PL: //p' "$f" | head -1)
  if [ -n "$declared_pl" ]; then
    command -v pltotf >/dev/null || {
      printf '  SKIP  %-20s pltotf is required for its PL font\n' "$name"
      FAILED=1
      continue
    }
    pl_name=$(basename "$declared_pl")
    tfm_name=${pl_name%.pl}
    cp "$declared_pl" "$TMP/$pl_name"
    ( cd "$TMP" && pltotf "$pl_name" "$tfm_name.tfm" >/dev/null 2>&1 ) || {
      printf '  SKIP  %-20s its PL font is malformed\n' "$name"
      FAILED=1
      continue
    }
  fi

  # Both engines get `max_print_line=1000` so that neither wraps a line the
  # other is being compared on — except for a case that says otherwise on its
  # first line, which then runs at TeX's default of 79 and is *about* the
  # wrapping. §58 breaks a line the instant `file_offset` reaches the limit, and
  # a line that ends exactly there has been ended already, which changes how many
  # blank lines the `print_ln`s after it produce.
  MPL=1000
  head -1 "$f" | grep -q '^%% max_print_line: default' && MPL=""
  ( cd "$TMP" && env ${MPL:+max_print_line=$MPL} timeout 120 tex -ini \
      -interaction=nonstopmode b.tex >/dev/null 2>&1 )
  filter "$TMP/b.log" > "$TMP/ref.seq"
  if [ ! -s "$TMP/ref.seq" ]; then
    printf '  SKIP  %-20s reference traced nothing — the case is malformed\n' "$name"
    FAILED=1
    continue
  fi

  # Node building is off unless the engine is asked for output, and a trace of
  # a run that never typeset anything is not the trace being tested.
  ( cd "$TMP" && env ${MPL:+max_print_line=$MPL} AITEX_MAX_STEPS=400000000 \
      timeout 120 "$BIN" pass b.tex --ini -o b.dvi >ours.out 2>&1 )
  filter "$TMP/ours.out" > "$TMP/our.seq"

  n=$(wc -l < "$TMP/ref.seq" | tr -d ' ')
  TOTAL=$((TOTAL + n))
  if diff -q "$TMP/ref.seq" "$TMP/our.seq" >/dev/null; then
    printf '  ok    %-20s %s lines\n' "$name" "$n"
  else
    d=$(diff "$TMP/ref.seq" "$TMP/our.seq" | grep -c '^[<>]')
    printf '  FAIL  %-20s %s lines, %s differ\n' "$name" "$n" "$d"
    diff "$TMP/ref.seq" "$TMP/our.seq" | head -"${DIFFLINES:-20}" | sed 's/^/          /'
    FAILED=1
  fi
done

echo
if [ "$CASES" = 0 ]; then
  echo "no trace cases matched"
  exit 2
fi
if [ "$FAILED" = 0 ]; then
  echo "transcripts match TeX's — $TOTAL lines"
else
  echo "transcripts differ from TeX's — see above"
fi
exit "$FAILED"
