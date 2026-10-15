# Blind 200-paper arXiv holdout

Generated from the locked result file on 2026-10-06T04:08:55+00:00. The sample was
selected before any source was inspected or compiler was run: 200 entries were
drawn with a fixed seed from the 400 most recent arXiv-wide API results after
excluding every previously examined ID available in the local corpus records.

The selection was blind, but the corpus was subsequently used to diagnose and
fix compiler defects. It is therefore now a locked regression and release
qualification set, not an untouched estimate of generalization. A future
generalization claim requires a newly selected sample that remains unseen
until the candidate is frozen.

## Results

| Question | Result |
|---|---:|
| TeX source with an automatically identified root | 191/200 (95.5%; 95% CI 91.7–97.6%) |
| Source roots evaluated | 191/191 (100.0%; 95% CI 98.0–100.0%) |
| Stock pdfLaTeX produces a valid PDF | 148/191 (77.5%; 95% CI 71.1–82.8%) |
| Stock pdfTeX produces valid DVI | 58/191 (30.4%; 95% CI 24.3–37.2%) |
| aiTeX produces a valid PDF | 157/191 (82.2%; 95% CI 76.2–87.0%) |
| aiTeX produces valid DVI | 60/191 (31.4%; 95% CI 25.3–38.3%) |
| aiTeX PDF, conditional on stock PDF success | 148/148 (100.0%; 95% CI 97.5–100.0%) |
| aiTeX DVI, conditional on stock DVI success | 58/58 (100.0%; 95% CI 93.8–100.0%) |
| Valid semantic DVI comparisons, when both compile | 58/58 (100.0%; 95% CI 93.8–100.0%) |
| Exact positioned DVI glyph/rule semantics, when comparable | 58/58 (100.0%; 95% CI 93.8–100.0%) |
| Exact positioned DVI driver-special stream, when either emits specials | 58/58 (100.0%; 95% CI 93.8–100.0%) |
| Exact combined DVI semantics, when comparable | 58/58 (100.0%; 95% CI 93.8–100.0%) |
| Matching PDF page count, when both succeed | 148/148 (100.0%; 95% CI 97.5–100.0%) |
| Visually equivalent PDF pages at 72 DPI, when stock rendered every EPS asset | 144/144 (100.0%; 95% CI 97.4–100.0%) |
| Stock PDFs containing EPS draft placeholders | 4/148 (2.7%; 95% CI 1.1–6.7%) |
| aiTeX PDFs containing EPS draft placeholders | 0/148 (0.0%; 95% CI -0.0–2.5%) |
| Extractable Unicode sequence from both PDFs | 148/148 (100.0%; 95% CI 97.5–100.0%) |
| Candidate text has no more invalid code points than stock | 148/148 (100.0%; 95% CI 97.5–100.0%) |
| Matching normalized PDF Unicode sequence, when extractable | 15/148 (10.1%; 95% CI 6.2–16.0%) |
| At least 99% extracted-text similarity | 118/148 (79.7%; 95% CI 72.5–85.4%) |
| Median extracted-text similarity | 99.85% |

A valid compile requires exit status zero, a readable output file, and at least
one page. Each engine gets two clean passes with shell escape disabled. DVI
drawing matching requires the exact normalized sequence of positioned glyphs,
rules, and fonts; driver specials are graded separately and then as a strict
combined stream. None require byte-identical movement encoding.
PDF text uses MuPDF, NFKC normalization, and a whitespace-free character sequence because spaces in an untagged TeX PDF are reader inferences from the geometry already graded by the DVI oracle. Stock pdfTeX's unmapped TS1 U+0088 bullet is normalized to U+2022.
Exact stock text remains a diagnostic: the release quality gate is that aiTeX introduces no additional Unicode replacement, control, or private-use characters, because a corrected mapping can legitimately differ from stock.
PDF rendering uses the same reader at 72 DPI and requires each page to have normalized MAE <= 0.020000 and no more than 5.00% of pixels outside 12.5% fuzz.
A raster pair is marked non-comparable only when the stock PDF's extracted text contains an EPS filename that the exact DVI special stream names; this is pdfLaTeX's ruled draft-box fallback with shell escape disabled. aiTeX is still required to render the EPS rather than emit the same placeholder.
The PDF row is product-level
compatibility with pdfLaTeX. The DVI row is the like-for-like TeX algorithm route:
both engines run the same DVI-mode LaTeX driver semantics.

## Performance against stock pdfTeX

Both figures cover two compiler passes. The aggregate ratio is aiTeX divided
by pdfTeX, so lower is better. Only papers successfully compiled by both
engines on that route are included.

This v0.3.1 candidate run clears every correctness and resource gate. PDF CPU
is 0.860× stock, DVI CPU is 1.297×, and PDF p95 RSS is 1.732× stock. DVI
therefore remains an optimization target while passing the explicit 1.30×
release ceiling.

| Route | Paired papers | aiTeX wall | pdfTeX wall | Wall ratio | aiTeX CPU | pdfTeX CPU | CPU ratio | aiTeX faster |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| PDF end-to-end | 148 | 317.02s | 363.93s | 0.87× | 309.65s | 360.21s | 0.86× | 32/148 |
| DVI compiler | 58 | 84.24s | 65.47s | 1.29× | 82.68s | 63.77s | 1.30× | 0/58 |

Peak RSS is the larger GNU `time -v` high-water mark from either pass.

| Route | Paired papers | aiTeX median | pdfTeX median | Median ratio | aiTeX p95 | pdfTeX p95 | aiTeX max | pdfTeX max | aiTeX lower |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| PDF end-to-end | 148 | 49.1 MiB | 51.8 MiB | 0.95× | 106.6 MiB | 61.5 MiB | 179.7 MiB | 90.9 MiB | 91/148 |
| DVI compiler | 58 | 45.6 MiB | 49.1 MiB | 0.93× | 58.0 MiB | 50.0 MiB | 114.2 MiB | 59.8 MiB | 45/58 |

The successful-pair table excludes pathological failures. Across all 191
evaluated sources, including failures, resource use was:

| Route | RSS measured | Median RSS | p95 RSS | Maximum RSS | Timeouts | Memory-limit aborts |
|---|---:|---:|---:|---:|---:|---:|
| pdfTeX PDF | 191/191 | 51.5 MiB | 61.5 MiB | 90.9 MiB | 0 | 0 |
| pdfTeX DVI | 191/191 | 49.0 MiB | 51.8 MiB | 62.9 MiB | 0 | 0 |
| aiTeX PDF | 191/191 | 50.1 MiB | 110.6 MiB | 179.7 MiB | 0 | 0 |
| aiTeX DVI | 191/191 | 45.9 MiB | 71.5 MiB | 118.8 MiB | 0 | 0 |

## Source outcomes

- `no_tex_source`: 9
- `ready`: 191

## aiTeX PDF failure categories

- `tex_error`: 30
- `missing_dependency_or_file`: 3
- `different_engine_required`: 1

## Systematic failure clusters

These tables condition on the corresponding stock route succeeding. Paths and
line numbers are normalized so one engine subsystem is one work item even when
dozens of papers expose it.

### PDF product-compatibility failures

| Subsystem | Fingerprint | Papers | Representatives |
|---|---|---:|---|

### DVI compile failures

| Subsystem | Fingerprint | Papers | Representatives |
|---|---|---:|---|

### DVI glyph/rule divergences after both compilers succeed

| Signature | Papers | Representatives |
|---|---:|---|

## aiTeX PDF success by primary category

| Category | Success |
|---|---:|
| `astro-ph.CO` | 1/2 |
| `astro-ph.EP` | 1/2 |
| `astro-ph.GA` | 1/1 |
| `astro-ph.HE` | 4/4 |
| `astro-ph.SR` | 3/3 |
| `cond-mat.mtrl-sci` | 2/2 |
| `cond-mat.quant-gas` | 1/1 |
| `cond-mat.soft` | 1/1 |
| `cond-mat.stat-mech` | 1/1 |
| `cond-mat.str-el` | 1/1 |
| `cs.AI` | 6/10 |
| `cs.AR` | 0/1 |
| `cs.CE` | 1/1 |
| `cs.CG` | 1/1 |
| `cs.CL` | 9/12 |
| `cs.CR` | 7/8 |
| `cs.CV` | 9/12 |
| `cs.DB` | 0/1 |
| `cs.DL` | 1/1 |
| `cs.DM` | 1/1 |
| `cs.DS` | 1/2 |
| `cs.GT` | 3/3 |
| `cs.HC` | 0/1 |
| `cs.IR` | 1/2 |
| `cs.IT` | 1/1 |
| `cs.LG` | 13/15 |
| `cs.LO` | 1/1 |
| `cs.MA` | 1/2 |
| `cs.PL` | 1/1 |
| `cs.RO` | 2/2 |
| `cs.SE` | 4/4 |
| `cs.SI` | 1/1 |
| `eess.SP` | 1/1 |
| `eess.SY` | 3/3 |
| `gr-qc` | 2/2 |
| `hep-ex` | 0/1 |
| `hep-lat` | 1/1 |
| `hep-ph` | 3/4 |
| `hep-th` | 3/4 |
| `math.AC` | 1/1 |
| `math.AG` | 5/5 |
| `math.AP` | 2/2 |
| `math.CA` | 2/2 |
| `math.CO` | 5/6 |
| `math.CT` | 0/1 |
| `math.CV` | 1/1 |
| `math.DG` | 2/2 |
| `math.DS` | 1/1 |
| `math.FA` | 1/1 |
| `math.GR` | 1/1 |
| `math.GT` | 1/1 |
| `math.KT` | 1/1 |
| `math.LO` | 2/2 |
| `math.NA` | 2/2 |
| `math.NT` | 5/5 |
| `math.OC` | 4/4 |
| `math.PR` | 2/4 |
| `math.QA` | 2/2 |
| `math.RA` | 2/2 |
| `math.RT` | 0/1 |
| `math.ST` | 2/2 |
| `nlin.PS` | 0/1 |
| `physics.ao-ph` | 1/1 |
| `physics.app-ph` | 1/1 |
| `physics.flu-dyn` | 3/3 |
| `q-bio.QM` | 0/1 |
| `quant-ph` | 9/11 |
| `stat.ME` | 5/6 |
| `stat.ML` | 1/1 |

## Method and benchmark environment

The classical reference is TeX Live's stock pdfTeX in pdfLaTeX mode;
Knuth TeX cannot load the e-TeX/pdfTeX-dependent LaTeX used by this corpus.
PDF timing is end-to-end for each CLI, while DVI timing isolates compilation.
Stock routes, timings, and correctness oracles were reused from the pinned
result named by `reference_results_sha256`; aiTeX alone was remeasured from
clean source copies. Every stock-clean PDF had to be byte-identical to the
previously compared candidate, and every stock-clean DVI had to retain its
exact normalized drawing and driver-special streams. Any changed case received
a fresh four-route comparison. Ratios therefore pair a fresh aiTeX measurement
with the saved stock measurement on the same locked machine and corpus.
Each compiler pass had a 120s timeout and a 2048 MiB address-space safety ceiling.
aiTeX's configurable virtual worker-stack reservation was set to 64 MiB; peak RSS, not virtual size, is reported.
TeX packages come from the host TeX Live plus the isolated, exact-deb overlay locked in `qualification/texlive-overlay.lock.json`; no host packages are installed.

- CPU: AMD Ryzen 9 7950X3D 16-Core Processor (32 logical CPUs)
- RAM: 125.3 GiB
- Platform: Linux-6.8.0-134-generic-x86_64-with-glibc2.39; kernel 6.8.0-134-generic
- Reference: pdfTeX 3.141592653-2.6-1.40.25 (TeX Live 2023/Debian)
- PDF validation: Ghostscript 10.02.1
- Graphics sidecars: This is extractbb Version 20220710
- Graphics rendering and text extraction: mutool version 1.23.10
- Raster comparison: Version: ImageMagick 6.9.12-98 Q16 x86_64 18038 https://legacy.imagemagick.org
- PNG normalization: pngtopam: Using libnetpbm from Netpbm Version: Netpbm 11.5.2
- Measurement: time (GNU Time) UNKNOWN

## Reproduction

The exact population, selected IDs, versioned source URLs, source-tree hashes,
tool environment, compiler binary hash, per-engine results, timeouts, and first
errors are retained in `qualification/holdout-200.*.json`.

```sh
# On a clean checkout, materialize the ignored source trees without replacing
# the tracked lock. The run below verifies every tree against that lock.
python3 tools/arxiv_holdout.py fetch \
  --output target/qualification/holdout-200-materialized.sources.json \
  --delay 0.2 --retry
python3 tools/texlive_overlay.py
cargo build --release --locked -p aitex-cli
python3 tools/arxiv_holdout.py run --jobs 3 --timeout 120 --memory-limit-mib 2048 \
  --output target/qualification/holdout-200-current.results.json
python3 tools/arxiv_holdout.py report \
  --results target/qualification/holdout-200-current.results.json \
  --output target/qualification/HOLDOUT-200-CURRENT.md
```
