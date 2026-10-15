#!/usr/bin/env bash
# Which of TeX's primitives do we have?
#
# TRIP answers this incidentally — it reports what *it* happens to use. This
# answers it directly, and uses real TeX as the oracle rather than a list
# someone typed: at INITEX there is no format, so every control sequence that
# is *defined* is a primitive and everything else is not. `\ifx\name\undefined`
# is the whole test.
#
# Candidates are harvested from trip.tex, plain.tex and latex.ltx (every
# control sequence they mention) plus a written list, so a primitive that
# appears in none of those and that nobody remembered could still be missed —
# the number is a floor, not a certificate. `\catcode`\@=11` matters: without
# it every `\@name` parses as `\@` followed by text and the answers are noise.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
command -v tex >/dev/null || { echo "tex not found"; exit 1; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT; cd "$TMP" || exit 1

cat "$ROOT/vendor/trip/trip.tex" "$ROOT/vendor/plain/plain.tex" "$(kpsewhich latex.ltx)" 2>/dev/null \
  | grep -oE '\\[a-zA-Z@]+' | sed 's/^\\//' | sort -u > cand.txt
# Primitives that none of those files mention, added by hand.
printf '%s\n' batchmode botmark errhelp errorstopmode holdinginserts lastbox noboundary \
  nonstopmode pagefilllstretch pagefillstretch pagefilstretch pausing scrollmode \
  showboxbreadth showboxdepth splitbotmark splitfirstmark tracingcommands tracinglostchars \
  tracingmacros tracingonline tracingoutput tracingpages tracingparagraphs tracingrestores \
  tracingstats >> cand.txt
sort -u cand.txt -o cand.txt

{ printf '\\catcode`\\{=1\n\\catcode`\\}=2\n\\catcode`\\#=6\n\\catcode`\\@=11\n'
  while read -r n; do printf '\\message{[%s=\\ifx\\%s\\undefined U\\else D\\fi]}\n' "$n" "$n"; done < cand.txt
  printf '\\end\n'; } > probe.tex

max_print_line=2000 timeout 300 tex -ini -interaction=nonstopmode probe.tex >tex.out 2>&1
grep -oaE '\[[^]=]+=[UD]\]' tex.out | sed 's/[][]//g' | awk -F= '$2=="D"' | cut -d= -f1 | sort -u > tex_prims.txt

AITEX_MAX_COMMANDS=200000000 AITEX_MAX_STEPS=200000000 timeout 300 "$FAST" pass probe.tex --ini >ours.out 2>&1
grep -oaE '\[[^]=]+=[UD]\]' ours.out | sed 's/[][]//g' | awk -F= '$2=="D"' | cut -d= -f1 | sort -u > our_prims.txt

have=$(comm -12 tex_prims.txt our_prims.txt | wc -l)
total=$(wc -l < tex_prims.txt)
printf '%d of %d TeX82 primitives implemented (%d%%)\n' "$have" "$total" "$(( have * 100 / total ))"
missing=$(comm -23 tex_prims.txt our_prims.txt)
if [ -n "$missing" ]; then
  echo
  echo "missing:"
  printf '%s\n' "$missing" | tr '\n' ' ' | fold -s -w 78 | sed 's/^/  /'
fi
