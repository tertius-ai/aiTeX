#!/usr/bin/env bash
# Compare two DVI files by what a driver draws, not by how the writer chose to
# encode it.  `dvitype` supplies exact DVI-unit positions after resolving TFM
# widths; dvi_semantic.awk removes byte offsets, movement-register choices,
# font numbers and comments while retaining every semantic field.
#
# Usage:
#   tools/dvi_semantic.sh <reference.dvi> <ours.dvi>
#   tools/dvi_semantic.sh --self-test
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NORMALIZER="$ROOT/tools/dvi_semantic.awk"

command -v dvitype >/dev/null || { echo "dvi semantics: dvitype not found"; exit 2; }
command -v gawk >/dev/null || { echo "dvi semantics: gawk not found"; exit 2; }

if [ "${1:-}" = "--self-test" ]; then
  command -v tex >/dev/null || { echo "dvi semantic self-test: tex not found"; exit 2; }
  TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
  mkdir -p "$TMP/ref" "$TMP/same" "$TMP/different" "$TMP/binary-ref" "$TMP/binary-same"
  fixture() { # directory, kern, special
    local d=$1 kern=$2 special=$3
    printf '\\catcode`\\{=1 \\catcode`\\}=2\n\\font\\f=cmr10\n\\shipout\\hbox{\\f A\\kern%s\\vrule height2pt width3pt\\special{%s}}\n\\end\n' \
      "$kern" "$special" > "$d/a.tex"
    ( cd "$d" && tex -ini -interaction=batchmode a.tex >/dev/null 2>&1 )
    [ -s "$d/a.dvi" ] || return 2
  }
  fixture "$TMP/ref" 1pt same || { echo "dvi semantic self-test: reference failed"; exit 2; }
  fixture "$TMP/same" 1pt same || { echo "dvi semantic self-test: twin failed"; exit 2; }
  fixture "$TMP/different" 2pt changed || { echo "dvi semantic self-test: variant failed"; exit 2; }
  fixture "$TMP/binary-ref" 1pt 'xpacket^^ffdata' || {
    echo "dvi semantic self-test: binary-special reference failed"; exit 2; }
  fixture "$TMP/binary-same" 1pt 'xpacket^^ffdata' || {
    echo "dvi semantic self-test: binary-special twin failed"; exit 2; }
  "$0" "$TMP/ref/a.dvi" "$TMP/same/a.dvi" >/dev/null || {
    echo "dvi semantic self-test: equivalent files were rejected"; exit 1; }
  if "$0" "$TMP/ref/a.dvi" "$TMP/different/a.dvi" >/dev/null; then
    echo "dvi semantic self-test: shifted rule/special was not detected"; exit 1
  fi
  "$0" "$TMP/binary-ref/a.dvi" "$TMP/binary-same/a.dvi" >/dev/null || {
    echo "dvi semantic self-test: equivalent binary specials were rejected"; exit 1; }
  echo "dvi semantic self-test: equivalent, divergent, and binary-special fixtures detected"
  exit 0
fi

[ $# -eq 2 ] || { echo "usage: tools/dvi_semantic.sh <reference.dvi> <ours.dvi>"; exit 2; }
REF=$1
OURS=$2
[ -s "$REF" ] || { echo "dvi semantics: missing reference DVI: $REF"; exit 2; }
[ -s "$OURS" ] || { echo "dvi semantics: missing candidate DVI: $OURS"; exit 2; }

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
normalize() { # DVI, destination
  # A DVI can name project-local TFM files. Run dvitype beside the artifact,
  # just as a driver invoked on that project would; running from the aiTeX
  # checkout made valid corpus outputs look non-comparable whenever their TFM
  # was generated or vendored locally.
  local dvi=$1 destination=$2 directory filename
  directory=$(cd "$(dirname "$dvi")" && pwd) || return 2
  filename=$(basename "$dvi")
  local search=$directory texmf_var=
  while [ "$search" != / ]; do
    if [ -d "$search/.texrepair-state/texmf-var" ]; then
      texmf_var="$search/.texrepair-state/texmf-var"
      break
    fi
    search=$(dirname "$search")
  done
  (
    cd "$directory" || exit 2
    export LC_ALL=C
    if [ -n "$texmf_var" ]; then export TEXMFVAR="$texmf_var"; fi
    exec dvitype -output-level=4 -page-start='*' "$filename"
  ) \
    2>"$destination.err" \
    | LC_ALL=C gawk -f "$NORMALIZER" > "$2"
  local status=("${PIPESTATUS[@]}")
  # DVI xxx payloads are byte strings, but dvitype diagnoses bytes outside
  # printable ASCII even after it has decoded the complete, otherwise valid
  # file.  PDF/XMP specials routinely contain such bytes.  The normalizer has
  # a complete postamble/page check, so this one known dvitype limitation is a
  # warning; any other decoder failure still makes the artifact unavailable.
  local known_binary_special=0
  if [ "${status[0]}" -ne 0 ] &&
     grep -q 'non-ASCII character in xxx command' "$destination.err"; then
    known_binary_special=1
  fi
  if { [ "${status[0]}" -ne 0 ] && [ "$known_binary_special" -ne 1 ]; } ||
     [ "${status[1]}" -ne 0 ] || [ ! -s "$2" ]; then
    sed -n '1,5p' "$destination.err" >&2
    return 2
  fi
}

normalize "$REF" "$TMP/ref" || exit 2
normalize "$OURS" "$TMP/ours" || exit 2

ref_n=$(wc -l < "$TMP/ref" | tr -d ' ')
ours_n=$(wc -l < "$TMP/ours" | tr -d ' ')
chg=$(diff "$TMP/ref" "$TMP/ours" | grep -c '^<' || true)
matched=$((ref_n - chg))
pct=$((matched * 100 / ref_n))
printf 'trip.dvi vs this machine: %s of %s semantic records match (%s%%); ours %s records\n' \
  "$matched" "$ref_n" "$pct" "$ours_n"

if cmp -s "$TMP/ref" "$TMP/ours"; then
  echo "trip.dvi semantic comparison: exact"
  exit 0
fi

i=$(gawk 'NR==FNR { a[FNR]=$0; n=FNR; next }
          { if (FNR > n || a[FNR] != $0) { print FNR; found=1; exit } }
          END { if (!found) print n + 1 }' "$TMP/ref" "$TMP/ours")
ref_line=$(sed -n "${i}p" "$TMP/ref")
ours_line=$(sed -n "${i}p" "$TMP/ours")
printf 'first DVI semantic difference at record %s:\n  tex:  %s\n  ours: %s\n' \
  "$i" "${ref_line:-<end of stream>}" "${ours_line:-<end of stream>}"
exit 1
