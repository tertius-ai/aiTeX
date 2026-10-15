# aiTeX — measured, 2026-08-05

Every number here is reproducible from the repo with the command beside it.
Wall-clock figures move with machine load; the instruction counts do not, so
where the two disagree the instruction count is the one to believe.

Reference engines are TeX Live 2023/Debian: `tex` 3.141592653, `pdftex` 1.40.25.

## Against `tex -ini`, typesetting

`tools/bench_typeset.sh` with `PLAIN=1`. Plain TeX, so the format load is
milliseconds and the measurement is nearly all typesetting.

| | ours | `tex -ini` | ratio |
|---|---|---|---|
| 1231 pages, instructions (callgrind) | 823,391,865 | 500,206,532 | **1.65×** |
| 1231 pages, wall clock | ~360 ms | ~215 ms | ~1.7× |
| memory, 308 pages | 12 MB | 15 MB | **0.8×** |
| memory, 1231 pages | 36 MB | 15 MB | 2.4× |

TeX's arrays are preallocated, so its footprint is flat at 15 MB whatever the
document. Ours starts smaller and crosses over around 300 pages.

The gap is entirely the node path. The same characters *tokenized only* — read
into `\toks0`, never typeset — run at **1.11×**, and the `latex.ltx` load,
which is almost pure expansion, at **1.05–1.21×**. See `docs/ARCHITECTURE.md`
§6 for the decomposition and what is left.

## Against real `pdflatex`, end to end

`tools/bench_typeset.sh` (no `PLAIN`), plus the format and snapshot paths. An
11-page paper with seven packages:

| | time |
|---|---|
| from source, no format | 2657 ms |
| `--fmt` (dumped format) | 79 ms |
| **`--fmt --cache` (preamble snapshot, steady state)** | **49 ms** |
| real `pdflatex`, with its own dumped format | 105 ms |

The format is 9.2 MB raw, **0.6 MB zstd** (pdfTeX's own `pdflatex.fmt` is
6.8 MB), restores in ~20 ms, and is guarded by `FMT=1 tools/latex_check.sh` —
all 35 LaTeX cases run from a dump and compared to pdfTeX as usual.

The preamble snapshot converges after two runs: run 1 writes it (there is no
`.aux` yet), run 2 rewrites it (now there is one), run 3 onward hit.

## Fidelity

| harness | result |
|---|---|
| `tools/typeset_check.sh` | 30/30 byte-identical to `tex -ini` |
| `tools/latex_check.sh` | **35/35** byte-identical to `pdftex` |
| `tools/battery.sh` | 626 syntactic forms agree with TeX |
| `tools/plain_check.sh` | plain.tex 1241/1241 lines, 27/27 state probes |
| `tools/primitives.sh` | **321 of 321** TeX82 primitives |
| `tools/packages.sh` | **49** of 53 common LaTeX packages load |
| a 480-page LaTeX article | byte-identical over 633,149 glyph positions |

Not passing: TRIP. `tools/trip.sh` runs it as the two-pass test it is and
reports pages against TeX's 16. **Pass one now matches**: `tex -ini trip.tex` dumps `trip.fmt` at line 92 and
sets not one character, and neither do we — `BISECT=1 tools/trip.sh` reports
zero divergence across all 448 lines, where it used to find twelve pages of
material TeX never typesets. Every one of those was a scan that stopped early
and left its tokens behind to be set as text. TRIP is largely an *error-recovery* test —
`tex -ini` reports sixty-odd errors in its second pass — so the page count is
a coarse signal that moves when recovery changes.

Its pass-1 transcript now agrees with TeX's for the whole of trip.tex's
setup, which it did not at line one. Where it stands, by `\count0` of each
shipped page:

    tex   0  -5000  10000  -333  -333  -2 ×10  -1
    ours  0  10000  -5000  10000  -333  -333  -2  -2

Page one now matches. Page two is where they part: ours fires with
`\outputpenalty` 10000, which means it did not break at a penalty at all,
where TeX breaks at the `\penalty-5000` inside a `\vadjust` — and the
`\tracingpages` diff says why, or at least where. TeX's page two takes 29
trace lines we never produce, beginning with `% split100 to 803.0,267.7
p=-101`.

## Profiling

No working `perf` on the development machine.

```sh
cargo build --profile profiling -p aitex-cli     # release opt, lto off, symbols kept
valgrind --tool=callgrind --cache-sim=no --branch-sim=no \
  --callgrind-out-file=cg.out target/profiling/aitex doc.tex
```

Do **not** wrap the binary in `env` — callgrind writes an empty file. Export
the variables instead. `AITEX_PROFILE=1` reports time by phase and allocation
volume from inside the engine; `AITEX_MEM=1` reports where the memory is,
separating peak heap, live at exit, and the engine's own buffers.
