#!/usr/bin/env bash
# Head-to-head on *typesetting*, which `bench_expand.sh` does not measure.
#
# Both engines are fed the identical LaTeX document from preloaded formats:
# pdfTeX's system `pdflatex.fmt` and an aiTeX `fmt.afmt` built immediately before
# the run. `FROM_SOURCE=1` retains the older diagnostic that interprets
# `latex.ltx` inside every measured process. Several sizes are measured and the
# *slope* is reported, separating startup from marginal typesetting cost.
#
# Peak RSS comes from /usr/bin/time -v, which reports the high-water mark of the
# whole process — the honest number for "how much memory does a 500-page
# document take", including the format load that precedes it.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST="$ROOT/target/release/aitex"
LTX="$(kpsewhich latex.ltx)"
REPS="${REPS:-3}"
SIZES="${SIZES:-100 400 1600}"
export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
[ -x "$FAST" ] || { echo "build first: cargo build --release -p aitex-cli"; exit 1; }
if [ -n "${BENCH_WORKDIR:-}" ]; then
  TMP="$BENCH_WORKDIR"
  mkdir -p "$TMP"
else
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
fi
cd "$TMP" || exit 1
head -n "$(( $(wc -l < "$LTX") - 4 ))" "$LTX" > kernel.tex
FROM_SOURCE="${FROM_SOURCE:-0}"
if [ "$FROM_SOURCE" != 1 ] && [ "${PLAIN:-0}" != 1 ]; then
  "$ROOT/tools/mkfmt.sh" "$TMP" >/dev/null
fi
L='The quick brown fox jumps over the lazy dog, while office traffic waffles fluffily past the coffee shop on the corner of the avenue near the old railway station.'

doc() { # $1 = sections
  { if [ "$FROM_SOURCE" = 1 ]; then
      printf '\\input kernel\n'
    fi
    printf '\\documentclass{article}\n\\begin{document}\n'
    for i in $(seq 1 "$1"); do
      printf '\\section{Section %s}\n' "$i"
      for j in 1 2 3 4 5 6; do printf '%s\n\n' "$L"; done
    done
    printf '\\end{document}\n'; }
}

# Median child CPU ms, and peak RSS in MB from one additional run.  CPU time is
# the release metric: short wall measurements on a shared machine mostly grade
# scheduler noise.  BENCH_CPU pins both engines when a stable core is available.
rss() { # $1..=cmd
  local command=("$@")
  if [ -n "${BENCH_CPU:-}" ]; then
    command=(taskset -c "$BENCH_CPU" "${command[@]}")
  fi
  local value
  value=$(/usr/bin/time -v "${command[@]}" 2>&1 >/dev/null | sed -nE 's/.*Maximum resident set size \(kbytes\): ([0-9]+)/\1/p')
  printf '%s' "$((${value:-0}/1024))"
}

run_pair() { # command A, --and--, command B
  BENCH_CPU="${BENCH_CPU:-}" python3 "$ROOT/tools/cpu_time.py" --pair "$REPS" "$@"
}

run() { # retained for one-engine diagnostics
  local tag=$1; shift
  local command=("$@")
  local cpu
  cpu=$(BENCH_CPU="${BENCH_CPU:-}" python3 "$ROOT/tools/cpu_time.py" "$REPS" "${command[@]}") || return
  printf '%s %s' "$cpu" "$(rss "${command[@]}")"
}

if [ "${PLAIN:-0}" = 1 ]; then
  # Plain TeX against `tex -ini`. The format load is ~11 ms rather than ~2.7 s,
  # so this is nearly all typesetting — the cleanest read on that path, and the
  # one to profile. Against TeX82 itself, not pdfTeX.
  cp "$ROOT/vendor/plain/plain.tex" .
  printf '%6s  %6s  %-22s  %-22s  %s\n' 'paras' 'pages' 'aitex  ms /  MB' 'tex    ms /  MB' 'ratio t / m'
  for n in $SIZES; do
    { printf '\\input plain\n'
      for i in $(seq 1 "$n"); do printf '%s\n\n' "$L"; done
      printf '\\bye\n'; } > p.tex
    cp p.tex p_r.tex
    ours=(env AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 AITEX_DVI=p_o.dvi "$FAST" pass p.tex --ini)
    ref=(tex -ini -interaction=nonstopmode p_r.tex)
    read -r ot rt <<<"$(run_pair "${ours[@]}" --and-- "${ref[@]}")"
    om=$(rss "${ours[@]}"); rm=$(rss "${ref[@]}")
    pages=$(dvitype p_o.dvi 2>/dev/null | grep -c 'beginning of page')
    printf '%6s  %6s  %10s / %5s        %10s / %5s        %.2fx / %.2fx\n' \
      "$n" "$pages" "$ot" "$om" "$rt" "$rm" \
      "$(echo "$ot $rt" | awk '{printf "%.2f", ($2>0)?$1/$2:0}')" \
      "$(echo "$om $rm" | awk '{printf "%.2f", ($2>0)?$1/$2:0}')"
  done
  exit 0
fi

if [ "$FROM_SOURCE" = 1 ]; then mode='latex.ltx as source'; else mode='preloaded LaTeX'; fi
echo "$mode"
printf '%6s  %6s  %-22s  %-22s  %s\n' 'secs' 'pages' 'aitex  ms /  MB' 'pdftex ms /  MB' 'ratio t / m'
first_n=; first_ot=; first_om=; first_rt=; first_rm=; first_p=
last_n=;  last_ot=;  last_om=;  last_rt=;  last_rm=;  last_p=
for n in $SIZES; do
  doc "$n" > b.tex; cp b.tex b_r.tex; rm -f b.aux b_r.aux
  if [ "$FROM_SOURCE" = 1 ]; then
    ours=(env AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 AITEX_DVI=b_o.dvi "$FAST" pass b.tex --ini)
    ref=(pdftex -ini -etex -interaction=nonstopmode -output-format=dvi b_r.tex)
  else
    ours=(env AITEX_MAX_COMMANDS=2000000000 AITEX_MAX_STEPS=2000000000 AITEX_DVI=b_o.dvi "$FAST" pass b.tex --fmt fmt.afmt)
    ref=(pdflatex -interaction=nonstopmode -output-format=dvi b_r.tex)
  fi
  # two passes each, as a real run is; measure the second
  "${ours[@]}" >/dev/null 2>&1
  "${ref[@]}" </dev/null >/dev/null 2>&1
  read -r ot rt <<<"$(run_pair "${ours[@]}" --and-- "${ref[@]}")"
  om=$(rss "${ours[@]}"); rm=$(rss "${ref[@]}")
  pages=$(dvitype b_o.dvi 2>/dev/null | grep -c 'beginning of page')
  printf '%6s  %6s  %10s / %5s        %10s / %5s        %.2fx / %.2fx\n' \
    "$n" "$pages" "$ot" "$om" "$rt" "$rm" \
    "$(echo "$ot $rt" | awk '{printf "%.2f", $1/$2}')" \
    "$(echo "$om $rm" | awk '{printf "%.2f", $1/$2}')"
  [ -z "$first_n" ] && { first_n=$n; first_ot=$ot; first_om=$om; first_rt=$rt; first_rm=$rm; first_p=$pages; }
  last_n=$n; last_ot=$ot; last_om=$om; last_rt=$rt; last_rm=$rm; last_p=$pages
done

echo
awk -v op="$first_p" -v ot="$first_ot" -v om="$first_om" -v rt="$first_rt" -v rm="$first_rm" \
    -v np="$last_p"  -v nt="$last_ot"  -v nm="$last_om"  -v mt="$last_rt"  -v mm="$last_rm" '
BEGIN {
  dp = np - op
  if (dp <= 0) { print "need two distinct sizes"; exit }
  printf "marginal cost per page  (slope between %d and %d pages)\n", op, np
  printf "  aitex   %7.3f ms/page   %7.1f kB/page\n", (nt-ot)/dp, (nm-om)*1024/dp
  printf "  pdftex  %7.3f ms/page   %7.1f kB/page\n", (mt-rt)/dp, (mm-rm)*1024/dp
  # The pdfTeX arrays are fixed-size, so its memory slope is 0 and the ratio
  # is infinite. "n/a" is the honest form: a difference in kind, not degree.
  # (No apostrophes in here: the awk program is single-quoted.)
  if (mt-rt > 0) { printf "  ratio   %7.2fx", (nt-ot)/(mt-rt) } else { printf "  ratio        n/a" }
  if (mm-rm > 0) { printf "          %7.2fx\n", (nm-om)/(mm-rm) } else { printf "        n/a (pdfTeX does not grow)\n" }
}'
