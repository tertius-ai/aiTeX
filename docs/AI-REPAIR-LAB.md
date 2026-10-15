# Controlled AI repair lab

The repair lab is a small, original protocol-conformance suite for developing
aiTeX's diagnostic-to-model boundary. It asks which build failures need a model
and how little source context can still produce a grounded repair. It is not a
general model-accuracy benchmark.

The active [v1.3 protocol](AI-REPAIR-PROTOCOL-V1.3.md) powers the read-only
`aitex explain` command and validation-gated `aitex fix` workflow. The
[agentic v1.1 protocol](AI-AGENTIC-REPAIR-V1.md) defines bounded progressive
source inspection and multi-file repair.

## Product boundary

Ordinary `aitex`, `aitex watch`, `aitex pass`, and `aitex lint`
operations never construct the repair provider layer or contact a network
service. AI support is present in the default binary, while `make no-ai` (or
Cargo's `--no-default-features`) removes the harness, HTTP client, TLS stack,
and provider implementations entirely.

A repair proposal is never applied merely because a model returned it. aiTeX:

1. runs the complete build and deterministic local diagnosis;
2. shows the same source-mapped diagnostic to the user and model;
3. requests explicit consent before selected source is uploaded;
4. accepts only exact, bounded edits to existing author-controlled files;
5. applies them in an isolated private copy;
6. requires a structurally valid complete PDF or DVI build;
7. shows the resulting diff and diagnostic delta; and
8. writes the real project only after explicit acceptance.

The one-shot route is limited to one author file. The agentic route may read
and edit several author files through bounded host-owned tools, but receives no
shell or arbitrary filesystem access.

## Original fixture corpus

[`corpus/repair-lab/manifest.json`](../corpus/repair-lab/manifest.json) is an
`aitex-repair-corpus-v1` manifest over eight project-authored broken/fixed
fixtures. It covers TeX, BibTeX, Biber, MakeIndex, MakeGlossaries, and
nomenclature failures. Every case declares its expected diagnostic kind,
location, build root, routing class, and repair intent.

Index, glossary, and nomenclature tools report errors in generated `.idx`,
`.glo`, or `.nlo` files. The fixture records also name the causal author
line, letting the compact packet explain the relationship without inviting a
model to patch generated output.

Run the offline lab with:

```sh
make repair-lab
target/debug/aitex-repair-lab
target/debug/aitex-repair-lab --profile compact
target/debug/aitex-repair-lab --show-prompt tex-unclosed-brace
```

Scoring copies each project, applies a proposal with the same strict parser as
`aitex fix`, and invokes the native complete-build scheduler:

```sh
target/debug/aitex-repair-lab --score \
  --aitex bin/aitex --results target/repair-lab.jsonl
```

A byte-for-byte match with the fixture's fixed source is recorded separately
from successful compilation because another small edit may be semantically
valid.

## Diagnostic and context profiles

Every request carries the same compact ANSI-free `Diagnose` and `Error`
block shown in the CLI. It can also carry distinct reported/causal locations,
include and expansion provenance, generated-artifact mappings, and collapsed
occurrence counts.

The bounded profiles are:

- `diagnostic`: classification, message, phase, and source location;
- `compact`: diagnostic plus three lines around each reported or causal
  point; and
- `expanded`: twenty lines around each point plus relevant preamble package
  declarations.

The model returns `aitex-repair-response-v2` JSON with a short cause,
confidence, and exact `path`/`old`/`new` edits. Each nonempty old string
must occur exactly once. aiTeX performs no fuzzy matching or guessed
relocation. Saved v1 unified-diff responses remain replayable under the older
strict parser.

## Explain and fix

Read a diagnosis and locally rendered prompt without making a provider call:

```sh
bin/aitex explain main.tex
bin/aitex explain main.tex --prompt-only
bin/aitex fix main.tex --dry-run --offline
```

Run a guided terminal session or a noninteractive agent preview:

```sh
bin/aitex fix main.tex
bin/aitex fix main.tex --dry-run --route ai-agent \
  --provider openrouter --model MODEL --allow-source-upload \
  --max-cost-usd 0.10
```

A successful dry run records an acceptance command. Reapply the exact validated
transaction without another model call:

```sh
bin/aitex fix main.tex --session SESSION_ID --apply
```

Application checks unchanged input hashes, repeats private validation, backs up
every original under `.aitex/backups/`, writes atomically, and performs one
real complete build. A stale digest or failed final build refuses or rolls back
the transaction.

## Providers, credentials, and privacy

Live requests require both a model and `--allow-source-upload`. The CLI
reports selected source lines/bytes before consent, then reports provider,
served model, token classes, elapsed time, and authoritative charged cost when
available. Use `--json` for the machine protocol and `--offline` to forbid
model routes.

OpenRouter uses `OPENROUTER_API_KEY` from the environment. If it is absent,
and only after an OpenRouter operation was explicitly requested, aiTeX reads
`$XDG_CONFIG_HOME/aitex/env` or `~/.config/aitex/env`. The file is parsed as
key/value data rather than executed. Unix group/world permissions cause
rejection. Pin a serving route with `--openrouter-provider PROVIDER`; otherwise
OpenRouter may route normally.

The local Codex adapter invokes an already authenticated `codex` executable
from `PATH` (or `AITEX_CODEX`). It gives each turn structured input in an
empty read-only workspace; the model has no filesystem access to the TeX
project.

Provider policies can change independently of aiTeX. Users should verify the
current provider privacy and retention terms before uploading confidential or
unpublished source. aiTeX requests OpenRouter's zero-data-retention/no-training
route controls, but local consent and minimization remain the primary boundary.

## Budgets and records

The default agent limits are 12 model/tool turns, four complete builds, 100,000
aggregate input tokens, 16,384 aggregate output tokens, 16 KiB of selected
source, 200 changed lines, 100 deleted lines, and 600 seconds. CLI flags can
tighten or raise them. A requested dollar cap is accepted only when the
provider supplies authoritative charges; a provider-side key limit is the hard
pre-request spending boundary.

Every attempt, including rejected output, is stored in a private
`aitex-repair-session-v1` record under the project's `.aitex/repairs/`
directory. It retains bounded context, raw response, normalized proposal,
digests, safety and build results, provider/model identity, token/timing/cost
facts, tool actions, and user decision. Generated products and paths outside the
project can never be patch targets.

These records are private by default. Explicit export is available for a user
who wants to contribute their own data:

```sh
python3 tools/export_repairs.py PROJECT -o records.jsonl
python3 tools/export_repairs.py PROJECT -o metadata.jsonl --metadata-only
```

A full export may contain source context and provider output. The metadata-only
form replaces private fields with digests.

## Offline campaign check

`tools/model_campaign.py` retains a small, resumable multi-model experiment
around the original fixture corpus. Raw prompts, patches, reviews, and model
catalogs default to ignored `.aitex/benchmarks/` storage. It records
provider-native token, cost, and timing data and supports content-addressed
offline rescoring without paying for the same answer again.

```sh
make repair-check
python3 tools/model_campaign.py --self-test
python3 tools/model_campaign.py \
  --campaign .aitex/benchmarks/pilot \
  --model vendor/model --profiles compact --repeats 3 \
  --budget-usd 1.00 --plan-only
```

Model, prompt, tool-policy, and diagnostic experiments at larger scale belong
in a separate research workspace. No private source or raw provider response
is committed to this compiler repository.

## Historical protocols

The current request/response contract is
[protocol v1.3](AI-REPAIR-PROTOCOL-V1.3.md). Protocol
[v1.2](AI-REPAIR-PROTOCOL-V1.2.md) introduced typed causal provenance, and
[v1.1](AI-REPAIR-PROTOCOL-V1.1.md) made the canonical human diagnostic part of
model context. The original [v1](AI-REPAIR-PROTOCOL-V1.md) remains an immutable
comparison baseline. Detailed development campaigns through 0.3.0 are preserved
by the v0.3.0 Git tag rather than carried in the public-release tree.
