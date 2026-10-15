#!/usr/bin/env bash
# Qualify source-byte encodings independently of PDF font encodings.
#
# Each successful case is written as the bytes named by inputenc, then graded
# twice: positioned DVI drawing semantics against LaTeX and exact PDF text
# extraction against pdfLaTeX.  The malformed UTF-8 cases must fail as LaTeX
# errors without panicking in the byte-oriented input reader.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/aitex"

[ -x "$BIN" ] || { echo "source encodings: build first: cargo build --release -p aitex-cli"; exit 2; }
for command in latex pdflatex dvitype gawk mutool; do
  command -v "$command" >/dev/null || { echo "source encodings: $command not found"; exit 2; }
done

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/ours" "$TMP/ref"
"$ROOT/tools/mkfmt.sh" "$TMP/ours" >/dev/null 2>&1 || {
  echo "source encodings: could not build format"; exit 1;
}

header() {
  local encoding=$1
  printf '\\documentclass{article}\n'
  printf '\\usepackage[T1]{fontenc}\n'
  printf '\\usepackage[%s]{inputenc}\n' "$encoding"
  printf '\\usepackage{lmodern,textcomp}\n'
  printf '\\pagestyle{empty}\n'
  printf '\\begin{document}\n'
}

case_payload() {
  case "$1" in
    utf8)   printf '%b\n' 'UTF-8: R\303\251sum\303\251; na\303\257ve; \342\200\234quotes\342\200\235; \342\200\224; \342\202\254; \305\222uvre.' ;;
    latin1) printf '%b\n' 'Latin-1: R\351sum\351; na\357ve; \243; \251; Stra\337e.' ;;
    latin9) printf '%b\n' 'Latin-9: R\351sum\351; \244; \274uvre; c\275ur; \276.' ;;
    cp1252) printf '%b\n' 'CP1252: R\351sum\351; \200; \223quotes\224; \227; \214uvre; c\234ur.' ;;
  esac
}

normalize_dvi() {
  LC_ALL=C dvitype -output-level=4 -page-start='*' "$1" 2>/dev/null |
    LC_ALL=C gawk -f "$ROOT/tools/dvi_semantic.awk" |
    grep -v '^special '
}

passed=0
for encoding in utf8 latin1 latin9 cp1252; do
  if ! kpsewhich "$encoding.def" >/dev/null 2>&1; then
    echo "source encodings: required $encoding.def is not installed"
    exit 1
  fi
  {
    header "$encoding"
    case_payload "$encoding"
    printf '\\end{document}\n'
  } > "$TMP/ref/$encoding.tex"
  cp "$TMP/ref/$encoding.tex" "$TMP/ours/$encoding.tex"

  ( cd "$TMP/ref" && timeout 300 latex -interaction=nonstopmode -halt-on-error \
      "$encoding.tex" >/dev/null 2>&1 ) || {
    echo "source encodings: reference LaTeX rejected $encoding"; exit 1;
  }
  ( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
      -jobname="$encoding-pdf" "$encoding.tex" >/dev/null 2>&1 ) || {
    echo "source encodings: reference pdfLaTeX rejected $encoding"; exit 1;
  }
  ( cd "$TMP/ours" && timeout 300 "$BIN" pass "$encoding.tex" --fmt fmt.afmt \
      -halt-on-error -o "$encoding.dvi" >/dev/null 2>"$encoding-dvi.err" ) || {
    echo "source encodings: aiTeX rejected $encoding DVI input"
    sed -n '1,12p' "$TMP/ours/$encoding-dvi.err"
    exit 1
  }
  ( cd "$TMP/ours" && timeout 300 "$BIN" pass "$encoding.tex" --fmt fmt.afmt \
      -halt-on-error -o "$encoding.pdf" >/dev/null 2>"$encoding-pdf.err" ) || {
    echo "source encodings: aiTeX rejected $encoding PDF input"
    sed -n '1,12p' "$TMP/ours/$encoding-pdf.err"
    exit 1
  }

  normalize_dvi "$TMP/ref/$encoding.dvi" > "$TMP/ref/$encoding.draws"
  normalize_dvi "$TMP/ours/$encoding.dvi" > "$TMP/ours/$encoding.draws"
  if ! cmp -s "$TMP/ref/$encoding.draws" "$TMP/ours/$encoding.draws"; then
    echo "source encodings: $encoding DVI drawing semantics differ"
    diff -u "$TMP/ref/$encoding.draws" "$TMP/ours/$encoding.draws" | sed -n '1,24p'
    exit 1
  fi

  mutool draw -q -F txt -o "$TMP/ref/$encoding.txt" "$TMP/ref/$encoding-pdf.pdf"
  mutool draw -q -F txt -o "$TMP/ours/$encoding.txt" "$TMP/ours/$encoding.pdf"
  if ! cmp -s "$TMP/ref/$encoding.txt" "$TMP/ours/$encoding.txt"; then
    echo "source encodings: $encoding PDF text extraction differs"
    diff -u "$TMP/ref/$encoding.txt" "$TMP/ours/$encoding.txt" | sed -n '1,24p'
    exit 1
  fi
  if ! grep -qi "${encoding%%[0-9]*}" "$TMP/ours/$encoding.txt"; then
    echo "source encodings: $encoding extracted no identifying text"
    exit 1
  fi
  passed=$((passed + 1))
  echo "OK source $encoding: positioned DVI and exact extracted PDF text"
done

# These are syntactically invalid UTF-8 byte strings after inputenc has made
# the leading bytes active.  Both engines must report a TeX/LaTeX error.  The
# aiTeX process must neither panic nor leave a misleading PDF artifact.
malformed=0
for specimen in bad-continuation truncated-sequence; do
  {
    header utf8
    case "$specimen" in
      bad-continuation)   printf '%b\n' 'Bad UTF-8: \303(' ;;
      truncated-sequence) printf '%b' 'Truncated UTF-8: \342\202' ;;
    esac
    printf '\n\\end{document}\n'
  } > "$TMP/ref/$specimen.tex"
  cp "$TMP/ref/$specimen.tex" "$TMP/ours/$specimen.tex"

  if ( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
      "$specimen.tex" >/dev/null 2>&1 ); then
    echo "source encodings: malformed reference case $specimen unexpectedly succeeded"
    exit 1
  fi
  if ( cd "$TMP/ours" && timeout 300 "$BIN" pass "$specimen.tex" --fmt fmt.afmt \
      -halt-on-error -o "$specimen.pdf" >"$specimen.out" 2>"$specimen.err" ); then
    echo "source encodings: malformed aiTeX case $specimen unexpectedly succeeded"
    exit 1
  fi
  if grep -qiE 'panicked at|not valid UTF-8|thread .* panicked|stack overflow' \
      "$TMP/ours/$specimen.out" "$TMP/ours/$specimen.err"; then
    echo "source encodings: malformed case $specimen panicked below TeX's error layer"
    sed -n '1,12p' "$TMP/ours/$specimen.err"
    exit 1
  fi
  if [ -e "$TMP/ours/$specimen.pdf" ]; then
    echo "source encodings: malformed case $specimen left a PDF artifact"
    exit 1
  fi
  malformed=$((malformed + 1))
  echo "OK malformed $specimen: explicit error, no panic or artifact"
done

[ "$passed" -eq 4 ] && [ "$malformed" -eq 2 ] || {
  echo "source encodings: a loop examined no cases"; exit 1;
}
echo "source encodings pass: $passed byte encodings, $malformed malformed cases"
