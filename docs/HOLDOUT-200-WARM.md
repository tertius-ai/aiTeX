# Warm LaTeX preamble benchmark

The locked 191-root holdout selects every paper whose stock-DVI route
succeeded. Each selected source is stabilized afresh, then stock LaTeX,
an aiTeX project-cache miss, and the resulting aiTeX snapshot hit are
measured against the same auxiliary state. Both aiTeX DVI streams must
match the fresh stock stream exactly.

- Eligible locked stock-DVI successes: 56
- Valid cache miss/hit pairs: 56/56

| Stabilized pass | Aggregate wall | Aggregate CPU | Median RSS |
|---|---:|---:|---:|
| Stock LaTeX | 257.78s | 256.93s | 39.8 MiB |
| aiTeX, cold document preamble | 148.72s | 147.89s | 58.6 MiB |
| aiTeX, warm preamble snapshot | 78.68s | 77.89s | 41.9 MiB |

Warm/cold aiTeX CPU ratio: 0.53×.
Warm/stock LaTeX CPU ratio: 0.30×.

Setup, source copying, auxiliary convergence, and DVI validation are outside
the timed region. Every reported pass has an output-validity or exact-DVI
oracle result. The exact per-paper results and source, runner, binary, format,
and cold-selection hashes are stored in
`qualification/holdout-200.warm.json`.
