# Persistent LaTeX cache qualification

The locked 191-root holdout selects every paper whose stock-DVI route
succeeded. Each selected source is stabilized afresh, then stock LaTeX,
an aiTeX exact-output-cache population pass, and the resulting hit are
measured against the same auxiliary state. Every aiTeX DVI stream must
match the fresh stock stream exactly, and the disabled class/project
snapshot layers must not be published.

- Eligible locked stock-DVI successes: 56
- Valid persistent output-cache sets: 56/56
- Exact converged-output hits: 56/56
- Class/project snapshots published: 0

| Stabilized pass | Aggregate wall | Aggregate CPU | Median RSS |
|---|---:|---:|---:|
| Stock LaTeX | 32.89s | 32.06s | 47.2 MiB |
| aiTeX, populate document/output cache | 42.63s | 41.84s | 58.9 MiB |
| aiTeX, converged-output cache hit | 1.30s | 0.47s | 20.2 MiB |

Warm/cold aiTeX CPU ratio: 0.01×.
Warm/stock LaTeX CPU ratio: 0.01×.

Setup, source copying, auxiliary convergence, and DVI validation are outside
the timed region. Every reported pass has an output-validity or exact-DVI
oracle result. The exact per-paper results and source, runner, binary, format,
and cold-selection hashes are stored in
`qualification/holdout-200-v02.warm.json`.
