# Building aiTeX from source

An aiTeX installation is the native `aitex` executable. A release artifact
does not contain a format copied from the build machine's TeX installation.
Instead, the executable creates `latex.afmt` from the user's installed LaTeX
kernel on the first document build and validates it before later use.

Version 0.3.1's public distribution is the tagged source tree. Native
Linux/macOS bundles, wheels, package-registry entries, and hosted compiler
services are not published. The repository retains local packaging tools for
qualification and future distribution work.

## Runtime dependencies

A bundle is not a TeX distribution. Linux requires TeX Live and macOS requires
MacTeX or BasicTeX, with `kpsewhich` on `PATH`. The host tree supplies the
LaTeX kernel, document classes, packages, TFM metrics, Type 1 fonts, and
`extractbb` fallback. If these are absent, plain TeX can still run when invoked
explicitly in INITEX-style workflows, but an ordinary LaTeX document reports a
clear setup error while preparing its local format.

A complete build additionally invokes only the semantic programs the document
uses:

- BibTeX or Biber;
- MakeIndex or nomenclature processing; and
- MakeGlossaries.

Native image paths need no helper. Complex PDF/EPS or alpha normalization may
use MuPDF, Ghostscript, Netpbm, or ImageMagick on demand. See the README for the
exact graphics boundary.

## Local format lifecycle

On the first normal document build, aiTeX loads the installed `latex.ltx` in
its native `--ini` mode and atomically publishes a format plus manifest under:

- `$XDG_CACHE_HOME/aitex/formats/latex.afmt`, when `XDG_CACHE_HOME` is an
  absolute path;
- `~/.cache/aitex/formats/latex.afmt` on Linux/Unix;
- `~/Library/Caches/aiTeX/formats/latex.afmt` on macOS; or
- `.aitex-cache/formats/latex.afmt` beside the input when no home directory is
  available.

`--cache PATH` also places the format under `PATH/formats`. `--no-cache`
disables document/output snapshot reuse, not this required derived LaTeX state.
Use `aitex format` to prewarm the standard location, or
`aitex format --output PATH` for a developer/qualification format. `--fmt PATH`
and `AITEX_FORMAT=PATH` remain explicit, unmanaged overrides.

The manifest records the format checksum, exact engine-format identity,
relevant TeX search environment, and every stable TeX file read while loading
the kernel. aiTeX automatically regenerates the format after an engine change,
a TeX Live/MacTeX update, a changed input, or corruption. A lock and atomic
rename make concurrent first invocations safe. Some MacTeX installations
enable Ibycus patterns whose loader uses unsupported `\savinghyphcodes`; aiTeX
stages a private `language.dat` with only Ibycus disabled and never modifies
the host installation.

## Build and install from a checkout

On Linux or macOS:

```sh
make
./bin/aitex --version
./bin/aitex main.tex
```

The default target creates an AI-enabled `bin/aitex` and prewarms
`.aitex/format/fmt.afmt` from the local TeX installation. `make no-ai` omits
the repair harness, HTTP client, TLS stack, and provider implementations while
using the same local-format model.

A system prefix normally needs privilege:

```sh
make
sudo make install
```

Only the executable is installed, so a privileged install never creates a
root-owned user cache. The default prefix is `/usr/local`. An unprivileged
install is:

```sh
make install-user
# If needed:
export PATH="$HOME/.local/bin:$PATH"
```

Packagers can stage the conventional layout without privilege:

```sh
make install DESTDIR="$PWD/build/package-root" PREFIX=/usr
```

## Unpublished native-bundle checks

Developers can build and structurally verify a local bundle for the current
host:

```sh
make dist-bundle
```

This path builds no format and therefore does not require TeX on the artifact
builder. The archive is named
`dist/aitex-<version>-<rust-host-triple>.tar.gz`. It contains:

- `bin/aitex`;
- installation guidance, README, both project licenses, and third-party
  notices under `share/doc/aitex/`;
- a deterministic SPDX 2.3 dependency inventory; and
- an internal `SHA256SUMS` covering every payload file.

A deterministic sidecar `.sha256` covers the archive itself. Entries have
normalized owner, group, mode, order, and timestamp. The timestamp comes from
`SOURCE_DATE_EPOCH` or the candidate commit.

Run a clean-install smoke build on a machine where TeX Live is already
available:

```sh
python3 tools/bundle_check.py \
  dist/aitex-0.3.1-$(rustc -vV | sed -n 's/^host: //p').tar.gz --smoke
```

The smoke test uses an isolated cache and verifies both PDF output and local
format creation. To install an extracted bundle:

```sh
PREFIX="$HOME/.local"
install -d "$PREFIX/bin"
install -m 0755 bin/aitex "$PREFIX/bin/aitex"
```

## Source snapshot

From a clean exact candidate commit:

```sh
make dist-source
```

This uses `git archive`, prefixes every entry with `aitex-<version>/`, and
writes a deterministic gzip stream plus sidecar digest. The public tag itself
is the release boundary; this local archive verifies what the tag contains and
is not uploaded as a separate asset.

## Version identity

The package version comes from the workspace `Cargo.toml`. Between exact
release tags, `aitex --version` appends `git describe` so a development binary
is not mistaken for a release. An exact release-tag or source-archive build
reports the package version alone. Packagers building an exported tree may set
`AITEX_BUILD_GIT_DESCRIBE` when they have equivalent source identity metadata.
The pre-tag qualification sets it to the future `v<version>` tag and verifies
the resulting bare version, ensuring that creating the approved tag does not
change the qualified binary's identity.

An `.afmt` from incompatible engine source is rejected by the embedded format
identity check. The normal managed cache then rebuilds it automatically.

## Unpublished packaging tools

The repository retains `make dist-bundle`, `make wheel`,
`tools/bundle_check.py`, and `tools/wheel_check.py` for development, but none of
their outputs is a v0.3.1 release artifact. Any future promoted package must
preserve the executable-only contract and the same first-run local format
bootstrap; there is no Python wrapper in the compile path.
