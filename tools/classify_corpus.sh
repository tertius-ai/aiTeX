#!/usr/bin/env bash
# Which fetched arXiv projects compile under pdfTeX at all?
#
# P7's gate is "we match pdfTeX wherever pdfTeX succeeds", so the first thing
# needed is the list of places it succeeds. `docs/history/BASELINE.md` recorded 105 of 192 on
# another machine; this recomputes it here, because a project that fails for a
# missing TeX Live package on *this* box is not a project we have to match.
#
# DVI, not PDF: DVI is the differential oracle (docs/history/PLAN.md decision 3), and it is what
# tools/corpus_check.sh will compare against. A project that only builds as PDF
# — anything loading pdfpages, or \pdfoutput=1 outright — counts as a failure
# here and is out of P7's denominator, which is correct: we cannot match a DVI
# nobody can produce.
#
# Writes corpus/meta/clean.txt (one root .tex path per line, repo-relative).
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPECS="$ROOT/corpus/meta/arxiv_specs.json"
OUT="$ROOT/corpus/meta/clean.txt"
[ -f "$SPECS" ] || { echo "no $SPECS — run tools/make_arxiv_specs.py first"; exit 1; }
command -v pdflatex >/dev/null || { echo "pdflatex not found"; exit 1; }

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
: > "$OUT"
ok=0; bad=0; n=0
total=$(python3 -c "import json;print(len(json.load(open('$SPECS'))))")

while IFS=$'\t' read -r id d b; do
  n=$((n + 1))
  # Each project is built in place but in a scratch copy of nothing — pdflatex
  # writes its .aux/.log beside the source, which is fine here because the whole
  # corpus is git-ignored. -interaction=nonstopmode so a broken project fails
  # rather than blocking on the terminal.
  if ( cd "$d" && timeout 120 pdflatex -output-format=dvi -interaction=nonstopmode \
        -no-shell-escape -halt-on-error "$b" >/dev/null 2>&1 ) \
     && [ -s "$d/${b%.tex}.dvi" ]; then
    ok=$((ok + 1)); rel=${d#"$ROOT/"}; printf '%s\t%s\n' "$rel" "$b" >> "$OUT"
    printf '  ok    [%3d/%3d] %s\n' "$n" "$total" "$id"
  else
    bad=$((bad + 1)); printf '  FAIL  [%3d/%3d] %s\n' "$n" "$total" "$id"
  fi
done < <(python3 -c "
import json
for r in json.load(open('$SPECS')):
    print(r['name'], r['path'], r['root'], sep='\t')
")

echo
printf '%d of %d arXiv projects build to DVI under pdfTeX (%d%%)\n' \
  "$ok" "$((ok + bad))" "$(( ok * 100 / (ok + bad) ))"
echo "clean list: corpus/meta/clean.txt"
