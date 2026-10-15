# Agentic repair protocol v1.1

`aitex-agentic-repair-v1.1` is the bounded investigation loop behind
`aitex fix --route ai-agent`. It addresses failures that cannot be repaired
from one preselected excerpt: non-local macro expansion, cross-file package or
class interactions, generated semantic-tool inputs, and compound failures that
only become visible after an earlier error is removed.

The model is a planner, not a process supervisor. It receives no shell, no
working-directory access, and no arbitrary file tool. aiTeX owns an isolated
`RepairWorkspace` and offers eight host operations:

| Action | Effect |
|---|---|
| `list_files` | List bounded author-source paths only |
| `search` | Search a literal string in one author file or the bounded project set |
| `read` | Read a numbered source range, capped by line, byte, and session budgets |
| `read_log` | Read a bounded TeX or semantic-tool log, or list the available logs |
| `edit` | Replace one exact, unique string in an existing author file |
| `revert` | Undo the most recent agent edit in the private workspace |
| `build` | Run the complete cached aiTeX fixed-point build and return a diagnostic delta |
| `stop` | End without changing the real project |

Every model turn must be one `aitex-agent-action-v1` JSON object. All action
fields are present so providers can enforce one stable JSON Schema; confidence
is not part of the provider-constrained control message. Project source and
tool results are delimited as untrusted data. Each control response is capped
at 1,024 tokens. Malformed responses remain in the private trace but are not
echoed into later prompts, and two consecutive parse failures stop the route.

## Progressive context

The first request contains the same canonical, ANSI-free diagnostic shown to
the user: tool, kind, error text, reported and causal locations, occurrence
count, and typed provenance. It also identifies the root input, output type,
and available tools. Version 1.1 additionally includes one bounded seven-line
preview around the best causal location when author source is available. The
preview is counted against the normal line, byte, and token budgets; the agent
is told to use it instead of spending its first turn reading the same range.
It does not include the project tree or a broad source excerpt.

The v1.1 diagnostic boundary performs only bounded, auditable deductions. A
missing-math recovery point is checked on its own line and the immediately
preceding line for an odd inline-dollar count. Generated glossary records are
read from the root input directory, their `\glossentry`/`\glsentry` key is
mapped back to its author definition, and an empty field becomes the causal
line. Suspicious missing package and graphics names may carry a conservative
spelling candidate as provenance. These are evidence, not silent edits: the
agent still makes an exact change and must validate it with a complete build.

Later turns add deterministic state—current diagnostic, whether validation is
stale after an edit, changed-file names, and operation counts—plus at most the
four most recent action/result pairs. Older source is re-read only when it
becomes relevant. Search output is clipped per line; reads are line-ranged;
tool results and total shared source bytes are bounded. Before every call aiTeX
estimates the complete request against the cumulative input budget.
Provider-reported input, cache, output, reasoning, latency, and cost replace
estimates in the audit record.

A `build` result does not return a transcript dump. It reports whether the
previous causal diagnostic is unchanged, replaced, or cleared by a complete
successful build, together with the next structured diagnostic when one
exists. This makes aiTeX's diagnostic quality an explicit model input and a
measurable repair-system variable. If a turn or token budget ends after an
edit but before the model requests `build`, aiTeX spends one remaining build
allowance on final deterministic validation. It accepts only a genuinely
successful complete build; otherwise the run remains incomplete with the
replacement diagnostic preserved. An unresolved citation or reference that
the initial failure identified with no literal author-side definition is also
carried as a compound repair obligation: producing a PDF does not clear that
obligation, but a converged build in which the warning disappears does.

## Safety and transaction boundary

Source actions are restricted to `.tex`, `.ltx`, `.sty`, `.cls`, `.cfg`,
`.bib`, `.bst`, `.bbx`, and `.cbx` files already inside the project copy.
Generated artifacts, symlinks, absolute paths, traversal, shell escape, and
network tools are unavailable. An edit's old text must occur exactly once.
Every edit passes the normal patch checks; the final aggregate multi-file diff
is checked again so many small edits cannot bypass changed/deleted-line limits.

The real project remains untouched while the agent works. A complete,
structurally valid PDF or DVI build is required before a proposal can be shown.
`--dry-run` stores all per-file patches and SHA-256 digests in the repair
session. `--session ID --apply` replays those patches against unchanged input,
rebuilds the private copy, backs up every original, atomically applies the
transaction, and performs one real complete build. Any stale digest, failed
write, or final build failure refuses or rolls back the transaction.

Default limits are 12 model/tool turns, four complete builds, 100,000 input
tokens, 16,384 output tokens, 16 KiB of selected source, 200 changed lines, 100
deleted lines, and 600 seconds. CLI flags can lower or raise each bound.
Source inspection still requires explicit consent; authoritative cost caps are
accepted only for providers that report charged cost.

## Audit and evaluation

The local `aitex-repair-session-v1` record embeds one
`aitex-agent-trace-v1`. It records every action and result, model/provider,
per-turn token and elapsed facts, aggregate charged cost, reads, searches,
builds, diagnostic deltas, edits, reverts, final status, and the validated
multi-file change set. Human JSONL output intentionally emits compact progress
metadata rather than source contents; the full trace stays in the protected
local session file.

The v1.1 policy keeps the v1 turn/build/output limits and adds the causal
preview and conservative deductions above. Evaluations must keep protocol,
model, provider route, compiler, format, token/cost accounting, and correctness
oracle in their immutable identity; results from different identities are not
relabeled or merged.

Large model and harness comparisons live outside the compiler repository. The
checked-in eight-case repair lab tests the request, response, safety, and build
funnel offline; it is deliberately too small to support a general model-quality
claim. Detailed campaigns through the 0.3.0 milestone remain available from
the v0.3.0 tag.

Development evaluation exposed two distinct reporting problems: some source
changes still compile but alter meaning, while genuine failures can lack a
useful source location. The first class belongs to explicit lint or differential
analysis rather than fabricated compiler errors; see [`LINT.md`](LINT.md). The
second motivated recovery-line preservation, conservative math-state
localization, unique failing-token search, and semantic-tool source mapping.

The bounded source-enrichment stage is shared by ordinary builds, watch mode,
`explain`, and `fix`. Consequently, a causal location or generated-artifact
mapping is part of the normal human CLI before it is reused in model context or
a repair-session record; the AI path no longer owns a more informative private
diagnostic.

## Examples

```sh
# Human-guided; local deterministic rules still run first.
bin/aitex fix main.tex --provider codex --model gpt-5.6-luna

# Explicit unattended preview through OpenRouter.
bin/aitex fix main.tex --dry-run --route ai-agent \
  --provider openrouter --model MODEL --allow-source-upload \
  --max-agent-turns 12 --max-agent-builds 4 --max-cost-usd 0.10

# Apply the exact validated multi-file proposal without another model call.
bin/aitex fix main.tex --session SESSION_ID --apply
```

The local Codex adapter currently invokes a fresh structured `codex exec` turn
with the bounded rolling context. A future persistent-thread adapter may reduce
repeated prefix tokens, but it must preserve the same host-owned tools, limits,
trace, and validation boundary.
