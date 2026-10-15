#!/usr/bin/env bash
# Find the first character where our DVI stops agreeing with pdfTeX's.
#
# `tools/corpus_check.sh` passes a project only when the *whole* `setchar`
# stream matches, so a document can report identical page and character counts
# and still fail — the same characters set in a different order. That is the
# hardest class to work on with the transcript alone, because
# `tools/logdiff.sh` compares *events* and a reordering need not produce one:
# for 2608.18006v1 it reports "identical for all 0 events" while the DVIs
# differ.
#
# So compare what actually reaches the page. This prints the first differing
# character, which page it is on, how far into the document it is, and enough
# decoded text either side to recognise the passage in the source.
#
# Character codes are printed as text where the font makes that meaningful.
# The corpus is Latin Modern in EC/OT1, so codes 32-126 read as ASCII and the
# rest are shown as `<NNN>` — a math italic `x` and a text `x` are both 120 and
# are not distinguished. Read the decoded run as a hint for locating the
# passage, not as a claim about which font set it.
#
# Usage:
#   tools/dvidiff.sh <corpus-dir-or-file.tex> [context]
#
#     context   characters of decoded text either side of the divergence (60)
#
# pdfTeX's DVI is the one `tools/classify_corpus.sh` left in the corpus
# directory; ours is built here in a copy, from a format built here, so a stale
# format cannot report a divergence that is really a failed run. Every file
# pdfTeX generated is removed from the copy first: its DVI came from a single
# pass over a clean directory, so ours has to start from one too, or we compare
# a second pass against a first.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
CTX="${2:-60}"

[ $# -ge 1 ] || { sed -n '/^# Usage:/,/^#$/p' "$0" | sed 's/^# \?//'; exit 2; }
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
command -v dvitype >/dev/null || { echo "dvitype not found"; exit 1; }

# Accept either the project directory or the main .tex inside it.
arg="$1"
if [ -d "$arg" ]; then
  dir="$arg"
  root="$(cd "$dir" && grep -l documentclass ./*.tex 2>/dev/null | head -1)"
  root="$(basename "${root:-}")"
  # `ls *.tex | head -1` picks an include often enough to have cost this
  # investigation three false "identical" readings.
  [ -n "$root" ] || { echo "no file with \\documentclass in $dir"; exit 1; }
else
  dir="$(cd "$(dirname "$arg")" && pwd)"
  root="$(basename "$arg")"
fi
b="${root%.tex}"

[ -f "$dir/$b.dvi" ] || { echo "no pdfTeX DVI at $dir/$b.dvi — run tools/classify_corpus.sh"; exit 1; }

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

LTX="$(kpsewhich latex.ltx)"
[ -n "$LTX" ] || { echo "latex.ltx not found"; exit 1; }
head -n "$(( $(wc -l < "$LTX") - 4 ))" "$LTX" > "$TMP/kernel.tex"
printf '\\input kernel\n' > "$TMP/mkfmt.tex"
( cd "$TMP" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$FAST" mkfmt.tex --dump fmt.afmt >/dev/null 2>&1 )
[ -s "$TMP/fmt.afmt" ] || { echo "could not build a format"; exit 1; }

w="$TMP/w"; mkdir -p "$w"
cp -r "$dir/." "$w/" 2>/dev/null
cp "$TMP/fmt.afmt" "$w/"
# `.bbl` and `.ind` are *inputs*, not droppings: arXiv runs neither BibTeX nor
# makeindex, so a submission ships them and `tools/classify_corpus.sh` had them
# in hand when it made pdfTeX's reference DVI. Deleting them here left us to
# typeset a document with no bibliography against a reference that had one —
# 2608.16845v1 came out 17p/28766c against 19p/33177c, and matched exactly once
# the `.bbl` was left alone. `.idx` and `.blg` really are generated, by LaTeX
# and by BibTeX, so they still go.
rm -f "$w/$b.dvi" "$w"/*.aux "$w"/*.log "$w"/*.toc "$w"/*.lof "$w"/*.lot \
      "$w"/*.out "$w"/*.blg "$w"/*.idx "$w"/*.nav \
      "$w"/*.snm "$w"/*.brf "$w"/*.spl 2>/dev/null
( cd "$w" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$FAST" pass "$root" --fmt fmt.afmt -o "$b-ours.dvi" >/dev/null 2>err.txt )
if grep -q 'built by a different engine' "$w/err.txt" 2>/dev/null; then
  echo "the format no longer loads — the binary changed. Rebuild and re-run."; exit 2
fi
[ -s "$w/$b-ours.dvi" ] || { echo "we produced no DVI; stderr:"; head -5 "$w/err.txt"; exit 1; }

# `page<TAB>code`, one line per character set, in document order.
stream() {
  dvitype -output-level=2 -page-start='*' "$1" 2>/dev/null |
    awk '/beginning of page/ { p = $(NF-0); next }
         match($0, /set(char|_char_)[0-9]+/) {
           s = substr($0, RSTART, RLENGTH); gsub(/[^0-9]/, "", s);
           print p "\t" s }'
}

stream "$dir/$b.dvi"      > "$TMP/tex.txt"
stream "$w/$b-ours.dvi"   > "$TMP/ours.txt"

tc=$(wc -l < "$TMP/tex.txt"); oc=$(wc -l < "$TMP/ours.txt")
tp=$(dvitype "$dir/$b.dvi" 2>/dev/null | grep -c 'beginning of page')
op=$(dvitype "$w/$b-ours.dvi" 2>/dev/null | grep -c 'beginning of page')

echo "$b"
printf '  pdftex  %sp  %sc\n  ours    %sp  %sc\n\n' "$tp" "$tc" "$op" "$oc"

if cmp -s "$TMP/tex.txt" "$TMP/ours.txt"; then
  echo "  the character streams are identical"
  exit 0
fi

# First line where they part company.
# `exit` still runs END, so the END clause needs the flag or it prints a second
# answer and every arithmetic use of `i` below fails on a two-line value.
i=$(awk 'NR==FNR { a[FNR]=$0; next }
         { if (a[FNR] != $0) { print FNR; found=1; exit } }
         END { if (!found) print FNR+1 }' "$TMP/tex.txt" "$TMP/ours.txt")

# Decode a slice of one stream to readable text.
decode() {  # <file> <from> <count>
  awk -v from="$2" -v n="$3" -F'\t' '
    FNR >= from && FNR < from + n {
      c = $2 + 0
      if (c >= 32 && c < 127) printf "%c", c
      else printf "<%d>", c
    }' "$1"
}
pageof() { awk -v i="$2" -F'\t' 'FNR == i { print $1; exit }' "$1"; }

from=$(( i - CTX )); [ "$from" -lt 1 ] && from=1
echo "  first difference at character $i of $tc (page $(pageof "$TMP/tex.txt" "$i") for pdftex, page $(pageof "$TMP/ours.txt" "$i") for us)"
echo
echo "  pdftex: ...$(decode "$TMP/tex.txt"  "$from" $(( i - from )))|$(decode "$TMP/tex.txt"  "$i" "$CTX")..."
echo "  ours:   ...$(decode "$TMP/ours.txt" "$from" $(( i - from )))|$(decode "$TMP/ours.txt" "$i" "$CTX")..."
echo
echo "  (| marks the divergence; the text before it is common to both)"
