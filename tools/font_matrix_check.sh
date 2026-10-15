#!/usr/bin/env bash
# Exercise every classic LaTeX font encoding available in the locked image.
# The DVI font names prove which encoding-specific metrics were actually used;
# positioned drawing parity, embedded programs, ToUnicode maps, extraction,
# and raster comparison grade the complete route to the final PDF.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/aitex"
FIXTURE="$ROOT/tests/pdf/font-matrix.tex"
OVERLAY_FIXTURE="$ROOT/tests/pdf/overlay-fonts.tex"
TYPEWRITER_FIXTURE="$ROOT/tests/pdf/typewriter-bitmap.tex"
OVERLAY="$ROOT/corpus/texlive-overlay/root/usr/share/texlive/texmf-dist"

[ -x "$BIN" ] || { echo "font matrix: build first: cargo build --release -p aitex-cli"; exit 2; }
[ -f "$FIXTURE" ] || { echo "font matrix: missing fixture"; exit 2; }
[ -f "$OVERLAY_FIXTURE" ] || { echo "font matrix: missing overlay fixture"; exit 2; }
[ -f "$TYPEWRITER_FIXTURE" ] || { echo "font matrix: missing typewriter fixture"; exit 2; }
[ -d "$OVERLAY" ] || { echo "font matrix: locked TeX Live overlay is missing"; exit 2; }
for command in latex pdflatex dvitype gawk mutool; do
  command -v "$command" >/dev/null || { echo "font matrix: $command not found"; exit 2; }
done

required_files='ot1enc.def t1enc.def ts1enc.def omlenc.def omsenc.def ot1lmr.fd t1lmr.fd ts1lmr.fd omllmm.fd omslmsy.fd omxlmex.fd ulasy.fd'
for file in $required_files; do
  kpsewhich "$file" >/dev/null 2>&1 || {
    echo "font matrix: required TeX Live file $file is not installed"; exit 1;
  }
done

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
TMP="$(mktemp -d)"
if [ "${KEEP_TMP:-0}" = 1 ]; then
  echo "font matrix work directory: $TMP"
else
  trap 'rm -rf "$TMP"' EXIT
fi
mkdir -p "$TMP/ours" "$TMP/ref"
cp "$FIXTURE" "$TMP/ours/"
cp "$FIXTURE" "$TMP/ref/"
cp "$OVERLAY_FIXTURE" "$TMP/ours/"
cp "$OVERLAY_FIXTURE" "$TMP/ref/"
cp "$TYPEWRITER_FIXTURE" "$TMP/ours/"
cp "$TYPEWRITER_FIXTURE" "$TMP/ref/"
"$ROOT/tools/mkfmt.sh" "$TMP/ours" >/dev/null 2>&1 || {
  echo "font matrix: could not build format"; exit 1;
}

( cd "$TMP/ref" && timeout 300 latex -interaction=nonstopmode -halt-on-error \
    font-matrix.tex >/dev/null 2>&1 ) || {
  echo "font matrix: reference LaTeX rejected the fixture"; exit 1;
}
( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    font-matrix.tex >/dev/null 2>&1 ) || {
  echo "font matrix: reference pdfLaTeX rejected the fixture"; exit 1;
}
( cd "$TMP/ours" && timeout 300 "$BIN" pass font-matrix.tex --fmt fmt.afmt \
    -halt-on-error -o font-matrix.dvi >/dev/null 2>dvi.err ) || {
  echo "font matrix: aiTeX rejected the DVI fixture"
  sed -n '1,80p' "$TMP/ours/dvi.err"
  exit 1
}
( cd "$TMP/ours" && timeout 300 "$BIN" pass font-matrix.tex --fmt fmt.afmt \
    -halt-on-error -o font-matrix.pdf >/dev/null 2>pdf.err ) || {
  echo "font matrix: aiTeX rejected the PDF fixture"
  sed -n '1,80p' "$TMP/ours/pdf.err"
  exit 1
}

normalize_dvi() {
  LC_ALL=C dvitype -output-level=4 -page-start='*' "$1" 2>/dev/null |
    LC_ALL=C gawk -f "$ROOT/tools/dvi_semantic.awk" |
    grep -v '^special '
}
normalize_dvi "$TMP/ref/font-matrix.dvi" > "$TMP/ref/draws"
normalize_dvi "$TMP/ours/font-matrix.dvi" > "$TMP/ours/draws"
if ! cmp -s "$TMP/ref/draws" "$TMP/ours/draws"; then
  echo "font matrix: positioned DVI drawing semantics differ"
  diff -u "$TMP/ref/draws" "$TMP/ours/draws" | sed -n '1,24p'
  exit 1
fi

# These DVI names are a non-vacuous witness for each LaTeX encoding row.  U is
# represented by lasy; OMX does not have an *enc.def because it is declared by
# the LaTeX math kernel, but its lmex metrics are just as observable here.
dvitype -output-level=2 "$TMP/ours/font-matrix.dvi" > "$TMP/ours/dvitype.txt" 2>/dev/null
while read -r encoding font; do
  if ! grep -qE "Font [0-9]+: ${font}---loaded" "$TMP/ours/dvitype.txt"; then
    echo "font matrix: $encoding did not select expected DVI font $font"
    exit 1
  fi
  echo "OK font encoding $encoding: $font"
done <<'EOF'
OT1 rm-lmr10
T1 ec-lmr10
TS1 ts1-lmr10
OML lmmi10
OMS lmsy10
OMX lmex10
U lasy10
EOF

# Compact production PDFs put small dictionaries in object streams. Ask MuPDF
# to enumerate the parsed objects before inspecting structure; grepping either
# the production bytes or `mutool clean` output is sensitive to object-stream
# packing and to whitespace differences between MuPDF releases.
mutool clean -d "$TMP/ours/font-matrix.pdf" "$TMP/ours/clean.pdf" \
  >"$TMP/ours/clean.out" 2>&1
if [ -s "$TMP/ours/clean.out" ]; then
  echo "font matrix: candidate PDF did not validate cleanly"
  sed -n '1,12p' "$TMP/ours/clean.out"
  exit 1
fi
mutool show "$TMP/ours/font-matrix.pdf" grep > "$TMP/ours/objects.txt" \
  2>"$TMP/ours/show.err" || {
  echo "font matrix: could not enumerate candidate PDF objects"
  sed -n '1,12p' "$TMP/ours/show.err"
  exit 1
}

count_objects() {
  { grep -aoE "$1" "$2" || true; } | wc -l | tr -d ' '
}

# Every embedded program is subset, so PDF names carry the standard six-letter
# subset tag. Match the semantic PostScript name while accepting that tag.
for base in LMRoman10-Regular LMMathItalic10-Regular LMMathSymbols10-Regular \
    LMMathExtension10-Regular LASY10; do
  if ! grep -aEq "/BaseFont[[:space:]]*/([A-Z]{6}\+)?$base([^[:alnum:]_.-]|$)" \
      "$TMP/ours/objects.txt"; then
    echo "font matrix: PDF is missing embedded base font $base"
    exit 1
  fi
done
type1_count=$(count_objects '/Subtype[[:space:]]*/Type1([^[:alnum:]_.-]|$)' \
  "$TMP/ours/objects.txt")
unicode_count=$(count_objects '/ToUnicode([^[:alnum:]_.-]|$)' \
  "$TMP/ours/objects.txt")
fontfile_count=$(count_objects '/FontFile([^[:alnum:]_.-]|$)' \
  "$TMP/ours/objects.txt")
if [ "$type1_count" -lt 7 ] || [ "$unicode_count" -ne "$type1_count" ] || \
    [ "$fontfile_count" -lt 1 ] || [ "$fontfile_count" -gt "$type1_count" ]; then
  echo "font matrix: Type1=$type1_count ToUnicode=$unicode_count FontFile=$fontfile_count"
  exit 1
fi

mutool draw -q -F txt -o "$TMP/ref/text.txt" "$TMP/ref/font-matrix.pdf" 2>/dev/null
mutool draw -q -F txt -o "$TMP/ours/text.txt" "$TMP/ours/font-matrix.pdf" 2>/dev/null
python3 - "$TMP/ref/text.txt" "$TMP/ours/text.txt" <<'PYEOF'
import sys

reference, candidate = (open(path, encoding="utf-8").read() for path in sys.argv[1:])
reference = "".join(reference.split())
candidate = "".join(candidate.split())

def invalid(text):
    return sum(
        char == "\ufffd"
        or ord(char) < 32
        or 0x7f <= ord(char) <= 0x9f
        or 0xe000 <= ord(char) <= 0xf8ff
        for char in text
    )

if not candidate or invalid(candidate) > invalid(reference):
    print("font matrix: candidate introduced invalid Unicode extraction")
    raise SystemExit(1)
for label in ("OT1LatinModern:", "T1LatinModern:", "TS1LatinModern:",
              "OML/OMS:", "OMX:", "U/lasy:"):
    if label not in candidate:
        print(f"font matrix: extraction omitted non-vacuous label {label}")
        raise SystemExit(1)
# pdfLaTeX leaves these three LM extension-font glyphs unmapped.  A corrected
# ToUnicode map is deliberately better than byte-for-byte parity with its three
# U+FFFD characters, so require the meaningful sequence explicitly.
if "OMX:n∑i=0xi={11+x2}2." not in candidate:
    print("font matrix: OMX extraction omitted sum or extensible braces")
    raise SystemExit(1)
print(f"OK semantic Unicode extraction; invalid stock={invalid(reference)} candidate={invalid(candidate)}")
PYEOF

mutool draw -q -F pgm -r 144 -o "$TMP/ref/page.pgm" \
  "$TMP/ref/font-matrix.pdf" 2>/dev/null
mutool draw -q -F pgm -r 144 -o "$TMP/ours/page.pgm" \
  "$TMP/ours/font-matrix.pdf" 2>/dev/null
python3 - "$TMP/ref/page.pgm" "$TMP/ours/page.pgm" <<'PYEOF'
import sys

def pgm(path):
    raw = open(path, "rb").read()
    magic, dimensions, maximum, pixels = raw.split(b"\n", 3)
    if magic != b"P5" or maximum != b"255":
        raise ValueError(f"unexpected PGM header in {path}")
    return tuple(map(int, dimensions.split())), pixels

reference_size, reference = pgm(sys.argv[1])
candidate_size, candidate = pgm(sys.argv[2])
if reference_size != candidate_size or len(reference) != len(candidate):
    print(f"font matrix: raster dimensions differ: {reference_size} vs {candidate_size}")
    raise SystemExit(1)
differences = [abs(a - b) for a, b in zip(reference, candidate)]
mean = sum(differences) / len(differences)
large = sum(value > 32 for value in differences) / len(differences)
if mean > 0.75 or large > 0.005:
    print(f"font matrix: raster differs: mean={mean:.4f}, >32={large:.4%}")
    raise SystemExit(1)
print(f"OK classic font raster: mean={mean:.4f}, >32={large:.4%}")
PYEOF

# The corpus overlay contains three paths that the ordinary classic matrix did
# not reach: two unmapped MnSymbol extension fonts and yhmath's extension font
# fall back to PK bitmaps, while bbold10 is a Type 1 program with no internal
# encoding.  Their semantic vectors live in TeX Live's consolidated
# `dvips-all.enc`; failure to consult it used to reject four otherwise-correct
# corpus papers only at the final PDF conversion step.
( cd "$TMP/ref" && TEXMFHOME="$OVERLAY" timeout 300 latex \
    -interaction=nonstopmode -halt-on-error overlay-fonts.tex >/dev/null 2>&1 ) || {
  echo "font matrix: reference LaTeX rejected the overlay-font fixture"; exit 1;
}
( cd "$TMP/ours" && TEXMFHOME="$OVERLAY" timeout 300 "$BIN" pass overlay-fonts.tex \
    --fmt fmt.afmt -halt-on-error -o overlay-fonts.dvi >/dev/null 2>overlay-dvi.err ) || {
  echo "font matrix: aiTeX rejected the overlay-font DVI fixture"
  sed -n '1,80p' "$TMP/ours/overlay-dvi.err"
  exit 1
}
TEXMFHOME="$OVERLAY" normalize_dvi "$TMP/ref/overlay-fonts.dvi" > "$TMP/ref/overlay-draws"
TEXMFHOME="$OVERLAY" normalize_dvi "$TMP/ours/overlay-fonts.dvi" > "$TMP/ours/overlay-draws"
if ! cmp -s "$TMP/ref/overlay-draws" "$TMP/ours/overlay-draws"; then
  echo "font matrix: overlay-font positioned DVI semantics differ"
  diff -u "$TMP/ref/overlay-draws" "$TMP/ours/overlay-draws" | sed -n '1,24p'
  exit 1
fi
( cd "$TMP/ours" && TEXMFHOME="$OVERLAY" timeout 300 "$BIN" pass overlay-fonts.tex \
    --fmt fmt.afmt -halt-on-error -o overlay-fonts.pdf >/dev/null 2>overlay-pdf.err ) || {
  echo "font matrix: aiTeX rejected the overlay-font PDF fixture"
  sed -n '1,80p' "$TMP/ours/overlay-pdf.err"
  exit 1
}
mutool clean -d "$TMP/ours/overlay-fonts.pdf" "$TMP/ours/overlay-clean.pdf" \
  >"$TMP/ours/overlay-clean.out" 2>&1
if [ -s "$TMP/ours/overlay-clean.out" ]; then
  echo "font matrix: overlay-font PDF did not validate cleanly"
  sed -n '1,12p' "$TMP/ours/overlay-clean.out"
  exit 1
fi
mutool show "$TMP/ours/overlay-fonts.pdf" grep > "$TMP/ours/overlay-objects.txt" \
  2>"$TMP/ours/overlay-show.err" || {
  echo "font matrix: could not enumerate overlay PDF objects"
  sed -n '1,12p' "$TMP/ours/overlay-show.err"
  exit 1
}
for name in MnSymbolE9 MnSymbolE10 yrcmex10; do
  grep -aEq "/Name[[:space:]]*/$name([^[:alnum:]_.-]|$)" \
      "$TMP/ours/overlay-objects.txt" || {
    echo "font matrix: overlay PDF is missing bitmap font $name"; exit 1;
  }
done
# A host pdftex.map supplies BBOLD10; the locked overlay intentionally tests
# the no-map path, where the same program keeps its TeX name bbold10.
grep -aEq '/BaseFont[[:space:]]*/([A-Z]{6}\+)?(BBOLD10|bbold10)([^[:alnum:]_.-]|$)' \
    "$TMP/ours/overlay-objects.txt" || {
  echo "font matrix: overlay PDF is missing Type 1 font bbold10"; exit 1;
}
overlay_type3=$(count_objects '/Subtype[[:space:]]*/Type3([^[:alnum:]_.-]|$)' \
  "$TMP/ours/overlay-objects.txt")
overlay_unicode=$(count_objects '/ToUnicode([^[:alnum:]_.-]|$)' \
  "$TMP/ours/overlay-objects.txt")
if [ "$overlay_type3" -lt 3 ] || [ "$overlay_unicode" -lt 5 ]; then
  echo "font matrix: overlay Type3=$overlay_type3 ToUnicode=$overlay_unicode"
  exit 1
fi
echo "OK overlay fonts: 3 semantic PK Type 3 fonts and encoded BBOLD10 Type 1"

# Generated EC/TS1 typewriter bitmaps retain literal printable source bytes in
# their ToUnicode maps. Adobe glyph names describe several visual slots as
# mathematical symbols (for example `minus` at byte `=`), which must not turn
# source-code punctuation into different Unicode scalars during extraction.
( cd "$TMP/ours" && timeout 300 "$BIN" pass typewriter-bitmap.tex \
    --fmt fmt.afmt -halt-on-error -o typewriter-bitmap.pdf \
    >/dev/null 2>typewriter-bitmap.err ) || {
  echo "font matrix: aiTeX rejected the typewriter bitmap fixture"
  sed -n '1,80p' "$TMP/ours/typewriter-bitmap.err"
  exit 1
}
mutool show "$TMP/ours/typewriter-bitmap.pdf" grep \
  > "$TMP/ours/typewriter-objects.txt" 2>"$TMP/ours/typewriter-show.err" || {
  echo "font matrix: could not enumerate typewriter bitmap objects"
  sed -n '1,12p' "$TMP/ours/typewriter-show.err"
  exit 1
}
for name in ectt0800 tctt0800; do
  grep -aEq "/Name[[:space:]]*/$name([^[:alnum:]_.-]|$)" \
      "$TMP/ours/typewriter-objects.txt" || {
    echo "font matrix: typewriter fixture is missing bitmap font $name"; exit 1;
  }
done
mutool draw -q -F txt -o "$TMP/ours/typewriter-text.txt" \
  "$TMP/ours/typewriter-bitmap.pdf" 2>/dev/null
python3 - "$TMP/ours/typewriter-text.txt" <<'PYEOF'
import sys

text = "".join(open(sys.argv[1], encoding="utf-8").read().split())
for marker in ("T1typewriterliteral:=*`", "TS1typewriterliteral:=*`"):
    if marker not in text:
        print(f"font matrix: literal bitmap extraction omitted {marker!r}: {text!r}")
        raise SystemExit(1)
print("OK literal EC/TS1 typewriter bitmap extraction")
PYEOF

optional=0
for entry in 'LY1 ly1enc.def' 'LGR lgrenc.def' 'T2A t2aenc.def' 'T5 t5enc.def'; do
  set -- $entry
  if kpsewhich "$2" >/dev/null 2>&1; then
    echo "font matrix: $1 became available but has no locked fixture"
    exit 1
  fi
  echo "SKIP font encoding $1: $2 is not installed in the locked environment"
  optional=$((optional + 1))
done

[ "$optional" -eq 4 ] || { echo "font matrix: optional inventory examined nothing"; exit 1; }
echo "font matrix pass: 7 installed encodings, $type1_count embedded Type 1 fonts, 4 explicit unavailable rows"
