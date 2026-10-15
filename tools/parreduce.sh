#!/usr/bin/env bash
# Shrink a paragraph until it is small enough to read, keeping the divergence.
#
# `tools/logdiff.sh` says *that* two engines disagree about a paragraph.
# This says *which words*, by cutting the text down and re-asking. It exists
# because doing it by hand takes twenty approvals and re-learns four things
# every time:
#
#   1. **Cut from both ends.** Trimming only the tail bottoms out — one
#      paragraph in this corpus would not go below 239 words that way, and went
#      to 45 the moment the head moved too. A paragraph's breaks depend on its
#      whole shape, so a divergence can need context on both sides.
#   2. **Never cut inside `$…$`.** An unbalanced `$` makes a malformed document,
#      and the divergence you then find is the cut's fault, not the engine's.
#      Both ends are advanced until the slice is balanced.
#   3. **Check the reference is clean.** If pdfTeX itself errors on the slice,
#      the comparison means nothing. Reported, and the slice is refused.
#   4. **Sweep `\hsize` at the end.** A badness that differs in a *trial* break
#      may never reach the output at the document's own measure. Changing
#      `\hsize` turns a trial into the chosen break, and then `\showbox` prints
#      the two boxes side by side and nothing has to be inferred. That is what
#      finally showed §915's missing hyphen kern.
#
# Usage:
#   tools/parreduce.sh <words.txt> <class> [first] [last]
#   tools/parreduce.sh <words.txt> <class> sweep <first> <last>
#
#     words.txt   the paragraph's source text, whitespace separated
#     class       document class to set it in (e.g. `iau`, `article`)
#     first,last  word indices; omitted means the whole thing
#     sweep       after reducing, sweep \hsize and report box differences
#
# Prints `hunks` — differing hunks in the `\tracingparagraphs` trace. Zero
# means this slice does not reproduce.
set -u
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
BIN=$ROOT/target/release/aitex
[ -x "$BIN" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
[ $# -ge 2 ] || { sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }

WORDS=$1; CLASS=$2; shift 2
SWEEP=0
if [ "${1:-}" = sweep ]; then SWEEP=1; shift; fi
FIRST=${1:-0}; LAST=${2:-999999}

TMP=$(mktemp -d)
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$TMP"' EXIT
[ "${KEEP:-0}" = 1 ] && echo "runs kept in $TMP"
mkdir -p "$TMP/ref" "$TMP/ours"
# A class sitting beside the text file (an arXiv paper's own `.cls`) has to
# come along or the slice will not typeset the way the paper does.
cp "$(dirname "$WORDS")"/*.cls "$TMP/ref/" 2>/dev/null
cp "$(dirname "$WORDS")"/*.cls "$TMP/ours/" 2>/dev/null

LTX=$(kpsewhich latex.ltx)
head -n "$(( $(wc -l < "$LTX") - 4 ))" "$LTX" > "$TMP/ours/kernel.tex"
printf '\\input kernel\n' > "$TMP/ours/mkfmt.tex"
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 600 "$BIN" mkfmt.tex --dump fmt.afmt >/dev/null 2>&1 )
[ -s "$TMP/ours/fmt.afmt" ] || { echo "could not build a format"; exit 1; }

# $1 = hsize in pt, or empty for the class's own
build() {
  python3 - "$WORDS" "$FIRST" "$LAST" "$CLASS" "${1:-}" "$TMP" <<'PY'
import sys
words, first, last, cls, hsize, tmp = sys.argv[1:7]
b = open(words, encoding='latin-1').read().split()
k, n = int(first), min(int(last), len(b))
# Trap 2: advance both ends until the slice has balanced `$`.
while n < len(b) and ' '.join(b[k:n]).count('$') % 2: n += 1
while k < n and ' '.join(b[k:n]).count('$') % 2: k += 1
head = '\\documentclass{%s}\n\\begin{document}\n' % cls
if hsize:
    head += '\\hsize=%spt\n\\showboxdepth=5 \\showboxbreadth=200 \\tracingonline=0\n' % hsize
    body = '\\setbox0=\\vbox{\\noindent %s\\par}\\showbox0\n' % ' '.join(b[k:n])
else:
    head += '\\tracingparagraphs=1 \\tracingonline=0\n'
    body = ' '.join(b[k:n]) + '\n'
doc = head + body + '\\end{document}\n'
for d in ('ref', 'ours'):
    open('%s/%s/t.tex' % (tmp, d), 'w', encoding='latin-1').write(doc)
print('%d %d' % (k, n))
PY
}

run() {
  for d in ref ours; do rm -f "$TMP/$d"/t.log "$TMP/$d"/t.aux "$TMP/$d"/t.dvi; done
  ( cd "$TMP/ref" && max_print_line=1000 timeout 600 \
      pdflatex -output-format=dvi -interaction=nonstopmode t.tex >/dev/null 2>&1 )
  ( cd "$TMP/ours" && max_print_line=1000 AITEX_DVI=t.dvi \
      AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
      timeout 600 "$BIN" pass t.tex --fmt fmt.afmt >/dev/null 2>&1 )
}

SPAN=$(build)
run
ERRS=$(grep -ac '^! ' "$TMP/ref/t.log")
if [ "$ERRS" != 0 ]; then
  echo "words $SPAN: reference has $ERRS error(s) — the slice is malformed, not the engine"
  exit 1
fi
H=$(diff <(grep -a '^@' "$TMP/ref/t.log") <(grep -a '^@' "$TMP/ours/t.log") | grep -c '^[0-9]')
echo "words $SPAN: $H hunks in the paragraph trace"

[ "$SWEEP" = 1 ] || exit 0
# Trap 4: a trial break becomes the chosen one at some other measure.
echo
for hs in 70 80 90 100 110 120 130 140 150 160 180 200 220 260 300; do
  SPAN=$(build "$hs"); run
  n=$(diff <(sed -n '/> .box0=/,$p' "$TMP/ref/t.log"  | grep -aE '^[.>]') \
           <(sed -n '/> .box0=/,$p' "$TMP/ours/t.log" | grep -aE '^[.>]') | grep -c '^[0-9]')
  printf 'hsize=%-4s %s box hunks\n' "$hs" "$n"
  if [ "$n" != 0 ] && [ "${SHOW:-1}" = 1 ]; then
    diff <(sed -n '/> .box0=/,$p' "$TMP/ref/t.log"  | grep -aE '^[.>]') \
         <(sed -n '/> .box0=/,$p' "$TMP/ours/t.log" | grep -aE '^[.>]') | head -16 | sed 's/^/    /'
    break
  fi
done
