#!/usr/bin/env bash
# Do real papers come out where pdfTeX puts them?
#
# The gate docs/history/FINISH.md §1 calls "real-corpus parity", and the one thing none of
# the other tools here can tell you. `latex_check.sh` runs 35 cases someone
# wrote on purpose; this runs documents nobody wrote for us, which is the whole
# point — real papers use packages, encodings, fonts and idioms no synthetic
# case thinks to.
#
# The denominator is `corpus/meta/clean.txt`, reproduced from the tracked
# corpus lock (or built by tools/classify_corpus.sh): the projects **pdfTeX
# itself can build to DVI in the locked environment**. The bar is
# matching pdfTeX wherever pdfTeX succeeds, never 100% of arXiv — a large
# minority of arXiv does not compile under pdflatex either, and a gate that
# counted those against us would be measuring the corpus and not the engine.
#
# What is compared, exactly as tools/latex_check.sh compares its cases: the page
# count, and the *sequence of characters set*. Not the DVI bytes — TeX reuses
# its w/x/y/z movement registers more aggressively than we do, which differs
# without anything being wrong, and not dvitype's `[...]` text runs, which group
# by movement command and so inherit that same difference.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
CLEAN="$ROOT/corpus/meta/clean.txt"
LOCK="$ROOT/qualification/corpus.lock.json"
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
[ -s "$CLEAN" ] || { echo "no $CLEAN — run tools/classify_corpus.sh first"; exit 1; }
[ -s "$LOCK" ] || { echo "no $LOCK — the qualification corpus is not pinned"; exit 1; }
command -v dvitype >/dev/null || { echo "dvitype not found"; exit 1; }
LTX="$(kpsewhich latex.ltx)"
[ -n "$LTX" ] || { echo "latex.ltx not found"; exit 1; }

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
python3 - "$LOCK" > "$TMP/reference-counts.tsv" <<'PYEOF'
import json, sys
for item in json.load(open(sys.argv[1]))["entries"]:
    print(item["id"], item["reference_pages"],
          item["reference_characters"], sep="\t")
PYEOF

# One format for the whole run. Building it per project would dominate the
# measurement and tell us nothing new — tools/done_check.sh already gates on the
# kernel loading.
head -n "$(( $(wc -l < "$LTX") - 4 ))" "$LTX" > "$TMP/kernel.tex"
printf '\\input kernel\n' > "$TMP/mkfmt.tex"
( cd "$TMP" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
    timeout 300 "$FAST" mkfmt.tex --dump fmt.afmt >/dev/null 2>&1 )
[ -s "$TMP/fmt.afmt" ] || { echo "could not build a format"; exit 1; }

chars() { dvitype -output-level=2 -page-start='*' "$1" 2>/dev/null |
            grep -oE 'setchar[0-9]+|set_char_[0-9]+'; }
pages() { dvitype "$1" 2>/dev/null | grep -c 'beginning of page'; }

ok=0; bad=0; n=0; total=$(wc -l < "$CLEAN")
failed=""
while IFS=$'\t' read -r dir root; do
  n=$((n + 1))
  case "$dir" in /*) source="$dir" ;; *) source="$ROOT/$dir" ;; esac
  b="${root%.tex}"
  if [ ! -f "$source/$root" ]; then
    bad=$((bad + 1)); name=$(basename "$source"); failed="$failed $name"
    printf '  FAIL  [%2d/%2d] %-16s locked source/root is absent\n' "$n" "$total" "$name"
    continue
  fi
  # Both engines run from independent, freshly cleaned copies. Reusing the DVI
  # left by classify_corpus makes the oracle stale as soon as TeX Live changes
  # and makes a copied checkout depend on generated files that are not tracked.
  ref="$TMP/ref"; ours="$TMP/ours"
  rm -rf "$ref" "$ours"; mkdir -p "$ref" "$ours"
  cp -r "$source/." "$ref/" 2>/dev/null
  cp -r "$source/." "$ours/" 2>/dev/null
  cp "$TMP/fmt.afmt" "$ours/"
  # Every file a previous TeX run generated has to go, not just the .aux. If
  # either side reads one, it becomes a later pass and the comparison invents
  # content (most commonly a table of contents) that the other side lacks.
  # That is exactly what 2608.17952v1 was doing: 555 characters of
  # `1.1.Purposeofthepaper1 1.2.Background…` inserted at character 703, read out
  # of the `.toc` the reference run left behind.
  # `.bbl` and `.ind` are *inputs*, not droppings: arXiv runs neither BibTeX nor
  # makeindex, so a submission ships them and `tools/classify_corpus.sh` had them
  # in hand when it made pdfTeX's reference DVI. Deleting them here left us to
  # typeset a document with no bibliography against a reference that had one —
  # 2608.16845v1 came out 17p/28766c against 19p/33177c, and matched exactly once
  # the `.bbl` was left alone. `.idx` and `.blg` really are generated, by LaTeX
  # and by BibTeX, so they still go.
  for work in "$ref" "$ours"; do
    rm -f "$work/$b.dvi" "$work"/*.aux "$work"/*.log "$work"/*.toc \
          "$work"/*.lof "$work"/*.lot "$work"/*.out "$work"/*.blg \
          "$work"/*.idx "$work"/*.nav "$work"/*.snm "$work"/*.brf \
          "$work"/*.spl "$work"/*.fls "$work"/*.fdb_latexmk \
          "$work"/*.out.ps "$work"/*.run.xml "$work"/*.bcf 2>/dev/null
  done
  ( cd "$ref" && timeout 120 pdflatex -output-format=dvi -interaction=nonstopmode \
      -no-shell-escape -halt-on-error "$root" >/dev/null 2>&1 )
  rr=$?
  ( cd "$ours" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
      timeout 120 "$FAST" pass "$root" --fmt fmt.afmt -o "$b-ours.dvi" >/dev/null 2>err.txt )
  # The format carries an engine fingerprint, so rebuilding the binary while
  # this is running makes every remaining project fail to load it — and a
  # project that never started looks exactly like one that set no characters.
  # A whole 34-project run was thrown away to that. Stop instead of reporting
  # zeros as data.
  if grep -q 'built by a different engine' "$ours/err.txt" 2>/dev/null; then
    echo
    echo "ABORT: the binary changed under this run — the format no longer loads."
    echo "Rebuild finished mid-run; re-run tools/corpus_check.sh from a quiet tree."
    exit 2
  fi
  name=$(basename "$source")
  if [ "$rr" -ne 0 ] || [ ! -s "$ref/$b.dvi" ]; then
    bad=$((bad + 1)); failed="$failed $name"
    printf '  FAIL  [%2d/%2d] %-16s locked pdfTeX reference no longer builds\n' \
      "$n" "$total" "$name"
    continue
  fi
  tp=$(pages "$ref/$b.dvi"); op=$(pages "$ours/$b-ours.dvi")
  tc=$(chars "$ref/$b.dvi" | wc -l); oc=$(chars "$ours/$b-ours.dvi" | wc -l)
  expected=$(awk -F '\t' -v id="$name" '$1 == id { print $2, $3 }' \
    "$TMP/reference-counts.tsv")
  read -r expected_pages expected_chars <<<"$expected"
  if [ -z "${expected_pages:-}" ] || [ "$tp" != "$expected_pages" ] \
     || [ "$tc" != "$expected_chars" ]; then
    bad=$((bad + 1)); failed="$failed $name"
    printf '  FAIL  [%2d/%2d] %-16s pdfTeX reference drift: locked %sp/%sc, now %sp/%sc\n' \
      "$n" "$total" "$name" "${expected_pages:-?}" "${expected_chars:-?}" "$tp" "$tc"
    continue
  fi
  if [ "$tc" -eq 0 ]; then
    # pdfTeX set nothing: the entry does not belong in the clean list at all.
    printf '  skip  [%2d/%2d] %-16s pdftex set no characters\n' "$n" "$total" "$name"
    continue
  fi
  if [ "$tp" = "$op" ] && diff -q <(chars "$ref/$b.dvi") <(chars "$ours/$b-ours.dvi") >/dev/null; then
    ok=$((ok + 1))
    printf '  ok    [%2d/%2d] %-16s %sp/%sc\n' "$n" "$total" "$name" "$tp" "$tc"
  else
    bad=$((bad + 1)); failed="$failed $name"
    printf '  FAIL  [%2d/%2d] %-16s pdftex %sp/%sc  ours %sp/%sc\n' \
      "$n" "$total" "$name" "$tp" "$tc" "$op" "$oc"
  fi
done < "$CLEAN"

echo
if [ "$((ok + bad))" -eq 0 ]; then
  echo "0 projects examined — nothing in $CLEAN could be compared"
  exit 2
fi
printf '%d of %d projects match pdfTeX (%d%%)\n' \
  "$ok" "$((ok + bad))" "$(( ok * 100 / (ok + bad) ))"
# The list *is* the work queue; a bare percentage is not actionable.
[ -n "$failed" ] && printf 'not yet:%s\n' "$failed"
[ "$bad" -eq 0 ]
