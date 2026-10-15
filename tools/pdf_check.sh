#!/usr/bin/env bash
# End-to-end qualification of the page-event PDF path used by real packages.
#
# The fixture deliberately takes the default dvips route: graphicx emits a
# PSfile special, xcolor emits a scoped color stack, and hyperref emits pdfmark
# destinations, URI/internal annotations, an outline and document metadata.
# One EPS stays inside the native vector subset; the other uses a PostScript
# procedure and therefore proves the CLI's Ghostscript raster fallback.
# Passing means both that the engine's DVI still places every glyph and rule
# where pdfTeX-in-DVI-mode does, that direct page events match an independent
# parse of those DVI bytes, and that the PDF driver materializes the specials
# into independently readable PDF objects.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/aitex"
FIXTURE="$ROOT/tests/pdf"

[ -x "$BIN" ] || { echo "pdf features: build first: cargo build --release -p aitex-cli"; exit 2; }
[ -f "$FIXTURE/driver-features.tex" ] || { echo "pdf features: missing fixture"; exit 2; }
command -v latex >/dev/null || { echo "pdf features: latex not found"; exit 2; }
command -v pdflatex >/dev/null || { echo "pdf features: pdflatex not found"; exit 2; }
command -v dvitype >/dev/null || { echo "pdf features: dvitype not found"; exit 2; }
command -v gawk >/dev/null || { echo "pdf features: gawk not found"; exit 2; }
command -v gs >/dev/null || { echo "pdf features: ghostscript not found"; exit 2; }
command -v mutool >/dev/null || { echo "pdf features: mutool not found"; exit 2; }
command -v pdffonts >/dev/null || { echo "pdf features: pdffonts not found"; exit 2; }
command -v convert >/dev/null || { echo "pdf features: ImageMagick not found"; exit 2; }
command -v extractbb >/dev/null || { echo "pdf features: extractbb not found"; exit 2; }
command -v dvipdfmx >/dev/null || { echo "pdf features: dvipdfmx not found"; exit 2; }

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
TMP="$(mktemp -d)"
if [ -n "${PDF_CHECK_KEEP:-}" ]; then
  echo "pdf features: keeping $TMP"
else
  trap 'rm -rf "$TMP"' EXIT
fi
mkdir -p "$TMP/ours" "$TMP/ref"
cp "$FIXTURE/driver-features.tex" "$FIXTURE/driver-image.eps" \
  "$FIXTURE/driver-procedure.eps" "$FIXTURE/xypic-driver.tex" \
  "$FIXTURE/pdfpages-source.tex" "$FIXTURE/pdfpages-driver.tex" \
  "$FIXTURE/font-encodings.tex" "$FIXTURE/bitmap-font.tex" \
  "$FIXTURE/unmapped-font.tex" "$FIXTURE/advanced-features.tex" \
  "$FIXTURE/issue-26-regressions.tex" "$FIXTURE/classic-face-sharing.tex" \
  "$FIXTURE/pdfx-shellescape.tex" "$FIXTURE/explicit-pdftex-graphics.tex" \
  "$FIXTURE/pdf-page-size.tex" \
  "$TMP/ours/"
cp "$FIXTURE/raster-formats.tex" "$TMP/ours/"
cp "$FIXTURE/driver-features.tex" "$FIXTURE/driver-image.eps" \
  "$FIXTURE/driver-procedure.eps" "$FIXTURE/xypic-driver.tex" \
  "$FIXTURE/pdfpages-source.tex" "$FIXTURE/pdfpages-driver.tex" \
  "$FIXTURE/font-encodings.tex" "$FIXTURE/bitmap-font.tex" \
  "$FIXTURE/unmapped-font.tex" "$FIXTURE/advanced-features.tex" \
  "$FIXTURE/issue-26-regressions.tex" "$FIXTURE/classic-face-sharing.tex" \
  "$FIXTURE/pdfx-shellescape.tex" "$FIXTURE/explicit-pdftex-graphics.tex" \
  "$FIXTURE/pdf-page-size.tex" \
  "$TMP/ref/"
cp "$FIXTURE/raster-formats.tex" "$TMP/ref/"

# Generate compact deterministic raster assets inside the disposable fixture
# tree. The alpha image is deliberately translucent over a yellow TeX box, so
# flattening it onto white is a visible failure rather than nominal coverage.
convert -size 120x80 xc:none \
  -fill 'rgba(10,70,235,0.58)' -draw 'circle 58,40 91,40' \
  -fill 'rgba(230,25,70,0.82)' -draw 'rectangle 12,12 72,33' \
  "$TMP/ours/alpha.png"
convert -size 120x80 gradient:'#2050d0-#e09020' -alpha on \
  PNG32:"$TMP/ours/opaque-alpha.png"
convert -size 120x80 gradient:'#d02020-#20d060' -quality 88 \
  "$TMP/ours/colour.jpg"
cp "$TMP/ours/alpha.png" "$TMP/ours/opaque-alpha.png" \
  "$TMP/ours/colour.jpg" "$TMP/ref/"
( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    -jobname='figure with space' pdfpages-source.tex >/dev/null 2>&1 )
cp "$TMP/ref/figure with space.pdf" "$TMP/ours/figure with space.pdf"
"$ROOT/tools/mkfmt.sh" "$TMP/ours" >/dev/null 2>&1 || {
  echo "pdf features: could not build format"; exit 1; }

# An explicit historic `[pdftex]` package option must not bypass aiTeX's image
# transport. Compare the extracted text as a compact assertion that the vector
# PDF page was imported between both surrounding text runs.
( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    -jobname=explicit-reference explicit-pdftex-graphics.tex >/dev/null 2>&1 )
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass explicit-pdftex-graphics.tex --fmt fmt.afmt \
      -halt-on-error -o explicit-ours.pdf >/dev/null 2>explicit-ours.err )
mutool draw -q -F txt -o "$TMP/ref/explicit.txt" "$TMP/ref/explicit-reference.pdf"
mutool draw -q -F txt -o "$TMP/ours/explicit.txt" "$TMP/ours/explicit-ours.pdf"
python3 - "$TMP/ref/explicit.txt" "$TMP/ours/explicit.txt" <<'PYEOF'
import sys

def words(path):
    return open(path, encoding='utf-8').read().replace('\f', ' ').split()

reference = words(sys.argv[1])
candidate = words(sys.argv[2])
if candidate != reference:
    raise SystemExit(
        f"pdf features: explicit pdftex graphic text differs: {candidate!r} "
        f"vs {reference!r}"
    )
print("OK explicit pdftex graphic option uses aiTeX's qualified image transport")
PYEOF

# A zero pdfTeX page dimension is an automatic-size request, not a request to
# fall back to Letter. Compare the resulting MediaBox so cropped documents
# such as `standalone` retain their per-page geometry.
( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    -jobname=page-size-reference pdf-page-size.tex >/dev/null 2>&1 )
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass pdf-page-size.tex --fmt fmt.afmt \
      -halt-on-error -o page-size-ours.pdf >/dev/null 2>page-size-ours.err )
mutool info "$TMP/ref/page-size-reference.pdf" >"$TMP/ref/page-size.info"
mutool info "$TMP/ours/page-size-ours.pdf" >"$TMP/ours/page-size.info"
python3 - "$TMP/ref/page-size.info" "$TMP/ours/page-size.info" <<'PYEOF'
import re
import sys

def media_boxes(path):
    text = open(path, encoding='utf-8').read()
    return [
        tuple(round(float(value), 2) for value in match.split())
        for match in re.findall(r'\[\s*0\s+0\s+([^]]+)\]', text)
    ]

reference = media_boxes(sys.argv[1])
candidate = media_boxes(sys.argv[2])
if candidate != reference or len(candidate) != 1:
    raise SystemExit(
        f"pdf features: automatic page dimensions differ: {candidate!r} "
        f"vs {reference!r}"
    )
print("OK zero pdfTeX page dimensions derive the stock cropped MediaBox")
PYEOF

# pdfx must observe aiTeX's pdfTeX-compatible \pdfcreationdate and avoid its
# XeTeX-style generic \shellescape fallback entirely. Exercise both exactly
# one pass and an ordinary clean complete build.
mkdir -p "$TMP/pdfx-one" "$TMP/pdfx-full"
cp "$FIXTURE/pdfx-shellescape.tex" "$TMP/pdfx-one/"
cp "$FIXTURE/pdfx-shellescape.tex" "$TMP/pdfx-full/"
for _pass in 1 2; do
  ( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
      pdfx-shellescape.tex >/dev/null 2>&1 )
done
( cd "$TMP/pdfx-one" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass pdfx-shellescape.tex --fmt "$TMP/ours/fmt.afmt" \
      -halt-on-error -o pdfx-one.pdf >/dev/null 2>pdfx-one.err )
( cd "$TMP/pdfx-full" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pdfx-shellescape.tex --fmt "$TMP/ours/fmt.afmt" \
      --no-cache -halt-on-error -o pdfx-full.pdf >/dev/null 2>pdfx-full.err )
for mode in one full; do
  directory="$TMP/pdfx-$mode"
  pdf="$directory/pdfx-$mode.pdf"
  if [ -e "$directory/creationdate.timestamp" ]; then
    echo "pdf features: pdfx $mode build unexpectedly executed its Lua helper"
    exit 1
  fi
  if [ -e "$directory/creationdate.lua" ]; then
    echo "pdf features: pdfx $mode build unexpectedly entered its Lua fallback"
    exit 1
  fi
  mutool clean -d "$pdf" "$directory/pdfx-clean.pdf" \
    >"$directory/pdfx-mutool.out" 2>&1
  gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage "$pdf" \
    >"$directory/pdfx-gs.out" 2>&1
  if [ -s "$directory/pdfx-mutool.out" ] || [ -s "$directory/pdfx-gs.out" ]; then
    echo "pdf features: pdfx $mode PDF failed an independent structural check"
    sed -n '1,12p' "$directory/pdfx-mutool.out"
    sed -n '1,12p' "$directory/pdfx-gs.out"
    exit 1
  fi
  mutool draw -q -F txt -o "$directory/pdfx.txt" "$pdf"
  mutool draw -q -F ppm -r 72 -o "$directory/pdfx.ppm" "$pdf"
done
mutool draw -q -F txt -o "$TMP/ref/pdfx.txt" "$TMP/ref/pdfx-shellescape.pdf"
mutool draw -q -F ppm -r 72 -o "$TMP/ref/pdfx.ppm" "$TMP/ref/pdfx-shellescape.pdf"
python3 - "$TMP/ref/pdfx.txt" "$TMP/ref/pdfx.ppm" \
  "$TMP/pdfx-one/pdfx.txt" "$TMP/pdfx-one/pdfx.ppm" \
  "$TMP/pdfx-full/pdfx.txt" "$TMP/pdfx-full/pdfx.ppm" <<'PYEOF'
import sys

def text(path):
    return ' '.join(open(path, encoding='utf-8').read().replace('\f', ' ').split())

def ppm(path):
    raw = open(path, 'rb').read()
    magic, dimensions, maximum, pixels = raw.split(b'\n', 3)
    if magic != b'P6' or maximum != b'255':
        raise ValueError(f'unexpected PPM header in {path}')
    return tuple(map(int, dimensions.split())), pixels

reference_text = text(sys.argv[1])
reference_size, reference = ppm(sys.argv[2])
if 'SHELL-ESCAPE-ZERO' not in reference_text:
    raise SystemExit('pdf features: pdfx reference marker is absent')
for mode, text_path, raster_path in [
    ('one-shot', sys.argv[3], sys.argv[4]),
    ('complete', sys.argv[5], sys.argv[6]),
]:
    candidate_text = text(text_path)
    if candidate_text != reference_text:
        raise SystemExit(
            f'pdf features: pdfx {mode} text differs: {candidate_text!r} '
            f'vs {reference_text!r}'
        )
    candidate_size, candidate = ppm(raster_path)
    if candidate_size != reference_size or len(candidate) != len(reference):
        raise SystemExit(
            f'pdf features: pdfx {mode} raster dimensions differ: '
            f'{candidate_size} vs {reference_size}'
        )
    differences = [abs(a - b) for a, b in zip(reference, candidate)]
    mean = sum(differences) / (255 * len(differences))
    large = sum(value > 32 for value in differences) / len(differences)
    if mean > 0.02 or large > 0.05:
        raise SystemExit(
            f'pdf features: pdfx {mode} raster differs: '
            f'mae={mean:.6f}, >32={large:.4%}'
        )
print('OK pdfx one-shot/complete output with disabled shell escape and visual parity')
PYEOF

# Two passes resolve the internal links and section bookmark in both engines.
for _pass in 1 2; do
  ( cd "$TMP/ref" && timeout 300 latex -interaction=nonstopmode -output-format=dvi \
      driver-features.tex >/dev/null 2>&1 )
  ( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
      AITEX_PDF_DIFFERENTIAL=1 timeout 300 "$BIN" pass driver-features.tex \
      --fmt fmt.afmt -o features.pdf \
      >/dev/null 2>aitex.err )
done
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass driver-features.tex --fmt fmt.afmt -o features.dvi \
    >/dev/null 2>aitex-dvi.err )

ref_errors=$(grep -c '^!' "$TMP/ref/driver-features.log" 2>/dev/null || true)
our_errors=$(grep -c '^!' "$TMP/ours/driver-features.log" 2>/dev/null || true)
if [ "${ref_errors:-0}" -ne 0 ] || [ "${our_errors:-0}" -ne 0 ]; then
  echo "pdf features: LaTeX errors ours=${our_errors:-0} reference=${ref_errors:-0}"
  exit 1
fi

# Compare the DVI drawing semantics while intentionally excluding specials:
# the candidate has one redundant xcolor pop that restores black to black, and
# the PDF checks below grade the special semantics directly.
normalize_draws() {
  LC_ALL=C dvitype -output-level=4 -page-start='*' "$1" 2>/dev/null |
    LC_ALL=C gawk -f "$ROOT/tools/dvi_semantic.awk" |
    grep -v '^special '
}
normalize_draws "$TMP/ref/driver-features.dvi" > "$TMP/ref.draws"
normalize_draws "$TMP/ours/features.dvi" > "$TMP/ours.draws"
if ! cmp -s "$TMP/ref.draws" "$TMP/ours.draws"; then
  echo "pdf features: package document DVI placement differs from pdfTeX"
  diff -u "$TMP/ref.draws" "$TMP/ours.draws" | sed -n '1,24p'
  exit 1
fi

dvi_chars=$(dvitype -output-level=2 -page-start='*' "$TMP/ours/features.dvi" 2>/dev/null |
  grep -cE 'setchar[0-9]+|set_char_[0-9]+' || true)

mutool clean -d "$TMP/ours/features.pdf" "$TMP/ours/features-clean.pdf" \
  >"$TMP/ours/features-mutool.out" 2>&1
if [ -s "$TMP/ours/features-mutool.out" ]; then
  echo "pdf features: mutool rejected feature PDF"
  sed -n '1,12p' "$TMP/ours/features-mutool.out"
  exit 1
fi
python3 - "$TMP/ours/features-clean.pdf" "${dvi_chars:-0}" <<'PYEOF'
import re, sys

path, dvi_chars = sys.argv[1], int(sys.argv[2])
try:
    pdf = open(path, 'rb').read()
except OSError as error:
    print(f"pdf features: cannot read output ({error})")
    raise SystemExit(1)

def require(condition, message):
    if not condition:
        print(f"pdf features: {message}")
        raise SystemExit(1)

require(pdf.startswith(b'%PDF-'), "output has no PDF header")
require(
    re.search(rb'/Type\s*/Pages\s*/Count\s+2\b', pdf),
    "page tree is not two pages",
)
require(
    re.search(
        rb'/MediaBox\s*\[\s*0(?:\.0+)?\s+0(?:\.0+)?\s+'
        rb'612(?:\.0+)?\s+792(?:\.0+)?\s*\]',
        pdf,
    ),
    "dvips paper size was not applied",
)

# Every xref entry must land at the object it names.
sx = pdf.rfind(b'startxref')
try:
    start = int(pdf[sx + 9:].split()[0])
    rows = pdf[start:].split(b'\n')
    count = int(rows[1].split()[1])
    bad = 0
    for obj in range(1, count):
        offset, generation, state = rows[obj + 2].split()[:3]
        if state == b'n' and not pdf[int(offset):].startswith(
            b'%d %d obj' % (obj, int(generation))
        ):
            bad += 1
except Exception as error:
    print(f"pdf features: unreadable xref ({error})")
    raise SystemExit(1)
require(bad == 0, f"{bad} xref offsets do not address their objects")

# Count only page-content text strings, never bytes coincidentally present in
# an embedded Type 1 program or the EPS form stream.
glyphs = 0
for stream in re.finditer(rb'<<([^>]*)>>\nstream\n(.*?)\nendstream', pdf, re.S):
    dictionary, body = stream.groups()
    if (b'/Length1' in dictionary or b'/Subtype /Form' in dictionary or
            b'/Subtype /Image' in dictionary):
        continue
    if b'TJ' not in body and b'Tj' not in body:
        continue
    for literal in re.finditer(rb'\((.*?)(?<!\\)\)', body, re.S):
        glyphs += len(re.sub(rb'\\[0-7]{3}|\\.', b'.', literal.group(1)))
require(glyphs == dvi_chars, f"PDF draws {glyphs} glyphs but DVI sets {dvi_chars}")

# xcolor: explicit red body text plus hyperref's magenta and blue links.
for operator, name in [
    (b'1 0 0 rg', 'red'),
    (b'1 0 1 rg', 'magenta'),
    (b'0 0 1 rg', 'blue'),
]:
    require(operator in pdf, f"{name} color operation is absent")

# graphicx: the simple EPS stays a native vector form. A procedure-heavy EPS
# is normalized by Ghostscript and imported as a PDF form when possible, with
# a bounded image fallback for inputs that cannot remain vector. Both resources
# must be invoked from page content whichever qualified route was available.
require(b'/Subtype /Form' in pdf and
        len(re.findall(rb'/(?:Im|Fm)[0-9]+ Do', pdf)) == 2,
        "graphicx resources are absent or not both painted")
require(b'1 0.85 0.2 rg' in pdf and b'0.1 0.2 0.8 rg' in pdf,
        "EPS drawing operations did not reach the form")
if b'/Subtype /Image' in pdf:
    require(re.search(
                rb'/Subtype\s*/Image\s*/Width\s+240\s*/Height\s+120', pdf
            ),
            "procedure-heavy EPS fallback did not use Ghostscript's cropped raster")

# hyperref: two first-page links plus a second-page return link, URI action,
# internal target, named destinations, bookmark and untruncated metadata.
links = len(re.findall(rb'/Subtype /Link\b', pdf))
require(links == 3, f"expected 3 link annotations, found {links}")
require(b'/URI (https://example.com/a?b=1)' in pdf, "external URI action is absent")
require(b'/Dest (section.1)' in pdf, "internal link destination is absent")
require(re.search(rb'/Names\s*<<\s*/Dests', pdf) and
        re.search(rb'\(section\.1\)\s*\[', pdf),
        "named destination tree is absent")
require(b'/Type /Outlines' in pdf and b'<FEFF005400610072006700650074>' in pdf,
        "section bookmark is absent")
require(b'/Info ' in pdf and
        b'<FEFF006100690054006500580020006400720069007600650072002000660065006100740075007200650073>' in pdf,
        "UTF-16 document title is absent or truncated")

print(f"OK 2 pages, {glyphs} glyphs, 3 links, color, vector+raster EPS, navigation, xref")
PYEOF

# Stress the parts of the pdfmark transport which are easy to get subtly
# wrong: links split over several lines, links inside a graphicx rotation,
# nested outline topology, every standard destination view, non-ASCII PDF
# strings, and page-local media boxes.  DVI drawing parity remains the first
# oracle; two independent readers then validate the materialized PDF.
for _pass in 1 2; do
  ( cd "$TMP/ref" && timeout 300 latex -interaction=nonstopmode -output-format=dvi \
      advanced-features.tex >/dev/null 2>&1 )
  ( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
      AITEX_PDF_DIFFERENTIAL=1 timeout 300 "$BIN" pass advanced-features.tex \
      --fmt fmt.afmt -o advanced.pdf \
      >/dev/null 2>advanced.err )
done
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass advanced-features.tex --fmt fmt.afmt -o advanced.dvi \
    >/dev/null 2>advanced-dvi.err )

ref_errors=$(grep -c '^!' "$TMP/ref/advanced-features.log" 2>/dev/null || true)
our_errors=$(grep -c '^!' "$TMP/ours/advanced-features.log" 2>/dev/null || true)
if [ "${ref_errors:-0}" -ne 0 ] || [ "${our_errors:-0}" -ne 0 ]; then
  echo "pdf features: advanced LaTeX errors ours=${our_errors:-0} reference=${ref_errors:-0}"
  exit 1
fi
normalize_draws "$TMP/ref/advanced-features.dvi" > "$TMP/ref-advanced.draws"
normalize_draws "$TMP/ours/advanced.dvi" > "$TMP/ours-advanced.draws"
if ! cmp -s "$TMP/ref-advanced.draws" "$TMP/ours-advanced.draws"; then
  echo "pdf features: advanced document DVI placement differs from pdfTeX"
  diff -u "$TMP/ref-advanced.draws" "$TMP/ours-advanced.draws" | sed -n '1,24p'
  exit 1
fi

mutool clean -d "$TMP/ours/advanced.pdf" "$TMP/ours/advanced-clean.pdf" \
  >"$TMP/ours/advanced-mutool.out" 2>&1
if [ -s "$TMP/ours/advanced-mutool.out" ]; then
  echo "pdf features: mutool rejected advanced PDF"
  sed -n '1,12p' "$TMP/ours/advanced-mutool.out"
  exit 1
fi
gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage "$TMP/ours/advanced.pdf" \
  >"$TMP/ours/advanced-gs.out" 2>&1
if [ -s "$TMP/ours/advanced-gs.out" ]; then
  echo "pdf features: Ghostscript rejected advanced PDF"
  sed -n '1,12p' "$TMP/ours/advanced-gs.out"
  exit 1
fi

python3 - "$TMP/ours/advanced-clean.pdf" <<'PYEOF'
import math, re, sys

pdf = open(sys.argv[1], 'rb').read()

def require(condition, message):
    if not condition:
        print(f"pdf features: {message}")
        raise SystemExit(1)

require(re.search(rb'/Type\s*/Pages\s*/Count\s+3\b', pdf),
        "advanced page tree is not three pages")
def media_boxes(width, height):
    return len(re.findall(
        rb'/MediaBox\s*\[\s*0(?:\.0+)?\s+0(?:\.0+)?\s+' +
        str(width).encode() + rb'(?:\.0+)?\s+' +
        str(height).encode() + rb'(?:\.0+)?\s*\]',
        pdf,
    ))

require(media_boxes(612, 792) == 2,
        "portrait media box was not restored page-locally")
require(media_boxes(792, 612) == 1,
        "landscape media box was not applied page-locally")

wrapped = len(re.findall(rb'/URI \(https://example\.com/', pdf))
rotated = len(re.findall(rb'/URI \(https://example\.org/rotated\)', pdf))
require(wrapped >= 2, f"wrapped URI has only {wrapped} annotation segment(s)")
require(rotated == 1, f"rotated URI has {rotated} annotations rather than one")
require(b'^^c3' not in pdf and b'\\303\\274ber' in pdf,
        "non-ASCII URI was not decoded and safely escaped")

rects = re.findall(rb'/Subtype\s*/Link\s*/Rect\s*\[([^]]+)\]', pdf)
require(len(rects) == wrapped + rotated + 1, "link annotation count is inconsistent")
for raw in rects:
    values = [float(value) for value in raw.split()]
    require(len(values) == 4 and all(math.isfinite(value) for value in values),
            "link rectangle is not finite")
    x0, y0, x1, y1 = values
    require(0 <= x0 < x1 <= 792 and 0 <= y0 < y1 <= 792,
            f"link rectangle is outside a page: {values}")

require(re.search(rb'0\.9510\d* 0\.3090\d* -0\.3090\d* 0\.9510\d* [^\r\n]+ cm', pdf),
        "graphicx rotation matrix is absent")
for view in (b'Fit', b'FitB', b'FitH', b'FitBH', b'FitV', b'FitBV'):
    require(re.search(rb'/' + view + rb'(?=\s|\])', pdf),
            f"named destination view /{view.decode()} is absent")

require(re.search(rb'/Type\s*/Outlines', pdf), "outline tree is absent")
require(len(re.findall(rb'/First\s+', pdf)) >= 2 and
        re.search(rb'/Count\s+1\b', pdf),
        "section/subsection outline hierarchy is flat")
require(b'<FEFF004D00E90074006100200028007100750061006C0069006600690063006100740069006F006E0029>' in pdf,
        "Unicode document title is absent")
require(b'<FEFF00C5007500740068006F0072>' in pdf,
        "Unicode document author is absent")

print(f"OK advanced PDF: 3 page sizes, {wrapped} wrapped-link segments, rotation, views, Unicode, nested outline, two readers")
PYEOF

# Exercise aiTeX's private image transport independently of EPS and pdfpages:
# true-colour JPEG, alpha PNG over a non-white backing, trim/clip, and rotations
# in both directions. The reference is pdfLaTeX because an author-selected DVI
# backend is intentionally ignored by pdfTeX; a locked raster tolerance grades
# the actual direct-PDF composition.
( cd "$TMP/ours" && extractbb alpha.png >/dev/null && extractbb colour.jpg >/dev/null )
( cd "$TMP/ref" && \
    timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
      -jobname=raster-reference raster-formats.tex >/dev/null 2>&1 )
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass raster-formats.tex --fmt fmt.afmt -halt-on-error \
      -o raster-ours.pdf >/dev/null 2>raster-ours.err )
mutool clean -d "$TMP/ours/raster-ours.pdf" "$TMP/ours/raster-clean.pdf" \
  >"$TMP/ours/raster-mutool.out" 2>&1
gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage "$TMP/ours/raster-ours.pdf" \
  >"$TMP/ours/raster-gs.out" 2>&1
[ ! -s "$TMP/ours/raster-mutool.out" ] && [ ! -s "$TMP/ours/raster-gs.out" ] || {
  echo "pdf features: PNG/JPEG PDF failed independent validation"; exit 1;
}
mutool draw -q -F ppm -r 96 -o "$TMP/ref/raster.ppm" "$TMP/ref/raster-reference.pdf"
mutool draw -q -F ppm -r 96 -o "$TMP/ours/raster.ppm" "$TMP/ours/raster-ours.pdf"
python3 - "$TMP/ours/raster-ours.pdf" "$TMP/ref/raster.ppm" \
  "$TMP/ours/raster.ppm" <<'PYEOF'
import re, sys

pdf = open(sys.argv[1], 'rb').read()
if len(re.findall(rb'/Subtype\s*/Image\b', pdf)) < 3:
    raise SystemExit('pdf features: PNG/JPEG fixture did not embed three images')
if pdf.count(b'/SMask') != 1:
    raise SystemExit('pdf features: opaque RGBA PNG retained a redundant soft mask')

def ppm(path):
    raw = open(path, 'rb').read()
    magic, dimensions, maximum, pixels = raw.split(b'\n', 3)
    if magic != b'P6' or maximum != b'255':
        raise SystemExit(f'pdf features: unexpected PPM header in {path}')
    return tuple(map(int, dimensions.split())), pixels

ref_size, ref = ppm(sys.argv[2])
our_size, ours = ppm(sys.argv[3])
if ref_size != our_size or len(ref) != len(ours):
    raise SystemExit(f'pdf features: raster image dimensions differ: {ref_size}/{our_size}')
delta = [abs(a - b) for a, b in zip(ref, ours)]
mae = sum(delta) / (255 * len(delta))
large = sum(value > 32 for value in delta) / len(delta)
# Fixed before the first run. This permits renderer edge antialiasing but not a
# flattened alpha channel, missing crop, wrong rotation, or colour conversion.
if mae > 0.0025 or large > 0.004:
    raise SystemExit(f'pdf features: PNG/JPEG raster differs: mae={mae:.6f}, >32={large:.4%}')
print(f'OK PNG/JPEG crop, rotation, colour and alpha; mae={mae:.6f}, >32={large:.4%}')
PYEOF

# Xy-pic selects its direct-pdf driver because aiTeX truthfully advertises a
# final PDF artifact.  Its pdfTeX literals must survive the private DVI and
# retain pdfTeX's local origin, precision, and stroke width.
( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    xypic-driver.tex >/dev/null 2>&1 && mutool clean -d xypic-driver.pdf xypic-clean.pdf )
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass xypic-driver.tex --fmt fmt.afmt -halt-on-error \
    -o xypic.pdf >/dev/null 2>xypic.err && mutool clean -d xypic.pdf xypic-clean.pdf )
python3 - "$TMP/ref/xypic-clean.pdf" "$TMP/ours/xypic-clean.pdf" <<'PYEOF'
import re, sys

ref, ours = (open(path, 'rb').read() for path in sys.argv[1:])
pattern = rb'0\.398 w 1 J 1 j \[\]0 d [^\r\n]+ [mlS]'
ref_paths = re.findall(pattern, ref)
our_paths = re.findall(pattern, ours)
if len(ref_paths) != 2 or our_paths != ref_paths:
    print(f"pdf features: Xy-pic paths differ ({len(our_paths)} vs {len(ref_paths)})")
    raise SystemExit(1)
print("OK Xy-pic direct literals and positioned vector paths")
PYEOF

# T1/Latin Modern proves the complete classic-font lookup chain: the DVI name
# `ec-lmr10` is mapped to the `LMRoman10-Regular` PostScript name and
# `lmr10.pfb`, reencoded through `lm-ec.enc`, and paired with LaTeX's
# glyphtounicode table. Exact extraction grades accents, punctuation, German
# sharp-s and a two-code-point `ff` ligature rather than merely checking that
# the PDF contains a font object.
( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    -jobname=font-reference font-encodings.tex >/dev/null 2>&1 )
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass font-encodings.tex --fmt fmt.afmt -halt-on-error \
    -o font-ours.pdf >/dev/null 2>font-ours.err )
mutool draw -q -F txt -o "$TMP/ref/font.txt" "$TMP/ref/font-reference.pdf"
mutool draw -q -F txt -o "$TMP/ours/font.txt" "$TMP/ours/font-ours.pdf"
if ! cmp -s "$TMP/ref/font.txt" "$TMP/ours/font.txt"; then
  echo "pdf features: T1 font text extraction differs from pdfLaTeX"
  diff -u "$TMP/ref/font.txt" "$TMP/ours/font.txt" | sed -n '1,24p'
  exit 1
fi
mutool clean -d "$TMP/ours/font-ours.pdf" "$TMP/ours/font-clean.pdf" \
  >"$TMP/ours/font-mutool.out" 2>&1
if [ -s "$TMP/ours/font-mutool.out" ]; then
  echo "pdf features: font PDF did not validate cleanly"
  sed -n '1,12p' "$TMP/ours/font-mutool.out"
  exit 1
fi
python3 - "$TMP/ours/font-clean.pdf" <<'PYEOF'
import re, sys

pdf = open(sys.argv[1], 'rb').read()
requirements = [
    (rb'/BaseFont\s+/(?:[A-Z]{6}\+)?LMRoman10-Regular', 'font-map PostScript alias'),
    (rb'/Encoding\s*<<\s*/Type\s*/Encoding\s*/Differences', 'T1 encoding vector'),
    (rb'/ToUnicode\s+', 'ToUnicode CMap'),
    (rb'<1B>\s*<00660066>', 'ff ligature Unicode mapping'),
    (rb'/BaseFont\s+/(?:[A-Z]{6}\+)?NimbusRomNo9L-Regu', 'extended map font program'),
    (rb'/FontMatrix\s*\[0\.00082000 0 0\.00000000 0\.001 0 0\]', 'ExtendFont program transform'),
    (rb'/BaseFont\s+/(?:[A-Z]{6}\+)?URWBookmanL-Ligh', 'slanted map font program'),
    (rb'/FontMatrix\s*\[0\.00100000 0 0\.00016700 0\.001 0 0\]', 'SlantFont program transform'),
    (rb'/BaseFont\s+/(?:[A-Z]{6}\+)?NimbusRomNo9L-Regu[^\r\n]*\s+/FirstChar', 'virtual Times expansion to physical font'),
]
missing = [name for pattern, name in requirements if not re.search(pattern, pdf)]
if missing:
    print('pdf features: missing ' + ', '.join(missing))
    raise SystemExit(1)
print("OK T1 map/encoding, embedded Latin Modern, and exact Unicode extraction")
PYEOF

# The same lmr10 Type 1 program is selected at two point sizes and through a
# second TeX encoding. The physical face receives the union glyph closure;
# use-specific widths, encoding, and ToUnicode still produce distinct PDF font
# dictionaries where their observable semantics differ.
( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    -jobname=classic-face-reference classic-face-sharing.tex >/dev/null 2>&1 )
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    AITEX_PDF_CLASSIC_XREF=1 timeout 300 "$BIN" pass classic-face-sharing.tex \
    --fmt fmt.afmt -halt-on-error -o classic-face-ours.pdf \
    >/dev/null 2>classic-face-ours.err )
mutool draw -q -F txt -o "$TMP/ref/classic-face.txt" \
  "$TMP/ref/classic-face-reference.pdf"
mutool draw -q -F txt -o "$TMP/ours/classic-face.txt" \
  "$TMP/ours/classic-face-ours.pdf"
python3 "$ROOT/tools/pdf_size.py" --json "$TMP/ours/classic-face-ours.pdf" \
  >"$TMP/ours/classic-face-size.json"
python3 - "$TMP/ours/classic-face-size.json" "$TMP/ref/classic-face.txt" \
  "$TMP/ours/classic-face.txt" <<'PYEOF'
import json, sys

fonts = json.load(open(sys.argv[1], encoding='utf-8'))['results'][0]['classic_fonts']
reference = open(sys.argv[2], encoding='utf-8').read().split()
candidate = open(sys.argv[3], encoding='utf-8').read().split()
if candidate != reference:
    print(f"pdf features: shared classic-face text is {candidate}, expected {reference}")
    raise SystemExit(1)
expected = {
    'physical_faces': 1,
    'embedded_programs': 1,
    'descriptors': 1,
    'encodings': 2,
    # Point size is selected by the page text matrix, so the two rm-lmr10
    # sizes share one dictionary exactly as pdfLaTeX does. The distinct T1
    # encoding still requires the second dictionary.
    'pdf_fonts': 2,
}
if fonts != expected:
    print(f"pdf features: classic face/use counts are {fonts}, expected {expected}")
    raise SystemExit(1)
print("OK one classic face/program/descriptor serves two encodings and three uses")
PYEOF
gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage "$TMP/ours/classic-face-ours.pdf" \
  >"$TMP/ours/classic-face-gs.out" 2>&1
if [ -s "$TMP/ours/classic-face-gs.out" ]; then
  echo "pdf features: Ghostscript rejected shared classic-face PDF"
  sed -n '1,12p' "$TMP/ours/classic-face-gs.out"
  exit 1
fi

# Five compatibility regressions found by the #26 differential sweep share a
# compact package-level document: pb-diagram's implicit `#`, ytableau's saved
# alignment token, PGF's generic save-position family, graphicx's quoted PDF
# name, and Fourier's PFB-resident/StandardEncoding math fonts. pdfLaTeX is the
# acceptance oracle; independent readers additionally require aiTeX's subset
# Type 1 programs and final PDF structure to be valid.
ISSUE26_TEXMFHOME="$ROOT/corpus/texlive-overlay/root/usr/share/texlive/texmf-dist"
[ -d "$ISSUE26_TEXMFHOME" ] || {
  echo "pdf features: locked TeX Live overlay is absent"; exit 1;
}
( cd "$TMP/ref" && TEXMFHOME="$ISSUE26_TEXMFHOME" \
    timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    -jobname=issue26-reference issue-26-regressions.tex >/dev/null 2>&1 )
( cd "$TMP/ours" && TEXMFHOME="$ISSUE26_TEXMFHOME" \
    AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass issue-26-regressions.tex --fmt fmt.afmt -halt-on-error \
    -o issue26-ours.pdf >/dev/null 2>issue26-ours.err )
mutool clean -d "$TMP/ours/issue26-ours.pdf" "$TMP/ours/issue26-clean.pdf" \
  >"$TMP/ours/issue26-mutool.out" 2>&1
if [ -s "$TMP/ours/issue26-mutool.out" ]; then
  echo "pdf features: issue #26 PDF did not validate cleanly"
  sed -n '1,12p' "$TMP/ours/issue26-mutool.out"
  exit 1
fi
pdffonts "$TMP/ours/issue26-ours.pdf" >"$TMP/ours/issue26-fonts.out" \
  2>"$TMP/ours/issue26-fonts.err"
if [ -s "$TMP/ours/issue26-fonts.err" ] || \
   ! grep -q 'Fourier-Math-Symbols.*Type 1.*Custom.*yes.*yes.*yes' \
     "$TMP/ours/issue26-fonts.out" || \
   ! grep -q 'Fourier-Math-BlackBoard.*Type 1.*Custom.*yes.*yes.*yes' \
     "$TMP/ours/issue26-fonts.out"; then
  echo "pdf features: Fourier Type 1 subsets are absent or invalid"
  sed -n '1,12p' "$TMP/ours/issue26-fonts.err"
  sed -n '1,12p' "$TMP/ours/issue26-fonts.out"
  exit 1
fi
gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage "$TMP/ours/issue26-ours.pdf" \
  >"$TMP/ours/issue26-gs.out" 2>&1
if [ -s "$TMP/ours/issue26-gs.out" ]; then
  echo "pdf features: Ghostscript rejected issue #26 PDF"
  sed -n '1,12p' "$TMP/ours/issue26-gs.out"
  exit 1
fi
echo "OK #26 scanners, savepos, spaced graphic, and valid Fourier Type 1 subsets"

# TS1 Computer Modern is deliberately not installed as a scalable font in the
# locked base image. Start aiTeX with an empty, private TEXMFVAR so this also
# proves its resolver explicitly asks TeX Live to generate `tcrm1000.600pk`,
# then parses and embeds the real bitmaps as Type 3 rather than substituting
# another face.
( cd "$TMP/ref" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    -jobname=bitmap-reference bitmap-font.tex >/dev/null 2>&1 )
( cd "$TMP/ours" && mkdir -p texmf-var && TEXMFVAR="$TMP/ours/texmf-var" \
    AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass bitmap-font.tex --fmt fmt.afmt -halt-on-error \
    -o bitmap-ours.pdf >/dev/null 2>bitmap-ours.err )
mutool clean "$TMP/ours/bitmap-ours.pdf" "$TMP/ours/bitmap-clean.pdf" \
  >"$TMP/ours/bitmap-clean.out" 2>&1
if [ -s "$TMP/ours/bitmap-clean.out" ]; then
  echo "pdf features: bitmap PDF did not validate cleanly"
  sed -n '1,12p' "$TMP/ours/bitmap-clean.out"
  exit 1
fi
mutool draw -q -F txt -o "$TMP/ref/bitmap.txt" "$TMP/ref/bitmap-reference.pdf"
mutool draw -q -F txt -o "$TMP/ours/bitmap.txt" "$TMP/ours/bitmap-ours.pdf"
mutool draw -q -F pgm -r 144 -o "$TMP/ref/bitmap.pgm" "$TMP/ref/bitmap-reference.pdf"
mutool draw -q -F pgm -r 144 -o "$TMP/ours/bitmap.pgm" "$TMP/ours/bitmap-ours.pdf"
python3 - "$TMP/ours/bitmap-clean.pdf" "$TMP/ref/bitmap.pgm" \
  "$TMP/ours/bitmap.pgm" "$TMP/ours/bitmap.txt" <<'PYEOF'
import re, sys

pdf = open(sys.argv[1], 'rb').read()
for pattern, name in [
    (rb'/Subtype\s*/Type3', 'Type 3 font'),
    (rb'/CharProcs\s*<<', 'bitmap character procedures'),
    (rb'/Name\s*/tcrm1000', 'actual tcrm1000 family'),
    (rb'/ToUnicode\s+', 'bitmap ToUnicode CMap'),
]:
    if not re.search(pattern, pdf):
        print(f"pdf features: bitmap PDF is missing {name}")
        raise SystemExit(1)

text = ' '.join(open(sys.argv[4], encoding='utf-8').read().replace('\f', '').split())
expected = 'Bitmap TS1: 20°C; 20℃; №7; 10‰; ¤ ¦ © ®. Sizes: ° ° °.'
if text != expected:
    print(f"pdf features: bitmap text is {text!r}, expected {expected!r}")
    raise SystemExit(1)

def pgm(path):
    body = open(path, 'rb').read()
    magic, dimensions, maximum, pixels = body.split(b'\n', 3)
    if magic != b'P5' or maximum != b'255':
        raise ValueError(f'unexpected PGM header in {path}')
    return tuple(map(int, dimensions.split())), pixels

ref_size, ref = pgm(sys.argv[2])
our_size, ours = pgm(sys.argv[3])
if ref_size != our_size or len(ref) != len(ours):
    print(f"pdf features: bitmap raster dimensions differ: {ref_size} vs {our_size}")
    raise SystemExit(1)
differences = [abs(a - b) for a, b in zip(ref, ours)]
mean = sum(differences) / len(differences)
large = sum(value > 32 for value in differences) / len(differences)
# Fixed before inspecting this fixture's result: sub-pixel anti-aliasing may
# move gray edge values, but not more than 0.5% of a page materially.
if mean > 0.75 or large > 0.005:
    print(f"pdf features: bitmap raster differs: mean={mean:.4f}, >32={large:.4%}")
    raise SystemExit(1)
print(f"OK generated PK bitmap is searchable Type 3; raster mean={mean:.4f}, >32={large:.4%}")
PYEOF

# A local TFM with no corresponding map/program used to produce a plausible
# PDF that silently relied on viewer substitution. The CLI must reject it,
# name the unresolved program, and not leave private DVI bytes under `.pdf`.
cp "$(kpsewhich cmr10.tfm)" "$TMP/ours/aitex-unmapped-font.tfm"
if ( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass unmapped-font.tex --fmt fmt.afmt -halt-on-error \
    -o unmapped.pdf >/dev/null 2>unmapped.err ); then
  echo "pdf features: unmapped font unexpectedly produced a PDF"
  exit 1
fi
if ! grep -q 'font aitex-unmapped-font maps to aitex-unmapped-font.pfb, and no 600dpi PK bitmap was found' \
    "$TMP/ours/unmapped.err"; then
  echo "pdf features: unmapped font did not produce the expected diagnostic"
  sed -n '1,12p' "$TMP/ours/unmapped.err"
  exit 1
fi
if [ -e "$TMP/ours/unmapped.pdf" ]; then
  echo "pdf features: failed font resolution left a misleading PDF artifact"
  exit 1
fi
echo "OK missing font program fails explicitly without a corrupt artifact"

# pdfpages must use the dvipdfmx graphics transport, not infer its direct-pdf
# backend merely from `\pdftexversion`.  Include source pages in reverse order
# so the check also proves that `pdf:epdf page N` reaches the rasterizer.
( cd "$TMP/ours" && timeout 300 pdflatex -interaction=nonstopmode -halt-on-error \
    pdfpages-source.tex >/dev/null 2>&1 )
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$BIN" pass pdfpages-driver.tex --fmt fmt.afmt -halt-on-error \
    -o pdfpages.pdf >/dev/null 2>pdfpages.err )
mutool draw -q -F ppm -r 36 -o "$TMP/ours/pdfpages-%d.ppm" \
  "$TMP/ours/pdfpages.pdf" 1-2
python3 - "$TMP/ours/pdfpages-1.ppm" "$TMP/ours/pdfpages-2.ppm" <<'PYEOF'
import sys

def center(path):
    raw = open(path, 'rb').read()
    magic, size, maximum, pixels = raw.split(b'\n', 3)
    width, height = map(int, size.split())
    if magic != b'P6' or maximum != b'255':
        raise ValueError(f"unexpected PPM header in {path}")
    at = 3 * ((height // 2) * width + width // 2)
    return tuple(pixels[at:at + 3])

first, second = map(center, sys.argv[1:])
if not (first[2] > 220 and first[0] < 35 and
        second[0] > 220 and second[2] < 35):
    print(f"pdf features: pdfpages source-page order/colors differ: {first}, {second}")
    raise SystemExit(1)
print("OK pdfpages dvipdfmx transport and selected source pages")
PYEOF
