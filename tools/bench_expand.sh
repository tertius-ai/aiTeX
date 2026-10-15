#!/usr/bin/env bash
# Head-to-head against pdfTeX on workloads BOTH engines complete, so the numbers
# mean something. The latex.ltx bench (tools/parity.sh) stays gated on a complete
# load -- timing a load that skips work against one that does not is not a
# measurement, it is a flattering number.
#
# Two workloads:
#   expand  a tail-recursive counter loop -- \ifnum, \advance, \let and a macro
#           call per iteration. No files, no fonts, no hyphenation: this is the
#           expansion/execution core, which is what the fast engine exists to
#           make fast. Both engines are checked to produce the same count first.
#   plain   loading plain.tex to completion. Real input, but NOT symmetric:
#           pdfTeX also builds the hyphenation trie and reads TFM metrics, which
#           we skip, so this one flatters us. Read it as a sanity check, not a
#           verdict.
#
# The latex.ltx row used to flatter us badly and no longer does: with \openin
# and \read reading real files, both engines now parse expl3's Unicode tables
# and babel's hyphenation patterns, which is most of the work in a kernel load.
#
# Process startup is measured separately and subtracted, since pdfTeX's fixed
# cost (~12ms) would otherwise swamp a short run.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
REPS="${REPS:-5}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
command -v pdftex >/dev/null || { echo "pdftex not found"; exit 1; }
command -v tex >/dev/null || { echo "tex not found"; exit 1; }

best() { # <cmd...> -> best wall ms of $REPS
  local b=999999 i t0 t1 m
  for ((i = 0; i < REPS; i++)); do
    t0=$(date +%s%N); "$@" >/dev/null 2>&1; t1=$(date +%s%N)
    m=$(( (t1 - t0) / 1000000 )); [ "$m" -lt "$b" ] && b=$m
  done
  echo "$b"
}

cd "$TMP"
printf '\\end\n' > empty.tex
N="${N:-400000}"
cat > loop.tex <<EOF
\\catcode\`\\{=1
\\catcode\`\\}=2
\\catcode\`\\#=6
\\count0=0
\\def\\iterate{\\ifnum\\count0<$N \\advance\\count0 by1 \\let\\next\\iterate\\else\\let\\next\\relax\\fi\\next}
\\iterate
\\message{[COUNT=\\the\\count0]}
\\end
EOF
printf '\\catcode`\\{=1\n\\catcode`\\}=2\n\\catcode`\\#=6\n\\let\\primend\\end\n\\input plain.tex\n\\primend\n' > plainload.tex

# Same answer, or the timing is meaningless.
tex -ini -interaction=batchmode loop.tex >/dev/null 2>&1
want=$(grep -aoE 'COUNT=[0-9]+' loop.log | head -1)
got=$(AITEX_NO_INPUT=1 AITEX_MAX_STEPS=900000000 AITEX_MAX_COMMANDS=900000000 \
        "$FAST" pass loop.tex --ini 2>&1 | grep -aoE 'count0=[0-9]+' | head -1)
[ "${want#COUNT=}" = "${got#count0=}" ] || {
  echo "DISAGREE: tex $want vs aitex $got -- not benchmarking that"; exit 1; }
echo "loop agrees: $want"

# The reference is `tex` — TeX82 — wherever the case will run under it, and
# `pdftex -ini -etex` only where e-TeX is unavoidable (latex.ltx will not load
# without it). Two startup baselines, since they are two binaries.
su_t=$(best tex -ini -interaction=batchmode empty.tex)
su_p=$(best pdftex -ini -etex -interaction=batchmode empty.tex)
su_a=$(best env AITEX_NO_INPUT=1 "$FAST" pass empty.tex --ini)
printf '\nstartup            tex %5s ms   pdftex %5s ms   aitex %5s ms\n' "$su_t" "$su_p" "$su_a"

run() { # <label> <ref-engine: tex|etex> <ref-file> <aitex-env-and-file...>
  local label="$1" eng="$2" file="$3"; shift 3
  local p a np na su_r name
  if [ "$eng" = tex ]; then
    p=$(best tex -ini -interaction=batchmode "$file"); su_r=$su_t; name=tex
  else
    p=$(best pdftex -ini -etex -interaction=batchmode "$file"); su_r=$su_p; name=etex
  fi
  a=$(best "$@")
  np=$((p - su_r)); na=$((a - su_a))
  [ "$np" -lt 1 ] && np=1
  [ "$na" -lt 1 ] && na=1
  printf '%-10s (net)   %-6s %5s ms   aitex %5s ms   aitex/ref %sx\n' \
    "$label" "$name" "$np" "$na" "$(echo "scale=2; $na/$np" | bc)"
}

run "expand" tex loop.tex env AITEX_NO_INPUT=1 AITEX_MAX_STEPS=900000000 AITEX_MAX_COMMANDS=900000000 "$FAST" pass loop.tex --ini
run "plain" tex plainload.tex env AITEX_MAX_STEPS=900000000 "$FAST" pass plainload.tex --ini

# latex.ltx: both engines now run it to completion, but NOT doing the same work.
LTXF=$(kpsewhich latex.ltx)
printf '\\let\\primend\\end\n\\input latex.ltx\n\\primend\n' > ltxload.tex
run "latex.ltx" etex ltxload.tex env AITEX_MAX_STEPS=2000000000 AITEX_MAX_COMMANDS=2000000000 "$FAST" pass "$LTXF" --ini
undef=$(AITEX_MAX_STEPS=2000000000 AITEX_MAX_COMMANDS=2000000000 "$FAST" pass "$LTXF" --ini 2>&1 |
          grep -aoE 'undef=[0-9]+ \([0-9]+ distinct\)' | head -1)
echo
echo "The latex.ltx row is close to like-for-like now: $undef, and since"
echo "\\openin/\\read read real files both engines load expl3's Unicode tables"
echo "(UnicodeData.txt is 1.9MB and dominates the load) and babel's hyphenation"
echo "patterns. What we still skip there is TFM metrics and typesetting."
echo "The 'expand' row remains the strictly like-for-like number, and it says we"
echo "are SLOWER per operation."
echo
echo "note: 'plain' still flatters us (hyphenation trie + TFM)."
