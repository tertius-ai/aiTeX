#!/bin/bash
# Every `\showlists` in trip.tex, ours against Knuth's transcript.
#
# trip.tex calls `\showlists` nine times, and each one is a complete statement
# of what the engine believes: what is on the page, what is waiting in the
# contributions, how tall the page thinks it is, what each insertion class is
# charging it, how many lines the last paragraph came to. Nothing else in the
# test says all of that at once, and nothing in a DVI file says any of it.
#
# So this is the page builder's differential oracle. `tools/trip.sh` counts
# pages, which tells you *that* something diverged; this tells you where.
#
# What is compared: each block from `### vertical mode entered` down to the
# `prevdepth` line that closes it, in order, verbatim.
#
# What is not: TeX follows each with `! OK.` and an error context, and our
# diagnostics are deliberately a different shape (they carry the step number,
# which is what makes a divergence findable). Those lines are dropped from both
# sides — see the note in tools/showbox_check.sh.
#
#   tools/trip_lists.sh [block]     — all blocks, or just the n'th
set -u
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
BIN=$ROOT/target/release/aitex
ONLY=${1:-}

command -v tex >/dev/null || { echo "no tex(1) — nothing to compare against"; exit 2; }

TMP=$(mktemp -d); [ -n "${KEEP:-}" ] || trap 'rm -rf "$TMP"' EXIT
[ -n "${KEEP:-}" ] && echo "working in $TMP"
cp vendor/trip/trip.tex vendor/trip/trip.pl "$TMP/" 2>/dev/null || {
  echo "vendor/trip/trip.tex not found"; exit 2; }
# trip.tex loads a font called `trip`, and without its metrics *both* engines
# fall back to `\nullfont` and typeset nothing — a comparison that agrees about
# an empty page proves nothing at all.
command -v pltotf >/dev/null || { echo "no pltotf(1) — cannot build trip.tfm"; exit 2; }
( cd "$TMP" && pltotf trip.pl trip.tfm >/dev/null 2>&1 )

# Knuth's own binary, both passes: the format is dumped by pass one.
# `max_print_line` at its default of 79 wraps TeX's long lines and ours never
# wrap, which would show up as a divergence in every `\showthe` of a token list.
# Line-breaking the transcript is a real difference and worth its own fix; it is
# not the page builder, which is what this tool is for.
( cd "$TMP" && max_print_line=1000 tex -ini -interaction=batchmode trip.tex >/dev/null 2>&1
  max_print_line=1000 tex -interaction=batchmode '&trip' trip.tex >/dev/null 2>&1
  # Keep it under a name of our own *before* our engine runs. The job is called
  # `trip`, so every engine writes `trip.log`; ours started writing one too, and
  # it overwrote the reference this tool reads. The comparison then had our
  # output on both sides and reported 7 of 7 blocks matching, which is what a
  # tool says when it is comparing a file with itself.
  cp trip.log ref-trip.log 2>/dev/null )

# Ours, the same way.
export AITEX_MAX_COMMANDS=400000000 AITEX_MAX_STEPS=400000000
( cd "$TMP" && AITEX_DVI=p1.dvi timeout 600 "$BIN" pass trip.tex --ini --tex82 >/dev/null 2>&1
  mv trip.fmt ours.fmt 2>/dev/null
  AITEX_DVI=p2.dvi timeout 600 "$BIN" pass trip.tex --fmt ours.fmt --tex82 >ours.out 2>&1 )

# A `\showlists` block: from the `### vertical mode entered` header down to the
# `prevdepth` line, which is always the last thing §218 prints.
split_blocks() {
  awk -v dir="$2" '
    /^### vertical mode entered/ { n++; inblk = 1 }
    inblk { print > (dir "/" n) }
    /^prevdepth/ { inblk = 0 }
  ' "$1"
}

mkdir -p "$TMP/ref" "$TMP/our"
split_blocks "$TMP/ref-trip.log" "$TMP/ref"
split_blocks "$TMP/ours.out" "$TMP/our"

n_ref=$(ls "$TMP/ref" 2>/dev/null | wc -l)
n_our=$(ls "$TMP/our" 2>/dev/null | wc -l)
if [ "$n_ref" = 0 ]; then
  echo "no \\showlists blocks in ref-trip.log — the reference run failed"
  exit 2
fi

FAILED=0
MATCHED=0
for i in $(seq 1 "$n_ref"); do
  [ -n "$ONLY" ] && [ "$i" != "$ONLY" ] && continue
  ref="$TMP/ref/$i"
  our="$TMP/our/$i"
  lines=$(wc -l < "$ref" | tr -d ' ')
  if [ ! -f "$our" ]; then
    printf '  MISS  block %-2s %4s lines — we never showed one\n' "$i" "$lines"
    FAILED=1
    continue
  fi
  if diff -q "$ref" "$our" >/dev/null; then
    printf '  ok    block %-2s %4s lines\n' "$i" "$lines"
    MATCHED=$((MATCHED + 1))
  else
    d=$(diff "$ref" "$our" | grep -c '^[<>]')
    printf '  FAIL  block %-2s %4s lines, %s differ\n' "$i" "$lines" "$d"
    diff "$ref" "$our" | head -"${DIFFLINES:-16}" | sed 's/^/          /'
    FAILED=1
  fi
done

echo
echo "showlists blocks: $MATCHED of $n_ref match (we produced $n_our)"
exit "$FAILED"
