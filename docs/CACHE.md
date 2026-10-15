# Layered snapshot cache

> **v0.2.0 status:** only the preloaded format and exact converged-output
> layers are active in the release path. Class/project/body snapshot code and
> the design history below are retained but not admitted: a host lexer cannot
> prove that a literal `\begin{document}` executes. Prefix reuse returns only
> with an engine-observed semantic checkpoint and validated prefix pointer.
> `cache warm` converges TeX-written auxiliary files and publishes the exact
> completed DVI/PDF entry. It assumes bibliography/index/glossary products are
> already current. Use bare `aitex main.tex` for a complete native scheduled
> build (`aitex build main.tex` is the explicit alias).

For an operational sequence that selects the fastest mode for one-shot, CI,
and interactive work, start with [`FAST-PATH.md`](FAST-PATH.md). This document
specifies why those reuse paths are safe.

aiTeX has three persistent LaTeX cache layers:

```text
preloaded LaTeX format
    -> document class and options
        -> project preamble through \begin{document}
            -> converged document output
```

The class layer is shared by documents with the same safe literal
`\documentclass` prefix. The project layer is specific to the complete
preamble. On a project hit only the body executes; on a class hit packages and
project definitions execute before a new project snapshot is written. An
unusual catcode-driven or dynamically constructed class command simply skips
the class layer.

For an ordinary one-shot build, a successful pass whose auxiliary files are
already at a fixed point can also publish its final DVI or PDF. A later exact
hit validates the complete source, all transitively read inputs, driver/font
resources, clock use, and every non-primary output before restoring the primary
artifact. Auxiliary files, custom write streams, and the `.log` remain normal
files rather than opaque cache payloads: if any is missing or changed, aiTeX
executes a real pass and repairs it. Deleting or replacing only the requested
DVI/PDF is safe because that file is the checksummed cache payload.

## Cache identity

Persistent keys are SHA-256 hashes over length-delimited fields. Schema 5
includes:

- cache schema and object kind;
- aiTeX package version and compiled feature set;
- SHA-256 identity of the base `.afmt` format;
- the exact source prefix, including class options;
- project job name (and class job name only when observed), output-driver mode,
  TeX82 mode, and `max_print_line`;
- every `TEX*` and `KPSE*` environment variable plus the relevant bibliography,
  font, source-date, time-zone, and locale variables.

The converged-output key additionally includes the complete top-level source,
the exact input, output, and format request spellings, and the output route.
Its manifest adds every auxiliary/custom/transcript artifact and every PDF
driver dependency to the transitive input set.

The serialized engine has its own source-derived format version. A snapshot
from an incompatible engine is rejected by `Fast::undump` even if it is found
under the same persistent key.

Each object also has a manifest containing every file lookup observed while
building it. The manifest records the logical request, the resolved path or
its absence, and a SHA-256 content digest. Binary TFM reads are included.
Metadata reads through `\pdffilemoddate` additionally record the modification
timestamp. Validation checks these facts without executing TeX. Project-local
resolution is checked first, so a newly added local package or class
immediately shadows an installed one. With the resolution environment
unchanged, an existing absolute TeX-tree path is reused instead of making
hundreds of kpathsea round trips; its metadata and content are still validated.
Unchanged file signatures avoid rereading and hashing contents, while a size,
modification-time, or change-time difference falls back to SHA-256 before
rejecting the entry. Thus a new local shadow file, a removed file, a changed
package, an auxiliary file appearing after the first pass, or a TeX
installation upgrade in place becomes a miss. Installing a higher-priority
package elsewhere without changing the resolution environment is the
deliberately rare exception; prune the cache after such a system-level
search-path change.

TeX's `\time`, `\day`, `\month`, and `\year` receive usage-sensitive,
field-granular treatment. The engine records which values execution consumes,
and a snapshot validates only those fields. A preamble that reads `\year`
survives the next minute or day but expires when the year changes; an ordinary
class that reads none of them has no clock dependency.
`\jobname` is tracked the same way at the class boundary: an observing class
gets a job-specific object, while an ordinary class can share one object
across projects. Project snapshots remain job-specific because
`\begin{document}` has opened job-named auxiliary streams.

The current LaTeX3 kernel eagerly reads the clock from a private `\everyjob`
helper to define its `\c_sys_*` constants. The standard `latex.afmt` builder
marks that initialization as memoization: the corresponding dependency is
recorded if the document later consumes one of those constants. Arbitrary and
custom formats remain conservative unless built through that explicit path;
user code in `\everyjob` is not exempt. PGF's eager `\time * \year` random-seed
initialization is handled the same way for reviewed official package bodies;
unknown versions and project-local shadows conservatively retain the clock
dependency. Loading a recognized TikZ alone is clock-independent, while
consuming its default random state records the relevant fields. Thus an
ordinary LaTeX document can remain cached indefinitely, and a document that
prints `\today` remains valid across minute changes but expires at midnight.
Reproducible builds can still pin the complete TeX-visible clock with
`SOURCE_DATE_EPOCH` and `FORCE_SOURCE_DATE=1`, matching pdfTeX's opt-in behavior.

## Publication and recovery

Payloads and manifests have independent size limits (512 MiB and 16 MiB) and
the manifest contains the payload length and SHA-256 checksum. Writers use an
exclusive per-key lock and publish temporary files with `sync_all` and atomic
rename; the manifest is published last. Locks and temporary files older than
five minutes are recoverable debris. Corrupt, partial, oversized, stale,
unknown-schema, and incompatible entries are diagnostic misses, never fatal
compiler errors.

The default cache bound is 1 GiB. `AITEX_CACHE_MAX_BYTES` changes it. Once a
write exceeds the bound, the oldest manifest entries are evicted until the
cache fits.

Complete builds use the platform cache location by default. aiTeX honors an
absolute `XDG_CACHE_HOME` and stores data in `$XDG_CACHE_HOME/aitex`; otherwise
Unix/Linux uses `~/.cache/aitex` and macOS uses
`~/Library/Caches/aiTeX`. If no home directory is available, it falls back to
`.aitex-cache` beside the input. `--cache <directory>` always overrides this
choice and `--no-cache` disables persistent caching.

Class and project snapshot admission is disabled in v0.2. `aitex cache
warm` instead repeats the complete job until auxiliary state converges and
publishes the requested exact completed output. The mandatory `-o` keeps a
successful warm command from becoming a silent no-op.

## Commands

```sh
# Warm the exact completed-output layer; defaults to the platform cache when
# --cache is omitted. `-o` is required because the artifact is part of the
# cache identity and payload.
# LaTeX passes repeat (up to five) until TeX-written auxiliary inputs stabilize.
# Semantic products must already be current; ordinary complete builds use
# bare `aitex main.tex` instead.
aitex cache warm main.tex --fmt target/performance/fmt.afmt -o main.pdf

# Validate and describe every entry.
aitex cache inspect .aitex-cache

# Remove invalid entries, stale writer debris, and oldest entries above a cap.
aitex cache prune .aitex-cache --max-bytes 536870912

# Run a complete build without persistent caching. Use `pass` for exactly one
# TeX pass, with an optional explicit project cache.
aitex main.tex --no-cache
aitex pass main.tex --cache .aitex-cache --no-project-cache
aitex pass main.tex --cache .aitex-cache --no-class-cache
```

Cache diagnostics distinguish `hit`, `miss`, `invalid`, and `disabled`, and
identify a `persistent output cache hit` separately from a project snapshot;
the `AITEX_METRICS=1` JSON record reports class and project lookup state,
validation/load time, class/preamble/body execution, snapshot writes, and
snapshot bytes.

## Persistent watch service

```sh
aitex watch main.tex --fmt target/performance/fmt.afmt \
  --cache .aitex-cache -o main.pdf
```

The initial build runs immediately. Bare `watch` then polls
the exact project dependency graph recorded by TeX, the PDF writer, and
semantic build tools. It debounces rapid saves, notices atomic replacements,
and refreshes its baseline only after generated writes finish. Metadata changes
are verified against content, so rewriting or atomically replacing a file with
identical bytes does not rebuild. Inputs observed through `\pdffilemoddate`
remain metadata-sensitive. A failed build retains both the service and the
previous good output.

Completion is framed on standard error as:

```text
AITEX_WATCH ready sequence=3 code=0 duration_ms=19.600
```

For an editor that already owns change detection, add `--stdin`. The initial
one-pass build runs immediately, each subsequent input line requests one more
pass, and `quit` exits. This is also the stable benchmark protocol. The
process retains its kpathsea helper, verified snapshot payload bytes, format
identities, and PDF asset caches, but restores a fresh engine for every
request. Consequently a failed job cannot contaminate the next request. On
glibc Linux, a watch re-execs itself once with one allocator arena; PDF and the
opt-in body-checkpoint path also use a bounded trim threshold so transient
assembly or checkpoint-compression buffers do not become permanent service
RSS. Other platforms keep their native allocator policy.

Linux also provides the experimental copy-on-write worker:

```sh
aitex fork-watch main.tex --fmt target/performance/fmt.afmt \
  --cache .aitex-cache -o main.pdf
```

It first converges and validates a project snapshot, deserializes that engine
once in a single-threaded parent, and forks one child per request. Each child
installs fresh I/O callbacks and mutates only its private COW pages; the parent
retains the pristine preamble state. Before every fork the parent re-reads the
source, recomputes the conservative key, and validates the dependency
manifest. A preamble, package, auxiliary-file, or environment change therefore
falls back to an ordinary build and refreshes the parent on the following
request. Completion uses `AITEX_FORK_WATCH ready ...`; the frame includes
child CPU, peak-RSS, and block-I/O counters so the benchmark accounts for the
process that has already exited. `aitex watch --stdin` is the portable
request-driven counterpart and is used automatically outside Linux or when the
profiling sampler requires a thread. Fork mode remains explicit rather than the
default: it removes snapshot-deserialization latency, but the parent plus COW
child have a larger counted process-tree RSS than one persistent process.

PDF-side file bytes are deduplicated by SHA-256 after a metadata-identity fast
path. Parsed TFM metrics, requested `pdftex.map` entries, encodings, Type 1
encoding vectors, and virtual-font bytes remain hot. Unwrapped/transformed
Type 1 programs are keyed by program digest, PostScript name, slant, and
extension. Normalized graphics and selected source-PDF pages are keyed by
source digest, file type, and page.

Parsed DVI page events are reused only when the exact DVI digest, page
rotations, font identities, and every implicit TFM character advance match.
This avoids reparsing no-op builds without bypassing the DVI oracle or reusing
positions after a metric change. Source bytes, prepared Type 1 programs,
parsed DVI events, and normalized assets each have a 32 MiB bound; oversized
objects are used once and not retained. A source rewrite changes its signature
and digest, so a live watch cannot return the old asset.

P3 adds two more bounded PDF layers. Resolved font resources retain shared,
immutable program, bitmap, encoding, Unicode-map, and width data in at most
eight dependency-validated font sets. Exact final PDF graphs share a 64 MiB
budget. For changed documents, eligible page content streams share a separate
32 MiB budget and are keyed by page geometry, every DVI event and special, font
object IDs and metrics, and destinations. Pages that create annotations,
outlines, named objects, graphics/XObjects, resource fragments, or other
cross-page effects are not admitted. A cache miss at any layer performs the
ordinary PDF work.

The locked-corpus class census and candidate prewarm order are in
[`CLASS-FREQUENCY.md`](CLASS-FREQUENCY.md). The opt-in, process-local P3 body
index and its stricter invalidation rules are documented in
[`CHECKPOINTS.md`](CHECKPOINTS.md); it is not a persistent cache entry. Its
64 MiB admission accounting includes compressed checkpoint chunks, retained
DVI prefixes, decoded-payload fast lanes, and one prepared tail engine charged
at its serialized size.
