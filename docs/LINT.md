# Conservative lint and silent-success detection

Status: the first conservative v0.3 command is implemented; expansion-aware
and differential rules remain planned.

## Why this is separate from compilation

TeX can successfully produce a document after a source mutation that changes
meaning, removes content, or leaves a command semantically ineffective.
Development mutation campaigns contain examples where both stock TeX and
aiTeX complete without a compiler diagnostic. Inventing an error for such a
case would make ordinary compilation less trustworthy and train repair systems
on evidence the compiler never observed.

`aitex lint` will therefore be an explicit analysis layer. A successful build
continues to mean that the build completed. Lint findings are warnings with
their own rule identity, evidence, confidence, and source span; they are not
retrospectively labeled TeX errors.

## Command contract

```text
aitex lint main.tex                 deterministic checks after a full build
aitex lint main.tex --deny warnings nonzero exit when a finding is present
aitex lint main.tex --json          versioned machine-readable findings
aitex fix main.tex                  may offer lint-backed repair after success
```

The default command remains `aitex main.tex`, with no extra stock-TeX process
and no lint latency on the compilation fast path. Lint is local and model-free
by default. Any future model escalation or source upload retains the existing
explicit consent and cost boundaries.

The initial implementation contains three high-confidence, artifact-backed
rules:

- `reference/undefined@1` for a reference that is still unresolved in the
  converged TeX log;
- `citation/undefined@1` for a citation observed in the converged auxiliary
  tree or log without a resolved bibliography entry; and
- `label/duplicate@1` for a label defined more than once in the executed
  auxiliary tree.

Detection comes from files produced by the completed build, so text in a
comment, an unexecuted branch, or an unused macro definition does not create a
finding. Bounded source inspection is used only to map already-observed
evidence back to an author line. `aitex lint` works in the no-AI build and does
not contact a model or stock TeX. `aitex lint --json` emits one
`aitex-lint-report-v1` object; `--deny warnings` changes only the exit status.

## Detection tiers

1. **Source checks.** High-precision lexical and structural invariants over
   author files, includes, bibliography records, and index/glossary commands.
   A rule must identify concrete source evidence and decline ambiguous input.
2. **Expansion-aware checks.** Bounded engine observations for constructs whose
   problematic form appears only after macro substitution. These reuse typed
   expansion/include provenance rather than guessing globally from source text.
3. **Build-artifact checks.** Cross-reference, citation, bibliography, index,
   glossary, and generated-file consistency after the complete fixed point.
4. **Optional differential checks.** Explicitly requested comparison against a
   prior artifact or stock TeX. This is slower, may require TeX Live, and must
   distinguish source-semantic differences from harmless PDF byte differences.

Every release should contain only rules with measured precision high
enough for an interactive compiler tool. Benchmark-only mutation knowledge is
never shown to a user as though it were observable from their current project.

## Finding shape and display

A finding should use the same compact human/machine presentation as build
diagnostics while remaining typed as lint evidence:

```text
  ◆ Lint      undefined-reference · chapter.tex:42
    Evidence  LaTeX Warning: Reference `sec:model' ... undefined on input line 42.
    Rule      reference/undefined@1 · high confidence
```

The v1 record contains the schema version, rule ID/version, severity,
confidence, subject, best source span, bounded evidence, and suppressibility.
Typed expansion provenance and compiler/format hashes will be added when the
second detection tier lands. JSON output does not contain source beyond the
bounded evidence already shown to the user.

## Qualification plan

1. Triage external silent-success cases by observable signal and explicitly
   mark those that require a golden original or rendered-output comparison.
2. Freeze a development split and an untouched evaluation split before writing
   rules; add diverse clean counterexamples.
3. Measure precision, recall, runtime, and peak memory per rule. Report
   abstentions rather than treating them as failures.
4. Gate every proposed repair with the existing isolated rebuild and semantic
   artifact oracle. A lint warning alone never authorizes an edit.
5. Promote lint evidence into model comparisons only after the human-facing
   rule and display have passed the same frozen evaluation.

Success means a useful zero-surprise command with an honest detectable subset
and essentially no noise on clean projects, not perfect recall over mutations.
