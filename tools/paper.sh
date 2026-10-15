#!/usr/bin/env bash
# Run one corpus project through us — and optionally through real TeX — in a
# clean working copy, with tracing switched on if you ask for it.
#
# This is the setup every corpus investigation starts with, and it was being
# rebuilt by hand each time out of `cp -r`, `rm -f`, a `python3` heredoc to
# inject `\tracingparagraphs`, `tools/mkfmt.sh` and a long `aitex` invocation.
# Each rebuild re-learned the same traps:
#
#   1. **Rebuild the format after every engine build.** A format carries an
#      engine fingerprint; a stale one is *rejected before the run starts*, so
#      no `.log` is written and the previous run's log is read as fresh. Every
#      count then comes back as whatever the old binary said — or as zero, which
#      reads as data. This has cost five separate investigations here.
#   2. **A clean directory per run.** Two runs in one directory means the second
#      reads the `.aux` the first wrote: citations and references resolve
#      differently and the comparison invents divergences that are not there.
#   3. **`.bbl` and `.ind` are inputs.** arXiv runs neither BibTeX nor
#      makeindex, so the submission ships them and pdfTeX's reference DVI was
#      made with them. Deleting them costs a whole bibliography.
#   4. **The main file is the one with `\documentclass`.** `ls *.tex | head -1`
#      picks an include often enough to have produced three false "identical"
#      readings.
#
# Usage:
#   tools/paper.sh <corpus-dir|file.tex> [options]
#
#     --inject '<tex>'   insert this right after `\begin{document}`
#     --tex              run `latex` too, into its own clean directory
#     --grep '<re>'      print matching lines from the log(s) instead of errors
#     --keep             keep the working directories and print their paths
#
# With no options it reports the error count and the first error in context.
# The working directory is always printed, so `--keep` is only needed if you
# want it to survive.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"

[ $# -ge 1 ] || { sed -n '/^# Usage:/,/^#$/p' "$0" | sed 's/^# \?//'; exit 2; }
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }

arg="$1"; shift
INJECT=""; RUNTEX=0; GREP=""; KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --inject) INJECT="$2"; shift 2 ;;
    --tex)    RUNTEX=1; shift ;;
    --grep)   GREP="$2"; shift 2 ;;
    --keep)   KEEP=1; shift ;;
    *) echo "unknown option: $1"; exit 2 ;;
  esac
done

if [ -d "$arg" ]; then
  dir="$(cd "$arg" && pwd)"
  root="$(cd "$dir" && grep -l documentclass ./*.tex 2>/dev/null | head -1)"
  root="$(basename "${root:-}")"
  [ -n "$root" ] || { echo "no file with \\documentclass in $dir"; exit 1; }
else
  dir="$(cd "$(dirname "$arg")" && pwd)"
  root="$(basename "$arg")"
fi
b="${root%.tex}"

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
BASE="$(mktemp -d)"
[ "$KEEP" -eq 1 ] || trap 'rm -rf "$BASE"' EXIT

# One clean copy per engine, so neither reads the other's `.aux`.
prepare() {  # $1 = destination
  local w="$1"
  mkdir -p "$w"
  cp -r "$dir/." "$w/" 2>/dev/null
  # What the reference run generated goes; what the submission shipped stays.
  rm -f "$w/$b.dvi" "$w"/*.aux "$w"/*.log "$w"/*.toc "$w"/*.lof "$w"/*.lot \
        "$w"/*.out "$w"/*.blg "$w"/*.idx "$w"/*.nav "$w"/*.snm "$w"/*.brf \
        "$w"/*.spl 2>/dev/null
  # Spliced with head/tail and `printf %s`, not `awk -v`: awk expands escape
  # sequences in a `-v` assignment, so the backslashes in `\show\norm` are eaten
  # before the text ever reaches the file. Every one of them has to survive.
  if [ -n "$INJECT" ]; then
    local ln
    ln="$(grep -n '\\begin{document}' "$w/$root" | head -1 | cut -d: -f1)"
    if [ -n "$ln" ]; then
      { head -n "$ln" "$w/$root"
        printf '%s\n' "$INJECT"
        tail -n "+$((ln + 1))" "$w/$root"
      } > "$w/.injected" && mv "$w/.injected" "$w/$root"
    else
      echo "warning: no \\begin{document} in $root; --inject ignored" >&2
    fi
  fi
}

OURS="$BASE/ours"; prepare "$OURS"
"$ROOT/tools/mkfmt.sh" "$OURS" >/dev/null 2>&1 || { echo "could not build a format"; exit 1; }
# Timed, because "is it just slow?" is asked often enough to be worth having
# without a separate run — `tools/corpus_check.sh` kills a project at 120s and
# a paper that trips that limit reports a truncated DVI, which looks exactly
# like lost content.
START="$(date +%s.%N 2>/dev/null || echo 0)"
( cd "$OURS" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$FAST" pass "$root" --fmt fmt.afmt -o "$b-ours.dvi" >/dev/null 2>err.txt )
END="$(date +%s.%N 2>/dev/null || echo 0)"
ELAPSED="$(awk -v a="$START" -v b="$END" 'BEGIN{ d=b-a; if (d<0) d=0; printf "%.2f", d }')"
if grep -q 'built by a different engine' "$OURS/err.txt" 2>/dev/null; then
  echo "the format no longer loads — the binary changed under this run."; exit 2
fi

if [ "$RUNTEX" -eq 1 ]; then
  TEX="$BASE/tex"; prepare "$TEX"
  ( cd "$TEX" && max_print_line=1000 latex -interaction=nonstopmode "$root" >/dev/null 2>&1 )
fi

report() {  # $1 = label, $2 = log
  [ -f "$2" ] || { printf '  %-7s (no log)\n' "$1"; return; }
  if [ -n "$GREP" ]; then
    printf '  === %s ===\n' "$1"
    grep -E "$GREP" "$2" | head -40 | sed 's/^/    /'
  else
    if [ "$1" = "ours" ]; then
      printf '  %-7s errors=%s  %ss\n' "$1" "$(grep -c '^!' "$2")" "$ELAPSED"
    else
      printf '  %-7s errors=%s\n' "$1" "$(grep -c '^!' "$2")"
    fi
  fi
}

echo "$b"
report ours "$OURS/$b.log"
[ "$RUNTEX" -eq 1 ] && report tex "$TEX/$b.log"

# Pages and characters from each DVI, which is what `tools/corpus_check.sh`
# actually compares. Worth having beside the error counts: a document can run
# clean and still be short, and the totals say which.
if [ "$RUNTEX" -eq 1 ] && command -v dvitype >/dev/null; then
  dvi_pages() { dvitype "$1" 2>/dev/null | grep -c 'beginning of page'; }
  dvi_chars() { dvitype -output-level=2 -page-start='*' "$1" 2>/dev/null |
                  grep -cE 'setchar[0-9]+|set_char_[0-9]+'; }
  od="$OURS/$b-ours.dvi"; td="$TEX/$b.dvi"
  if [ -s "$od" ] && [ -s "$td" ]; then
    echo
    printf '  ours    %sp  %sc\n  tex     %sp  %sc\n' \
      "$(dvi_pages "$od")" "$(dvi_chars "$od")" \
      "$(dvi_pages "$td")" "$(dvi_chars "$td")"
  fi
fi

if [ -z "$GREP" ] && [ "$(grep -c '^!' "$OURS/$b.log" 2>/dev/null)" != "0" ]; then
  echo
  echo "  first error:"
  grep -m1 -A6 '^!' "$OURS/$b.log" | sed 's/^/    /' | cut -c1-100
fi
echo
echo "  ours: $OURS"
[ "$RUNTEX" -eq 1 ] && echo "  tex:  $TEX"
[ "$KEEP" -eq 1 ] || echo "  (working copies are deleted on exit; pass --keep to inspect them)"
