#!/usr/bin/env bash
# Long-output qualification: the same 10,000-page LaTeX source through aiTeX
# and stock pdfTeX, on both DVI and PDF routes.  This is deliberately a small
# page body: it isolates page retention, output offsets, xref/page-tree growth,
# and driver memory from the paragraph benchmark in bench_typeset.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/aitex"
FIXTURE="$ROOT/tests/scale/ten-thousand-pages.tex"

[ -x "$BIN" ] || { echo "scale: build first: cargo build --release -p aitex-cli"; exit 2; }
[ -f "$FIXTURE" ] || { echo "scale: missing $FIXTURE"; exit 2; }
for command in pdflatex mutool gs; do
  command -v "$command" >/dev/null || { echo "scale: $command not found"; exit 2; }
done
[ -x /usr/bin/time ] || { echo "scale: /usr/bin/time not found"; exit 2; }

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/ours" "$TMP/ref"
cp "$FIXTURE" "$TMP/ours/scale.tex"
cp "$FIXTURE" "$TMP/ref/scale.tex"
"$ROOT/tools/mkfmt.sh" "$TMP/ours" >/dev/null

measure() { # label, directory, command...
  local label=$1 directory=$2
  shift 2
  ( cd "$directory" && /usr/bin/time -f '%U %S %M' -o "$TMP/$label.time" \
      timeout "${SCALE_TIMEOUT:-600}" "$@" >/dev/null 2>"$TMP/$label.err" )
}

measure ours-dvi "$TMP/ours" env \
  AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
  "$BIN" pass scale.tex --fmt fmt.afmt -o scale.dvi
measure ref-dvi "$TMP/ref" pdflatex -interaction=nonstopmode \
  -halt-on-error -output-format=dvi scale.tex

# PDF mode is one public command even though aiTeX deliberately renders its
# private DVI through the PDF driver. It must scale and validate as one route.
measure ours-pdf "$TMP/ours" env \
  AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
  "$BIN" pass scale.tex --fmt fmt.afmt -o scale.pdf
measure ref-pdf "$TMP/ref" pdflatex -interaction=nonstopmode \
  -halt-on-error -output-format=pdf scale.tex

# Read the DVI postamble directly. Running dvitype over 20,000 pages merely to
# inspect its two-byte page field would make this gate needlessly slow.
python3 - "$TMP/ours/scale.dvi" "$TMP/ref/scale.dvi" <<'PYEOF'
import pathlib
import sys

for name in sys.argv[1:]:
    data = pathlib.Path(name).read_bytes()
    end = len(data) - 1
    while end >= 0 and data[end] == 223:
        end -= 1
    if end < 5 or data[end] != 2 or data[end - 5] != 249:
        raise SystemExit(f"scale: malformed DVI trailer: {name}")
    post = int.from_bytes(data[end - 4:end], "big", signed=True)
    if post < 0 or post + 29 > len(data) or data[post] != 248:
        raise SystemExit(f"scale: bad DVI post pointer: {name}")
    pages = int.from_bytes(data[post + 27:post + 29], "big")
    if pages != 10_000:
        raise SystemExit(f"scale: {name} reports {pages} DVI pages, expected 10000")
PYEOF

# MuPDF and Ghostscript independently parse the page tree; Ghostscript then
# renders the whole file to a null device, catching stream/xref errors neither
# page counter necessarily visits.
for pdf in "$TMP/ours/scale.pdf" "$TMP/ref/scale.pdf"; do
  mutool info "$pdf" >"$pdf.mutool" 2>&1
  grep -Eq '^Pages: 10000$' "$pdf.mutool" || {
    echo "scale: mutool did not report 10000 pages for $pdf"; exit 1;
  }
  pages=$(gs -q -dNODISPLAY -dNOSAFER \
    -c "($pdf) (r) file runpdfbegin pdfpagecount = quit")
  [ "$pages" = 10000 ] || {
    echo "scale: Ghostscript reports ${pages:-?} pages for $pdf"; exit 1;
  }
  gs -q -dNOPAUSE -dBATCH -sDEVICE=nullpage "$pdf" >"$pdf.gs" 2>&1
  [ ! -s "$pdf.gs" ] || {
    echo "scale: Ghostscript reported errors for $pdf"; sed -n '1,12p' "$pdf.gs"; exit 1;
  }
done

read -r ou os orss < "$TMP/ours-dvi.time"
read -r ru rs rrss < "$TMP/ref-dvi.time"
read -r opu ops oprss < "$TMP/ours-pdf.time"
read -r rpu rps rprss < "$TMP/ref-pdf.time"

python3 - "$ou" "$os" "$orss" "$ru" "$rs" "$rrss" \
  "$opu" "$ops" "$oprss" "$rpu" "$rps" "$rprss" \
  "$(stat -c%s "$TMP/ours/scale.dvi")" "$(stat -c%s "$TMP/ours/scale.pdf")" <<'PYEOF'
import sys

v = [float(x) for x in sys.argv[1:13]]
ou, os, om, ru, rs, rm, pu, ps, pm, qpu, qps, qpm = v
dvi_bytes, pdf_bytes = map(int, sys.argv[13:15])
oc, rc, pc, qc = ou + os, ru + rs, pu + ps, qpu + qps
if om > 2 * rm:
    raise SystemExit(f"scale: DVI RSS {om/1024:.1f} MiB exceeds 2x stock {rm/1024:.1f} MiB")
print("scale: 10000/10000 pages validate through DVI and PDF")
print(f"  DVI CPU: aiTeX {oc:.3f}s, pdfTeX {rc:.3f}s ({oc/rc:.2f}x); "
      f"RSS {om/1024:.1f}/{rm/1024:.1f} MiB ({om/rm:.2f}x)")
print(f"  PDF CPU: aiTeX {pc:.3f}s, pdfTeX {qc:.3f}s ({pc/qc:.2f}x); "
      f"RSS {pm/1024:.1f}/{qpm/1024:.1f} MiB ({pm/qpm:.2f}x)")
print(f"  aiTeX output: {dvi_bytes} DVI bytes, {pdf_bytes} PDF bytes")
PYEOF
