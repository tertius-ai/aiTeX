#!/usr/bin/env bash
# Which LaTeX packages load, and which do not.
#
# The honest answer to "can I use this?" for anyone arriving at the repo. A
# package that fails to *load* cannot be used at all; one that loads may still
# have gaps, so this is a necessary condition, not a sufficient one — it is a
# floor on compatibility, and it is the number the README must not overstate.
#
# A package counts as loading when the engine reaches the end of the document
# with **no undefined control sequences, no errors, no leaked group, and without
# hitting a budget**. All four are necessary and the middle two were missing:
# an unimplemented primitive usually shows up as an undefined name, a missing
# conditional as a runaway — but a package can also load "successfully" while
# raising errors pdfTeX does not raise, and this tool counted that as a pass.
#
# `\usepackage{xcolor}` alone raised three `Undefined color model ``''` and
# ended at `group_depth=1`; pdfTeX raises none. Tightening the criterion took
# the count from 47/53 to 40/53, which is the number that agrees with
# `tools/corpus_check.sh` finding 0 of 34 real papers matching. The looser one
# was the reason those two instruments told opposite stories.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
LTX="$(kpsewhich latex.ltx)"
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT; cd "$TMP" || exit 1
head -n "$(( $(wc -l < "$LTX") - 4 ))" "$LTX" > kernel.tex
printf '\\input kernel\n' > mkfmt.tex
AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 "$FAST" mkfmt.tex --dump fmt.afmt >/dev/null 2>&1

# The packages people actually load, by rough frequency on arXiv. Keeping the
# catalog in one tracked file lets environment.lock.json pin both the present
# package contents and the explicitly absent names; the denominator can no
# longer drift silently with the host's TeX Live installation.
CATALOG="$ROOT/qualification/packages.txt"
[ -s "$CATALOG" ] || { echo "missing package catalog: $CATALOG"; exit 1; }
DEFAULT_PACKAGES=$(sed -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$CATALOG" | tr '\n' ' ')
PACKAGES="${*:-$DEFAULT_PACKAGES}"

# Where each failure stands. A package that fails for a known reason is a
# different thing from one nobody has looked at.
#
#   microtype   needs \pdfmatch, a pdfTeX regex primitive we do not have. It is
#               P6's problem: \pdffontexpand is what microtype actually *does*,
#               and no \special can stand in for it.
#   hyperref    unexamined since the criterion tightened to require zero errors
#   biblatex    and a balanced group. Both used to "load" under the old, looser
#   listings    test.
#   parskip     13 errors, unexamined.
#
# Settled, and kept only because each cost a session to learn:
#
#   * pgf/tikz/pgfplots/todonotes load as of `6fc453a`. Every theory in this
#     comment before that one was wrong — the eight-\expandafter chain, the
#     key store, the delimited match that "did not find \pgfeov". The 39
#     undefined \pgfeov were downstream of §507 answering false for
#     `\ifx \pgfkeys@possibleslash /`, where the left side is a \futurelet to
#     an explicit `/`. See docs/history/engine-notes/pgf-implicit-char-ifx.md, which also records
#     that eight hand-written reductions all matched tex and narrowed nothing.
#   * xcolor loads as of `2293030` (§495/§510's condition stack).
#   * cite never was broken: this tool was reading biblatex's leftover .aux.
#   * §479's literal `#` (`b4f42c0`) is real and fixed, and was *not* pgf's
#     cause, which is why it is filed under §479 and not here.
ok=0; bad=0; skipped=0; failed=""
for p in $PACKAGES; do
  # Every package is typeset under the same job name, so `\end{document}` reads
  # back whatever `.aux` the *previous* one left behind. That is not a
  # hypothetical: `biblatex` immediately precedes `cite` in the list below, and
  # cite's run was reading biblatex's `\abx@aux@…` lines and reporting the one
  # undefined name they produce. cite loads fine — `tools/packages.sh cite` and
  # `tools/packages.sh natbib cite` both pass — and had been counted as a
  # failure for as long as this list has had both in it.
  # A package this TeX Live does not have is not an engine failure. Three of
  # the fifty-three — siunitx, algorithm2e, algorithmic — are simply not
  # installed here, and each was being counted against us on the strength of
  # LaTeX's own `File \`x.sty\' not found`. Same mistake as counting arXiv
  # projects pdfTeX cannot build: measuring the machine, not the engine.
  if ! kpsewhich "$p.sty" >/dev/null 2>&1; then
    skipped=$((skipped + 1))
    printf '  skip  %-14s not installed in this TeX Live\n' "$p"
    continue
  fi
  rm -f p.aux p.dvi p.log p.out p.toc
  printf '\\documentclass{article}\n\\usepackage{%s}\n\\begin{document}\nx\n\\end{document}\n' "$p" > p.tex
  # Errors and a leaked group count as failures, not just undefined names and
  # budget caps. `\usepackage{xcolor}` alone raised three
  # `Undefined color model ``''` and ended with `group_depth=1`, and this tool
  # called it `ok` for months, because an unimplemented primitive usually shows
  # up as an undefined name and this one does not. pdfTeX raises none of them.
  # A package that loads with errors is not a package that loads — it is the
  # difference between `tools/packages.sh` saying 47/53 and the corpus saying
  # 0/34.
  full=$(AITEX_DVI=p.dvi timeout 60 "$FAST" pass p.tex --fmt fmt.afmt 2>&1)
  out=$(echo "$full" | tail -2)
  errs=$(echo "$full" | grep -ac '^!' || true)
  if echo "$out" | grep -q 'undef=0 (0 distinct)' \
     && echo "$out" | grep -q 'cap=false' \
     && echo "$out" | grep -q 'group_depth=0' \
     && [ "${errs:-0}" -eq 0 ]; then
    ok=$((ok+1)); [ -n "${VERBOSE:-}" ] && printf '  ok    %s\n' "$p"
  else
    bad=$((bad+1)); failed="$failed $p"
    why=$(echo "$out" | grep -oE 'undef=[0-9]+ \([0-9]+ distinct\)|cap=true|group_depth=[1-9][0-9]*' | tr '\n' ' ')
    [ "${errs:-0}" -gt 0 ] && why="$why${errs} error(s)"
    printf '  FAIL  %-14s %s\n' "$p" "$why"
  fi
done
echo
printf '%d of %d available packages load (%d%%); %d catalog entries, %d unavailable' \
  "$ok" "$((ok+bad))" "$(( ok * 100 / (ok+bad) ))" \
  "$((ok+bad+skipped))" "$skipped"
echo
[ -n "$failed" ] && printf 'not yet:%s\n' "$failed"
[ "$bad" -eq 0 ]
