#!/usr/bin/env bash
# Run one document through pdfTeX and through us, and diff what came out.
#
# This is the workhorse of every investigation in `docs/history/engine-notes/`. It was rebuilt by
# hand a dozen times before it was written down, and each rebuild re-learned the
# same four traps:
#
#   1. **Separate directories.** Both engines write `<job>.log` and `<job>.dvi`.
#      Run them in one directory and the second clobbers the first, so you diff
#      a file against itself and see nothing. Two of this session's "identical!"
#      readings were that.
#   2. **Undo the wrapping.** TeX breaks the transcript at `max_print_line`
#      (79). Without joining continuation lines a diff aligns on wrap points
#      rather than on events, and reports hundreds of differences that are not
#      there. A line of *exactly* 79 characters is a wrapped one.
#   3. **`\escapechar` is not always a backslash.** latex.ltx runs whole
#      stretches under `\escapechar\m@ne` and TeX then prints names bare. This
#      is real output, not noise — the engine now reproduces it — but a filter
#      that requires a leading `\` drops those lines silently.
#   4. **The format matters.** Ours needs one built from the same `latex.ltx`,
#      or the comparison is INITEX against LaTeX.
#
# Usage:
#   tools/logdiff.sh <file.tex> [mode] [-- <extra aitex args>]
#
#     events   every transcript event, first divergence only   (default)
#     all      every transcript event, whole diff
#     par      `\tracingparagraphs` lines only
#     pages    `\tracingpages` lines only
#     box      `\showbox`/`\showlists` output
#     chars    the DVI character sequence — what actually got typeset
#     text     the same, as *readable text* with page markers
#
# `text` is worth reaching for before `chars`. A `setchar` diff says two blocks
# of glyphs moved; the same diff in words says *which* words, and a paragraph
# read in order is often enough to name the bug without another run. That is
# how `2608.17545v1` was found: the display had been hoisted to the top of the
# page, which is obvious in a sentence and invisible in a column of opcodes.
#
#   KEEP=1     leave the two run directories behind and say where they are
#   CONTEXT=n  lines of context around the first divergence (default 8)
#   TRACE=...  turn tracing on in the *copies*, so the paper in the corpus is
#              never edited: `pages`, `paragraphs`, `commands`, `macros`,
#              `output`, or several comma-separated. Inserted right after
#              `\begin{document}`, so the preamble's own noise stays out of it.
#
# The document may use a class or package sitting beside it, so its whole
# directory is copied; `\tracingparagraphs` and friends must be turned on in
# the document itself.
set -u
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
BIN=$ROOT/target/release/aitex
[ -x "$BIN" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
[ $# -ge 1 ] || { sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }

SRC=$1; shift
MODE=${1:-events}; [ $# -gt 0 ] && shift
[ -f "$SRC" ] || { echo "no such file: $SRC"; exit 1; }
SRCDIR=$(cd "$(dirname "$SRC")" && pwd)
JOB=$(basename "$SRC" .tex)
CONTEXT=${CONTEXT:-8}

TMP=$(mktemp -d)
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$TMP"' EXIT
[ "${KEEP:-0}" = 1 ] && echo "runs kept in $TMP"

mkdir -p "$TMP/ref" "$TMP/ours"
cp -r "$SRCDIR"/. "$TMP/ref/" 2>/dev/null
cp -r "$SRCDIR"/. "$TMP/ours/" 2>/dev/null
# Anything a previous run left behind is state neither engine produced. A stale
# `.toc` cost two iterations once — the page trace it explained was reading a
# table of contents pdfTeX's reference pass never wrote.
for d in ref ours; do
  rm -f "$TMP/$d"/*.aux "$TMP/$d"/*.log "$TMP/$d"/*.dvi "$TMP/$d"/*.toc \
        "$TMP/$d"/*.lof "$TMP/$d"/*.lot "$TMP/$d"/*.out "$TMP/$d"/*.bbl \
        "$TMP/$d"/*.blg "$TMP/$d"/*.idx "$TMP/$d"/*.ind "$TMP/$d"/*.nav \
        "$TMP/$d"/*.snm "$TMP/$d"/*.spl 2>/dev/null
done

if [ -n "${TRACE:-}" ]; then
  INS='\tracingonline=0'
  IFS=, read -ra WHAT <<< "$TRACE"
  for w in "${WHAT[@]}"; do
    case "$w" in
      pages)      INS="$INS \\tracingpages=1" ;;
      paragraphs) INS="$INS \\tracingparagraphs=1" ;;
      commands)   INS="$INS \\tracingcommands=2" ;;
      macros)     INS="$INS \\tracingmacros=2" ;;
      output)     INS="$INS \\tracingoutput=1 \\showboxdepth=5 \\showboxbreadth=100" ;;
      *) echo "unknown TRACE component: $w"; exit 1 ;;
    esac
  done
  for d in ref ours; do
    TEXFILE="$TMP/$d/$JOB.tex" INSERT="$INS" python3 -c '
import os
path, ins = os.environ["TEXFILE"], os.environ["INSERT"]
s = open(path, encoding="latin-1").read()
marker = "\\begin{document}"
s = s.replace(marker, marker + ins + "%\n", 1) if marker in s else ins + "%\n" + s
open(path, "w", encoding="latin-1").write(s)
'
  done
fi

LTX=$(kpsewhich latex.ltx)
head -n "$(( $(wc -l < "$LTX") - 4 ))" "$LTX" > "$TMP/ours/kernel.tex"
printf '\\input kernel\n' > "$TMP/ours/mkfmt.tex"
( cd "$TMP/ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 600 "$BIN" mkfmt.tex --dump fmt.afmt >/dev/null 2>&1 )
[ -s "$TMP/ours/fmt.afmt" ] || { echo "could not build a format"; exit 1; }

# `-output-format=dvi` so both sides produce the same kind of file.
( cd "$TMP/ref" && max_print_line=1000 timeout 600 \
    pdflatex -output-format=dvi -interaction=nonstopmode "$JOB.tex" >/dev/null 2>&1 )
( cd "$TMP/ours" && max_print_line=1000 AITEX_DVI="$JOB.dvi" \
    AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 600 "$BIN" pass "$JOB.tex" --fmt fmt.afmt "$@" >/dev/null 2>&1 )

A=$TMP/ref/$JOB.log
B=$TMP/ours/$JOB.log
[ -s "$A" ] || { echo "pdflatex produced no transcript"; exit 1; }
[ -s "$B" ] || { echo "aitex produced no transcript"; exit 1; }

chars() { # the DVI character sequence, which is what a reader sees
  dvitype -output-level=2 -page-start='*' "$1" 2>/dev/null |
    grep -oE 'setchar[0-9]+|set_char_[0-9]+'
}

text() { # the same characters, readable, one line per page
  DVI=$1 python3 -c '
import os, re, subprocess, sys
out = subprocess.run(["dvitype", "-output-level=2", "-page-start=*", os.environ["DVI"]],
                     capture_output=True, text=True).stdout
buf, n = [], 0
for line in out.split("\n"):
    for tok in re.finditer(r"setchar(\d+)|set_char_(\d+)|beginning of page (\d+)", line):
        if tok.group(3):
            buf.append("\n=== PAGE %s (char %d) ===\n" % (tok.group(3), n))
        else:
            c = int(tok.group(1) or tok.group(2)); n += 1
            # Anything outside printable ASCII is a font position, not a
            # letter: `{16}` is whatever slot 16 of the current font holds.
            buf.append(chr(c) if 32 <= c < 127 else "{%d}" % c)
sys.stdout.write("".join(buf))
'
}

case "$MODE" in
  text)
    # A page is one very long line, so `diff` on lines says "page 4 differs"
    # and nothing more. The first differing *character* is the useful figure,
    # and a window either side of it is usually the whole diagnosis.
    python3 - <(text "$TMP/ref/$JOB.dvi") <(text "$TMP/ours/$JOB.dvi") "$CONTEXT" <<'PY'
import sys
a = open(sys.argv[1]).read()
b = open(sys.argv[2]).read()
w = max(80, int(sys.argv[3]) * 20)
if a == b:
    print('identical: %d characters over %d pages' % (len(a), a.count('=== PAGE ')))
    raise SystemExit
i = 0
while i < min(len(a), len(b)) and a[i] == b[i]:
    i += 1
page = a.count('=== PAGE ', 0, i)
print('first divergence at character %d of the text, on page %d (%.1f%% in)'
      % (i, page, 100.0 * i / max(len(a), 1)))
print('shared: …%s' % a[max(0, i - w):i].replace('\n', ' '))
print('ref   : %s…' % a[i:i + w].replace('\n', ' '))
print('ours  : %s…' % b[i:i + w].replace('\n', ' '))
PY
    ;;
  chars)
    printf 'characters: pdftex %s, ours %s\n' \
      "$(chars "$TMP/ref/$JOB.dvi" | wc -l)" "$(chars "$TMP/ours/$JOB.dvi" | wc -l)"
    diff <(chars "$TMP/ref/$JOB.dvi") <(chars "$TMP/ours/$JOB.dvi") | head -40
    ;;
  par|pages|box)
    case "$MODE" in
      par)   pat='^@' ;;
      pages) pat='^%' ;;
      box)   pat='^[.>]' ;;
    esac
    printf 'lines: pdftex %s, ours %s, hunks %s\n' \
      "$(grep -acE "$pat" "$A")" "$(grep -acE "$pat" "$B")" \
      "$(diff <(grep -aE "$pat" "$A") <(grep -aE "$pat" "$B") | grep -c '^[0-9]')"
    diff <(grep -aE "$pat" "$A") <(grep -aE "$pat" "$B") | head -40
    ;;
  events|all)
    python3 - "$A" "$B" "$MODE" "$CONTEXT" <<'PY'
import re, sys
ref, ours, mode, ctx = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])

def events(path):
    raw = open(path, 'rb').read().decode('latin1').split('\n')
    # Trap 2: a line of exactly max_print_line characters is a wrapped one and
    # its successor is the rest of the same event.
    joined, buf = [], None
    for line in raw:
        buf = line if buf is None else buf + line
        if len(line) != 79:
            joined.append(buf); buf = None
    if buf is not None:
        joined.append(buf)
    # Trap 3: the leading escape character is optional.
    return [e for e in joined
            if e.startswith('{') or e.startswith('#')
            or re.match(r'^\\?[^ ]*->', e)]

a, b = events(ref), events(ours)
print('events: pdftex %d, ours %d' % (len(a), len(b)))
i = 0
while i < len(a) and i < len(b) and a[i] == b[i]:
    i += 1
if i == len(a) == len(b):
    print('identical for all %d events' % i); raise SystemExit
pct = (100.0 * i / len(a)) if a else 0.0
print('first divergence at event %d (%.1f%% in)' % (i, pct))
for k in range(max(0, i - ctx), min(len(a), i + ctx)):
    print(' ref%8d: %s' % (k, a[k][:160]))
print('---')
for k in range(max(0, i - ctx), min(len(b), i + ctx)):
    print('ours%8d: %s' % (k, b[k][:160]))
if mode == 'all':
    import difflib
    print('\n--- whole diff ---')
    for l in list(difflib.unified_diff(a, b, 'pdftex', 'ours', lineterm='', n=1))[:200]:
        print(l[:200])
PY
    ;;
  *) echo "unknown mode: $MODE"; exit 1 ;;
esac
