# Blind 200-paper arXiv holdout

Generated from the locked result file on 2026-09-02T18:23:00+00:00. The sample was
selected before any source was inspected or compiler was run: 200 entries were
drawn with a fixed seed from the 400 most recent arXiv-wide API results after
excluding every previously examined ID available in the local corpus records.

## Results

| Question | Result |
|---|---:|
| TeX source with an automatically identified root | 191/200 (95.5%; 95% CI 91.7–97.6%) |
| Source roots evaluated | 191/191 (100.0%; 95% CI 98.0–100.0%) |
| Stock pdfLaTeX produces a valid PDF | 143/191 (74.9%; 95% CI 68.3–80.5%) |
| Stock pdfTeX produces valid DVI | 56/191 (29.3%; 95% CI 23.3–36.1%) |
| aiTeX produces a valid PDF | 146/191 (76.4%; 95% CI 69.9–81.9%) |
| aiTeX produces valid DVI | 57/191 (29.8%; 95% CI 23.8–36.7%) |
| aiTeX PDF, conditional on stock PDF success | 143/143 (100.0%; 95% CI 97.4–100.0%) |
| aiTeX DVI, conditional on stock DVI success | 56/56 (100.0%; 95% CI 93.6–100.0%) |
| Valid semantic DVI comparisons, when both compile | 56/56 (100.0%; 95% CI 93.6–100.0%) |
| Exact positioned DVI glyph/rule semantics, when comparable | 56/56 (100.0%; 95% CI 93.6–100.0%) |
| Exact positioned DVI driver-special stream, when either emits specials | 56/56 (100.0%; 95% CI 93.6–100.0%) |
| Exact combined DVI semantics, when comparable | 56/56 (100.0%; 95% CI 93.6–100.0%) |
| Matching PDF page count, when both succeed | 143/143 (100.0%; 95% CI 97.4–100.0%) |
| Visually equivalent PDF pages at 72 DPI, when stock rendered every EPS asset | 140/140 (100.0%; 95% CI 97.3–100.0%) |
| Stock PDFs containing EPS draft placeholders | 3/143 (2.1%; 95% CI 0.7–6.0%) |
| aiTeX PDFs containing EPS draft placeholders | 0/143 (0.0%; 95% CI 0.0–2.6%) |
| Extractable Unicode sequence from both PDFs | 143/143 (100.0%; 95% CI 97.4–100.0%) |
| Candidate text has no more invalid code points than stock | 143/143 (100.0%; 95% CI 97.4–100.0%) |
| Matching normalized PDF Unicode sequence, when extractable | 14/143 (9.8%; 95% CI 5.9–15.8%) |
| At least 99% extracted-text similarity | 117/143 (81.8%; 95% CI 74.7–87.3%) |
| Median extracted-text similarity | 99.86% |

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

| Route | Paired papers | aiTeX wall | pdfTeX wall | Wall ratio | aiTeX CPU | pdfTeX CPU | CPU ratio | aiTeX faster |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| PDF end-to-end | 143 | 273.53s | 294.72s | 0.93× | 268.67s | 288.99s | 0.93× | 29/143 |
| DVI compiler | 56 | 83.28s | 65.19s | 1.28× | 81.52s | 63.52s | 1.28× | 1/56 |

Peak RSS is the larger GNU `time -v` high-water mark from either pass.

| Route | Paired papers | aiTeX median | pdfTeX median | Median ratio | aiTeX p95 | pdfTeX p95 | aiTeX max | pdfTeX max | aiTeX lower |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| PDF end-to-end | 143 | 44.3 MiB | 49.8 MiB | 0.89× | 106.2 MiB | 59.7 MiB | 147.8 MiB | 88.3 MiB | 99/143 |
| DVI compiler | 56 | 42.4 MiB | 47.4 MiB | 0.89× | 55.0 MiB | 48.2 MiB | 111.3 MiB | 57.5 MiB | 44/56 |

The successful-pair table excludes pathological failures. Across all 191
evaluated sources, including failures, resource use was:

| Route | RSS measured | Median RSS | p95 RSS | Maximum RSS | Timeouts | Memory-limit aborts |
|---|---:|---:|---:|---:|---:|---:|
| pdfTeX PDF | 191/191 | 49.5 MiB | 59.5 MiB | 88.3 MiB | 0 | 0 |
| pdfTeX DVI | 191/191 | 47.2 MiB | 49.6 MiB | 61.1 MiB | 0 | 0 |
| aiTeX PDF | 191/191 | 44.5 MiB | 92.2 MiB | 147.8 MiB | 0 | 0 |
| aiTeX DVI | 191/191 | 42.4 MiB | 67.1 MiB | 115.9 MiB | 0 | 0 |

## Source outcomes

- `no_tex_source`: 9
- `ready`: 191

## aiTeX PDF failure categories

- `tex_error`: 38
- `missing_dependency_or_file`: 6
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
| `cs.GT` | 2/3 |
| `cs.HC` | 0/1 |
| `cs.IR` | 1/2 |
| `cs.IT` | 1/1 |
| `cs.LG` | 13/15 |
| `cs.LO` | 1/1 |
| `cs.MA` | 1/2 |
| `cs.PL` | 0/1 |
| `cs.RO` | 1/2 |
| `cs.SE` | 4/4 |
| `cs.SI` | 1/1 |
| `eess.SP` | 1/1 |
| `eess.SY` | 3/3 |
| `gr-qc` | 2/2 |
| `hep-ex` | 0/1 |
| `hep-lat` | 1/1 |
| `hep-ph` | 2/4 |
| `hep-th` | 3/4 |
| `math.AC` | 1/1 |
| `math.AG` | 2/5 |
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
| `math.KT` | 0/1 |
| `math.LO` | 2/2 |
| `math.NA` | 2/2 |
| `math.NT` | 4/5 |
| `math.OC` | 3/4 |
| `math.PR` | 2/4 |
| `math.QA` | 2/2 |
| `math.RA` | 1/2 |
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
Each route starts from its own clean source copy, then runs twice so the second
pass consumes only that engine's own auxiliary files. Copying, DVI inspection,
Ghostscript validation, and text extraction are outside the timed region.
Engine order is alternated by paper ID; 3 worker(s) ran concurrently.
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
python3 tools/arxiv_holdout.py fetch --offline
python3 tools/texlive_overlay.py
cargo build --release --locked -p aitex-cli
python3 tools/arxiv_holdout.py run --jobs 3 --timeout 120 --memory-limit-mib 2048
python3 tools/arxiv_holdout.py report
```
