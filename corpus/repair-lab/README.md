# aiTeX repair lab

This is a deliberately small, hand-authored corpus for developing and testing
aiTeX's diagnostic-to-model boundary. Each case keeps:

- the broken source product;
- the corrected product;
- a short representative compiler/tool transcript; and
- the expected diagnostic classification, location, route, build root, and
  repair intent. The evaluator derives a stable difficulty tier and failure
  surface from those facts without changing the frozen corpus manifest.

Each fixed fixture now has a complete root document. Scoring copies its project,
applies the candidate with the same strict parser as `aitex fix`, and runs the
full native scheduler, including BibTeX, Biber, MakeIndex, MakeGlossaries, or
nomenclature where required. A byte-for-byte fixed-source match is recorded
separately from successful compilation, because a different patch may still be
semantically correct.

This is a protocol-conformance corpus stratified as `sanity`, `localized`, and
`nonlocal`; it is not a representative model-accuracy benchmark. The files are
project-authored test fixtures under the repository license. They
contain no downloaded papers, private documents, provider responses, or API
credentials. The transcript text is intentionally stored as `.txt` so it is
not confused with generated build output.

Run the offline lab from the repository root:

```sh
cargo run -p aitex-harness --bin aitex-repair-lab
```

Compare prompt-size profiles with `--profile diagnostic`, `compact`, or
`expanded` (or use `all`); inspect one exact packet with `--show-prompt CASE`.
Add `--score --aitex bin/aitex --results FILE.jsonl` to evaluate patches in
isolated project copies. A live Anthropic smoke test requires all of
`--provider anthropic`, `--model MODEL`, `--case CASE`,
`--allow-source-upload`, and `ANTHROPIC_API_KEY`. The full six-case,
three-profile run additionally requires the explicit `--all-model-cases` flag.
The lab never applies a patch to this corpus.

Real `aitex fix` runs are a separate, growing source of natural repair data.
They remain private under each project's `.aitex/repairs/` directory until the
user explicitly runs `tools/export_repairs.py PROJECT -o records.jsonl`.
Full-fidelity exports contain the bounded source context, patches, and raw
provider response needed for later model comparisons; use `--metadata-only`
when those private fields must be replaced by a digest.
