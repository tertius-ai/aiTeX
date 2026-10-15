# AI repair protocol v1.2

Protocol v1.2 is the active aiTeX repair handoff. It adds one typed,
source-mapped provenance chain to the compact diagnostic introduced by the
immutable [v1.1 protocol](AI-REPAIR-PROTOCOL-V1.1.md). The chain is collected
once and used by the terminal UI, JSON/session records, source selection, and
model request; these consumers do not reconstruct competing explanations of
the same failure.

The protocol identifier is `aitex-repair-protocol-v1.2` and the diagnostic
schema is `aitex-diagnostic-v1.1`. The model response remains
`aitex-repair-response-v1`. Old repair sessions remain readable because their
new provenance collection defaults to empty.

## Provenance chain

Frames are ordered from the observed failure toward its causes and build
context. A producer may emit these stable kinds:

- `failing-token`: the token and bounded source-line evidence at the failure;
- `macro-argument`: a bounded captured argument active at that point;
- `macro-expansion`: an active control sequence and its use location;
- `macro-definition`: the source assignment that established that meaning;
- `include`: the active file stack, inner file first;
- `generated-artifact`: an `.aux`, `.idx`, `.glo`, `.nlo`, or related product;
- `generated-origin`: an author-source command mapped from that product;
- `semantic-tool`: BibTeX, Biber, MakeIndex, MakeGlossaries, or nomenclature;
- `compilation-pass`: the TeX pass on which the failure surfaced.

aiTeX caps an engine chain at twelve frames and source evidence in one frame at
240 characters before it reaches a model. The human display shows the first
four causal frames under the normal error and caret. `aitex explain` and
`aitex fix` show up to twelve canonical `Trace` lines. ANSI styling is never
serialized.

Engine definition sites follow TeX grouping: a local redefinition restores
both the old meaning and its source coordinate, while a global definition
keeps the new coordinate. Format/bootstrap primitives intentionally have no
invented definition location.

Generated semantic-tool input is identified exactly. Until TeX write nodes
carry author-token coordinates, its author origin remains a bounded set of
high-signal index/glossary/nomenclature source lines rather than a falsely
precise location.

## Request contract

The system instruction, response fields, patch grammar, safety policy,
isolated validation, and acceptance gates are unchanged from v1.1. The
minified user envelope adds `diagnostic.provenance`:

```json
{
  "protocol": "aitex-repair-protocol-v1.2",
  "schema": "aitex-diagnostic-v1.1",
  "display": "Diagnose  aitex-engine · other · chapter.tex:80\nError     Double superscript",
  "diagnostic": {
    "tool": "aitex-engine",
    "severity": "error",
    "kind": "other",
    "message": "Double superscript",
    "count": 1,
    "reported": {"file": "chapter.tex", "line": 80, "column": null},
    "cause": null,
    "provenance": [
      {"kind": "failing-token", "label": "^", "detail": "a^b▶^c"},
      {"kind": "macro-argument", "label": "captured argument", "detail": "a^b^c"},
      {"kind": "macro-expansion", "label": "\\formula"},
      {"kind": "macro-definition", "label": "\\formula", "location": {"file": "macros.tex", "line": 12, "column": null}},
      {"kind": "include", "label": "chapter.tex"},
      {"kind": "compilation-pass", "label": "TeX pass 2"}
    ]
  }
}
```

Locations equal to `diagnostic.reported` are omitted from a frame and inherited.
The common external-tool and generated-artifact frames are omitted from the
model-only array when `diagnostic.tool`, `reported`, and
`reported_file_role: "generated"` already encode them exactly. They remain
explicit in the human presentation and repair-session record. All
nonredundant model frames are objects with `kind` plus applicable `label`,
`location`, and `detail`.

The `display` deliberately excludes the trace because the adjacent structured
chain is canonical model evidence; duplicating it wastes tokens. It otherwise
remains the exact v1.1 human-facing error block.

Compact and expanded source selection now adds distinct macro-definition,
include, and generated-artifact locations, with at most six total windows.
Reported and causal windows retain priority. The existing profile ceilings,
384-token library output default, and provider-native accounting remain in
force.

## Versioning and measurement

Repair-session diagnostic records add a structured `provenance` array and keep
the complete human `presentation`. New score rows and campaign manifests must
record protocol v1.2; saved v1/v1.1 responses are never relabeled. Model
comparisons must continue reporting correctness, tokens, provider cost, and
latency by protocol and by localization/provenance slice.

Any change to frame semantics or ordering, context selection, prompt bounds,
response parsing, patch normalization, safety, or acceptance creates a new
protocol/evaluator identity.
