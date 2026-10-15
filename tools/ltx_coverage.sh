#!/usr/bin/env bash
# Does our latex.ltx load end in the same state as pdfTeX's?
#
# `undef=N` counts control sequences the engine *tripped over*; it says nothing
# about code that silently never ran. This does: harvest the macros latex.ltx
# defines with \def/\gdef/\DeclareRobustCommand, sample them, and ask BOTH
# engines which exist after a full load.
#
# It is a differential oracle, not a checklist. "Every harvested name must be
# defined" is the wrong question -- latex.ltx ends with `\let\reserved@b=\@undefined`,
# and `\newattribute`/`\now@and@everyjob` live in a LuaTeX-only branch that pdfTeX
# skips too. Those are *correct* absences, and asking pdfTeX is how we tell them
# apart from work we skipped.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
N="${N:-300}"
LTX="$(kpsewhich latex.ltx)"
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
[ -n "$LTX" ] || { echo "latex.ltx not found"; exit 1; }
command -v pdftex >/dev/null || { echo "pdftex not found"; exit 1; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Names latex.ltx defines itself. Deliberately only the unambiguous forms --
# \csname-built names and \let aliases are skipped rather than guessed at.
{
  grep -oE '^\\(long\\)?\\?(def|gdef)\\[a-zA-Z@]+' "$LTX" | sed 's/.*\\\(def\|gdef\)\\//'
  grep -oE '^\\DeclareRobustCommand\\[a-zA-Z@]+'    "$LTX" | sed 's/.*\\//'
} | sort -u > "$TMP/all.txt"

total=$(wc -l < "$TMP/all.txt")
# Deterministic sample, so the number is comparable run to run.
shuf -n "$N" --random-source=<(yes) "$TMP/all.txt" | sort > "$TMP/sample.txt"
paste -sd, "$TMP/sample.txt" > "$TMP/sample.csv"

# --- our answer -------------------------------------------------------------
AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 \
  "$FAST" pass "$LTX" --ini --probe "$(cat "$TMP/sample.csv")" 2>&1 |
  grep -aE '^  (DEF|undef)' |
  sed -E 's/^  (DEF|undef).*\\([a-zA-Z@]+)$/\2 \1/' | sed 's/ DEF/ DEF/' | sort > "$TMP/ours.txt"

# --- pdfTeX's answer --------------------------------------------------------
# Drop latex.ltx's trailing `\makeatother \errorstopmode \dump` so the run can be
# asked questions instead of dumping a format.
head -$(( $(wc -l < "$LTX") - 4 )) "$LTX" > "$TMP/body.tex"
{
  echo '\makeatletter'
  echo "\\@for\\reserved@z:=$(cat "$TMP/sample.csv")\\do{%"
  echo '  \message{<<\reserved@z=\expandafter\ifx\csname\reserved@z\endcsname\relax undef\else DEF\fi>>}}'
  echo '\makeatother'
} >> "$TMP/body.tex"
cat > "$TMP/drv.tex" <<'EOF'
\input body
\end
EOF
( cd "$TMP" && timeout 300 pdftex -ini -etex -interaction=nonstopmode drv.tex </dev/null >/dev/null 2>&1 )
tr -d '\n' < "$TMP/drv.log" | grep -aoE '<<[a-zA-Z@]+=(DEF|undef)>>' |
  sed -E 's/<<([a-zA-Z@]+)=(DEF|undef)>>/\1 \2/' | sort > "$TMP/pdftex.txt"

[ -s "$TMP/pdftex.txt" ] || { echo "pdftex produced no answers -- see $TMP/drv.log"; exit 1; }

# --- compare ----------------------------------------------------------------
join "$TMP/ours.txt" "$TMP/pdftex.txt" > "$TMP/joined.txt"
checked=$(wc -l < "$TMP/joined.txt")
agree=$(awk '$2==$3' "$TMP/joined.txt" | wc -l)
pct=$(echo "scale=1; 100*$agree/$checked" | bc)

echo "latex.ltx defines $total macros by \\def/\\gdef/\\DeclareRobustCommand"
echo "sampled $checked:  agree with pdftex $agree   differ $((checked - agree))   ${pct}%"
echo "  (of those, pdftex itself leaves $(awk '$3=="undef"' "$TMP/joined.txt" | wc -l) undefined)"
if [ "$agree" != "$checked" ]; then
  echo
  echo "DISAGREE (name / ours / pdftex):"
  awk '$2!=$3 {printf "  \\%-32s %-6s %s\n", $1, $2, $3}' "$TMP/joined.txt" | head -30
fi

# Coverage by region. A sharp cliff here localises a blocker to a few lines --
# far more useful than a single percentage, because everything after a runaway
# reads as "missing" and the interesting question is where the cliff edge is.
python3 - "$LTX" "$FAST" <<'PY'
import re, subprocess, sys
ltx, fast = sys.argv[1], sys.argv[2]
lines = open(ltx, encoding='latin-1').read().split('\n')
pat = re.compile(r'^\\(?:long\\?)?(?:def|gdef)\\([a-zA-Z@]+)|^\\DeclareRobustCommand\\([a-zA-Z@]+)')
first = {}
for i, l in enumerate(lines, 1):
    m = pat.match(l)
    if m:
        first.setdefault(m.group(1) or m.group(2), i)
env = {'AITEX_MAX_COMMANDS': '2000000000', 'AITEX_MAX_STEPS': '2000000000',
       'AITEX_TRACE_DEFS': '1', 'AITEX_DEF_GREP': '', 'PATH': '/usr/bin:/bin'}
out = subprocess.run([fast, 'pass', ltx, '--ini'], capture_output=True, env=env).stderr.decode('utf-8', 'replace')
have = set(re.findall(r'def \\([a-zA-Z@]+)', out))
buckets = {}
for n, ln in first.items():
    b = (ln // 1000) * 1000
    d, t = buckets.get(b, (0, 0))
    buckets[b] = (d + (1 if n in have else 0), t + 1)
print("\nline-range      ran/total  executed")
for b in sorted(buckets):
    d, t = buckets[b]
    pct = 100 * d // t
    print(f"{b:6d}-{b+999:<6d} {d:4d}/{t:<4d} {pct:4d}%  {'#' * (pct // 5)}")
# The edge that matters is where misses become SUSTAINED. Isolated gaps are
# usually engine-conditional code we correctly skip (\newattribute and friends
# are LuaTeX-only), so require a long unbroken run of misses before calling it.
seq = sorted((ln, n) for n, ln in first.items())
RUN = 20
for i, (ln, n) in enumerate(seq):
    if n in have:
        continue
    if all(m not in have for _, m in seq[i:i + RUN]) and len(seq) - i >= RUN:
        prev = [(l, m) for l, m in seq[:i] if m in have]
        if prev:
            pl, pm = prev[-1]
            print(f"cliff: last defined \\{pm} at line {pl}; "
                  f"nothing from \\{n} at line {ln} onward")
        break
else:
    print("no cliff: definitions run to the end of the file")
PY
