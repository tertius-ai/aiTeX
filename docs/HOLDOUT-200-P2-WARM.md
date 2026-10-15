# Layered LaTeX snapshot qualification

The locked 191-root holdout selects every paper whose stock-DVI route
succeeded. Each selected source is stabilized afresh, then stock LaTeX,
an aiTeX layered-cache miss, the resulting project snapshot hit, and a
class-only snapshot hit are measured against the same auxiliary state.
Every aiTeX DVI stream must match the fresh stock stream exactly.

- Eligible locked stock-DVI successes: 56
- Valid layered-cache sets: 56/56
- Admitted and valid class snapshot hits: 55
- Safe class-cache fallbacks: 1

| Stabilized pass | Aggregate wall | Aggregate CPU | Median RSS |
|---|---:|---:|---:|
| Stock LaTeX | 33.83s | 33.04s | 47.5 MiB |
| aiTeX, cold document preamble | 52.36s | 51.50s | 92.9 MiB |
| aiTeX, warm preamble snapshot | 33.69s | 32.97s | 45.1 MiB |
| aiTeX, class-only snapshot (admitted set) | 44.57s | 43.87s | 61.5 MiB |

Warm/cold aiTeX CPU ratio: 0.64×.
Warm/stock LaTeX CPU ratio: 1.00×.

Setup, source copying, auxiliary convergence, and DVI validation are outside
the timed region. Every reported pass has an output-validity or exact-DVI
oracle result. The exact per-paper results and source, runner, binary, format,
and cold-selection hashes are stored in
`qualification/holdout-200-p2.warm.json`.
