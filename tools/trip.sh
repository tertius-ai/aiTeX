#!/usr/bin/env bash
# Knuth's TRIP test — what it says about us.
#
# TRIP is the canonical proof of a TeX implementation, and the first thing a
# knowledgeable reader asks about. We do not pass it. docs/history/PLAN.md §4 treats it as a
# late confidence check rather than a gate, which is the same call XeTeX and
# LuaTeX made — but "not a gate" is not a reason to leave it unrun, and it was
# unrun until 2026-08.
#
# The full protocol dumps a format, runs against it, and compares both pass
# transcripts, the terminal transcript, and `dvitype trip.dvi` against Knuth's
# references. We are not at parity yet, so this script reports gradients as
# well as hard milestones. In particular, the INITEX transcript is its own
# small semantic target; hiding it behind a successful format dump made a large
# part of the remaining work invisible.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
TMP="$(mktemp -d)"
# KEEP=1 leaves the two runs on disk, which is the only way to diff the
# transcripts by hand.
if [ -n "${KEEP:-}" ]; then
  echo "runs kept in $TMP"
else
  trap 'rm -rf "$TMP"' EXIT
fi
cp "$ROOT"/vendor/trip/* "$TMP/" || exit 1
cd "$TMP" || exit 1
# Knuth's reference transcript, out of the way before either engine runs: the
# job is named `trip`, so both `tex` and `aitex` write `trip.log` and would
# otherwise overwrite the very file they are being compared against.
[ -f trip.log ] && cp trip.log knuth-trip.log
[ -f tripin.log ] && cp tripin.log knuth-tripin.log
command -v pltotf >/dev/null && pltotf trip.pl trip.tfm >/dev/null 2>&1

# TRIP is a *two-pass* test and was not being run as one. `trip.tex` line 92
# dumps `trip.fmt` and halts if no format was preloaded; the real test is the
# second pass, against that format. One pass measures something else.
#
# `--tex82` is equally deliberate. aiTeX defaults to e-TeX's 32768 registers
# because modern LaTeX requires them, while canonical TRIP checks that register
# 256 is rejected. Compatibility mode makes that test meaningful without
# narrowing the product default.
AITEX_MAX_COMMANDS=200000000 AITEX_MAX_STEPS=200000000 \
  AITEX_DVI=pass1.dvi timeout 300 "$FAST" pass trip.tex --ini --tex82 >/dev/null 2>err.txt
case $? in
  124) echo "TRIP pass 1: DOES NOT FINISH within the timeout."; exit 1 ;;
  134) echo "TRIP pass 1: CRASHES."; exit 1 ;;
esac
if [ ! -s trip.fmt ]; then echo "TRIP pass 1: no trip.fmt written"; exit 1; fi
[ -f trip.log ] && cp trip.log ours-tripin.log
echo "TRIP pass 1: dumps trip.fmt ($(stat -c%s trip.fmt) bytes) and halts, as it should."

AITEX_MAX_COMMANDS=200000000 AITEX_MAX_STEPS=200000000 \
  AITEX_DVI=trip-ours.dvi timeout "${TRIP_TYPESET_TIMEOUT:-300}" "$FAST" pass trip.tex --fmt trip.fmt --tex82 \
  >/dev/null 2>err2.txt
case $? in
  124) echo "TRIP pass 2: DOES NOT FINISH within the timeout." ;;
  134) echo "TRIP pass 2: CRASHES." ;;
  *)   echo "TRIP pass 2: finishes." ;;
esac
# Our transcript, taken *now*. The job is named `trip`, so every engine that
# runs below — and the page-count comparison runs real `tex` twice — writes
# `trip.log` over it. Comparing whatever is left at the end of this script
# measured TeX Live's batchmode log against Knuth's and called the result ours.
[ -f trip.log ] && cp trip.log ours-trip.log

# Knuth's comparison is `dvitype trip.dvi` against `trip.typ`. Page count is
# retained as the coarsest result, and the semantic comparison below now also
# measures what every page draws without confusing harmless opcode choices for
# output differences.
if command -v dvitype >/dev/null; then
  rm -f trip.dvi
  timeout 300 tex -ini -interaction=batchmode trip.tex >/dev/null 2>&1
  [ -f trip.log ] && cp trip.log tex-tripin.log
  timeout 300 tex -interaction=batchmode "&trip" trip.tex >/dev/null 2>&1
  # This machine's TeX ran the full two-pass protocol just now, so its own
  # transcript is here — keep it, it is the oracle that matters (see below).
  [ -f trip.log ] && cp trip.log tex-trip.log
  ours=$(dvitype trip-ours.dvi 2>/dev/null | grep -c "beginning of page")
  theirs=$(dvitype trip.dvi 2>/dev/null | grep -c "beginning of page")
  knuth=$(grep -c "beginning of page" trip.typ)
  echo
  printf "pages: ours %s, tex %s, Knuth trip.typ %s\n" "$ours" "$theirs" "$knuth"
  echo
  "$ROOT/tools/dvi_semantic.sh" trip.dvi trip-ours.dvi || true
fi
echo
echo "primitives TRIP found undefined, by hit count:"
grep -aE '^ +[0-9]+ +\\' err.txt | head -30
n=$(grep -acE '^ +[0-9]+ +\\' err.txt || true)
echo
echo "$n distinct control sequences still missing."

# BISECT=1: which line of trip.tex first makes us typeset something TeX does
# not?
#
# `tex -ini trip.tex` dumps the format at line 92 and ships *no pages* — it
# sets not one character. So in pass one any character we set is a fault, and
# the line that introduces it can be found by bisection: run both engines on
# the first N lines and compare how many characters each sets.
#
# The cut file gets a `\fi` (trip.tex's `\ifx\initex\undefined` block opens at
# line 11 and closes at 91) and an `\end`. Both engines see the same file, so
# the errors that a truncated TRIP provokes are the same for both and cancel;
# it is the *character count* that is compared, nothing else.
#
# Observing from outside like this matters. Every attempt to bisect trip.tex by
# inserting a `\message` at the suspect line measured the probe instead: a
# `\message` in the middle of a number scan changes what the scan does, and
# differently in each engine, so two "findings" in a row turned out to be
# artifacts of looking.
if [ -n "${BISECT:-}" ]; then
  command -v tex >/dev/null || { echo "BISECT needs real tex"; exit 1; }
  chars() { dvitype -output-level=2 -page-start='*' "$1" 2>/dev/null | grep -c 'setchar'; }
  probe() { # $1 = line count; echoes "<ours> <tex>"
    { head -n "$1" trip.tex; printf '\\fi\n\\end\n'; } > cut.tex
    AITEX_MAX_COMMANDS=200000000 AITEX_MAX_STEPS=200000000 \
      AITEX_DVI=cut-ours.dvi timeout 120 "$FAST" pass cut.tex --ini --tex82 >/dev/null 2>&1
    timeout 120 tex -ini -interaction=batchmode cut.tex >/dev/null 2>&1
    echo "$(chars cut-ours.dvi) $(chars cut.dvi)"
  }
  total=$(wc -l < trip.tex)
  read -r o t <<EOF
$(probe "$total")
EOF
  if [ "$o" = "$t" ]; then
    echo "BISECT: we set the same $o characters as TeX over all $total lines."
    exit 0
  fi
  echo "BISECT: over $total lines we set $o characters, TeX $t."
  lo=1; hi=$total
  while [ "$lo" -lt "$hi" ]; do
    mid=$(( (lo + hi) / 2 ))
    read -r o t <<EOF
$(probe "$mid")
EOF
    if [ "$o" != "$t" ]; then hi=$mid; else lo=$((mid + 1)); fi
  done
  printf 'BISECT: first divergence is introduced by line %s:\n' "$lo"
  sed -n "${lo}p" trip.tex | sed 's/^/    /'
  exit 0
fi

# How long does real TeX take on the same file? TRIP is small; the interesting
# number is that it finishes at all.
if command -v tex >/dev/null; then
  t0=$(date +%s%N)
  timeout 300 tex -ini -interaction=batchmode trip.tex >/dev/null 2>&1
  t1=$(date +%s%N)
  echo
  printf 'tex -ini on trip.tex: %s ms (it does the typesetting pass too)\n' "$(( (t1-t0)/1000000 ))"
  t0=$(date +%s%N)
  AITEX_MAX_COMMANDS=200000000 AITEX_MAX_STEPS=200000000 timeout 300 "$FAST" pass trip.tex --ini --tex82 >/dev/null 2>&1
  t1=$(date +%s%N)
  printf 'aitex expansion pass:  %s ms (no typesetting — not comparable, listed for scale)\n' "$(( (t1-t0)/1000000 ))"
fi

# The canonical comparison: Knuth's `trip.log` against ours.
#
# This was "not attempted" for as long as the engine wrote no `.log` at all.
# It does now (`\jobname.log`, opened as §534 opens one and closed as §1333
# closes one), so the diff is a real number instead of a disclaimer.
#
# It is reported as *matching lines*, not as pass/fail. A transcript diff is a
# gradient — that is the property docs/history/PLAN.md §4 says makes TRIP useful — and a single
# early divergence shifts every line after it, so a raw `diff` line count
# understates progress badly. `comm` on sorted lines would overstate it. What is
# counted here is the longest common subsequence, which is what `diff` already
# computes: total lines minus the lines it reports as changed.
# The transcript comparison, against two references, because they answer two
# different questions and conflating them wasted a measurement already.
#
#   ours vs *this machine's* tex  — the actionable oracle. Every other tool here
#       is differential against the local engine for the same reason: a
#       divergence is then unambiguously ours.
#   tex  vs Knuth                 — the ceiling. TeX Live is not byte-identical
#       to Knuth's 1990 transcript either, and the gap is environmental: build
#       parameters, `\batchmode`, thirty years of `tex.ch`. Reported so nobody
#       reads the row below it as a defect count.
#   ours vs Knuth                 — the canonical claim, and the one a
#       knowledgeable reader asks about. Bounded above by the row before it.
#
# The first thing this comparison found was a difference that is *not* ours:
# Knuth truncates an error context at `\gobble...` where TeX Live writes
# `\gobble\newcs pos`, and ours agrees with TeX Live. `error_line`/
# `half_error_line` are kpathsea variables (79/50 here, 72/42 in tex.web), and
# setting them to Knuth's values does not reproduce his line either — so it is
# not the parameter, and chasing it against the wrong reference would have been
# a day spent making the engine less like TeX.
lcs() { # $1=reference $2=ours; echoes "<matching> <total>"
  tail -n +2 "$1" > .ref.log; tail -n +2 "$2" > .ours.log
  n=$(wc -l < .ref.log)
  chg=$(diff .ref.log .ours.log | grep -ac '^<')
  echo "$(( n - chg )) $n"
}
# TeX's string pool, mem[], hash, flattened font-word array, and packed trie
# have no counterparts in aiTeX's Rust representation. Keep the raw local
# score visible above the semantic score, and remove only those eight §1327
# inventory lines for the pass-one completion comparison. Format identity,
# every loaded font, the exception count, and the no-page status remain exact.
tripin_body() {
  tail -n +2 "$1" \
    | grep -avE '^[0-9]+ (strings of total length|memory locations dumped|multiletter control sequences|words of font info)' \
    | grep -avE '^Hyphenation trie of length |^  [0-9]+ for language '
}
lcs_tripin() { # $1=reference $2=ours; echoes "<matching> <total>"
  tripin_body "$1" > .ref.log; tripin_body "$2" > .ours.log
  n=$(wc -l < .ref.log)
  chg=$(diff .ref.log .ours.log | grep -ac '^<')
  echo "$(( n - chg )) $n"
}
# The main transcript has the same representation boundary as the format
# transcript, in two places. §639's per-page lines count TeX's `mem[]` words,
# and §1334's closing inventory counts that array, its string pool, hash, font
# word array, and stack capacities. None exists in aiTeX's typed Rust model;
# aiTeX reports its own allocator facts under its own header instead. The DVI
# filename and encoded byte count are likewise checked more strongly above by
# the exact 220-record semantic comparator.
#
# Keep the raw score visible, and remove only those enumerated records for the
# completion comparison. A blank printed solely after a §639 inventory goes
# with that inventory; every other blank, command, error, context, box line,
# page cost, and final-cleanup warning remains byte-for-byte significant.
trip_body() {
  tail -n +2 "$1" | awk '
    function flush_blanks(  i) {
      for (i=0; i<blanks; i++) print ""
      blanks=0
    }
    {
      sub(/[[:space:]]+$/, "")
      if ($0 ~ /^Memory usage before:/) { after_mem=1; next }
      if (after_mem && $0 == "") { after_mem=0; next }
      after_mem=0
      if ($0 ~ /^Here is how much of /) { blanks=0; stats=1; next }
      if (stats) {
        if ($0 ~ /^Output written on /) stats=0
        next
      }
      if ($0 ~ /^Output written on /) next
      if ($0 == "") { blanks++; next }
      flush_blanks(); print
    }
    END { flush_blanks() }
  '
}
lcs_trip() { # $1=reference $2=ours; echoes "<matching> <total>"
  trip_body "$1" > .ref.log; trip_body "$2" > .ours.log
  n=$(wc -l < .ref.log)
  chg=$(diff .ref.log .ours.log | grep -ac '^<')
  echo "$(( n - chg )) $n"
}
pct() { [ "$2" -gt 0 ] && echo $(( $1 * 100 / $2 )) || echo 0; }

echo
if [ -f tex-tripin.log ] && [ -f ours-tripin.log ]; then
  read -r m n <<EOF
$(lcs tex-tripin.log ours-tripin.log)
EOF
  printf 'tripin.log raw vs this machine:      %s of %s lines match (%s%%); ours %s lines\n' \
    "$m" "$n" "$(pct "$m" "$n")" "$(( $(wc -l < ours-tripin.log) - 1 ))"
  read -r m n <<EOF
$(lcs_tripin tex-tripin.log ours-tripin.log)
EOF
  printf 'tripin.log semantic vs this machine: %s of %s lines match (%s%%)\n' \
    "$m" "$n" "$(pct "$m" "$n")"
fi
if [ -f knuth-tripin.log ] && [ -f tex-tripin.log ]; then
  read -r m n <<EOF
$(lcs_tripin knuth-tripin.log tex-tripin.log)
EOF
  printf 'tripin.log semantic vs Knuth, ceiling: %s of %s (%s%%) — what this machine'"'"'s tex reaches\n' \
    "$m" "$n" "$(pct "$m" "$n")"
fi
if [ -f knuth-tripin.log ] && [ -f ours-tripin.log ]; then
  read -r m n <<EOF
$(lcs_tripin knuth-tripin.log ours-tripin.log)
EOF
  printf 'tripin.log semantic vs Knuth, ours:    %s of %s (%s%%)\n' "$m" "$n" "$(pct "$m" "$n")"
fi
if [ -f tex-tripin.log ] || [ -f ours-tripin.log ]; then
  echo
fi
if [ -f tex-trip.log ] && [ -f ours-trip.log ]; then
  read -r m n <<EOF
$(lcs tex-trip.log ours-trip.log)
EOF
  printf 'trip.log vs this machine: %s of %s lines match (%s%%); ours %s lines\n' \
    "$m" "$n" "$(pct "$m" "$n")" "$(( $(wc -l < ours-trip.log) - 1 ))"
  read -r m n <<EOF
$(lcs_trip tex-trip.log ours-trip.log)
EOF
  printf 'trip.log semantic vs this machine: %s of %s lines match (%s%%)\n' \
    "$m" "$n" "$(pct "$m" "$n")"
  if [ "$m" = "$n" ]; then
    echo 'trip.log semantic comparison: exact'
  fi
fi
if [ -f knuth-trip.log ] && [ -f tex-trip.log ]; then
  read -r m n <<EOF
$(lcs knuth-trip.log tex-trip.log)
EOF
  printf 'trip.log vs Knuth, ceiling: %s of %s (%s%%) — what this machine'"'"'s tex reaches\n' \
    "$m" "$n" "$(pct "$m" "$n")"
fi
if [ -f knuth-trip.log ] && [ -f ours-trip.log ]; then
  read -r m n <<EOF
$(lcs knuth-trip.log ours-trip.log)
EOF
  printf 'trip.log vs Knuth, ours:    %s of %s (%s%%)\n' "$m" "$n" "$(pct "$m" "$n")"
fi
[ -f ours-trip.log ] || echo "trip.log: not produced — no transcript to compare."
