# AI repair protocol v1.3

Protocol v1.3 keeps the v1.2 diagnostic and provenance envelope but changes
the model output boundary so repair quality is not confounded with unified-diff
formatting skill. Its identifiers are `aitex-repair-protocol-v1.3` and
`aitex-repair-response-v2`.

## Response contract

The model returns JSON with a short cause, confidence, and zero or more exact
source replacements:

```json
{
  "schema": "aitex-repair-response-v2",
  "cause": "The index command emits an empty key.",
  "edits": [
    {
      "path": "main.tex",
      "old": "\\index{}",
      "new": "\\index{topic}"
    }
  ],
  "confidence": "high"
}
```

All edits in one response must name the same existing author-controlled file.
They are applied in order, and each nonempty `old` string must occur exactly
once in the current bytes. aiTeX performs no fuzzy matching, whitespace
repair, or guessed relocation. It converts a grounded proposal to a canonical
diff and passes it through the existing changed-line safety limits, isolated
full build, and explicit acceptance boundary.

Generated build products remain forbidden targets. The source file's existing
final-newline convention is preserved. Up to eight edits and 64 KiB of total
replacement text are accepted by the evaluator.

One exact `json` Markdown fence may be removed as transport decoration and is
recorded as response normalization. Arbitrary surrounding prose remains an
invalid response. Saved `aitex-repair-response-v1` unified-diff responses stay
replayable under the old conservative diff normalizer; new requests always ask
for v2 structured edits.

## Measurement

Every score row distinguishes these cumulative stages:

1. provider response received;
2. response contract valid;
3. proposal nonempty;
4. author target resolved;
5. exact edit grounded;
6. safety gate passed;
7. complete build passed; and
8. exact oracle matched.

The tracked eight-case lab is explicitly a protocol-conformance suite. Its
`sanity`, `localized`, and `nonlocal` strata expose evaluator regressions but
do not stand in for model accuracy on a representative corpus. Larger model
selection campaigns live outside this repository and report the same funnel
with tokens, cost, latency, and an immutable correctness oracle.

Any change to edit grounding, bounds, safety, validation, or funnel semantics
requires another protocol/evaluator identity. The v1, v1.1, and v1.2 documents
remain immutable historical definitions.
