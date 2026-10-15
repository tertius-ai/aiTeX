# Privacy and network behavior

aiTeX has no telemetry. Ordinary compilation, one-pass compilation, watch,
cache, and lint are local operations and never contact a model provider.

## When source can leave the machine

Only an explicit `explain` or `fix` model route can send source. A live
request requires:

- an explicitly selected model;
- `--allow-source-upload` or interactive confirmation; and
- a provider route that can be initialized successfully.

Before an interactive upload, the CLI reports the selected source-line and byte
counts. Context is bounded and source-mapped; the model does not receive an
unrestricted project tree. `explain --prompt-only` prints the packet locally,
and `--offline` forbids provider routes.

The local Codex adapter places structured input in an empty read-only workspace.
The model cannot browse the TeX project. It still uses the separately installed
Codex client and is governed by that client's account and service settings.

OpenRouter requests ask for zero data retention and deny provider data
collection through supported routing controls. A routed request can still be
served by a third party. Pin `--openrouter-provider PROVIDER` when provider
identity is part of the decision, and review current OpenRouter and serving
provider policies before sending confidential or unpublished material.

## Credentials

`OPENROUTER_API_KEY` from the process environment takes precedence. If it is
absent, and only when an OpenRouter operation was requested, aiTeX reads:

- `$XDG_CONFIG_HOME/aitex/env`; or
- `~/.config/aitex/env`.

The file accepts a key assignment as data and is never evaluated by a shell.
On Unix, group- or world-accessible permissions are rejected. Ordinary compile
commands do not inspect the credential file. API keys are not included in
diagnostics, repair-session records, JSON events, or model context.

Use a provider key with its own spending ceiling. aiTeX can enforce a requested
`--max-cost-usd` between calls only when the provider reports authoritative
charges; the provider limit is the hard pre-request backstop.

## Local records

Repair attempts are stored under the project's ignored `.aitex/repairs/`
directory. Records may contain:

- the compact diagnostic and selected source context;
- raw provider output and normalized edits;
- file and content digests;
- tool actions and validation outcomes; and
- model/provider, token, time, and charged-cost facts.

Rejected and dry-run attempts are retained for auditability. aiTeX does not
upload these records automatically. Treat the directory like the source
project, and remove it according to the project's own retention policy.

`tools/export_repairs.py` is an explicit user action. Full-fidelity exports
can contain source and provider content; `--metadata-only` replaces those
fields with digests. No export is committed or transmitted by aiTeX.

## Host tools

TeX Live semantic tools and optional graphics converters run as local
subprocesses. aiTeX does not add network access for them, but their own
configuration and security behavior remain outside aiTeX's control. Use current
patched versions when compiling untrusted documents.
