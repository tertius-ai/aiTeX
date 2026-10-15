#!/usr/bin/env bash
# Where do we and real TeX first disagree about what to expand?
#
# Three problems are stuck at the same place: amsmath's `align`, pgf's
# unknown-key fallback, and TRIP's missing pages. In each, every ingredient
# tested in isolation matches TeX to the token and the composition still
# differs. Testing more ingredients one at a time has stopped paying; what is
# needed is the *first divergence*, which is what this finds.
#
# Both engines are asked for the sequence of macro expansions and the two are
# diffed. TeX gives it via `\tracingmacros=2` (`\name ->body` per expansion);
# we give it via `AITEX_STEP_TRACE`. Neither trace is the other's format, so
# both are reduced to the one thing they agree on — *which control sequence was
# expanded, in order*. That is coarse, and it is enough: an engine that expands
# the same sequence and produces different output has a different bug from one
# that diverges at expansion 4,000, and this says which.
#
# A predecessor of this existed for the layered engine and was deleted with it
# (`docs/ARCHITECTURE.md` §8). This is the same idea against `fast.rs`.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
[ $# -ge 1 ] || { echo "usage: difftrace.sh <file.tex> [max-expansions]"; exit 1; }
# The bound is on *entries kept*, not on how far the run goes. Bounding the
# step window instead silently truncated our side mid-document and every
# expansion after the cut read as a divergence — the reference engine has no
# such bound, so the two must not be compared under one.
SRC="$1"; CAP="${2:-2000000}"
LTX="$(kpsewhich latex.ltx)"
export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
# KEEP=1 leaves the traces behind; they are what you actually read once
# this points at a divergence.
TMP="$(mktemp -d)"
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$TMP"' EXIT
[ "${KEEP:-0}" = 1 ] && echo "traces in $TMP"
cp "$SRC" "$TMP/in.tex"
head -n "$(( $(wc -l < "$LTX") - 4 ))" "$LTX" > "$TMP/kernel.tex"
cd "$TMP" || exit 1

# Both traces must cover the *same span*, and the span that matters is the
# document, not the kernel. TeX's `\tracingmacros` is switched on after
# `\input kernel`; ours starts at the first command, so it would spend its
# whole entry budget inside the kernel load and compare the wrong thing.
#
# A dumped format fixes that: the run starts *past* the kernel, so the trace
# begins where TeX's does.
printf '\\input kernel\n' > mkfmt.tex
AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
  timeout 600 "$FAST" mkfmt.tex --dump fmt.afmt >/dev/null 2>&1
[ -s fmt.afmt ] || { echo "could not build a format"; exit 1; }

AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
  AITEX_STEP_TRACE="1,18446744073709551615" AITEX_TRACE_CAP="$CAP" \
  AITEX_DVI=o.dvi timeout 600 "$FAST" pass in.tex --fmt fmt.afmt >ours.raw 2>&1
# Our trace records several kinds (`prim`, `exp`, `num`, `macro`, `delim`);
# `\tracingmacros` reports exactly the last two — a macro call, undelimited or
# delimited. The others are primitive dispatch, which TeX does not trace, and
# comparing them against it diverges at entry 1 every time.
#
# `delim` is not optional: expl3 is built out of delimited `:w` functions, so
# dropping it loses two entries per generated character and desynchronises
# the two sequences inside the first filename LaTeX reads. `toks` is §323's
# token-list expansion, which TeX prints in the same `\\name->body` form.
sed -nE 's/^ *\[[0-9]+\] (macro|delim|toks) \\([^ ]*).*$/\2/p' ours.raw > ours.seq

# TeX. `\tracingmacros=2` also traces token-list expansions, which is what makes
# the two comparable; `\tracingonline=0` keeps it in the log.
{ printf '\\input kernel\n'
  printf '\\tracingmacros=2 \\tracingonline=0\n'
  cat in.tex; } > ref.tex
max_print_line=4000 timeout 600 pdftex -ini -etex -interaction=batchmode -output-format=dvi ref.tex </dev/null >/dev/null 2>&1
# TeX writes `<cs><parameter text>-><body>`, and both halves of that are
# easy to under-match. `print_cs` appends a space to a multi-letter name, so a
# *robust* command's inner control sequence — `\makeatletter␣`, what
# `\DeclareRobustCommand` really defines — prints with TWO spaces, and the
# parameter text is arbitrary delimiter tokens (`#1[#2][#3]#4`), not just
# `#n`. Matching one optional space and `(#[0-9])*` dropped every robust
# command and every macro with a delimited parameter.
#
# A parameter text cannot contain `{` (that would start the body), so `[^{]*`
# spans it without running into a wrapped body line.
#
# The escape character is *optional* because it is not always a backslash:
# `\set@curr@file@aux` sets `\escapechar=-1` for the whole of LaTeX's file-name
# handling, and every name TeX logs in there is printed bare. Requiring a
# leading backslash dropped all of them and reported a divergence at the first
# file LaTeX opens — which is to say, on every real document. Both sides are
# reduced to the bare name for the same reason.
# Splitting a logged trace into name and parameter text is not decidable from
# the log. TeX prints them adjacent, and both can contain spaces:
#
#   __hook class/before ->...        a name with spaces, and no parameters
#   __prg_F_true:w \fi: \use:n ->    a name, then delimiters, and no parameters
#
# and inside LaTeX's file-name handling `\escapechar` is -1, so the delimiters
# lose the backslash that is the last thing distinguishing the two cases.
#
# So both sides are cut at the first space. That collapses LaTeX's hooks —
# every one of them is `\__hook <name>` — to a bare `__hook`, which is a real
# loss of resolution and the right trade anyway: it is symmetric, so it cannot
# manufacture a divergence, and the full names are still in `ours.raw`, which
# is what you read once this points at a place.
#
# The leading escape character is optional for the same `\escapechar=-1`
# reason. That makes such a line indistinguishable from any other line
# containing `->`, so the first character must still look like the start of a
# control sequence — otherwise the `(filename` lines TeX prints match too.
grep -E '^(\\|[a-zA-Z@_:])' ref.log | grep -F -- '->' | awk '{
    i = index($0, "->"); if (i == 0) next
    n = substr($0, 1, i - 1)
    s = index(n, " "); if (s) n = substr(n, 1, s - 1)
    sub(/^\\/, "", n)
    if (n != "") print n
  }' > ref.seq

printf 'expansions: ours %s, tex %s\n' "$(wc -l < ours.seq)" "$(wc -l < ref.seq)"
# A truncated trace diverges at its own end, which is not a finding.
[ "$(grep -cE '^ *\[[0-9]+\] ' ours.raw)" -lt "$CAP" ] ||
  echo "WARNING: our trace hit the $CAP-entry cap; raise it (argument 2)"
if cmp -s ours.seq ref.seq; then
  echo "the same sequence throughout — the divergence is not in *what* is expanded"
  exit 0
fi
# The first *index* at which the two disagree — not `diff`'s first changed
# hunk. `diff` computes a minimal edit script, and when a name repeats (LaTeX
# expands `\@currname` a dozen times in a row) it is free to align the
# insertion anywhere in the run. It reported a divergence forty entries past
# the real one, which is forty entries of the wrong macro to go read.
n=$(awk 'NR==FNR{a[FNR]=$0; na=FNR; next}
         {if (FNR<=na && a[FNR]!=$0) {print FNR; exit}}
         END{if (!seen) exit}' ours.seq ref.seq)
: "${n:=$(( $(wc -l < ours.seq) + 1 ))}"
lo=$(( n > 8 ? n - 8 : 1 ))
echo
echo "first divergence at expansion $n:"
echo "  context (ours | tex), '>' marks it:"
paste <(sed -n "$lo,$(( n + 8 ))p" ours.seq) <(sed -n "$lo,$(( n + 8 ))p" ref.seq) \
  | expand -t 40 | awk -v lo="$lo" -v n="$n" '{printf "  %s %5d  %s\n", (NR+lo-1==n ? ">" : " "), NR+lo-1, $0}'
