#!/bin/bash
# Is the engine done? — the exit condition for unattended work.
#
# Every gate here is mechanical and compares against real TeX. That is the
# point: "done" decided by my own judgement has been wrong three times now.
#
#   1. E2 was marked complete while `\shipout` was literally `Ok(())`, because
#      the page builder passed its own tests and nothing checked that a page
#      ever came out.
#   2. The `latex.ltx` gate false-passed because latex.ltx opens with
#      `\ifx\eTeXversion\undefined \errmessage{...} \endinput` — an engine that
#      does not declare e-TeX is politely shown the door and exits *clean*.
#      Nothing loaded, no error, gate green. It now asks LaTeX to say its name.
#   3. On 2026-08-19 this file printed DONE while the engine dropped every
#      accented character in `naïve café`, panicked on any non-UTF-8 file, and
#      shipped 13 of TRIP's 16 pages. Nothing was lying: the gate set had simply
#      stopped growing while the frontier moved, and the one gate that would
#      have caught the first two iterated over `corpus/engine/*.tex` — a
#      directory that is git-ignored, has never existed in a fresh clone, and so
#      ran zero cases and passed.
#
# Hence the rule this file now enforces on itself: **a gate that examined
# nothing FAILS.** `no_cases` below is that rule, and it is used at every loop.
#
# The four gates that define done are in docs/history/FINISH.md §1:
#   1. real-corpus parity     — we match pdfTeX wherever pdfTeX succeeds
#   2. TRIP transcript parity — trip.log/trip.fot diffed against Knuth's
#   3. package coverage       — every available entry in the locked catalog
#   4. PDF output             — a .pdf comes out, graphicx and hyperref work
#
# The gates are ordered so the cheap ones fail first.
#
# Exit 0 means done. Any other exit means there is work left, and the output
# says which gate failed.
set -u
export PYTHONDONTWRITEBYTECODE=1
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
# Absolute: the gates below run the binary from inside temporary directories,
# and a relative path resolves against the wrong one.
BIN=$ROOT/target/release/aitex
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
FAILED=0

# Debian's installed latex.fmt embeds package-install time and is therefore not
# a cross-machine lock. Use the byte-locked fixed-epoch formats for every stock
# LaTeX oracle invoked by this complete gate.
if [ -z "${AITEX_REFERENCE_FORMATS:-}" ]; then
  AITEX_REFERENCE_FORMATS="$ROOT/target/qualification/reference-formats"
  python3 tools/reference_formats.py --output "$AITEX_REFERENCE_FORMATS" || exit 2
  export AITEX_REFERENCE_FORMATS
fi
export TEXFORMATS="$AITEX_REFERENCE_FORMATS:${TEXFORMATS:-}"

gate() { # $1=name  $2=result(0/1)  $3=detail
  if [ "$2" = 0 ]; then
    printf '  ok    %-28s %s\n' "$1" "$3"
  else
    printf '  FAIL  %-28s %s\n' "$1" "$3"
    FAILED=1
  fi
}

# A gate that examined nothing is a failure, never a pass. See the header.
no_cases() { # $1=name  $2=count  $3=what was expected
  if [ "${2:-0}" -eq 0 ]; then
    gate "$1" 1 "examined NOTHING — expected $3"
    return 0
  fi
  return 1
}

# The character sequence a DVI sets, in order. This is the comparison that
# matters and the one `tools/latex_check.sh` already makes: *not* dvitype's
# `[...]` text runs, which group by movement command and so differ whenever our
# w/x/y/z register reuse differs from TeX's without anything being wrong.
chars() { dvitype -output-level=2 -page-start='*' "$1" 2>/dev/null |
            grep -oE 'setchar[0-9]+|set_char_[0-9]+'; }
pages() { dvitype "$1" 2>/dev/null | grep -c 'beginning of page'; }

echo "aiTeX done-check"

# ---- 0. The external oracle and corpus are the locked qualification set -----
Q=$(python3 tools/qualification_env.py --require-corpus 2>&1); R=$?
gate "qualification environment" "$R" "$(echo "$Q" | tail -1)"

# ---- 1. It builds, the tests pass, the dependency line holds -----------------
# `cargo build --workspace --all-targets`, not a bare build: the test target had
# stopped *compiling* for weeks before 35c2d98, and every "0 failures" reported
# in that window was `cargo test | grep -c FAILED` reading 0 off a build error.
cargo build --workspace --all-targets -q 2>/dev/null
gate "builds (all targets)" $? ""

VERSION_OUTPUT=$("$BIN" --version 2>&1); VERSION_STATUS=$?
HELP_OUTPUT=$("$BIN" --help 2>&1); HELP_STATUS=$?
EMPTY_OUTPUT=$("$BIN" 2>&1); EMPTY_STATUS=$?
UNKNOWN_OUTPUT=$("$BIN" --definitely-unknown 2>&1); UNKNOWN_STATUS=$?
STRAY_OUTPUT=$("$BIN" examples/hello.tex stray 2>&1); STRAY_STATUS=$?
DUPLICATE_OUTPUT=$("$BIN" examples/hello.tex --fmt a --fmt b 2>&1); DUPLICATE_STATUS=$?
BAD_VALUE_OUTPUT=$("$BIN" cache prune .cache --max-bytes many 2>&1); BAD_VALUE_STATUS=$?
mkdir -p "$TMP/cli"
printf '\\font\\f=cmr10 \\f OK\\end\n' > "$TMP/cli/input.tex"
( cd "$TMP/cli" && "$BIN" pass -file-line-error -interaction=nonstopmode \
    input.tex --ini -o output.dvi >/dev/null 2>&1 ); OPTIONS_FIRST_STATUS=$?
OPTIONS_FIRST_PAGES=$(pages "$TMP/cli/output.dvi")
printf '\\immediate\\openout0=blocked/file.aux \\immediate\\write0{x}\\immediate\\closeout0\\end\n' \
  > "$TMP/cli/writefail.tex"
printf x > "$TMP/cli/blocked"
( cd "$TMP/cli" && "$BIN" pass writefail.tex --ini >/dev/null 2>writefail.err ); WRITE_FAIL_STATUS=$?
printf '\\font\\f=cmr10 \\f OK\\end\n' > "$TMP/cli/logfail.tex"
mkdir "$TMP/cli/logfail.log"
( cd "$TMP/cli" && "$BIN" pass logfail.tex --ini >/dev/null 2>logfail.err ); LOG_FAIL_STATUS=$?
( cd "$TMP/cli" && AITEX_MAX_STEPS=0 "$BIN" pass input.tex --ini -o capped.dvi \
    >cap.out 2>&1 ); CAP_STATUS=$?
WORKSPACE_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
GIT_DESCRIPTION=$(git describe --tags --always)
EXPECTED_VERSION="aitex $WORKSPACE_VERSION"
if [ "$GIT_DESCRIPTION" != "v$WORKSPACE_VERSION" ]; then
  EXPECTED_VERSION="$EXPECTED_VERSION ($GIT_DESCRIPTION)"
fi
if [ "$VERSION_STATUS" -eq 0 ] && [ "$VERSION_OUTPUT" = "$EXPECTED_VERSION" ] &&
   [ "$HELP_STATUS" -eq 0 ] && echo "$HELP_OUTPUT" | grep -q '^Usage: aitex ' &&
   [ "$EMPTY_STATUS" -eq 2 ] && echo "$EMPTY_OUTPUT" | grep -q '^Usage: aitex ' &&
   [ "$UNKNOWN_STATUS" -eq 2 ] && [ "$STRAY_STATUS" -eq 2 ] &&
   [ "$DUPLICATE_STATUS" -eq 2 ] && [ "$BAD_VALUE_STATUS" -eq 2 ] &&
   [ "$OPTIONS_FIRST_STATUS" -eq 0 ] &&
   [ "$OPTIONS_FIRST_PAGES" -eq 1 ] &&
   [ "$WRITE_FAIL_STATUS" -ne 0 ] && [ "$LOG_FAIL_STATUS" -ne 0 ] &&
   [ "$CAP_STATUS" -ne 0 ] && [ ! -e "$TMP/cli/capped.dvi" ] &&
   grep -q 'budget exhausted' "$TMP/cli/cap.out" &&
   ! printf '%s\n%s\n%s\n%s\n%s\n' "$EMPTY_OUTPUT" "$UNKNOWN_OUTPUT" \
       "$STRAY_OUTPUT" "$DUPLICATE_OUTPUT" "$BAD_VALUE_OUTPUT" | grep -q 'panicked'; then
  gate "public CLI front door" 0 "strict arguments, fatal caps/writes, and options-first compilation"
else
  gate "public CLI front door" 1 "help/version/usage contract differs"
fi

# The normal document command is aiTeX's own fixed-point scheduler. Exercise a
# clean build with cross-references, a TOC, BibTeX, and MakeIndex; then require
# the identical invocation to reuse the proven PDF without spawning either
# semantic processor again.
mkdir -p "$TMP/build" && cp tests/build/*.tex tests/build/*.bib "$TMP/build/"
# Deliberately populate an exact output entry through the one-pass front door
# while BBL/IND are absent. The complete builder must not trust that PDF as a
# semantic fixed point: its cached side-artifact manifest identifies the IDX,
# and the AUX identifies the bibliography inputs.
(cd "$TMP/build" && FORCE_SOURCE_DATE=1 SOURCE_DATE_EPOCH=1788292800 \
  timeout 120 "$BIN" pass main.tex --cache "$TMP/build/cache-home/aitex" \
  --no-banner >/dev/null 2>&1)
BUILD_PREP_ONE=$?
(cd "$TMP/build" && FORCE_SOURCE_DATE=1 SOURCE_DATE_EPOCH=1788292800 \
  timeout 120 "$BIN" pass main.tex --cache "$TMP/build/cache-home/aitex" \
  --no-banner >/dev/null 2>&1)
BUILD_PREP_TWO=$?
BUILD_OUTPUT=$(cd "$TMP/build" && FORCE_SOURCE_DATE=1 SOURCE_DATE_EPOCH=1788292800 \
  XDG_CACHE_HOME="$TMP/build/cache-home" timeout 120 "$BIN" main.tex --no-banner 2>&1)
BUILD_STATUS=$?
BUILD_SECOND=$(cd "$TMP/build" && FORCE_SOURCE_DATE=1 SOURCE_DATE_EPOCH=1788292800 \
  XDG_CACHE_HOME="$TMP/build/cache-home" timeout 120 "$BIN" main.tex --no-banner 2>&1)
BUILD_SECOND_STATUS=$?
BIBER_OUTPUT=$(cd "$TMP/build" && FORCE_SOURCE_DATE=1 SOURCE_DATE_EPOCH=1788292800 \
  timeout 120 "$BIN" build biber.tex --no-banner 2>&1)
BIBER_STATUS=$?
GLOSSARY_OUTPUT=$(cd "$TMP/build" && FORCE_SOURCE_DATE=1 SOURCE_DATE_EPOCH=1788292800 \
  timeout 120 "$BIN" build glossary.tex --no-banner 2>&1)
GLOSSARY_STATUS=$?
if [ "$BUILD_PREP_ONE" -eq 0 ] && [ "$BUILD_PREP_TWO" -eq 0 ] &&
   [ "$BUILD_STATUS" -eq 0 ] && [ "$BUILD_SECOND_STATUS" -eq 0 ] &&
   [ "$BIBER_STATUS" -eq 0 ] && [ "$GLOSSARY_STATUS" -eq 0 ] &&
   [ -s "$TMP/build/main.pdf" ] && [ -s "$TMP/build/main.bbl" ] &&
   [ -s "$TMP/build/main.ind" ] &&
   [ -s "$TMP/build/biber.pdf" ] && [ -s "$TMP/build/biber.bbl" ] &&
   [ -s "$TMP/build/glossary.pdf" ] && [ -s "$TMP/build/glossary.gls" ] &&
   [ -s "$TMP/build/glossary.acr" ] && [ -s "$TMP/build/glossary.nls" ] &&
   echo "$BUILD_OUTPUT" | grep -q 'aitex build: running BibTeX' &&
   echo "$BUILD_OUTPUT" | grep -q 'aitex build: running MakeIndex' &&
   echo "$BUILD_OUTPUT" | grep -q 'persistent output cache hit' &&
   echo "$BUILD_OUTPUT" | grep -q 'aitex build: converged after' &&
   ! grep -qE 'undefined references|Citation .* undefined|Reference .* undefined' \
       "$TMP/build/main.log" &&
   echo "$BUILD_SECOND" | grep -q 'persistent output cache hit' &&
   ! echo "$BUILD_SECOND" | grep -q 'aitex build: running' &&
   echo "$BIBER_OUTPUT" | grep -q 'aitex build: running Biber' &&
   echo "$GLOSSARY_OUTPUT" | grep -q 'aitex build: running MakeGlossaries' &&
   echo "$GLOSSARY_OUTPUT" | grep -q 'aitex build: running MakeIndex (nomenclature)'; then
  gate "native complete build" 0 "PDF default, all semantic tools selective, exact repeat hit"
else
  gate "native complete build" 1 "fixed-point scheduler or default PDF contract differs"
fi

# A clean, uncached fixed-point build must deserialize the base format once,
# then clone that already-decoded pristine state for later TeX passes. Keeping
# this separate from the cache test above makes the cross-pass claim visible:
# an exact output hit would correctly decode the format zero times.
mkdir -p "$TMP/build-hot-format" &&
  cp tests/build/main.tex tests/build/refs.bib "$TMP/build-hot-format/"
HOT_FORMAT_OUTPUT=$(cd "$TMP/build-hot-format" &&
  FORCE_SOURCE_DATE=1 SOURCE_DATE_EPOCH=1788292800 AITEX_METRICS=1 \
  timeout 120 "$BIN" main.tex --no-cache --no-banner 2>&1)
HOT_FORMAT_STATUS=$?
HOT_FORMAT_DECODES=$(echo "$HOT_FORMAT_OUTPUT" |
  grep -oE '"format_decode_count":[0-9]+' |
  awk -F: '{sum += $2} END {print sum + 0}')
HOT_FORMAT_CLONES=$(echo "$HOT_FORMAT_OUTPUT" |
  grep -c '"format_source":"hot-clone"')
if [ "$HOT_FORMAT_STATUS" -eq 0 ] && [ "$HOT_FORMAT_DECODES" -eq 1 ] &&
   [ "$HOT_FORMAT_CLONES" -ge 1 ] &&
   cmp -s "$TMP/build/main.pdf" "$TMP/build-hot-format/main.pdf"; then
  gate "cross-pass format reuse" 0 "one decode, $HOT_FORMAT_CLONES hot clone(s), exact PDF"
else
  gate "cross-pass format reuse" 1 "decode=$HOT_FORMAT_DECODES clones=$HOT_FORMAT_CLONES"
fi

TESTS=$(cargo test -q 2>&1 | grep -oE '[0-9]+ passed' | awk '{s+=$1} END {print s}')
cargo test -q >/dev/null 2>&1
R=$?
no_cases "tests pass" "${TESTS:-0}" "at least one test" || gate "tests pass" $R "$TESTS tests"

python3 tools/check_zero_deps.py >/dev/null 2>&1
gate "engine has zero deps" $? ""

# ---- 2. The format loads, completely and silently ---------------------------
# Two separate claims: every line executes, and loading typesets nothing. The
# second is not implied by the first — an engine that silently sets an
# unimplemented primitive's arguments as text passes the first and fails this.
mkdir -p "$TMP/fmt" && cp vendor/plain/plain.tex "$TMP/fmt/"
printf '\\input plain\n\\bye\n' > "$TMP/fmt/f.tex"
( cd "$TMP/fmt" && timeout 120 "$BIN" pass f.tex --ini -o f.dvi >/dev/null 2>err.txt )
grep -q 'undefined control' "$TMP/fmt/err.txt" 2>/dev/null && UNDEF=1 || UNDEF=0
gate "plain.tex: no undefined names" "$UNDEF" "$(grep -oE '[0-9]+ undefined' "$TMP/fmt/err.txt" | head -1)"

MINE=$(dvitype "$TMP/fmt/f.dvi" 2>/dev/null | grep -cE '^\[.*\]$')
[ "${MINE:-1}" = 0 ]
gate "loading a format sets nothing" $? "${MINE:-?} text runs"

# ---- 3. GATE 1a — plain-TeX documents match real TeX ------------------------
# The tracked fixtures in corpus/puretex/, which are in the repository and so
# exist in a fresh clone. This is what `corpus/engine/*.tex` was supposed to be
# and never was.
#
# The comparison is the *character sequence*, exactly — see `chars()`.
DOCS=0
if [ -f corpus/puretex/DOCUMENTS ]; then
  mkdir -p "$TMP/doc" && cp corpus/puretex/*.tex vendor/plain/plain.tex "$TMP/doc/"
  while read -r f; do
    case "$f" in ''|'#'*) continue ;; esac
    [ -f "corpus/puretex/$f" ] || { gate "doc: $f" 1 "listed in DOCUMENTS but missing"; continue; }
    DOCS=$((DOCS + 1))
    b=$(basename "$f" .tex)
    # The wrapper the corpus README describes: supply `\input plain` and `\bye`
    # only when the document does not.
    {
      grep -q '\\input plain' "corpus/puretex/$f" || echo '\input plain'
      cat "corpus/puretex/$f"
      grep -q '\\bye' "corpus/puretex/$f" || echo '\bye'
    } > "$TMP/doc/run_$b.tex"
    ( cd "$TMP/doc" && max_print_line=1000 timeout 120 tex -ini -interaction=nonstopmode "run_$b.tex" >/dev/null 2>&1 )
    ( cd "$TMP/doc" && timeout 120 "$BIN" pass "run_$b.tex" --ini -o "mine_$b.dvi" >/dev/null 2>&1 )
    tc=$(chars "$TMP/doc/run_$b.dvi" | wc -l)
    oc=$(chars "$TMP/doc/mine_$b.dvi" | wc -l)
    tp=$(pages "$TMP/doc/run_$b.dvi"); op=$(pages "$TMP/doc/mine_$b.dvi")
    if [ "$tc" -eq 0 ]; then
      gate "doc: $b" 1 "tex set no characters — the fixture or tex is broken"
    else
      diff -q <(chars "$TMP/doc/run_$b.dvi") <(chars "$TMP/doc/mine_$b.dvi") >/dev/null &&
        [ "$tp" = "$op" ]
      gate "doc: $b" $? "tex ${tp}p/${tc}c  ours ${op}p/${oc}c"
    fi
  done < corpus/puretex/DOCUMENTS
fi
no_cases "plain-TeX documents" "$DOCS" "corpus/puretex/DOCUMENTS to list some"

# ---- 4. The differential oracle ---------------------------------------------
ORACLE=$(./tools/plain_check.sh 2>&1)
echo "$ORACLE" | grep -qE '([0-9]+)/\1 lines'
gate "oracle: lines" $? "$(echo "$ORACLE" | head -1)"
echo "$ORACLE" | grep -qE '([0-9]+)/\1 probes'
gate "oracle: probes" $? ""

# ---- 4b. The syntax batteries ------------------------------------------------
# Every syntactic form TeX documents, ours against `pdftex -ini -etex`. This is
# a separate gate from the oracle above because it asks a different question:
# the oracle asks whether a real document still works, and the batteries ask
# whether a *form* is right. plain.tex loads 1241/1241 lines while `\ifdim 1pt>0pt`
# is broken outright, because it never writes one.
B=$(./tools/battery.sh 2>&1)
BN=$(echo "$B" | grep -oE '[0-9]+ forms$' | tail -1 | grep -oE '^[0-9]+')
no_cases "syntax batteries" "${BN:-0}" "hundreds of forms" || {
  echo "$B" | tail -1 | grep -q '^all batteries agree'
  gate "syntax batteries" $? "$BN forms"
}

# ---- 4c. The box displays ----------------------------------------------------
# `\showbox`, `\showlists` and `\tracingoutput`, ours against real TeX. A third
# question again: the batteries ask whether a *form* is right and the corpus
# asks whether a *document* is, and neither can see inside a box. Nothing a
# page-breaking or insertion question needs is in a DVI file — whether the glue
# above a line came from `\baselineskip` or `\lineskip`, how tall the page
# thinks it is, which insertion on the page is a split remainder — and all of it
# is here.
#
# This ran for weeks as a harness nobody's gate invoked. 154 displays were being
# compared by `tools/showbox_check.sh` and by nothing that decides whether the
# engine is done, so a regression in any of them passed every gate. That is the
# same fault this file was written to remove.
SB=$(./tools/showbox_check.sh 2>&1)
SBN=$(echo "$SB" | grep -oE '[0-9]+ displays$' | tail -1 | grep -oE '^[0-9]+')
no_cases "box displays" "${SBN:-0}" "dozens of displays" || {
  echo "$SB" | tail -1 | grep -q '^box displays match'
  gate "box displays" $? "$SBN displays"
}

# The transcript-level sibling of the box gate. It pins command tracing,
# paragraph/page narration, logical-line joining, and complete `\showlists`
# state in cases where comparing only extracted box displays would miss the
# surrounding lifecycle. Like every loop-backed gate here, an empty case set
# is a failure rather than a vacuous success.
TR=$(./tools/trace_check.sh 2>&1)
TRN=$(echo "$TR" | grep -oE '[0-9]+ lines$' | tail -1 | grep -oE '^[0-9]+')
no_cases "trace transcripts" "${TRN:-0}" "dozens of transcript cases" || {
  echo "$TR" | tail -1 | grep -q '^transcripts match TeX'
  gate "trace transcripts" $? "$TRN lines"
}

# A dumped format has to be indistinguishable from executing what produced it,
# or it is a way to be fast and wrong. Every LaTeX case is run again from a
# dump and compared to pdfTeX exactly as before.
F=$(FMT=1 ./tools/latex_check.sh 2>&1)
echo "$F" | tail -1 | grep -q 'sets where pdftex sets it'
gate "format dump/restore" $? "$(echo "$F" | grep -oE '[0-9]+/[0-9]+ cases' | tail -1) from a format"

# And with the preamble snapshot on top: each case is run more than once, so
# the later runs are snapshot *hits* — the path that must not change anything.
S=$(FMT=1 CACHE=1 ./tools/latex_check.sh 2>&1)
echo "$S" | tail -1 | grep -q 'sets where pdftex sets it'
gate "preamble snapshot" $? "$(echo "$S" | grep -oE '[0-9]+/[0-9]+ cases' | tail -1) from a snapshot"
if [ "$FAILED" = 1 ]; then echo "$B" | grep -A6 FAIL | sed 's/^/    /'; fi

# ---- 5. LaTeX ---------------------------------------------------------------
# The real target. `latex.ltx` loading with no undefined names is what says the
# engine is a TeX rather than a demonstration.
if L=$(kpsewhich latex.ltx 2>/dev/null) && [ -n "$L" ]; then
  # latex.ltx ends with `\makeatother \errorstopmode \dump`, and `\dump` does
  # what §1327 says: writes a format and *halts*. Correct — real INITEX behaves
  # the same — but it means the file cannot be `\input` and then asked
  # questions, so the last four lines come off.
  mkdir -p "$TMP/ltx"
  head -n "$(( $(wc -l < "$L") - 4 ))" "$L" > "$TMP/ltx/latex.ltx"
  # End with the *primitive* \end, which LaTeX preserves as \@@end -- LaTeX
  # redefines \end itself to close an environment. \@@end exists only if
  # latex.ltx actually loaded, so it doubles as a load check, and \fmtname
  # makes LaTeX say its own name (see the header's failure 2).
  printf '\\input latex.ltx\n\\immediate\\write-1{[fmt=\\fmtname]}\n\\catcode`\\@=11 \\@@end\n' > "$TMP/ltx/l.tex"
  ( cd "$TMP/ltx" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 timeout 300 "$BIN" pass l.tex --ini -o l.dvi >out.txt 2>err.txt )
  N=$(grep -oE '[0-9]+ undefined' "$TMP/ltx/err.txt" | head -1 | grep -oE '^[0-9]+')
  ERR=$(grep -oE 'aitex: [A-Za-z]+' "$TMP/ltx/err.txt" | head -1)
  grep -q 'fmt=LaTeX2e' "$TMP/ltx/out.txt" 2>/dev/null && LOADED=yes || LOADED=no
  if [ -z "$ERR" ] && [ -z "$N" ] && [ "$LOADED" = yes ]; then R=0; else R=1; fi
  gate "latex.ltx loads" "$R" "${N:-0} undefined; ${ERR:-no error}; \\fmtname=$LOADED"
else
  gate "latex.ltx loads" 1 "latex.ltx not found by kpsewhich"
fi

# ---- 6. GATE — characters above 127 -----------------------------------------
# docs/history/FINISH.md §1: input is byte-faithful, as pdfTeX is. Two claims, and the second
# is not implied by the first.
#
#   (a) A non-UTF-8 file must not panic. docs/history/PLAN.md §9 states the engine never panics
#       on arbitrary input, and real TeX reads Latin-1 without complaint.
#   (b) An accented UTF-8 document must set what pdfLaTeX sets. `naïve café`
#       came out `nave caf` — every non-ASCII character silently dropped —
#       because the reader decoded UTF-8 into chars and latex.ltx's own utf8
#       machinery, which makes the *bytes* active, never fired.
mkdir -p "$TMP/enc"
printf 'caf\xe9 na\xefve\n\\bye\n' > "$TMP/enc/latin1.tex"
( cd "$TMP/enc" && timeout 60 "$BIN" pass latin1.tex --ini -o l1.dvi >/dev/null 2>e.txt )
grep -qE "panicked at|not valid UTF-8" "$TMP/enc/e.txt" && R=1 || R=0
gate "latin-1 input: no panic" "$R" "$(grep -oE 'panicked at [^ ]*' "$TMP/enc/e.txt" | head -1)"

if L=$(kpsewhich latex.ltx 2>/dev/null) && [ -n "$L" ]; then
  mkdir -p "$TMP/u" && head -n "$(( $(wc -l < "$L") - 4 ))" "$L" > "$TMP/u/kernel.tex"
  # The kernel has to be *loaded*, and from a format, or this measures nothing:
  # the first version of this gate copied latex.ltx in and then ran a bare
  # `\documentclass{article}`, so our side had no LaTeX at all and set 0
  # characters — a red gate for the wrong reason, which is only marginally
  # better than a green one for the wrong reason. It is also the path that
  # matters: `\everyjob` restores the utf8 macros, and §1030 fires it when a
  # *format* is loaded, so running the kernel as ordinary input would skip the
  # very mechanism under test.
  printf '\\input kernel\n' > "$TMP/u/mkfmt.tex"
  ( cd "$TMP/u" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
      timeout 300 "$BIN" mkfmt.tex --ini --dump fmt.afmt >/dev/null 2>&1 )
  printf '\\documentclass{article}\n\\begin{document}\nna\xc3\xafve caf\xc3\xa9 \xc5\x93uvre\n\\end{document}\n' > "$TMP/u/u.tex"
  ( cd "$TMP/u" && timeout 300 pdflatex -output-format=dvi -interaction=nonstopmode u.tex >/dev/null 2>&1 )
  ( cd "$TMP/u" && AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
      timeout 300 "$BIN" pass u.tex --fmt fmt.afmt -o mine.dvi >/dev/null 2>&1 )
  tc=$(chars "$TMP/u/u.dvi" | wc -l); oc=$(chars "$TMP/u/mine.dvi" | wc -l)
  if [ "$tc" -eq 0 ]; then
    gate "utf-8 accents match pdflatex" 1 "pdflatex set nothing — check the oracle"
  else
    diff -q <(chars "$TMP/u/u.dvi") <(chars "$TMP/u/mine.dvi") >/dev/null
    gate "utf-8 accents match pdflatex" $? "pdflatex ${tc}c  ours ${oc}c"
  fi
fi

# ---- 7. GATE 3 — package coverage -------------------------------------------
P=$(./tools/packages.sh 2>&1)
PL=$(echo "$P" | grep -E '^[0-9]+ of [0-9]+ available packages load' | tail -1)
PN=$(echo "$PL" | grep -oE '^[0-9]+'); PD=$(echo "$PL" | grep -oE 'of [0-9]+' | grep -oE '[0-9]+')
no_cases "package coverage" "${PD:-0}" "a list of packages" || {
  [ "${PN:-0}" = "${PD:-1}" ]
  NOTYET=$(echo "$P" | grep -oE 'not yet:.*' | cut -c1-60)
  gate "package coverage" $? "$PL${NOTYET:+  ($NOTYET)}"
}

# ---- 8. GATE 2 — TRIP --------------------------------------------------------
# The DVI comparator is itself an instrument and must prove that it rejects a
# shifted rule/special rather than merely producing a parseable empty stream.
DS=$(./tools/dvi_semantic.sh --self-test 2>&1)
gate "dvi semantic comparator" $? "$DS"

# Page parity first: it is the coarsest way to be wrong, and a transcript diff
# underneath a page-count difference is noise.
T=$(./tools/trip.sh 2>&1)
TP=$(echo "$T" | grep -oE 'pages: ours [0-9]+, tex [0-9]+' | head -1)
TO=$(echo "$TP" | grep -oE 'ours [0-9]+' | grep -oE '[0-9]+')
TT=$(echo "$TP" | grep -oE 'tex [0-9]+' | grep -oE '[0-9]+')
no_cases "trip: pages" "${TT:-0}" "tex to ship some pages" || {
  [ "${TO:-0}" = "${TT:-1}" ]
  gate "trip: pages" $? "ours ${TO:-?}, tex ${TT:-?}"
}

TDVI=$(echo "$T" | grep -oE 'trip\.dvi vs this machine: [0-9]+ of [0-9]+ semantic records match \([0-9]+%\)')
TDVM=$(echo "$TDVI" | grep -oE ': [0-9]+ of' | grep -oE '[0-9]+')
TDVT=$(echo "$TDVI" | grep -oE 'of [0-9]+ semantic' | grep -oE '[0-9]+')
no_cases "trip: DVI semantics" "${TDVT:-0}" "visible DVI records to compare" || {
  [ "${TDVM:-0}" = "${TDVT:-1}" ]
  gate "trip: DVI semantics" $? "${TDVI:-no comparison}"
}

TL=$(./tools/trip_lists.sh 2>&1 | tail -1)
TLO=$(echo "$TL" | grep -oE '^showlists blocks: [0-9]+' | grep -oE '[0-9]+')
TLT=$(echo "$TL" | grep -oE 'of [0-9]+ match' | grep -oE '[0-9]+')
no_cases "trip: \\showlists blocks" "${TLT:-0}" "trip.tex to contain some" || {
  [ "${TLO:-0}" = "${TLT:-1}" ]
  gate "trip: \\showlists blocks" $? "$TL"
}

# The canonical proof: Knuth's protocol diffs `trip.log`. The engine writes one
# now, so this is a percentage rather than a disclaimer — but the gate is
# *equality*, not "a number was produced". `trip.sh` keeps the raw score and
# separately removes only TeX's representation-specific `mem[]` inventories
# and encoded DVI filename/byte count; this gate uses that semantic transcript.
# Grepping for the line alone would go green at 0% matching, which is the same
# class of mistake as a loop that examines nothing.
# Against *this machine's* tex, not Knuth's transcript. TeX Live only reaches
# 91% of Knuth's itself — the rest is environmental — so gating on Knuth would
# demand the engine reproduce differences that are not TeX's behaviour and not
# ours. trip.sh reports all three; this gates the one that is actionable.
TLOG=$(echo "$T" | grep -oE 'trip\.log semantic vs this machine: [0-9]+ of [0-9]+ lines match \([0-9]+%\)')
TLM=$(echo "$TLOG" | grep -oE ': [0-9]+ of' | grep -oE '[0-9]+')
TLT=$(echo "$TLOG" | grep -oE 'of [0-9]+ lines' | grep -oE '[0-9]+')
no_cases "trip: semantic transcript" "${TLT:-0}" "a tex transcript to compare against" || {
  [ "${TLM:-0}" = "${TLT:-1}" ]
  gate "trip: semantic transcript" $? "${TLOG:-no comparison}"
}

# ---- 9. GATE 4 — PDF output --------------------------------------------------
# The full varied suite is host-dependent and retained as versioned evidence.
# Its offline contract audit is cheap enough to block every done-check: a
# changed fixture, stale result, dropped coverage row, or failed recorded gate
# must never survive behind a remembered benchmark claim.
if [ -f tools/pdf_contract_check.py ]; then
  PDF_CONTRACT=$(python3 tools/pdf_contract_check.py 2>&1); R=$?
  gate "PDF qualification evidence" "$R" "$(echo "$PDF_CONTRACT" | tail -1)"
else
  gate "PDF qualification evidence" 1 "tools/pdf_contract_check.py is missing"
fi

# The dedicated check covers the package behavior this gate originally only
# promised in its heading: graphicx EPS inclusion, xcolor drawing state, and
# hyperref URI/internal links, destinations, outline and metadata. It also
# compares the underlying DVI placement to pdfTeX and validates the xref table.
if [ -x tools/pdf_check.sh ]; then
  PDFCHK=$(tools/pdf_check.sh 2>&1); R=$?
  gate "pdf output" "$R" "$(echo "$PDFCHK" | tail -1)"
else
  gate "pdf output" 1 "tools/pdf_check.sh is missing"
fi

# Source bytes and output-font encodings are independent compatibility axes.
# Keep them as separate gates so a working T1 font does not conceal a broken
# CP1252 reader (or vice versa), and so optional TeX Live encodings can never
# silently turn an empty loop into a pass.
if [ -x tools/encoding_check.sh ]; then
  ENCCK=$(tools/encoding_check.sh 2>&1); R=$?
  gate "source encoding matrix" "$R" "$(echo "$ENCCK" | tail -1)"
else
  gate "source encoding matrix" 1 "tools/encoding_check.sh is missing"
fi

if [ -x tools/font_matrix_check.sh ]; then
  FONTCK=$(tools/font_matrix_check.sh 2>&1); R=$?
  [ "$R" -eq 0 ] || printf '%s\n' "$FONTCK"
  gate "classic font matrix" "$R" "$(echo "$FONTCK" | tail -1)"
else
  gate "classic font matrix" 1 "tools/font_matrix_check.sh is missing"
fi

# A short feature fixture cannot expose retained-page growth, wrapped DVI
# postamble fields, or large PDF page trees. Keep the 10,000-page DVI/PDF run
# in the blocking battery so scale is a release property rather than a result
# remembered from one development machine.
if [ -x tools/scale_check.sh ]; then
  SCALECK=$(tools/scale_check.sh 2>&1); R=$?
  gate "10000-page DVI/PDF" "$R" "$(echo "$SCALECK" | head -1)"
else
  gate "10000-page DVI/PDF" 1 "tools/scale_check.sh is missing"
fi

if [ -x tools/perf_check.sh ]; then
  PERFCK=$(tools/perf_check.sh 2>&1); R=$?
  gate "long-document performance" "$R" "$(echo "$PERFCK" | tail -1)"
else
  gate "long-document performance" 1 "tools/perf_check.sh is missing"
fi

if [ -f tools/output_cache_check.py ]; then
  OUTPUT_CACHE=$(python3 tools/output_cache_check.py 2>&1); R=$?
  gate "persistent output cache" "$R" "$(echo "$OUTPUT_CACHE" | tail -1)"
else
  gate "persistent output cache" 1 "tools/output_cache_check.py is missing"
fi

if [ -f tools/watch_check.py ]; then
  WATCH=$(python3 tools/watch_check.py --binary "$BIN" \
    --fmt "$ROOT/.aitex/format/fmt.afmt" 2>&1); R=$?
  [ "$R" -eq 0 ] || printf '%s\n' "$WATCH"
  gate "native watch service" "$R" "$(echo "$WATCH" | tail -1)"
else
  gate "native watch service" 1 "tools/watch_check.py is missing"
fi

# The rebuilt-binary waiver is trustworthy only if both ordinary untracked and
# ignored compiler inputs are rejected. Keep that invariant in the blocking
# battery rather than relying on the holdout checker to exercise its own bad
# path during a normal release run.
if [ -f tools/holdout_selftest.py ]; then
  HOLDOUT_SELF=$(python3 tools/holdout_selftest.py 2>&1); R=$?
  gate "holdout policy self-test" "$R" "$(echo "$HOLDOUT_SELF" | tail -1)"
else
  gate "holdout policy self-test" 1 "tools/holdout_selftest.py is missing"
fi

# The historical 34-paper corpus is a useful exact-DVI regression suite, but
# it is not the product denominator. The blind holdout checker requires all
# 191 source roots, the exact release binary and locked environment hashes,
# conditional compile parity, final DVI/PDF artifacts, text quality, and the
# route-specific aggregate CPU and PDF-memory release ceilings.
if [ -f tools/holdout_check.py ]; then
  HOLDOUT_RESULTS=${AITEX_HOLDOUT_RESULTS:-target/qualification/holdout-200-current.results.json}
  HOLDOUT_ARGS=(--results "$HOLDOUT_RESULTS")
  if [ "${AITEX_ALLOW_REBUILT_BINARY:-0}" = 1 ]; then
    HOLDOUT_ARGS+=(--allow-rebuilt-binary)
  fi
  HOLDOUT=$(python3 tools/holdout_check.py "${HOLDOUT_ARGS[@]}" 2>&1); R=$?
  [ "$R" -eq 0 ] || printf '%s\n' "$HOLDOUT"
  gate "blind 191-paper holdout" "$R" "$(echo "$HOLDOUT" | head -1)"
else
  gate "blind 191-paper holdout" 1 "tools/holdout_check.py is missing"
fi

# ---- 10. GATE 1b — real-corpus parity ---------------------------------------
# The arXiv set, ours against pdfTeX wherever pdfTeX succeeds. 100% is not the
# bar and never was — a large minority of arXiv projects do not compile under
# pdflatex either, and those are out of the denominator (corpus/meta/clean.txt,
# built by tools/classify_corpus.sh).
if [ -x tools/corpus_check.sh ] && [ -s corpus/meta/clean.txt ]; then
  # The *summary* line, not the last one: `corpus_check.sh` ends with a
  # `not yet: …` roll-call of the projects that failed, so `tail -1` read the
  # roll-call, found no numbers in it, and this gate reported "examined
  # NOTHING" while the run underneath had examined all 34. A harness bug that
  # hides a real result is the same failure this file's header is about, in the
  # other direction.
  CO=$(./tools/corpus_check.sh 2>&1)
  C=$(echo "$CO" | grep -E '^[0-9]+ of [0-9]+ projects' | tail -1)
  CN=$(echo "$C" | grep -oE '^[0-9]+'); CD=$(echo "$C" | grep -oE 'of [0-9]+' | grep -oE '[0-9]+')
  no_cases "corpus parity" "${CD:-0}" "corpus/meta/clean.txt to list projects" || {
    [ "${CN:-0}" = "${CD:-1}" ]
    gate "corpus parity" $? "$C"
  }
else
  gate "corpus parity" 1 "tools/corpus_check.sh not built yet (docs/history/FINISH.md P7)"
fi

if [ -x tools/repo_check.sh ]; then
  CLEAN=$(tools/repo_check.sh --artifacts-only 2>&1); R=$?
  gate "repository hygiene" "$R" "$CLEAN"
else
  gate "repository hygiene" 1 "tools/repo_check.sh is missing"
fi

echo
if [ "$FAILED" = 0 ]; then
  echo "DONE — every gate passes."
else
  echo "NOT DONE — see the failing gates above."
fi
exit "$FAILED"
