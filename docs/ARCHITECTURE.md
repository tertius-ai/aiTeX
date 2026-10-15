# aiTeX Architecture

> **v0.2.0 note:** class/project/body snapshot code described below is
> dormant in the release path. Exact completed-output caching remains active.
> Prefix reuse requires an engine-observed semantic `\document` checkpoint;
> literal source splitting is not a correctness proof in TeX.

*How the compiler is built, why it is built that way, and where "insane fast"
actually lives.*

This document is the north star: the shape of the machine and the reasoning
behind it. `docs/history/PLAN.md` preserves the original build decisions, while
`docs/RELEASE.md` records the current qualification boundary. Read §1–§3 to
understand what exists and §4–§6 for the performance roadmap beyond the first
compiler-complete release.

---

## 0. The thesis in one paragraph

TeX is not a compiler in the LLVM sense (parse → IR → optimize → codegen). It is
a **Turing-complete macro interpreter fused to a typesetter**, and the fusion is
the difficulty: category codes mutate mid-stream, macro expansion can redefine
the tokenizer, and the output routine is itself a macro that runs in the middle
of page building. A faithful interpreter is therefore irreducibly sequential.
The leverage for a fast edit loop is on a different axis: **never do the same
work twice.** aiTeX already dumps formats and snapshots a LaTeX engine at
`\begin{document}`; its persistent host now also retains safe body checkpoints,
proves exact dependency boundaries and semantic reconvergence, and reuses
eligible PDF work. Nodes as real Rust types and I/O behind traits are what make
that layer possible.

The release metric is correctness, not an old timing copied between machines.
Against the locked Ubuntu 24.04 / TeX Live 2023 reference, aiTeX matches all 34
qualified arXiv projects (770 pages and 1,506,678 glyphs), all 16 TRIP pages and
the normalized TRIP semantics, and the complete package/PDF fixture set. See
`docs/RELEASE.md` for the measured matrix. Performance numbers belong in a
dated benchmark artifact with the exact compiler and machine recorded.

---

## 1. The pipeline (what exists today)

TeX's stages, as Knuth named them, mapped to our crates and modules:

```
                    ┌─────────────────────────────────────────────┐
   file / string    │  World  (trait)   — the only I/O boundary    │
        │           │  no std::fs / net in the engine; wasm-clean  │
        ▼           └─────────────────────────────────────────────┘
   ┌──────────────┐   chars, under a *live* catcode table the document mutates
   │  mouth       │   token.rs + `Fast`'s input stack  ── cannot pre-tokenize
   │  gullet      │   macros, conditionals, \the, \csname, \numexpr…
   │  stomach     │   builds boxes/glue/nodes, tracks mode, assigns meanings
   └──────┬───────┘   fast.rs — *one* struct, *one* `main_loop`
          │ Node (real Rust enums — NOT Knuth's flat mem[] array)   node.rs
   ┌──────▼───────────┐
   │ Typesetter       │  fastset.rs — modes, box stack, \halign (align.rs),
   │ Line breaker     │  math (math.rs), hyphenation (hyphenate.rs)
   │ Page builder     │  linebreak.rs, page.rs + \output, a TeX *macro* that
   └──────┬───────────┘  runs mid-build and can \unvbox255 material back
          │
   ┌──────▼─────────────┐   shipout.rs → canonical aitex-page events
   │ Output seam        │      ├─ direct in-process PDF
   └──────┬─────────────┘      └─ specialized exact DVI emission/oracle
          │
   aitex-driver + aitex-fonts → classic and OpenType PDF resources
          ▲
   aitex-unicode (host): ICU4X + unicode-bidi + fontdb → HarfRust
```

The three stages are **not three types**. TeX's own `get_x_token` is the seam
between mouth and gullet and it is called from inside the stomach's main loop;
modelling them as separate objects meant two token buffers, two expansion
mechanisms and an eqtb neither owned (§8 measures the cost). `fast.rs` is one struct whose `main_loop` *is* `main_control`, which
is why the token pump can be flat arrays instead of a chase through owners.

Crate map:

- **`aitex-core`** — source spans, the diagnostic model, error taxonomy, and
  lightweight parsing for semantic-tool transcripts. The shared vocabulary
  lets the scheduler and harness describe a failure identically.
- **`aitex-engine`** — the interpreter, typesetter, page builder, TFM support,
  DVI writer, and dump/restore state. It is `no_std` + `alloc`; only `serde` and
  `postcard` are allowed as external direct dependencies. Its first-party
  `aitex-page` dependency is itself `no_std` and dependency-free. This is
  enforced by `tools/check_zero_deps.py`.
- **`aitex-page`** — final page geometry and events: classic glyphs, already
  shaped glyph runs, rules, specials, and logical Unicode clusters. It contains
  no parsing, shaping, filesystem, or PDF policy.
- **`aitex-driver`** — a small DVI reader and PDF writer. Its normal PDF path
  consumes `aitex-page` events directly; the independent DVI reader maps its
  bytes to the same contract for differential and compatibility tests. It
  embeds Type 1 fonts and materializes the `graphicx`, `xcolor`, and `hyperref`
  specials exercised by the release gate, and uses zlib-rs for deterministic
  adaptive level-6 stream compression. Independent page streams are compressed in
  bounded batches. A process-wide FIFO permit pool grants each batch its whole
  requested worker budget atomically, so concurrent document compiles do not
  accidentally fragment the default four workers among themselves. Callers
  can assign a smaller per-document budget without changing PDF or cache
  identity. Exact font resources are interned independently of DVI point size,
  and page dictionaries expose only resources that page uses. A separate
  positioned PDF 1.7 layer consumes shaped glyph runs and embeds static
  TrueType and CFF faces as Type 0/CID fonts with semantic text maps.
- **`aitex-fonts`** — the bytes-in/bytes-out font subsystem shared by classic
  and Unicode output. It owns aiTeX's font-processing face, instance, source
  glyph, subset glyph, cluster, PDF code, and CID identities and adapts shaped
  output into the stable page vocabulary; it uses Fontations for
  OpenType parsing and metadata, HarfRust for shaping, and Skera behind a
  replaceable subsetter interface; and enforces OpenType embedding permissions.
  Filesystem access remains at the host boundary and system-font discovery is
  delegated to `aitex-unicode`; the heavier dependency graph remains outside
  `aitex-engine`. See `docs/FONTS.md`.
- **`aitex-unicode`** — the host-only itemization and fallback service. ICU4X
  supplies script properties and UAX #14 boundaries, `unicode-bidi` supplies
  bidi runs, and `fontdb` indexes ambient or project font directories. aiTeX
  owns candidate ordering and proves coverage by shaping with missing-glyph
  rejection. The service returns compact aiTeX `Node::NativeRun` values and
  retains the corresponding font bytes for the PDF resolver.
- **`aitex-cli`** — the `aitex` binary and the host boundary: filesystem and
  kpathsea lookup, timing, format/snapshot management, semantic-tool
  scheduling, and PDF asset loading. Its default `ai` feature adds read-only
  explanation and validation-gated, bounded multi-attempt repair front doors; a
  no-default-features build omits them.
- **`aitex-harness`** — deterministic repair rules, bounded model context,
  versioned session/event records, strict patch safety, isolated complete-build
  workspaces, and provider abstractions. It depends on `aitex-core`, not on
  engine internals, and is absent from the no-AI CLI dependency graph.

### Key state structures

- **`Fast`** (`fast.rs`) — the engine. TeX's eqtb is not one table here but a
  set of parallel `Vec`s indexed by `Symbol` (`meaning`, `eq_level`,
  `long_flag`, …) plus maps for the sparse registers, with one save stack for
  grouping. Macro bodies live in a single `arena: Vec<u32>` and arguments in
  `params`/`pranges`, so expanding a macro pushes a frame rather than copying a
  token list.
- The **input stack** (`cur: Frame` + `input: Vec<Frame>`, in `fast.rs`) —
  files and pushed token lists, read one token at a time so a catcode change
  takes effect on the very next token.
- **`Typeset`** (`fastset.rs`) — modes, the open-box stack, the vertical list
  and the page builder's contributions.
- **`Node`** (`node.rs`) — hlist/vlist elements as a real `enum`, owned and
  diffable. The single most important decision for the speed roadmap.

---

## 2. Why the obvious speedups are illegal

Three properties of TeX rule out entire categories of optimization. Internalize
these; most "why don't you just…" ideas die on one of them.

1. **Catcodes mutate mid-stream.** `\catcode` is a runtime assignment, and a
   macro can change how the *next* character tokenizes. → No static grammar, no
   parser generator, no batch pre-tokenization. The tokenizer must consult live
   state per character. (This is decision #9 in `history/PLAN.md`.)

2. **Macro expansion is Turing-complete and input-dependent.** You cannot
   statically know what a macro will expand to, how deep, or whether it
   terminates. expl3 is 100M+ expansions of exactly this. → No ahead-of-time
   whole-program analysis; no speculative expansion without a fallback.

3. **`\output` is a macro that runs mid-page-build and feeds back.** The stages
   are not a clean one-way pipeline: expansion is driven from inside the main
   loop rather than ahead of it, the page
   builder invokes `\output` which re-enters the main loop, and material flows
   *backward* into the contribution list. → No simple stage parallelism; the
   main loop is irreducibly sequential (Knuth designed it that way).

Corollary: **rayon-over-document-chunks is unsound.** Any "parallelize the
typesetting" scheme that ignores (3) will silently corrupt state.

---

## 3. What we already got right (and why it matters downstream)

Two decisions from `history/PLAN.md` §1 are load-bearing for everything in §4–§6:

- **Nodes as real Rust types, not `mem[]`.** rtex proved this passes TRIP. But
  the *reason it matters for speed* is that you can **own, clone, hash, and diff**
  a node subtree. You cannot meaningfully snapshot or incrementally invalidate a
  flat 3000-word memory array mid-run; you can trivially snapshot a `Vec<Node>`.
  Incremental compilation (§4.2) is *impossible* on `mem[]` and *natural* on
  real types. This is why §8 stops short of copying `mem[]`
  even where it would be faster.

- **Everything behind traits** (`World`, line breaker, fonts). The `World` seam
  is what keeps the engine wasm-clean and — crucially — makes execution
  **deterministic and mockable**: no wall-clock, no ambient fs, RNG seeded
  explicitly. Determinism is the precondition for caching: a cached result is
  only valid if the same inputs always produce the same output.

The performance work done so far has been removing accidental costs, not
changing the model:

- Killed the O(n²) traps: the file tokenizer re-collected the whole remaining
  file per token; the token levels and the gullet's scan buffer shifted on
  every read; the line breaker looked back over every prior breakpoint even
  once the line was overfull. All now cursor-based, borrowed, or bounded.
- Collapsed mouth/gullet/stomach into one struct (`fast.rs`), which is what let
  macro bodies live in one arena and expansion become a pushed frame rather
  than a copied token list. §8 is the measurement that motivated it; the layered engine it describes was deleted once the fast
  one passed every harness.

The 2026-09-01 release review found a more specific remaining cold profile.
`get_x_token`/expandable dispatch and conditional skipping dominate the long
PGF/expl3 papers, and an open alignment formerly sent enormous ordinary token
runs through its out-of-line boundary scanner. Keeping those tokens on the
compact path plus reusing macro-scanner storage reduced the worst paper by
about 17% and cut instrumented allocations from roughly 48.6 million to 19.2
million. The final v0.2 DVI aggregate is nevertheless still 1.283× stock, so the
next full-pass gain must come from token/meaning representation or a guarded
macro execution tier rather than another isolated allocator tweak.

---

## 4. Where "insane fast" lives: the not-repeating-work layer

This is the roadmap. Ordered by leverage. The mental model is **Salsa /
rust-analyzer's query engine + ccache + precompiled headers**, applied to a TeX
run. None of it is "make the interpreter faster"; all of it is "don't run the
interpreter again on work whose answer we already have."

### 4.1 Preamble snapshot / fork-at-`\begin{document}` — the ~60% win

The preamble (documentclass, packages, macro and catcode setup) is most of the
edit-loop latency and *almost never changes* between edits. Because engine state
is real Rust structs, we can **serialize the entire engine — the meaning and
`eq_level` vectors, the macro arena, the catcode and code tables, the register
maps, the font table — at `\begin{document}`** and, on every recompile,
**restore from that snapshot** instead of re-running the preamble.

This is a precompiled header for arbitrary engine state. It also dissolves the
format-build problem: snapshot *past* `latex.ltx` once, restore in milliseconds
forever after. Invalidation key = hash of (preamble source + package files +
engine version).

Requires: full-engine serialization (see §5.1 — same machinery as the debug
checkpoint), and a clean `\begin{document}` boundary.

### 4.2 Incremental body re-typesetting — the deep win

TeX is sequential, but **the engine state entering paragraph *N* is a pure
function of the source prefix before it.** So:

1. Checkpoint engine state at paragraph (or section) boundaries during a run.
2. On an edit, find the nearest checkpoint *before* the first changed byte.
3. Restore it and re-run only from there; stop early once the output re-converges
   with the previous run (state hash matches and downstream source is unchanged).

Most edits touch one paragraph → you redo one paragraph, not the document. This
is exactly a Salsa query graph: inputs are source spans, the memoized query is
"typeset from checkpoint C," and an edit invalidates only the dirty suffix.

The subtlety is *convergence detection*: cross-references, counters, and
`\vsize`-dependent pagination can make a late edit ripple. Be **conservative** —
only declare "converged, stop" when the engine state hash and all downstream
inputs provably match the prior run; otherwise fall through to a full pass.
TeXpresso demonstrates the approach is achievable in practice.

### 4.3 Content-addressed caching of everything

Packages, fonts, and sub-results keyed by content hash. Same package hash → never
re-execute it. Same font → parse and convert once, ever, across all documents.
This is also the wasm on-demand-fetch story and the shared-cache-across-projects
story. A cache lookup is a hash and a memcpy; a package load is millions of
commands.

### 4.4 Native document fixed points

Bare `aitex main.tex` (and its explicit `aitex build main.tex` alias) owns a
bounded fixed-point loop. It compares only TeX-written
files that can affect a following pass, fingerprints bibliography and index
inputs by content, and invokes the relevant semantic processor only after its
inputs or outputs change. `latexmk` is not the build engine. A future safe
optimization may predict that a following pass cannot observe a changed
auxiliary file, but it must remain conservative in the same sense as §4.2.

### 4.5 Macro-body JIT — the genuinely LLVM-flavored idea (cold-path win)

Every macro expansion today re-walks its body token list. A macro called millions
of times (all of expl3) could be **compiled once** to a Rust closure or a small
bytecode that emits its expansion directly, skipping token-by-token
interpretation and most of the allocation. This is a tracing JIT for TeX's macro
language.

It is the *one* lever that attacks the format-build/l3unicode pathology **without**
caching — turning that ~45× back toward the 2.4× baseline. It is also the most
speculative and highest-effort item here (you are writing a compiler for a
Turing-complete, self-modifying macro language whose semantics depend on live
catcodes). Sequence it *after* the caching layer, which delivers more for less.

### 4.6 Sound parallelism only

Off the main loop, per §2(3): font loading + Type1→CFF conversion, image decode,
independent documents, and possibly speculative re-typesetting of provably
independent floats. Never the main typesetting loop.

---

## 5. The bridge: one serialization mechanism, three payoffs

Formats, §4.1, and §4.2 all need the same thing: **serialize and restore the
full engine state at a safe main-loop boundary.** The serialization layer now
pays for formats, class and project-preamble snapshots, and live body
checkpoints.

### 5.1 Format and preamble snapshot/resume — implemented

**Status (2026-08): implemented on `Fast`.** The engine is one serde value.
`Fast::dump` writes a magic header, a source-derived schema fingerprint, and a
postcard body; `Fast::undump` validates and restores it, then the CLI reattaches
the host callbacks deliberately excluded from serialization. The same path
powers preloaded LaTeX formats and `--cache` snapshots taken through
`\begin{document}`. `tools/latex_check.sh` differentially checks both restore
paths, and the release gate exercises 45/45 format and snapshot cases.

The same persistent cache can publish a converged one-shot DVI/PDF as a
checksummed payload. This is an artifact proof rather than an engine snapshot:
the complete source and invocation identity select the entry, while transitive
inputs, driver/font resources, auxiliary files, custom write streams, and the
transcript remain independently validated manifest dependencies. Only the
primary output may be absent or replaced on a hit. Any other mismatch executes
the ordinary compiler, so the shortcut cannot become a correctness dependency.

The old layered engine had a separate `checkpoint.rs` over `Eqtb` + `Gullet` +
`Stomach` + `Input`; it was removed with those types and remains only in Git
history at `d85395c^`. The product capability was rebuilt directly on `Fast`,
where one owned state graph is both simpler and safer to fingerprint.

**Status (2026-08-31): the conservative body path is implemented and P3-qualified.**
`Fast::run_until_checkpoint` and `continue_until_checkpoint` pause after a
completed outer-file paragraph only when every nested/scanner/output invariant
is idle. `Fast::checkpoint` writes a live-job envelope, and
`Fast::resume_checkpoint` accepts only an exactly unchanged source prefix;
`resume_prepared_checkpoint` applies the same provenance check to one live tail
engine prepared between requests. The opt-in watch host keeps a bounded,
content-deduplicated index with distributed, section/include, and shipped-page
boundaries. Per-boundary dependency sets map a changed include subtree to the
last state before it was read. Whitespace/comment edits may stop only after the
complete engine-state and DVI-prefix identities reconverge and all downstream
inputs and artifacts remain exact. The PDF host reuses complete graphs,
immutable font resources, and strictly eligible page streams under separate
content identities. Continuous-vs-resumed engine tests, hostile invalidation
tests, 53/53 eligible locked-corpus roots, and exact DVI/PDF clean-rebuild
comparisons are permanent gates. The precise payload and refusal rules are in
[`CHECKPOINTS.md`](CHECKPOINTS.md).

The constraints learned from the earlier prototype still apply:

- **The boundary has a precise scope (learned 2026-07).** A checkpoint is sound
  only at the true between-top-level-commands boundary — the same place
  `tex.web` allows `\dump`. The output routine and math scripts re-enter the
  main loop *recursively*, with mode/group/prev-depth on the **Rust** call
  stack, which is not in any dump. Pausing inside one captures a half-run job:
  checkpointing inside `\@@end`'s output routine dumped with the job already
  finished and the routine's tokens lost, and resume reported a bogus
  `UnclosedGroup`. The fix is *not* to serialize the Rust stack — `tex.web`
  forbids `\dump` there for the same reason — but to refuse to pause below the
  outer level.
- **The `World` is not serialized.** On restore a fresh one is attached: files
  reopen by name, the RNG reseeds from the saved seed. Determinism (§3) is what
  makes resume ≡ continuous run.

### 5.2 The serialization mechanism: serde + postcard

Resolved 2026-07 in favour of **`serde` + `postcard`**, as unconditional engine
dependencies (the only two). The reasoning:

- serde + postcard are pure Rust, `no_std`+`alloc`, and **wasm-clean — verified
  by building `aitex-engine` for `wasm32-unknown-unknown`** — and permissively
  licensed. They meet every criterion the dep policy puts on an exception.
- Serialization is **core product machinery**, not a niche add-on: the preamble
  snapshot (§4.1) and incremental re-typesetting (§4.2) *are* this dump/restore.
  So the code would run in every real build. A feature gate on an always-on
  capability is friction, not optionality, so it was dropped.
- Manual `dump`/`undump` (what `tex.web`'s `\dump` does) was the zero-dep
  alternative. It was rejected: it is a large hand-written surface to keep in
  sync with every state type, exactly the bug-prone duplication serde's derive
  eliminates. The one benefit it offered — enumerating the state explicitly —
  we got anyway, by deriving across the graph and skipping the deliberate
  exclusions (below).

`tools/check_zero_deps.py` still enforces the surface: `serde` and `postcard`
are the *only* permitted direct runtime deps; anything else fails CI.

**What is deliberately not serialized**, and why it is safe:

| State | Handling | Why |
|---|---|---|
| `World` (I/O trait object) | `#[serde(skip)]` → re-attached fresh | files reopen by name; determinism makes resume ≡ continuous run |
| line breaker (trait object) | skip → default `KnuthPlass` | a strategy, not state |
| trace log | skip → empty | transient debug output |
| conditional state | **must be serialized** | a conditional whose `\fi` is past the dump point leaves `cond_depth` non-zero; dropping it made resume miscount `\fi`s |
| Rust call stack of a nested main loop | never dumped — pausing there must be *refused* | the output routine and math scripts re-enter the main loop with frame state on the Rust stack; only the outer boundary is sound (as `tex.web` restricts `\dump`) |

The state graph derives serde across its owned fields. Host callbacks, transient
diagnostics, strategies, and derived indexes use explicit skip/rebuild logic;
the schema fingerprint deliberately changes when any serialized source layout
changes.

---

## 6. The remaining interpreter micro-opts (the ~2× that is left)

### Where the gap actually is (measured 2026-08)

The same 4000-paragraph plain-TeX document through both engines, callgrind,
instructions:

| | ours | `tex -ini` | ratio |
|---|---|---|---|
| full run | 826,533,354 | 500,206,532 | **1.65×** |
| the same characters *tokenized only* (into `\toks0`, never typeset) | 213,962,262 | 192,756,324 | **1.11×** |

The mouth and gullet are at parity — they were rebuilt to §8's design, and the
`latex.ltx` load, which is almost pure expansion, runs at 1.21×. The gap was
*all* in the node path, which had never been rebuilt: at the start of this
work the same run was 1,328 M (2.44×), and typesetting alone was **3.27×**.

And it is **instruction count, not memory**. Cache-simulated:

| | ours | `tex -ini` |
|---|---|---|
| D1 miss rate | 1.3% | 0.5% |
| **last-level data misses** | **49,563** | **219,579** |

We touch more data and miss L1 more often, but we miss the *last* level — the
one that costs a hundred cycles — **4.4× less** than TeX, because a contiguous
`Vec` streams where a `mem[]` chase scatters. That is §8's bet, and it is
paying. It also means shrinking `Node` is not the lever it looks like.

### What that cost, and what fixed it

The gap was **ownership**: work TeX does not do at all, because its nodes are
cells in a preallocated array. It creates one with a free-list pop, destroys it
with a push, and moves a list by repointing a link. Ours were being copied and
destroyed, and callgrind's *caller* side found four places doing it — none of
them the one that looked obvious:

1. **`close_box` packed every `\vbox` twice.** The natural height came from
   `vpack(b.list.clone(), …)`: a deep copy of the whole list, packed, read for
   one scalar, dropped. `vpack`'s measuring loop only borrowed, so it is now
   `measure_vertical(&[Node]) -> VTotals` — and the measurement happens only
   when it is *used*, which for a box with neither `to` nor `spread` is never.
2. **`PageBuilder::step` cloned every node it examined**, and cloning a
   `Node::Box` copies the box's entire list recursively. It takes the head out
   with a `mem::replace` now, leaving a sentinel so `is_breakpoint`'s index
   arithmetic still holds, and puts it back on the two paths that do not
   consume it.
3. **The line breaker copied the paragraph into its lines.** They partition it,
   so `break_in_place` *drains* the buffer: nodes arrive by value, the buffer
   keeps its capacity for the next paragraph, and a discretionary's pre/post
   lists are moved out of the node rather than cloned. Cuts are planned on a
   borrow first, so a pass that falls back leaves the paragraph intact for the
   hyphenating pass.
4. **`\spaceskip` was fetched by interning its name on every space**, and
   `named_val`/`named_glue` were `BTreeMap`s — `par_params` walked a tree
   twenty-five times per paragraph. Both are symbol-indexed `Vec`s now, the
   same shape as `meaning` and `eq_level`.

Two more, not about ownership:

5. **The line breaker got TeX's active list** (§829/§835). It walked *every*
   earlier breakpoint; TeX keeps only the breaks a line may still begin at and
   *deletes* one the moment it cannot reach the break under consideration —
   sound because `e` only moves right and an earlier start only lengthens the
   line. O(b · active) with `active` one to five entries, instead of O(b²):
   `break_with` 293 M → 98 M.
6. **`CharMetrics` was `dyn`**, so measuring a character could not inline
   through the virtual call and three bounds checks — on a path walked four
   times per character. Nothing used `dyn LineBreaker` and there are two real
   impls, so it is `impl CharMetrics` now.

Ownership is 8.2% of the run now and the allocator 3.2%; the profile is flat —
`break_in_place` 12.8%, `main_loop` 10.3%, `typeset_char` 9.7%,
`measure_horizontal` 6.6%, nothing else above 6%.

### What is left

- **Building character nodes** — `typeset_char` + `append_char` + `lig_kern`
  is 145 M, 17.6%, about 240 instructions per character. TeX's is a tight
  ligature/kern state machine over `char_info`.
- **`break_in_place` at 12.8%.** No longer superlinear against TeX: the ratio
  *improves* with paragraph length (2.12× at one paragraph-length, 1.73× at
  sixteen), so what is left is constant factor.
- **A 16-byte `Node`** — 32 today, via interning glue specs (`tex.web` §150
  shares them already). A 24-byte experiment measured *slower* and was
  reverted, and the cache numbers above say this is not where the time is.

Not on the list, and not going to be: copying TeX's `mem[]`. §3 and §8 refuse
it, and the last-level miss counts are the reason it would not even be faster.

### Profiling recipe

There is no working `perf` on the dev machine.
`valgrind --tool=callgrind --cache-sim=no --branch-sim=no` gives deterministic
instruction counts. Build with `--profile profiling` (release optimisation,
`lto = false` so symbols survive). Do **not** wrap the binary in `env` —
callgrind writes an empty file if you do; export the variables instead.
`AITEX_PROFILE=1` reports time by phase and allocation volume from inside the
engine, `AITEX_MEM=1` reports where the memory is, and
`tools/bench_typeset.sh` (with `PLAIN=1` for the `tex -ini` comparison) reports
milliseconds and bytes *per page*.

A hand-written global allocator would attack the malloc cost at once but a
memory-safety bug there is catastrophic; not worth it in a codebase meant to be
trustworthy.

---

## 7. How the pieces sequence

```
  v0.1 ─────────────────────────────────────────────────────────► product
   │
   ├─ (5.2) serde + postcard state dump  ← shipped and release-qualified
   ├─ (4.1) preamble snapshot            ← shipped; document-specific today
   ├─ shared class snapshots             ← shipped and P2-qualified
   ├─ converged-output artifact cache     ← shipped, exact, and bounded
   ├─ (4.3) content-addressed asset cache ← shipped and bounded
   ├─ (4.2) incremental re-typesetting  ← P3-qualified body/DVI/PDF reuse
   ├─ (4.4) .aux fixpoint prediction     ← conservative allow-list shipped
   │
   ├─ (6) interpreter micro-opts        ← in parallel, as the ~2× baseline lift
   └─ (4.5) macro-body JIT              ← last; crushes the cold path if needed
```

The single most important idea remains: **formats, class/project snapshots, and
body checkpoints share one serialization mechanism, while completed artifacts
share the same dependency-proof machinery.** The first two proved the engine
mechanism; body checkpoints now prove exact unchanged-prefix resume at safe
paragraph boundaries, including a host-owned DVI prefix. P3 and the persistent
artifact layer add the stricter proofs needed to stop after semantic
reconvergence or skip execution entirely without making either optimization a
correctness dependency.

---

## 8. Appendix: how the fast core was arrived at (2026-07)

`fast.rs` did not start as the design; it replaced a layered
`Eqtb`/`Gullet`/`Stomach`/`Input` engine that was 3.6× slower than TeX. Two
design documents argued for the replacement and were folded in here when the
layered engine was deleted (`d85395c`). What is kept is the evidence and the
decisions still in force, not the staged plans, which were carried out.

### The measurement that condemned the layered engine

Symbolized pdftex vs aiTeX, identical `latex.ltx` load, self-time by bucket:

| bucket | TeX | us | ratio | what it is |
|---|---|---|---|---|
| READ | 1.42s | 2.41s | **1.7×** | fetching the next token |
| EXPAND | 0.72s | 3.62s | 5.0× | macro expansion machinery |
| COPY | 0.12s | 1.26s | 10.4× | `memmove`/`memcpy` of token data |
| SCAN | 0.10s | 0.99s | 10.2× | `scan_int`/`scan_dimen`/`scan_keyword` |
| ALLOC | 0 | 1.88s | ∞ | heap alloc/free (TeX does none) |
| TOTAL | 3.21s | 11.60s | **3.6×** | |

READ was only 1.7× off — the per-token machinery was fine. The blow-up was in
work TeX *never does*: copying token data, and allocating.

### TeX's five invariants (the checklist `fast.rs` is built against)

1. **One pre-allocated arena.** Alloc is an O(1) free-list pop (§120); when it
   empties, bump a frontier inside the owned array — never malloc, never
   realloc (§116–130).
2. **Tokens are single integers** — `cmd·256+chr` or `cs_token_flag+p` (§289).
3. **One unified input stack** for files *and* token lists (§300–307): macro
   bodies, `\the`, backed-up tokens, marks and `\output` all read through the
   same `get_next`.
4. **In-place reading, zero copy** — advancing is `loc := link(loc)` (§357);
   expansion pushes a *pointer* level (§323); `macro_call` captures each
   argument once and references the body through `param_stack` (§389–390), so a
   zero-argument macro allocates nothing.
5. **One token pump** — `get_x_token = get_next + expand` drives the whole main
   loop (§1030, §380).

TeX's 16-bit-word limits (`halfword`, the `qi/qo` macros) are 1970s-machine
artifacts and **not** load-bearing: all five survive with 32-bit indices.

### The two conclusions that are still decisions

**The abstraction tax is distributed, so it had to be collapsed all at once.**
Every localized optimization tried before the rewrite — unsafe frame reader,
`cur_input` hoist, packed token, `Stream` removal — measured neutral or worse,
because peeling one layer leaves the others wrapping every token. A prototype
of the *whole* macro-call cycle (`fastcore.rs::cinrust_match_bench`) was the
first thing to transfer in-tree: 2.81 → 1.03 ns/token, 2.71×, precisely because
it removed argument matching, body reading and frame management together.

**Contiguous beats linked, so we do not copy `mem[]`.** Measured on the same
cycle: a linked `mem[]` chase was 2.60 ns/token, a contiguous layout 1.23. The
chase is a dependent-load chain the CPU cannot prefetch. This is the one place
we deliberately diverge from invariant #1's *representation* while keeping its
*property* (no malloc on the hot path) — and it is also what keeps §4 and §5
possible, since you cannot snapshot or incrementally invalidate a flat word
array. See §3 and §6.

### What that produced

Macro bodies live in one `arena: Vec<u32>`; arguments in a contiguous `params`
with `(start, len)` ranges; the input stack is a `Vec<Frame>` read by raw index;
expansion pushes a frame instead of materialising a token list. The tight inner
loop opts into `unsafe` (workspace lint is `deny`, not `forbid`) with a
documented invariant per site, established immediately before the index that
relies on it.

The result is ~2.4× `tex -ini`, from 3.6×.
