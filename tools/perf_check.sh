#!/usr/bin/env bash
# Reproducible long-document performance gate. CPU measurements are useful but
# noisy on shared hosts, so the blocking 1.25x target uses Callgrind's retired
# instruction count between identical 100- and 400-section LaTeX documents.
# The ordinary benchmark is retained in the report as the real-time reading.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/aitex"
[ -x "$BIN" ] || { echo "performance: build first: cargo build --release -p aitex-cli"; exit 2; }
for command in pdflatex valgrind dvitype sha256sum; do
  command -v "$command" >/dev/null || { echo "performance: $command not found"; exit 2; }
done

export SOURCE_DATE_EPOCH=1000000000 FORCE_SOURCE_DATE=1
TMP="$(mktemp -d)"
if [ "${KEEP_TMP:-0}" = 1 ]; then
  echo "performance work directory: $TMP"
else
  trap 'rm -rf "$TMP"' EXIT
fi
mkdir -p "$TMP/fmt" "$ROOT/target/qualification"
"$ROOT/tools/mkfmt.sh" "$TMP/fmt" >/dev/null

REPORT="${PERF_REPORT:-$ROOT/target/qualification/performance.txt}"
exec > >(tee "$REPORT") 2>&1

echo "aiTeX 0.3.1 long-document performance"
echo "binary sha256: $(sha256sum "$BIN" | awk '{print $1}')"
echo "source commit: $(git -C "$ROOT" rev-parse HEAD)"

LINE='The quick brown fox jumps over the lazy dog, while office traffic waffles fluffily past the coffee shop on the corner of the avenue near the old railway station.'
document() { # sections
  local sections=$1
  printf '\\documentclass{article}\n\\begin{document}\n'
  for i in $(seq 1 "$sections"); do
    printf '\\section{Section %s}\n' "$i"
    for _j in 1 2 3 4 5 6; do printf '%s\n\n' "$LINE"; done
  done
  printf '\\end{document}\n'
}

for sections in 100 400; do
  directory="$TMP/$sections"
  mkdir -p "$directory"
  cp "$TMP/fmt/fmt.afmt" "$directory/"
  document "$sections" > "$directory/ours.tex"
  cp "$directory/ours.tex" "$directory/ref.tex"

  # Resolve the same one-pass auxiliary state before counting both engines.
  ( cd "$directory" && env AITEX_MAX_COMMANDS=2000000000 \
      AITEX_MAX_STEPS=2000000000 "$BIN" pass ours.tex --fmt fmt.afmt \
      -o ours.dvi >/dev/null 2>&1 )
  ( cd "$directory" && pdflatex -interaction=nonstopmode -halt-on-error \
      -output-format=dvi ref.tex >/dev/null 2>&1 )

  ( cd "$directory" && AITEX_MAX_COMMANDS=2000000000 \
      AITEX_MAX_STEPS=2000000000 valgrind -q --tool=callgrind \
      --callgrind-out-file=aitex.callgrind "$BIN" pass ours.tex --fmt fmt.afmt \
      -o ours.dvi >/dev/null 2>&1 )
  ( cd "$directory" && valgrind -q --tool=callgrind \
      --callgrind-out-file=pdftex.callgrind pdflatex -interaction=nonstopmode \
      -halt-on-error -output-format=dvi ref.tex >/dev/null 2>&1 )

  ours_pages=$(dvitype "$directory/ours.dvi" 2>/dev/null | grep -c 'beginning of page')
  ref_pages=$(dvitype "$directory/ref.dvi" 2>/dev/null | grep -c 'beginning of page')
  [ "$ours_pages" = "$ref_pages" ] || {
    echo "performance: page mismatch at $sections sections: $ours_pages/$ref_pages"; exit 1;
  }
  ours_ir=$(sed -nE 's/^summary: ([0-9]+)$/\1/p' "$directory/aitex.callgrind")
  ref_ir=$(sed -nE 's/^summary: ([0-9]+)$/\1/p' "$directory/pdftex.callgrind")
  [ -n "$ours_ir" ] && [ -n "$ref_ir" ] || {
    echo "performance: Callgrind did not record instruction totals"; exit 1;
  }
  printf '  %3d sections, %3d pages: aiTeX %s Ir; pdfTeX %s Ir\n' \
    "$sections" "$ours_pages" "$ours_ir" "$ref_ir"
  if [ "$sections" = 100 ]; then
    ai_first=$ours_ir; ref_first=$ref_ir; pages_first=$ours_pages
  else
    ai_last=$ours_ir; ref_last=$ref_ir; pages_last=$ours_pages
  fi
done

python3 - "$ai_first" "$ai_last" "$ref_first" "$ref_last" \
  "$pages_first" "$pages_last" <<'PYEOF'
import sys

ai0, ai1, ref0, ref1, p0, p1 = map(int, sys.argv[1:])
if not ai1 > ai0 or not ref1 > ref0 or not p1 > p0:
    raise SystemExit("performance: non-positive long-document slope")
ai = (ai1 - ai0) / (p1 - p0)
ref = (ref1 - ref0) / (p1 - p0)
ratio = ai / ref
print(f"  deterministic marginal work: {ai:.0f}/{ref:.0f} Ir/page = {ratio:.3f}x")
if ratio > 1.25:
    raise SystemExit("performance: marginal instruction work exceeds the 1.25x 0.1 limit")
PYEOF

echo
echo "Interleaved child-CPU measurement (informational; shared-host noise is not blocking):"
REPS="${PERF_REPS:-15}" SIZES="100 1600" \
  BENCH_CPU="${BENCH_CPU:-}" "$ROOT/tools/bench_typeset.sh"
echo "performance: deterministic long-document slope is within 1.25x"
