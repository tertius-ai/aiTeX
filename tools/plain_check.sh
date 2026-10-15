#!/bin/bash
# How faithfully we load Knuth's plain.tex, measured against real TeX.
#
# Two rules make this honest, and both were learned by getting them wrong:
#
# 1. Only truncation points where *real TeX* succeeds are considered. plain.tex
#    cannot be cut anywhere — line 268 is mid-definition of \newif, and both
#    engines fail there. Counting that as our limitation measured the scissors,
#    not the engine.
#
# 2. Reaching a line is not passing it. At each point both engines are asked the
#    same questions — register values, catcodes, dimensions — and the answers
#    are diffed. An engine that silently does the wrong thing must score worse
#    than one that fails loudly, or the metric rewards hiding bugs.
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${BIN:-$ROOT/target/release/aitex}
PLAIN=${PLAIN:-$ROOT/vendor/plain/plain.tex}
STEP=${STEP:-20}

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
TOTAL=$(wc -l < "$PLAIN" | tr -d ' ')

# Questions both engines are asked. Chosen to cover state that is set at
# different points in plain.tex and by different machinery: allocation
# counters, catcodes, code tables, the named parameters, glue with stretch and
# shrink, a box built by \setbox, and a font's own metrics.
#
# `@` is a letter here because plain.tex's own names use it, and probing them
# is the point: \p@renwd is the width of a character in a font plain loaded.
cat > "$tmp/probe.tex" <<'PROBE'
\catcode`\@=11
\message{[cc-at=\the\catcode`\@]}
\message{[cc-tilde=\the\catcode`\~]}
\message{[cc-dollar=\the\catcode`\$]}
\message{[mc-A=\the\mathcode`\A]}
\message{[sf-A=\the\sfcode`\A]}
\message{[c10=\the\count10]}
\message{[c11=\the\count11]}
\message{[c12=\the\count12]}
\message{[c15=\the\count15]}
\message{[parindent=\the\parindent]}
\message{[hsize=\the\hsize]}
\message{[vsize=\the\vsize]}
\message{[maxdimen=\the\maxdimen]}
\message{[parskip=\the\parskip]}
\message{[baselineskip=\the\baselineskip]}
\message{[topskip=\the\topskip]}
\message{[bigskipamount=\the\bigskipamount]}
\message{[hideskip=\the\hideskip]}
\message{[zskip=\the\z@skip]}
\message{[tolerance=\the\tolerance]}
\message{[pretolerance=\the\pretolerance]}
\message{[hbadness=\the\hbadness]}
\message{[thinmuskip=\the\thinmuskip]}
\message{[medmuskip=\the\medmuskip]}
\message{[strut-ht=\the\ht\strutbox]}
\message{[strut-dp=\the\dp\strutbox]}
\message{[parenwd=\the\p@renwd]}
\message{[quad=\the\fontdimen6\tenrm]}
\message{[xheight=\the\fontdimen5\tenrm]}
PROBE

# Ask an engine the probe questions after loading N lines. Prints the answers,
# or nothing if it failed.
ask() { # $1=engine  $2=lines
  local out="$tmp/$1.$2"
  head -n "$2" "$PLAIN" > "$tmp/body.tex"
  cat "$tmp/probe.tex" >> "$tmp/body.tex"
  printf '\\end\n' >> "$tmp/body.tex"
  if [ "$1" = ref ]; then
    cp "$tmp/body.tex" "$tmp/r.tex"
    ( cd "$tmp" && max_print_line=1000 timeout 60 tex -ini -interaction=nonstopmode r.tex >/dev/null 2>&1 )
    # If real TeX itself errored, this is not a valid truncation point. Its log
    # still *looks* like it answered: a runaway definition dumps the absorbed
    # text, which includes the probe lines verbatim, and an unexpanded
    # `[c10=\the\count10]` matches the same pattern as a real answer. Taking
    # that as ground truth measures where the file was cut, not the engine.
    if grep -q '^! ' "$tmp/r.log" 2>/dev/null; then
      : > "$out"
      return
    fi
    grep -oE '\[[a-z0-9-]+=[^]]*\]' "$tmp/r.log" 2>/dev/null | sort > "$out"
  else
    ( cd "$tmp" && max_print_line=1000 timeout 60 "$BIN" pass body.tex --ini -o m.dvi 2>&1 ) \
      | grep -oE '\[[a-z0-9-]+=[^]]*\]' | sort > "$out"
  fi
  [ -s "$out" ] && cat "$out"
}

# Every STEP lines, and then the whole file — otherwise the last few lines are
# never tested and the score is capped by the step size rather than by the
# engine. Reporting 1240/1241 when the file loads completely is a measurement
# artefact, and a misleading one.
points=$(seq "$STEP" "$STEP" "$TOTAL"); points="$points $TOTAL"
best=0; best_agree=0; best_total=0; first_bad=0
for n in $points; do
  ref=$(ask ref "$n")
  # Not a valid truncation point for anyone; skip rather than blame ourselves.
  [ -z "$ref" ] && continue
  mine=$(ask mine "$n")
  if [ -z "$mine" ]; then
    [ "$first_bad" = 0 ] && first_bad=$n
    break
  fi
  total=$(printf '%s\n' "$ref" | wc -l | tr -d ' ')
  agree=$(comm -12 <(printf '%s\n' "$mine") <(printf '%s\n' "$ref") | wc -l | tr -d ' ')
  if [ "$agree" != "$total" ]; then
    [ "$first_bad" = 0 ] && first_bad=$n
    best=$n; best_agree=$agree; best_total=$total
    break
  fi
  best=$n; best_agree=$agree; best_total=$total
done

printf 'plain.tex: %d/%d lines loaded (%d%%), %s/%s probes agree with real TeX\n' \
  "$best" "$TOTAL" "$(( best * 100 / TOTAL ))" "$best_agree" "$best_total"

# Loading a format must typeset nothing.
#
# Every probe above is a `\the`-style state query, and a whole class of bug is
# invisible to them: an engine that silently sets an unimplemented primitive's
# arguments as text changes no register. `\skewchar\teni='177` put the
# characters `='177` on the page while all 27 answers stayed correct.
printf '\\input plain\n\\bye\n' > "$tmp/fmt.tex"
cp "$PLAIN" "$tmp/plain.tex"
( cd "$tmp" && max_print_line=1000 timeout 60 tex -ini -interaction=nonstopmode fmt.tex >/dev/null 2>&1 )
ref_chars=$(dvitype "$tmp/fmt.dvi" 2>/dev/null | grep -cE '^\[.*\]$')
( cd "$tmp" && timeout 60 "$BIN" pass fmt.tex --ini -o fmt-mine.dvi >/dev/null 2>&1 )
mine_chars=$(dvitype "$tmp/fmt-mine.dvi" 2>/dev/null | grep -cE '^\[.*\]$')
undef=$( ( cd "$tmp" && timeout 60 "$BIN" pass fmt.tex --ini -o /dev/null 2>&1 ) | grep -oE '[0-9]+ undefined' | head -1)
if [ "${ref_chars:-0}" = "${mine_chars:-0}" ] && [ -z "$undef" ]; then
  printf 'loading plain.tex typesets nothing, as it should\n'
else
  printf 'loading plain.tex leaks onto the page: %s text runs (tex: %s)%s\n' \
    "${mine_chars:-0}" "${ref_chars:-0}" "${undef:+, $undef}"
fi

if [ "$first_bad" != 0 ]; then
  ref=$(ask ref "$first_bad"); mine=$(ask mine "$first_bad")
  if [ -z "$mine" ]; then
    printf '  stops before line %d\n' "$first_bad"
    head -n "$first_bad" "$PLAIN" > "$tmp/b.tex"; printf '\\end\n' >> "$tmp/b.tex"
    printf '  error: %s\n' "$( ( cd "$tmp" && timeout 60 "$BIN" pass b.tex --ini -o b.dvi 2>&1 ) | grep 'aitex: [A-Z]' | head -1)"
  else
    printf '  disagrees by line %d:\n' "$first_bad"
    while IFS= read -r l; do
      k=${l%%=*}; k=${k#[}
      m=$(printf '%s\n' "$mine" | grep -oE "\[$k=[^]]*\]" | head -1)
      [ "$m" = "$l" ] && continue
      printf '    %-12s ours %-18s tex %s\n' "$k" "${m:-<missing>}" "$l"
    done <<< "$ref"
  fi
fi
