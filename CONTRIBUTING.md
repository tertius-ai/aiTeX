# Contributing to aiTeX

aiTeX is developed in the open, and focused bug reports, reduced TeX examples,
documentation corrections, portability fixes, and measured performance work are
welcome.

## Before opening a change

Search the [issue tracker](https://github.com/tertius-ai/aiTeX/issues) first.
For behavior changes, an issue or short design discussion is useful before a
large patch. Security-sensitive reports follow [SECURITY.md](SECURITY.md), not a
public issue.

The stable product surface is classic pdfTeX-compatible compilation, direct
PDF/exact DVI, complete builds, cache/watch, diagnostics, and conservative
lint. AI repair is preview; native Unicode/OpenType is experimental. A change
must not quietly move a preview or experimental behavior into the stable
contract.

## Development setup

The workspace pins Rust 1.90.0. A normal compiler build also needs a local TeX
Live or MacTeX installation. The test suite uses `kpsewhich` and the LaTeX
kernel to check the real TeX resolver, so they must be available on `PATH`.
On Debian or Ubuntu, install the required base packages with:

```sh
sudo apt-get install texlive-base texlive-latex-base
```

Then build and test:

```sh
make
bin/aitex --version
cargo test --workspace --locked
```

Useful proportional checks are:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -A warnings -D clippy::correctness
python3 tools/docs_check.py
python3 tools/model_campaign.py --self-test
python3 tools/pdf_contract_check.py
tools/repo_check.sh --artifacts-only
```

See [tools/README.md](tools/README.md), [tests/README.md](tests/README.md), and
[docs/RELEASE.md](docs/RELEASE.md) before changing compiler or release gates.
Do not run the full release qualification merely for a small documentation or
isolated unit-test change.

## Correctness and performance

A TeX compatibility fix should include the smallest redistributable regression
that distinguishes the old and new behavior. Prefer a general engine rule over
a package-name special case. Compare DVI semantics or rendered PDF output when
process success alone is insufficient.

Performance changes need a correctness oracle and measurements from comparable
inputs, builds, machines, affinity, worker budgets, and toolchain versions.
Do not use parallel outer runners for latency claims or remove failures from a
denominator. The repository's detailed rules are in
[AGENTS.md](AGENTS.md) and [docs/PERFORMANCE.md](docs/PERFORMANCE.md).

## Test-data licenses and privacy

Never commit a downloaded paper, private repair session, raw provider response,
API key, or locally materialized corpus. Public availability is not a
redistribution license.

Every third-party fixture, font, image, or source excerpt needs:

- an explicit compatible redistribution license;
- provenance and a stable upstream URL;
- a recorded immutable revision or digest; and
- required attribution.

CC0, CC BY, CC BY-SA, and appropriately recorded OFL fixtures are the default
allowlist. Prefer original deterministic surrogates whenever rights are unclear.
Generated and private state belongs under ignored `.aitex/`, `target/`, or
`corpus/` materialization paths.

## Pull requests

Keep changes scoped and explain:

- the user-visible problem and chosen contract;
- tests and independent correctness evidence;
- compatibility or performance tradeoffs;
- new dependencies, host tools, network use, or stored data; and
- any documentation or release-note impact.

Run `git status --short` before submission. The repository hygiene check
rejects generated TeX products and Python bytecode outside ignored build/corpus
trees. Contributions are accepted under the repository's dual MIT or
Apache-2.0 license.
