# FINISH.md — the plan that finished the engine

> Historical build diary: its phases and measurements describe the path to the
> compiler-complete engine and are no longer the active release queue. See
> [`ROADMAP.md`](../../ROADMAP.md) for the active post-v0.1 development plan.

Written 2026-08-19 on `fast-engine` at `cfed505`. Supersedes `HANDOFF.md`,
whose live content is folded in below. `docs/history/PLAN.md` stays the product plan and
`docs/ARCHITECTURE.md` stays the shape of the machine; this is the build order
for the one remaining question, *when is the engine done*.

---

## 0. Where we actually are

Every number here was re-measured on 2026-08-19 against a fresh
`cargo build --release`, not copied forward.

| Gate | Result |
|---|---|
| `cargo test --workspace` | 315 tests |
| `tools/battery.sh` | 858 forms |
| `tools/latex_check.sh` | 35/35 cases, and 35/35 again from a format dump and from a preamble snapshot |
| `tools/plain_check.sh` | 1241/1241 lines, 27/27 probes |
| `latex.ltx` loads | 0 undefined, `\fmtname=LaTeX2e`, **3.4 s** |
| `tools/packages.sh` | **50/50 on this machine** (3 of the 53 are not installed here) |
| `tools/trip.sh` | pass 1 dumps and halts, pass 2 finishes, **0 primitives missing**, pages **13 vs 16** |
| `tools/trip_lists.sh` | **2 of 9** `\showlists` blocks match |
| `tools/done_check.sh` | **"DONE — every gate passes"** |

`docs/ARCHITECTURE.md` claims the `latex.ltx` load costs "~160s+, ~45×". It is
3.4 s. That row, and the paragraph under it calling the cost a red herring, are
stale and should be corrected when §5's numbers are next touched.

### The three things `done_check` calls green that are not

This is the finding that orders everything below. The tool's own header says
*"done decided by my own judgement has been wrong twice already"* — and it is
wrong a third time now, not because a gate is lying but because **the gate set
stopped growing while the frontier moved.**

1. **Accented UTF-8 is silently dropped.**

   ```
   \documentclass{article}\begin{document}naïve café — œuvre\end{document}
     pdflatex:  [na] [ve caf] …   (é composed from an accent, ï from another)
     ours:      [ nave caf uvre]  (every non-ASCII character gone)
   ```

2. **Any non-UTF-8 file panics.** `crates/aitex-cli/src/main.rs:620` is
   `std::fs::read_to_string(&path).expect("read")`; a Latin-1 `.tex` gives
   `panicked at … stream did not contain valid UTF-8`. Real TeX reads it fine,
   and PLAN §9 states the engine *must never panic on arbitrary input*.

3. **`corpus/engine/` does not exist on this machine**, so done_check's whole
   "documents match real TeX" section iterates over nothing and passes. The
   corpus in `docs/history/BASELINE.md` was built on a Mac and is git-ignored. The single
   most product-relevant gate we have has been running zero cases.

Add to that: `tools/trip.sh` says outright that the `trip.log` comparison is
**not attempted**, because our transcript is a different shape from TeX's. The
canonical proof of a TeX implementation is therefore not merely unpassed, it is
currently ungradeable.

---

## 1. What "done" means, decided 2026-08-19

Four gates. The engine is finished when all four hold, and `done_check.sh` is
rewritten to be exactly these (§P1).

1. **Real-corpus parity** — N real papers land glyphs where `pdflatex
   -output-format=dvi` lands them, page for page.
2. **TRIP transcript parity** — `trip.log` and `trip.fot` diffed against
   Knuth's, which first requires writing a real `.log`.
3. **Package coverage** — `tools/packages.sh` at 53/53.
4. **PDF output** — a `.pdf` comes out, with `graphicx` and `hyperref` working
   through `\special`.

### And one architectural decision that unblocks the rest

**Character input becomes byte-faithful, as pdfTeX is.** The current state is
incoherent and cannot be argued for: `catcode.rs` is deliberately Unicode-native
("as XeTeX and LuaTeX are", line 117), the reader decodes UTF-8 into `char`, we
advertise `\pdftexversion` — so `latex.ltx` configures us as an 8-bit engine and
makes bytes 0x80–0xFF *active* — and then those active characters never fire,
because the tokenizer hands the kernel a precomposed `ï` it has no rule for.
Hence finding 1.

Byte-faithful is the right first target for one reason above all: **every
instrument we own compares against `tex` or `pdftex`.** The battery, the LaTeX
cases, `trip`, the corpus, the oracle — all of them. A Unicode-native engine
makes that oracle meaningless for every character above 127, and needs the
OpenType/`harfrust` stack (PLAN §11, seam 5, still unsigned-off) before it can
set a glyph at all. Unicode-native is a real milestone, but it arrives *with*
fonts, not before them.

---

## 2. The phases

Ordered by what unblocks what, not by size. Estimates are in commits, because
this repo runs at ~19/day and days are the wrong unit.

### P0 — rebuild the corpus  ·  starts first, runs unattended  ·  ~3 commits

Nothing in gate 1 can be measured without it, and it is I/O-bound and
rate-limited, so it should be running in the background while P1–P3 happen.

- `tools/fetch_arxiv.py` + `tools/make_arxiv_specs.py` + `tools/finalize_corpus.py`
  already exist and built the set `docs/history/BASELINE.md` describes. Re-run them here.
- Populate `corpus/engine/` — the plain-TeX documents done_check §3 wants — from
  `corpus/puretex` plus the `story.tex`/`plipsum` family, so that section stops
  passing vacuously **even before the arXiv fetch finishes**.
- Re-run `tools/bench.py` so `docs/history/BASELINE.md`'s timings are this machine's. The
  current file is macOS/14-core and this is Linux; every ratio in it is
  comparing against a machine we are not on.

**Exit:** the doc gate reports real numbers; ≥100 clean arXiv projects on disk.

**Status 2026-08-19.** Done except the denominator. 205 projects fetched;
`corpus/puretex/` is now the tracked document fixture and both its documents
pass at exact character parity. But `tools/classify_corpus.sh` finds only
**34 of 205** build to DVI under pdfTeX here, against the 105 of 192 that
`docs/history/BASELINE.md` recorded on the Mac. The gap is environmental, not ours, and it is
two causes in roughly equal measure:

- **This TeX Live is thin.** `revtex4-2.cls`, `aastex631.cls`, `MnSymbol.sty`,
  `algorithm.sty`, `bbm.sty` and friends are simply not installed. Needs a
  fuller install (`tlmgr`, or the distribution's `texlive-full`) — a machine
  change, so it wants sign-off rather than being done silently.
- **DVI mode cannot size PNG or PDF graphics.** `Cannot determine size of
  graphic` is inherent to `-output-format=dvi`, not a missing file. This part of
  the denominator only reopens with **P6**, when there is a PDF to compare.

34 is a real gate and far better than the zero that was being measured, so P7
proceeds on it. Re-run `classify_corpus.sh` after either cause is addressed.

### P1 — make `done_check.sh` honest  ·  ~2 commits

Everything else in this plan is measured by this file, and unattended work
*stops* when it prints DONE. It printed DONE today. Fix it before anything else,
or the next unattended run will do nothing.

- Add the four gates of §1. Three of them fail on day one — that is the point.
- **Fail loudly on an empty gate.** Any `for f in …; do` that iterates zero
  times must be an error, not a pass. This is the bug that hid finding 3, and it
  is a class of bug, not an instance: grep the other tools for the same shape.
- Add a Unicode/encoding gate: the `naïve café` case above, and a Latin-1 file,
  both diffed against `pdflatex -output-format=dvi`.
- Add `tools/trip_lists.sh`'s block count and `tools/trip.sh`'s page count as
  reported numbers, so regressions in them are visible without a separate run.

**Exit:** `done_check.sh` exits non-zero, and the reasons it gives are §1's four
gates.

### P2 — byte-faithful input  ·  ~6 commits

The change is far more localized than it looks. `FileState` (`fast.rs:1324`)
holds `chars: Vec<char>`, filled at exactly two sites — `fast.rs:6394` and
`fast.rs:16193`, both `source.chars().collect()`. Making a file yield one token
per **byte** (each byte as a `char` in `0..=255`) leaves the whole downstream
`char`-typed engine untouched, keeps the 256-entry catcode table exactly as it
is, and lets `latex.ltx`'s own utf8 machinery do the composition — which is
precisely what it is written to do, and what it does for pdfTeX.

1. `crates/aitex-cli/src/main.rs:620` — `read_to_string().expect()` becomes
   `read()` with no `expect`. This alone kills the panic (finding 2) and is
   worth its own commit with a malformed-input regression case, since PLAN
   §9's never-panic property is a stated invariant.
2. `FileState::chars` filled bytewise. `\catcode` of a byte then works, and
   `^^` forms and `\char` keep producing whatever they produce — §49's rule
   that a character above 127 prints as hex is already implemented (`268c283`).
3. **The tail, and the real risk: output encoding.** Every path that writes a
   character out — `\write`, `\message`, `\special`, the `.aux`, and the
   transcript of P4 — must emit the *byte*, not a UTF-8 encoding of a `char`
   whose value happens to be 233. If this is missed, `é` round-trips through
   `\jobname.aux` as mojibake and every `\label`/`\ref` with an accent breaks
   quietly. Probe it directly: `\immediate\write` a high byte, `xxd` the file,
   diff against `tex`.

**Exit:** the `naïve café — œuvre` case sets the same glyphs at the same
positions as `pdflatex`; a Latin-1 file compiles without a panic; `battery`,
`latex_check`, `typeset_check`, `plain_check` and `trip` all unchanged.

**Done 2026-08-19** (`9386d3b`, `0ff8b76`). Both gates green. It was two bugs,
not one: bytes got the characters as far as the kernel (0 → 12 of 18), and
§1030's missing `\everyjob` insertion was what stopped the kernel putting its
borrowed utf8 macros back (12 → 18 of 18, identical to pdfLaTeX). The prediction
above that the change would be localized to the input path was wrong by exactly
that second bug — worth remembering, since the reason it was invisible is that
`\everyjob` only matters when a *format* is loaded, and every gate we had either
built a format or read the kernel as input, never the two in sequence.

### P3 — `parse_body`'s missing arity, and the rest of `packages.sh`  ·  ~5 commits

`tools/packages.sh` records this as unexplained, and it is now explained. §479
says the recovery for an illegal parameter number is `back_error; cur_tok:=s` —
the `#` **stands for itself**:

```
if cur_cmd<>mac_param then
  if (cur_tok<=zero_token)or(cur_tok>t) then
    begin print_err("Illegal parameter number in definition of ");
    …  back_error; cur_tok:=s;
    end
  else cur_tok:=out_param_token-"0"+cur_chr;
```

`fast.rs:11799` does exactly that, and correctly. But
`macros.rs:560 parse_body` then re-scans the finished token list and collapses
`#`+digit into `BodyItem::Arg(n)` **without knowing the macro's arity**,
destroying the literal `#` downstream of the fix that produced it:

```
\long\def\dd#1{\gdef\inner{#1}}  \dd{P#1Q}
  tex:  \inner -> P##1Q      \the\toks0 = P##1Q
  ours: \inner -> P#1Q       \the\toks0 = PQ      <- the token vanishes
```

Note what this settles: the distinction TeX draws is **not** between an
argument-supplied `#` and a literal one, which is what `packages.sh`'s note
guessed at and what made the obvious fix (doubling every argument-supplied `#`)
take `latex_check` from 35/35 to 0/35. It is the **arity check**, nothing else.
`\newcommand` passing a body containing `#1` into `\def\foo#1{…}` keeps working
because there `t ≥ 1`; `\gdef\inner{#1}` has `t = 0`, so the `#` is literal.

1. Thread `nparams` into `parse_body`, so `#d` is `Arg(d)` only when
   `1 ≤ d ≤ n`. Keep the existing bare-catcode-6 tolerance — the
   `\char_generate:nn` case documented at `macros.rs:583` is real and rejecting
   it silently truncated an expl3 table.
2. Better: make the scanner's decision authoritative and stop re-deciding in a
   second pass. Two layers deciding the same question with different
   information is what caused this.
3. Then re-run `packages.sh`. pgf/tikz/pgfplots/todonotes are 4 of the 7
   failures and all four report the same 39 undefined `\pgfeov` — a delimited
   match that did not find its delimiter, which is exactly the failure mode a
   destroyed `#` produces in `pgfkeys.code.tex`'s
   `\long\def\pgfkeys@temp#1\pgfeov{…}`. **Verify this rather than assume it**;
   the bug was found *while* chasing pgf but was never shown to be pgf's cause.
4. Remaining three, each its own investigation: `cite` (1 undefined cs — cheap,
   do it first), `biblatex`, and `microtype`, which needs `\pdfmatch` and is
   really P6's problem (`\pdffontexpand` is what microtype *does*).

**Exit:** `packages.sh` ≥ 52/53, microtype excepted until P6.

### P4 — the transcript  ·  ~10 commits

This is the phase that turns TRIP from ungradeable into gradeable, and it pays
twice: the harness (PLAN §5, H1) parses `.log` files, and today our own engine
does not write one.

The pieces are largely built. `trace_check.sh` matches 2526 lines of `\tracing*`
output, `error_check.sh` matches 1457 errors in order, `showbox_check.sh`
matches 126 `\showbox` displays. What is missing is the *stream*:

- A real `\jobname.log` file, with TeX's banner, `**` line, file open/close
  parentheses, and closing statistics.
- **A character stream with a column, wrapped at `max_print_line`.**
  `trace_check.sh`'s header names this as the blocker: *"We have no column, only
  whole messages"*, which is why §638's bare `[0]` page echo — written against
  the current column, joining whatever line is in progress — cannot be produced
  at all. Everything else waits on this.
- `trip.fot` is the terminal transcript, a different stream from `trip.log`;
  both are diffed by the real protocol.

Then extend `tools/trip.sh` to do the comparison it currently declines to do,
and report `trip.log` matching lines as a first-class number.

**Exit:** `tools/trip.sh` reports an honest `trip.log` line-match count, and
`\jobname.log` from a LaTeX run is a file the harness's H1 parser accepts.

**Status 2026-08-20** (`86a32e3`, `fc153e0`, `352aef0`). The log is written,
§537's missing separator is fixed, and Knuth's `trip.log`/`tripin.log` are
vendored at last — `**/*.log` had been swallowing them, which is why the vendor
README documented two files that were not in the repository.

    trip.log vs this machine: 4256 of 7283 lines match (58%); ours 5554 lines
    trip.log vs Knuth, ceiling: 6713 of 7305 (91%) — what this machine's tex reaches
    trip.log vs Knuth, ours:    4011 of 7305 (54%)

**Two measurement errors were made and corrected here; both are worth keeping.**
The first reading, "41 of 7305 (0%)", was nonsense: the job is named `trip`, so
the page-count comparison's own `tex` runs overwrote our transcript before the
diff read it, and the number compared TeX Live's batchmode log against Knuth's.
Same shape as the `.aux` leak in `packages.sh` — a shared job name and a tool
reading a file someone else had since written.

The second was the reference itself. The first difference the comparison showed
was an error context truncated at `\gobble...` against our `\gobble\newcs pos`,
which made `error_line`/`half_error_line` look wrong (we use kpathsea's 79/50,
`tex.web` declares 72/42). **This machine's `tex` writes `\gobble\newcs pos`
too, at both settings.** Knuth's transcript is 592 lines from TeX Live's own,
on build parameters and thirty years of `tex.ch`; chasing it would have made the
engine *less* like TeX. Hence the three-row report, and `done_check` gates on
the local one.

**And the blocker is now identified exactly, which it was not before.** TeX keeps
**two** offsets, `term_offset` and `file_offset`, and §1279/§537 test
`(term_offset>0) or (file_offset>0)` — the *disjunction* is the whole point.
plain.tex's register allocations go out through `\wlog`, which advances the log
column and not the terminal's; so when the next `\message` arrives the log is at
column 0 while the terminal is not, and TeX writes a leading space into the log
that a single-stream model cannot produce:

```
tex:   \interfootnotelinepenalty=\count24
       ␣fonts, more fonts,
ours:  \interfootnotelinepenalty=\count24
       fonts, more fonts,
```

**Done 2026-08-20** (`f113b56`). `term_offset` tracks the terminal beside the
log, live only while the terminal's line survives. `story.tex`'s log went from
**12 differing lines to 4**, and the four are two blank lines and the `Output
written on` name and byte count. trip is unchanged at 58% — trip.tex barely uses
`\wlog`, where plain.tex and LaTeX lean on it for every allocation.

Two details had to be read rather than assumed, and both silently did nothing
when assumed: §1370's test is `j=17` and not `j<0` (§1351 maps the stream before
`write_out` sees it), and §245's `end_diagnostic(false)` closes the line on both
streams.

What is still not modelled, and has not yet been shown to matter: `\showbox`
under `\tracingonline=0` is log-only, and the error context is both. Neither
moves a gate today; do them when a diff asks for them.

**One real engine bug is already visible past it**, and it belongs to P5 rather
than P4. At `trip.log` line 484 TeX has

```
total height 1060.0 plus 458.0 plus -1.0fil plus 8.0filll minus 51.0
```

and we drop `plus -1.0fil` — a whole stretch order gone from the page totals,
which then shows up again in every `% t=…` line of `\tracingpages`. It is *not*
the obvious cause: multi-order accumulation is correct in isolation
(`\vskip 0pt plus -1fil` then `\vskip 0pt plus 8filll` gives all four components
in both engines), so the fil is being lost by something earlier in trip's state
— `\vsplit`, an insertion, or `\unvbox`. Page totals feed the page builder's
break decisions, so this is a candidate cause of 13 pages against 16.

### P5 — TRIP to parity  ·  ~30+ commits, the long tail

With P4 done this becomes gradeable and bisectable, which is why it comes after.
Current standing: 13 of 16 pages, 2 of 9 `\showlists` blocks, and (from the
previous session, not re-measured today) errors agreeing for a prefix of 52 of
182, 13 of 31 paragraph traces byte-identical.

Known frontier, carried from `HANDOFF.md`:

1. **`\the\prevgraf` reads 5 where TeX reads 7** at error 53, the first
   divergence. It is a line-breaking chain; everything below it is downstream.
2. **trip's paragraph 10** — `\vadjust{\ \closeout1BBBBBB}` at `trip.tex:201`.
   TeX hyphenates `BBBBBB` to `BB-`; we do not hyphenate at all. Four real
   pattern bugs were found chasing this (§962's `\lccode` folding, §963's
   duplicates, §961's all-zero op, §960's "too late") and **none was this one**.
   trip's patterns are spread across `\language\?` (98), `\language255` and
   `\language256`. *Next step: instrument which language each `\patterns` group
   lands in and which the paragraph asks for* — probing from outside has stopped
   paying.
3. **Exotic ligature operators.** §913's rebuild is implemented for `=:`
   ligatures and kerns, which covers every ordinary text font. `|=:`, `=:|` and
   their `>` forms (§911's op_byte cases 1,2,3,5,6,7,11) are not, and `trip.tfm`
   uses them. `crates/aitex-engine/src/hyphenate.rs`'s `relig`/`relig_spans` is
   where they go.
4. **The e-TeX primitives TRIP does not reach but LaTeX does.** Probed today:
   `\middle`, `\showtokens`, `\showgroups`, `\tracinggroups`, `\interactionmode`,
   `\lastlinefit`, `\savinghyphcodes`, `\savingvdiscards`, and the TeX--XeT set
   (`\TeXXeTstate`, `\beginL`/`\endL`/`\beginR`/`\endR`) are all undefined.
   `\middle` is the one real documents hit.

**Exit:** 16 of 16 pages; `trip_lists` 9 of 9; a stated `trip.log` match
percentage that stops moving.

### P6 — PDF output  ·  ~25 commits, a new crate

PLAN decision 3 put this in a separate driver crate and that still holds; DVI
stays the differential oracle, which is what makes P7 measurable without it.

- `crates/aitex-driver` — DVI → PDF. Type1 → CFF conversion is ours
  (`hayro-font` parses Type 1; dvipdfm-x is the reference). `subsetter` +
  `pdf-writer` for assembly. Each is a dependency needing PLAN §1 sign-off, and
  they live in the *driver* crate, so `aitex-engine`'s zero-dep line is
  untouched and `tools/check_zero_deps.py` keeps passing unchanged.
- `\special` handling for `graphicx` and `hyperref` — which is the whole reason
  decision 3 exists: it makes both packages *driver* work rather than engine
  work.
- The pdfTeX primitives real documents need, probed today as missing:
  `\pdfliteral`, `\pdfcolorstack`, `\pdfsavepos` (xcolor and hyperref want
  these), `\pdfprimitive`, `\pdfmatch` (microtype), `\pdffontexpand` +
  `\pdfprotrudechars` (microtype's actual function, and no `\special` can stand
  in for it).
- **No OTF substitution** — `cmex`/`cmsy`/`cmmi` have no OTF at all.

**Exit:** `packages.sh` 53/53; a `\usepackage{graphicx,hyperref,xcolor}`
document produces a PDF whose page count and text runs match `pdflatex`.

### P7 — real-corpus parity  ·  ~15 commits, plus whatever it finds

The gate that says the engine is a LaTeX rather than a demonstration. Needs P0's
corpus, P2 (any paper with an accent), and P3; measurable in DVI, so it does not
wait on P6.

- `tools/corpus_check.sh` — for each clean project, run both engines to DVI and
  compare page count and `setchar` positions, exactly as `latex_check.sh`
  already does for its 35 cases. Report as `N/M projects`, and *list the
  failures*, because the list is the work queue.
- Expect this to find things nothing else can: real papers use packages,
  encodings, fonts and idioms that no synthetic case does.

**Exit:** a stated fraction, and it stops moving. 100% is not the bar —
`docs/history/BASELINE.md` records that 87 of 192 arXiv projects do not compile under
`pdflatex` either. **The bar is that we match pdfTeX wherever pdfTeX succeeds.**

**Status 2026-08-20**, re-measured at the end of the second pass:

| | first run | after the `\ifx` fixes | now |
|---|---|---|---|
| projects matching pdfTeX exactly | 0 | 0 | **1** |
| projects producing nothing | 13 | 8 | **5** |
| runaway pagination | 103p vs 14p, 114p vs 37p, 60p vs 14p | 7p, 6p, 15p vs 14p | — |

**`2608.17213v1` is exact**: 17 pages, 27519 characters, the same sequence in
the same order as pdfTeX. That is the first real arXiv paper this engine
reproduces, and it is the answer to whether the remaining 33 are a missing
mechanism or a list of bugs.

`2608.17952v1` is now exact on both counts too — `14p/28264c` — and fails only
on where one page boundary falls; see `docs/history/engine-notes/corpus-near-misses.md`.

One project regressed, `2608.15840v1` (`20p/46141c` → `0p/0c`), and it is not
worth chasing: it loads microtype and hyperref, has 15 distinct undefined names,
and its earlier twenty pages were partial output rather than correctness.

### Where P3 stands (2026-08-20, second pass)

`packages.sh` reads **50 of 50 (100%)** — every package of the 53 that is
installed on this machine. The remaining three (siunitx, algorithm2e,
algorithmic) are not here to test; a fuller TeX Live would say whether they
pass. xcolor, pgf, biblatex,
parskip and kvsetkeys all load — the "empty `\csname`" family that this section
called the largest single bug left is **closed**, and it was three separate
causes, none of them delimited matching:

* §495/§510's condition stack (xcolor),
* §507 comparing an implicit character with an explicit one (pgf — see
  `docs/history/engine-notes/pgf-implicit-char-ifx.md`),
* `\unless\ifcsname` falling through a drifted copy of the conditional
  dispatch, so every kvsetkeys key lookup answered "undefined" and left its
  `\endcsname` in the stream (kvsetkeys, kvoptions, biblatex, hyperref).

**The method that closed them, and that the rest of P3 should use**, is in
`docs/history/engine-notes/hyperref-extra-fi.md`: trace the whole `\usepackage{…}` in both engines
with `\tracingmacros=2 \tracingcommands=2`, undo the 79-column wrapping, and
scan linearly for the first differing event. It took four transcript fixes
before the comparison was usable at all; it now runs 655,000 events deep and
every divergence it has reported since has been a real missing primitive
(`\quitvmode`, the pdftexcmds string primitives, `\pdffilemoddate`,
`\pdfprimitive`).

`\pdfmatch` is done — `crates/aitex-engine/src/regex.rs`, a POSIX
extended-regex engine with capture groups and no dependencies, because pdfTeX
gets its from the C library and we have none. With it, `\fontcharwd` and its
three siblings, a real `\iffontchar`, and `\lpcode`/`\rpcode`, **microtype
loads** and `packages.sh` reads **48 of 50 (96%)**. What is left is `hyperref`
(one `! Extra \fi.`, the trace bisection is at event 135,529 of it) and
`listings` (`group_depth=1`, unexamined).

### A known divergence, recorded rather than hidden

`\lpcode` and `\rpcode` are **stored and not applied**. In pdfTeX a character
with a non-zero code may hang into the margin, which moves every break in the
paragraph. We record the codes so microtype can set and read them back — which
is what it does for every character of every font — and the line breaker ignores
them. So a document that loads microtype now *loads*, and its line breaks are
pdfTeX's **without** protrusion rather than pdfTeX's with it. `packages.sh` will
call it a pass and `corpus_check.sh` will not, which is exactly the division of
labour those two tools are for. Applying protrusion is line-breaker work
(§866's `try_break` sees a different width for the first and last box of a
line) and belongs with P7, not here.

---

## 3. Dependency order

```
P0 corpus ──────────────────────────────────────────────┐
                                                         ▼
P1 honest gate ──► P2 bytes ──► P3 packages ──────────► P7 corpus parity
                                     │
                   P4 transcript ──► P5 TRIP
                                     │
                   P6 PDF ───────────┴──► packages 53/53
```

P1 first and alone: it is short, and until it is done the exit condition for
unattended work is a lie. P0 in parallel from the start because it is
rate-limited I/O. P4 before P5 because P5 is not gradeable without it. P6 is
independent of everything except its own crate and can be interleaved whenever
P5's tail goes quiet.

---

## 4. Not in scope, and why

- **Unicode-native input, OpenType, complex-script shaping.** Deferred with
  §1's decision. It arrives as one milestone *with* the font stack, not before
  it, and `harfrust` needs PLAN §1 sign-off when it does.
- **The macro-body JIT** (`ARCHITECTURE.md` §4.5). The `latex.ltx` load is 3.4 s,
  not the 160 s that motivated it. The premise has evaporated; re-measure before
  reviving it.
- **Speed work beyond what exists.** The format dump and preamble snapshot are
  both implemented and both gated. `ARCHITECTURE.md` §6's remaining ~2× is real
  but is not what "finished" means, and it is behind a passing suite by design.
- **`cargo fmt`.** ~175 diffs, advisory by design (see the comment in
  `.github/workflows/ci.yml`). Do not mass-reformat without asking.
- **The harness** (PLAN §5). A separate product. P4 is the one place this plan
  touches it, by giving it a real `.log` to parse.

---

## 5. Method — what has been working, keep doing it

Carried from `HANDOFF.md`, which this file replaces.

1. Reduce to a **minimal probe** and diff against real `tex -ini`. Never
   diagnose by reading our own code.
2. **Read the section in `vendor/knuth/tex.web` before believing the
   diagnosis.** `vendor/knuth/README.md` has the three-line recipe for splitting
   it into its 1380 sections. This is not optional: two different tie-breaking
   heuristics were fitted to line-breaker probes before §835/§845/§855 settled
   it in minutes, and §578 was got wrong in a commit because the probe used a
   non-last font and could not distinguish the two readings. **When a probe and
   a reading disagree, read the section.** P3 above is a fourth instance —
   `packages.sh`'s note guessed at a distinction TeX does not draw, and §479
   says plainly what it does.
3. Verify every case **fails without the change**: `git stash push -q crates/`,
   rebuild, run the one case, pop.
4. Run all gates, then commit quoting the section.

Three shapes worth expecting, all recurring:

- **TeX complains and proceeds; we proceeded.** Perhaps fifteen of the last
  session's fixes were this. The recovery is usually load-bearing — §503's
  `back_error` re-reads the offending token as the *second operand*, so a silent
  `=` also lost the operand and answered on a number the document never wrote.
  §479 in P3 is the same shape: we emit the error correctly and then discard the
  recovery.
- **The report's position is part of the answer.** §960, §783/§784, §476, §479
  and §346 all needed the check moved to where the token is *read*, because
  `error_check` compares the context lines.
- **plain.tex's values standing in for INITEX's.** `\hyphenpenalty` 50,
  `\lefthyphenmin` 2, `\righthyphenmin` 3 were all wrong; §240 leaves every
  integer parameter INITEX does not name at nought. Worth grepping for more.

### Gates to run before every commit

```
cargo build --workspace --all-targets     <- CI-blocking, run it
cargo test --workspace
tools/typeset_check.sh    tools/showbox_check.sh    tools/trace_check.sh
tools/error_check.sh      tools/battery.sh          tools/warn_check.sh
tools/latex_check.sh      tools/plain_check.sh      tools/packages.sh
BISECT=1 tools/trip.sh
```

**Do not gate on `cargo test --release | grep -c FAILED`.** That reads 0 when
the test target fails to *compile*, which it had been doing for weeks before
`35c2d98`. `cargo build --workspace --all-targets` is what catches it, and CI
runs it first.

`tools/trip.sh`'s format carries an engine fingerprint, so **pass 1 must be
re-run after every rebuild** — forgetting gives an empty output that looks like
a crash.

### Mistakes made, so they are not repeated

- A new test case was written straight over an existing git-tracked one
  (`tests/warn/paragraph-mode-line.tex`), losing its twelve warnings. Restored
  in `32e1d2b`. **Check whether a path exists before `cat >`.**
- Two commit messages asserted a cause that had not been checked. State what was
  measured.
- A gate that iterates over an empty set passes. Finding 3 above. Make emptiness
  an error.

---

## 6. Open notes in `docs/history/engine-notes/`

- `accent-do-assignments.md` — `\accent` calls `do_assignments` (§1123), so
  `\setbox` between it and its character is §1241's error. Needs `\accent` to
  read its character through the dispatcher, like `display_align_pending`.
  **Belongs to P2**: `\accent` is how every 8-bit accented character is built,
  so this lands in the same work.
- `first-pass-par-badness.md` — trip's paragraph 8; superseded in part but the
  probe is still good. **P5.**
- `extra-page-breakpoint.md`, `page-so-far-during-output.md`,
  `display-alignment-close.md`, `ligature-nodes.md`, `extra-right-runaway.md` —
  older, unverified against the vendored `tex.web`. Treat their claims as
  claims, not findings.
