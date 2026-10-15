#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BINARY=${AITEX_BINARY:-"$ROOT/bin/aitex"}
FONT_DIR="$ROOT/crates/aitex-fonts/tests/fonts"
SOURCE="$ROOT/tests/unicode/multilingual.tex"
PRODUCT="$ROOT/tests/unicode/product/main.tex"
POLYGLOSSIA="$ROOT/tests/unicode/product/polyglossia.tex"
CLUSTERS="$ROOT/tests/unicode/product/clusters.tex"
INPUT="$ROOT/tests/unicode/input"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/aitex-unicode.XXXXXX")
trap 'rm -rf "$WORK"' EXIT HUP INT TERM

cd "$WORK"
AITEX_FONT_PATH="$FONT_DIR" AITEX_PDF_DIFFERENTIAL=1 \
    "$BINARY" --unicode --no-cache "$SOURCE" \
    -o multilingual.pdf >/dev/null 2>&1
AITEX_FONT_PATH="$FONT_DIR" AITEX_PDF_DVI_COMPAT=1 \
    "$BINARY" --unicode --no-cache "$SOURCE" \
    -o multilingual-compat.pdf >/dev/null 2>&1
cmp multilingual.pdf multilingual-compat.pdf

test "$(dd if=multilingual.pdf bs=5 count=1 2>/dev/null)" = '%PDF-'
pdffonts multilingual.pdf >fonts.txt
test "$(grep -c 'CID TrueType' fonts.txt)" -ge 6
grep -q 'Type 1' fonts.txt
pdftotext -layout multilingual.pdf text.txt
grep -q 'office affine' text.txt
grep -q 'नमस्ते' text.txt
# Poppler emits RTL scalars in visual order; the structural PDF test checks
# that each ActualText span itself remains logical-order UTF-16.
grep -q 'םולש' text.txt
grep -q 'םלוע' text.txt
grep -q '日本語中文测试' text.txt
grep -q '漢字かなカナ' text.txt
grep -q 'ภาษาไทย' text.txt
grep -q 'ສະບາຍດີ' text.txt

"$BINARY" --unicode --no-cache "$PRODUCT" -o product.pdf >/dev/null 2>&1
"$BINARY" --unicode --no-cache "$PRODUCT" -o product-repeat.pdf >/dev/null 2>&1
cmp product.pdf product-repeat.pdf
"$BINARY" --unicode --no-cache "$POLYGLOSSIA" -o polyglossia.pdf >/dev/null 2>&1
"$BINARY" --unicode --no-cache "$CLUSTERS" -o clusters.pdf >/dev/null 2>&1

pdfinfo product.pdf >/dev/null
pdffonts product.pdf >product-fonts.txt
test "$(grep -c 'CID ' product-fonts.txt)" -ge 7
grep -q 'Type 1' product-fonts.txt
pdftotext -layout product.pdf product.txt
grep -q 'Ordinary UTF-8 and OpenType shaping' product.txt
grep -q 'नमस्ते' product.txt
grep -q '日本語中文测试' product.txt
grep -q 'ภาษาไทย' product.txt
grep -q 'ສະບາຍດີ' product.txt
pdftotext -layout polyglossia.pdf polyglossia.txt
grep -q 'English and' polyglossia.txt
pdftotext -layout clusters.pdf clusters-poppler.txt
mutool draw -q -F txt -o clusters-mupdf.txt clusters.pdf
mutool draw -q -F txt -o product-mupdf.txt product.pdf
python3 - clusters-poppler.txt clusters-mupdf.txt product.txt product-mupdf.txt <<'PYEOF'
import sys
import unicodedata

markers = ("नमस्तेभारत", "ภาษาไทยยินดีต้อนรับ")
for path in sys.argv[1:]:
    text = unicodedata.normalize(
        "NFKC", "".join(open(path, encoding="utf-8").read().split())
    )
    if "\ufffd" in text or any(unicodedata.category(char) == "Cs" for char in text):
        raise SystemExit(f"invalid Unicode extraction in {path}")
    for marker in markers:
        if text.count(marker) != 1:
            raise SystemExit(
                f"expected exactly one {marker!r} in {path}, found {text.count(marker)}"
            )
PYEOF

# A decoded scalar in U+0080..U+00FF must not re-enter LaTeX inputenc's
# byte-oriented active-character automaton. Exercise both the root and real
# input boundaries, including a generated-control-file extension, and compare
# the direct event route against the independently parsed DVI compatibility
# route byte for byte.
mkdir "$WORK/input"
cp "$INPUT"/*.tex "$INPUT"/*.bbl "$WORK/input/"
(
    cd "$WORK/input"
    "$BINARY" --unicode --no-cache main.tex --font "Noto Sans" \
        --font-path "$FONT_DIR" --language en --direction auto \
        -o input.pdf >/dev/null 2>&1
    AITEX_PDF_DVI_COMPAT=1 "$BINARY" --unicode --no-cache main.tex \
        --font "Noto Sans" --font-path "$FONT_DIR" --language en --direction auto \
        -o input-compat.pdf >/dev/null 2>&1
    cmp input.pdf input-compat.pdf
    pdfinfo input.pdf >/dev/null
    pdftotext -layout input.pdf input.txt
    python3 - input.txt <<'PYEOF'
import sys

text = open(sys.argv[1], encoding="utf-8").read()
normalized = " ".join(text.replace("\u00a0", " ").split())
for marker in [
    "scalar∣boundary",
    "direct space",
    "include∣boundary",
    "include space",
    "bibliography space",
]:
    if marker not in normalized:
        raise SystemExit(f"Unicode input marker is absent: {marker!r}")
PYEOF
    if "$BINARY" --unicode --no-cache legacy-inputenc.tex --font "Noto Sans" \
        --font-path "$FONT_DIR" --language en --direction auto \
        -o legacy-inputenc.pdf >legacy.out 2>legacy.err; then
        echo "Unicode input check: legacy inputenc unexpectedly succeeded"
        exit 1
    fi
    grep -q "Legacy input encoding .*latin1" legacy.out
    grep -q "Unicode mode" legacy.out
)
if command -v gs >/dev/null 2>&1; then
    gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage product.pdf
    gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage clusters.pdf
    gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage input/input.pdf
fi

echo "Unicode PDF check passed"
