# The corpus papers that nearly match, and what is left in each

Opened 2026-08-20, rewritten the same day after `packages.sh` reached 50/50.
`tools/corpus_check.sh` is at **6 of 34**; this note is about the ones just
behind it, because in each the divergence is now a *single* decision and
everything before it is identical.

Every paper written up below now matches. What they were worth is not the six:
each divergence, once reduced, turned out to be a `tex.web` rule with no
special connection to the paper that exposed it, and the tests they left behind
are in `tests/showbox/` and `tests/battery/`.

```
2608.17213v1   EXACT — 17p/27519c, the same sequence in the same order
2608.17978v1   EXACT — 6p/18169c   (was four moved hyphens; see below)
2608.17545v1   EXACT — 45p/64724c  (took six fixes; see below)
2608.17952v1   EXACT — 14p/28264c  (was \displaywidowpenalty; see below)
2608.18000v1   EXACT — 7p/15653c   (the `\scantokens` chain; see
                                    docs/history/engine-notes/scantokens-and-everyeof.md)
2608.17880v1   EXACT — 8p/20549c   (the same chain, and it was not being
                                    tracked — six fixes aimed at one paper
                                    carried a second one over with them)
2608.17818v1   EXACT — 7p/9644c    (§1076 and §1113; see below)
```

## The next three, by how close they are

`2608.15742v1` is **exact** now — 8p/18097c. It took the margin-kern primitives
and then protrusion itself; see `docs/history/engine-notes/protrusion.md`.

The other three all load microtype too, and protrusion moved every one of them
a long way without finishing any:

```
2608.17686v1   diverged 3.6% in   →  32.2%
2608.17397v1                      →  14.9%
2608.17805v1   8.9% in            →  8.9%, unmoved
```

The unmoved one is the interesting one, and two theories about it are already
dead.

It hyphenates `Grün-baum` where pdfTeX does not, which reads exactly like TeX's
rule against hyphenating across an `\accent` kern (§896 bails at an
`acc_kern`). It is not: `\showhyphens{Gr\"unbaum}` is **identical** in both
engines, so the hyphenation points agree and the difference is in which break
was *chosen*.

Nor is it protrusion in the first pass. pdfTeX does protrude there — at
`\pdfprotrudechars=2` a first pass that succeeded at 0 now fails and falls to
`@secondpass` — and ours matches it exactly at both settings, pass switch
included.

What the trace actually shows is that **pdfTeX line-breaks a paragraph we never
attempt at all**, before the first one we trace:

```
   ref    @firstpass
          @secondpass
          @\par via @@0 b=10000 p=-10000 d=*
          @@1: line 1.0- t=0 -> @@0
          @firstpass          ← from here the two agree
   ours   @firstpass          ← this is ref's *second* paragraph
```

One line, overfull, hyphenated, and it leaves no mark on the page there — the
first 1311 characters of output match.

microtype's trial paragraph was the first guess and is wrong: `\MT@get@prot`
runs only for an explicit `\leftprotrusion`/`\rightprotrusion`, which this
paper does not use, and the `\vbox`/`\vsplit`/`\lastbox` shape it uses behaves
identically in both engines when reproduced on its own.

What fits is the **running head**. The paper declares

```latex
\title[A Helly-Type Theorem]
      {…the long one…}
```

and amsart sets that short title as a one-line paragraph in a box. One line,
too long for its measure, hyphenated under protest, and contributing nothing to
page 1 — every part of the trace signature. Why pdfTeX breaks it where tracing
can see it and we do not is the open question: either we set it at a different
moment, or we do not break it as a paragraph at all.

**Tested, and it does not reproduce.** An amsart document with exactly that
`\title[short]{long}` shape and the paper's `geometry` options gives traces
that agree line for line. So the running head is not it either.

That is four readings and no fix: the accent rule, protrusion in the first
pass, microtype's trial paragraph, and the running head. Each was plausible,
each cost a tick, and the pattern is worth naming — the trace signature (*one
line, overfull, hyphenated, invisible on page 1*) is too generic to identify a
paragraph, and every attempt to guess *which* paragraph from it has failed.

The next attempt should not guess. Mark the source instead: a `\message` on its
own line before each paragraph, which is the technique
`docs/history/engine-notes/corpus-near-misses.md` already records for `2608.17978v1` and which
works because it is a measurement rather than a reading. This paper is parked
until someone does that.

## 2608.15742v1 — two fixes in, and then a wall

Six characters: an extra `−` in an arrow, and a stray `6` beside each equation
tag. The arrow was ours; the `6` is a missing primitive.

**§703/§730 — `mu` is not a length until a style says so.** We converted
`\mkern` and `\mskip` at scan time with the *text* quad; TeX converts while
walking the mlist, with `cur_mu` following `cur_style`. Here
`\fontdimen6\textfont2` is 10.95003pt and `\scriptfont2`'s is 8.50012pt, so
`\mkern9mu` came out 5.4749pt against 4.25006pt. amsmath builds every
`\xrightarrow` as `$\scriptstyle\mkern5mu{…}\mkern9mu$`, so **every
extensible arrow in the corpus was 2.4pt too wide** — and at that width one
more `\cleaders` copy of the shaft fitted than TeX had room for. That is the
extra `−`.

Deferring the conversion was necessary and not merely tidier: `$x^{a\mkern9mu
b}$` has no style node at all, and a superscript's list is scanned long before
anything decides it is a script. The inter-atom skips were already right, which
is why `$\scriptstyle x+x$` never showed this.

**§720 — simplify a trivial box.** One character followed by exactly one kern,
the kern goes. The comment sitting where this rule belonged argued it was
wrong, on the evidence that `$\scriptstyle y$` is 4.30675pt in pdfTeX — glyph
plus italic correction. The measurement was right and the conclusion was not:
§720 runs *after* `hpack`, so the box keeps the width and only the node is
dropped. Covered by `tests/showbox/math-mu-and-clean-box.tex`.

### And then: `\rightmarginkern`

The remaining `6` is not a math fault. The log says so outright:

```
! Missing number, treated as zero.
<to be read again> \rightmarginkern
```

`\rightmarginkern` is a pdfTeX primitive we do not have. microtype asks for it,
our number scan fails, and its leftovers partly typeset — so the failure is not
"protrusion kerns are missing" but "a digit lands on the page". pdfTeX puts
`\kern -1.28114` on each side of the tag; we put a `6` where the second one
belongs.

A source sweep found seventeen pdfTeX primitives with no name anywhere in the
engine, most of them this family:

```
leftmarginkern rightmarginkern pdffontexpand efcode
knbscode stbscode shbscode knbccode knaccode
pdfcopyfont letterspacefont pdffontsize
pdfeachlinedepth pdfeachlineheight pdffirstlineheight pdflastlinedepth pdfinsertht
```

The paper is blocked on those, not on anything it typesets.

### A probe that lied three times

Those sweeps were first run as
`\ifx\name\undefined\message{<MISSING name>}\fi`, and that **does not work in
this engine at INITEX without a format**: `\message` puts a space in the log and
no text, and ships a page as though the argument had been typeset. The sweep
reported "nothing missing" while seventeen pdfTeX primitives and thirty e-TeX
ones were absent, and it had already reported clean once earlier the same day.

Grepping the engine source for the primitive's name string is what tells the
truth. The `\message` form is only trustworthy once a format is loaded — which
is why every battery, which begins `\input plain`, passes.

## 2608.17978v1 — the sharpest lead in the repository

Same page count, **same character count**, and the whole difference is four
`setchar45` — four hyphens — in different places:

```
12304a12305  > setchar45
12498d12498  < setchar45
12644d12643  < setchar45
12750a12750  > setchar45
```

In words: pdfTeX sets `…(2024); values for dataset I…` and hyphenates
`avail-able`; we set `val-ues` and leave `available` whole. Two words swap which
of them gets broken.

What has been ruled out, each by measurement:

* **Not the hyphenation patterns.** `\showhyphens{values available preferred
  results indicate}` gives `val-ues avail-able pre-ferred re-sults in-di-cate`
  in both, to the character, and the reported `\hbox` dimensions and glue set
  match exactly. Both engines know the same breakpoints.
* **Not discretionary breaking in general.** A synthetic paragraph forced to
  hyphenate at `\hsize=100pt` produces identical boxes, identical glue sets and
  identical discretionary placement.

What `\tracingparagraphs=1` on the paper shows, and this is the finding:

```
             pdfTeX                                    ours
@ via @@127 b=24  p=0 d=11156          identical
@ via @@128 b=2   p=0 d=10144          identical
…                                      (every ordinary breakpoint identical)
@@157: line 10.0 t=7271952 -> @@132    identical
@\discretionary via @@129 b=69 p=50    b=73     <-- differs
@\discretionary via @@130 b=0  p=50    b=0
@\discretionary via @@131 b=1  p=50    b=1
@\discretionary via @@132 b=60 p=50    b=59     <-- differs
```

**Every ordinary breakpoint agrees; only breaks at a discretionary differ**, and
they differ in both directions — worse for a line that wants stretching, better
for one that wants shrinking. That is the signature of one line being slightly
*wider* in ours, not of a different set of candidates: badness is nonlinear in
the slack, so a single small width error moves loose and tight lines opposite
ways, and leaves a perfect fit (`b=0`) reading zero in both.

So: somewhere in this paragraph we measure a discretionary break a fraction of a
point wider than pdfTeX does, the demerits tip, and two words swap.

### Localised: the abstract, and it reduces

The paragraph is the **abstract** (`iauguide.tex:29`). It reproduces standalone —
`\documentclass{iau}`, the abstract's text, `\tracingparagraphs=1` — and the
whole 25-line trace agrees for 823 lines before the first
`@\discretionary via @@129 b=69` / `b=73`.

Finding it needed two tries. `\everypar` is no good as a paragraph marker: the
class's heading code overwrites it, and only two markers came out of a six-page
paper. Marking the *source* instead — a `\message` on its own line before each
paragraph's first line, harmless in vertical mode — gives one marker per
paragraph and lands it immediately.

Truncating the abstract by words bisects it further, provided the cut does not
land inside `$…$`: a cut that unbalances math produces a malformed document and
a divergence that is the cut's fault. With that guard, 238 words agree and 239
differ.

### Ruled out, each by measurement against pdfTeX

* **The hyphenation points.** `\showhyphens` on the words involved is identical,
  including the reported box dimensions and glue set.
* **The math.** Every inline formula in the abstract measured to the scaled
  point: `$\rm{a_*} = 0.90 \pm 0.06$` is 69.1722pt x 7.00063pt + 1.53749pt in
  both, and so are the other five.
* **Plain hyphenation in this font.** A forced break at `\hsize=90pt` in
  `ptmr7t at 10.25pt` gives identical lines, glue sets and hyphen width
  (3.41321pt, `\hyphenchar=45`).
* **Ligature words.** `identified`, `outflow`, `difficult` all hyphenate
  identically (`iden-ti-fied out-flow dif-fi-cult`) and their boxes match —
  including a forced break *inside* the ligature, which is §897's
  reconstruction doing real work.
* **Explicit hyphens.** `X-ray` produces no discretionary in either engine
  (TeX82 does not insert one), and `mass-energy` breaks identically.

### Found: §915 re-sets the fragment *with the hyphen in it*

None of the above was it, and the reduction that worked was different in kind.
Trimming the 239 words from *both* ends gives a 45-word window that still
diverges, and then sweeping `\hsize` over that window turns a *trial* break into
the *chosen* one — at which point `\showbox` prints the two discretionaries side
by side and there is nothing left to infer:

```
          tex                             ours
\discretionary replacing 1            \discretionary
.\OT1/ptm/m/n/10.25 r                 .\OT1/ptm/m/n/10.25 -
.\kern-0.20496
.\OT1/ptm/m/n/10.25 -
```

Times kerns `r` against `-`. We built the pre-break text by religaturing the
fragment and *then* pushing the hyphen on the end, so the font's lig/kern
program never saw it — every break of that kind was 0.20496pt wider than TeX's.
The test for "this step consulted the hyphen" had the same hole: a step counted
as crossing the boundary only if a ligature spanned it or a kern sat between the
two *word* letters, so a letter that kerns only against the hyphen fell through
to the branch that emits a bare-hyphen discretionary replacing nothing.

Fixed in `Engine: §915 re-sets a hyphenated fragment *with the hyphen in it*`.
A sweep of `\hsize` from 100pt to 220pt over the window is identical afterwards.

### And then it was the control space

The §915 hyphen kern was real but not the last word. The remaining
divergence reduced — with `tools/parreduce.sh`, which is the by-hand method
of this note turned into a script — to a 29-word window of heavy math, and the
`\hsize` sweep printed the answer:

```
tex:   \penalty 10000
       \glue 2.5625 plus 1.53749 minus 0.61497
ours:  \penalty 10000
```

`~` is `\nobreakspace` is `\leavevmode\nobreak\ `. The `\nobreak` landed and
the space did not, because §1030 dispatches `ex_space` in horizontal *and* math
mode and we refused math. Allowing it then put the glue outside `\mathon`,
because §729's "glue inside a formula belongs to the math list" lives in
`append` and not in `current_list` — right size, wrong place, and invisible to a
width measurement, which is why the half-fix looked complete.

**The paper is now exact.** Both bugs were needed; neither alone moved it.

### What was left on this paper

The paper still differs, and the same four hyphens still move — the fix advanced
the paragraph trace's first divergence from line 874 to **1539** without
reaching the output. The new one is a different bug and a different shape:

```
   ref                              ours
@ via @@266 b=0   p=0  d=10100      same
@ via @@267 b=77  p=0  d=7569       b=117 d=16129
@@284: line 16.1 t=1830075          @@284: line 16.0 t=1838635
```

*Ordinary* breakpoints now, not discretionaries, and the fitness class of line
16 differs (`.1` decent against `.0` tight). Line 15 above it ends hyphenated
(`@@281: line 15.0-`), so the first thing to check is §840's `break_width` —
what the *next* line starts with after a discretionary break, which is where the
`replace` count is subtracted and the post-break text added.

## 2608.17952v1 — EXACT, and it was `\displaywidowpenalty`

§890's widow penalty is an *argument* to `line_break`, and the two callers pass
different ones: §1096's `end_graf` passes `\widowpenalty`, and §1203 — the
paragraph a display interrupts — passes `\displaywidowpenalty`. Reading the
parameter from one place gives both the same number, and neither plain nor
LaTeX's own defaults show it, because they set the two alike. amsart does not:
`\widowpenalty=10000` with `\displaywidowpenalty` left at LaTeX's 50. Every AMS
paper with a display had page breakpoints we never offered — eleven in this one.

Covered by `tests/showbox/display-widow-penalty.tex`.

**Three of the four leads below were dead ends**, and are kept because each cost
a measurement to kill:

* The extra `\special{color pop}` is real but *cannot* move output — a
  `\special` has no width — and did not reproduce in isolation either.
* The `replacing 1` discretionary with no replaced node did not reproduce in
  `cmr9` at three measures; 11 discretionaries identical.
* The two `\kern 0.0` nodes and the display-math box were never examined, and
  are now moot.

Only the `p=50` breakpoints mattered. Finding *that* meant first discarding
`\interlinepenalty`, which measured 0 in both engines at all 135 paragraphs —
the obvious suspect, and wrong.

## What the four leads were

`28262c` in both and 14 pages in both. The whole difference is one block of 35
characters — a running head, `MIXED-NO…` — that we set at character 7848 and
pdfTeX sets at 7969. A page boundary falls early.

`TRACE=pages tools/logdiff.sh corpus/arxiv/2608.17952v1/paperv2.tex pages`
gives the page-builder trace of both. It agrees line for line except that
pdfTeX has **two breakpoints at `p=50` that we never offer**:

```
% t=584.27231 plus 59.12483 minus 5.19992 g=603.0 b=3 p=50 c=53
% t=23.0 g=632.0 b=10000 p=50 c=100000#
```

Neither is the *chosen* break — both engines take `c=43` — so the missing
penalty node does not move page 1; it moves something later. amsart sets
`\widowpenalty`, `\clubpenalty` and `\brokenpenalty` all to 10000, so 50 is
none of those, and where it comes from is not yet known.

`TRACE=output … box` compares the shipped boxes and finds four differences,
in order:

1. **An extra `\special{color pop}`**, immediately after the matching
   `\special{color push gray 0}` at the top of the page box. TeX's pop is at
   the *bottom*. A `\special` has no width, so this cannot move a break — but
   it is the earliest divergence and the most likely to be simple.
2. **A discretionary that replaces a node it did not emit**: in `dif-ficult`
   set in `cmr9`, TeX writes `\discretionary replacing 1 [] []` followed by the
   `ffi` ligature it replaces, and we write `\discretionary replacing 1 []`
   with no post-break and no replaced node. **Not reproduced in isolation** —
   `difficult`, `office` and `effective` forced to hyphenate in `cmr9` at three
   measures are identical, 11 discretionaries and all — so something about the
   paper's context is doing it.
3. Two `\kern 0.0` nodes we do not emit.
4. A display-math `\hbox(8.2125+2.73749)x13.9917, display` that differs.

## 2608.17952v1 — the original one-line summary

`14p/28264c` in both, failing only on where one page boundary falls: a running
head appears at character 7849 for us and later for pdfTeX. Eight hunks, all of
that one shape. Page builder, not line breaker.

## 2608.17545v1 — EXACT, and it took six fixes

The standing note said "a displayed equation landing on the wrong side of a
page break", with the two engines' page and character counts written down the
wrong way round: it was *ours* that had the extra page. Re-measuring was the
first useful act, and the second was reading the DVI as text rather than as a
list of `setchar` opcodes — `dvitext.py`, page markers and all. The divergence
then read itself off the page:

```
   pdftex   4 SU HU AND MIN-SOO KIM  various properties of ξE(s,x) have been …
            … defined via the logarithmic derivative  d/dx log Γ̃(x) = −ξE(1,x);
            which admits a Weierstrass–Hadamard product …

   ours     4 SU HU AND MIN-SOO KIM  d/dx log Γ̃(x) = −ξE(1,x);  various
            properties of ξE(s,x) have been … defined via the logarithmic
            derivative  which admits a Weierstrass–Hadamard product …
```

The display had been **hoisted to the top of the page**, with `\topskip` above
it and the rest of its own paragraph below. That was one fault of six in this
paper, and two more turned up while the tests for them were being written.
Each of the six reduced to a test:

1. **§1145 does not build the page.** `line_break` at `$$`, and *not*
   `end_graf` — so §1096's trailing `build_page` must not run. Ours did, so the
   page the display belonged to was shipped before the display box joined the
   vertical list, and the display landed at the head of the next one with its
   `\predisplaypenalty` and `\abovedisplayskip` discarded as top-of-page
   discardables. `tests/showbox/display-page-break.tex`.
2. **§752's `math_text_char`.** The rule is about italic corrections and reads
   the *next* noad: a character loses its correction only when another
   character of the same family follows. `l` and `o` of `\log` lose theirs and
   `g` keeps its 0.01389em — 0.1632pt at 12pt, on every displayed `\log` in the
   paper. Suppressing every text-font character's correction is right twice and
   wrong once.
3. **§752's ligature.** The same section runs the font's lig/kern program over
   adjacent ord noads and merges an `=:` pair into one noad. `` $``1\times2\cdots"$ ``
   was setting two grave accents where an opening quotation mark belongs.
   (2) and (3) share `tests/showbox/math-ord-ligature.tex`.
4. **§727's final binary.** A `bin_noad` becomes ordinary when nothing can
   follow it — at the end of the list, and before a relation, a closing
   delimiter or punctuation. `$--$` is the smallest case, and the previous code
   claimed the loop handled it "by seeing the corrected class", which is the
   wrong direction: §727 demotes the noad *before* the relation.
5. **§793's periodic preamble.** The lengthened column goes *after* the
   preamble's final tabskip and brings a fresh copy of its own following glue;
   §787 starts each column at `null_flag`, not zero, so a column of
   `\kern-\alignsep@` is −10pt wide; and §791 lengthens in `fin_col`, before it
   looks at the entry, so `\omit` does not exempt a column. All three are
   amsmath's `align`, whose tag column is `\omit\kern-\alignsep@` and whose
   rows therefore carry one column more than the preamble declares. Every
   `align` row came out `\minalignsep` too wide.
   `tests/showbox/align-preamble-loop.tex`.
6. **§812's `\noalign`.** A display alignment's list belongs to the *enclosing
   vertical list*, and `\noalign` material with it. Appending through the
   mode-sensitive path let §729 claim the glue for the math list, because the
   mode around a display is math — so amsmath's
   `\noalign{\penalty\interdisplaylinepenalty\vskip\jot}` between the lines of
   an `align` vanished. `tests/showbox/display-align-noalign.tex`.

A seventh came out of *writing* the test for (1): **`\vfil`, `\vfill`, `\vss`
and `\vfilneg` did not end a paragraph.** They are the `vskip` command with a
different `chr` (§208) and §1045 sends all five through `head_for_vmode`; ours
gave `\vskip` that and the four fil forms a weaker path that could not reach
inside an open box. In a `\vbox` the fill glue joined the *horizontal* list —
the line broke at it and the paragraph grew an empty second line.
`tests/showbox/vfill-ends-paragraph.tex`.

An eighth was flushed out by (6) breaking `latex_check.sh`'s `align` case, and
is the more interesting kind of bug: **two errors that had been cancelling.**
The first row of a display alignment was being given the interline parameters
in force when the display *opened*, rather than the alignment's own. The
comment defending it cited a real measurement — moving `\jot` to 20pt does not
move TeX's gap above an `align` — and drew the wrong conclusion from it. What
holds that gap still is amsmath's own `\noalign{\vskip-\jot}`, cancelling the
`\jot` its `\openup` added; and that `\noalign` was the one (6) was dropping.
Restore the `\noalign` and the `\jot` comes off twice. §774 is explicit that
the first row is spaced like any other, against the `\prevdepth` of two nest
levels up:

```text
push_nest; mode:=-vmode; prev_depth:=nest[nest_ptr-2].aux_field.sc;
```

With the override gone, both the ordinary case and `\jot=20pt` match pdfTeX.
Worth remembering as a shape: a measurement can be sound and still support the
wrong rule, when a second bug is holding the result in place.

### What is still known-wrong here, and was deliberately not chased

* **`, display`.** eTeX marks a display equation's box `dlist` and `show_box`
  prints `, display` after its dimensions. We do not, so every display in a
  `\tracingoutput` or `\showbox` comparison against `pdflatex` differs by that
  suffix. It is an eTeX extension: adding it unconditionally would break TRIP,
  whose oracle is Knuth's `tex`, so it wants a flag rather than a line.
* **§810 sets every row to the alignment's width.** TeX hpacks the *preamble*
  to the `to`/`spread` spec once and copies its `glue_set`, `glue_sign` and
  `glue_order` into each row; we hpack each row on its own. Identical whenever
  every row has every column, and visibly different when a row is short — a
  four-column alignment with a two-column row gives that row its own natural
  width instead of the alignment's.
* **`^^K` against a raw byte.** `fmtutil.cnf` builds every TeX Live format with
  `-translate-file=cp227.tcx`, which makes 0x09, 0x0a, 0x0b and 0x80–0xff
  printable. So `pdflatex` writes the `ff` ligature (char 11) into the log as a
  raw byte and we write `^^K`. `tex -ini` and `pdftex -ini` — no tcx — write
  `^^K` too, so our behaviour is right for the format we build and wrong for
  the one we compare against. `char_tag` 12–15 (`fi fl ffi ffl`) are unaffected,
  which is why only `ff` ever shows up.

## 2608.17818v1 — EXACT, and it was two rules about zero

One character apart, and the character was a hyphen: we broke a line where
pdfTeX did not. Two separate faults, and the one that was easier to see was not
the one that mattered.

**§1113 appends the italic correction whatever it comes to.**

```text
f:=font(p);
tail_append(new_kern(char_italic(f)(char_info(f)(character(p)))));
subtype(tail):=explicit;
```

We skipped it when the correction was zero. A zero one is not nothing: the
subtype is `explicit`, and §866 lets a line break at an explicit kern when glue
follows. LaTeX ends `\textbf{…}` with an italic correction and an upright bold
font's is zero, so every `\textbf{word} word` lost a breakpoint TeX had. It
shows in the paragraph trace as `@\kern via @@5` against our bare `@ via @@5`,
with every number identical — which is exactly why it was *not* the cause: a
zero-width node cannot change a badness.

**§1076 resets the space factor when a box joins a horizontal list.**

```text
else  begin if abs(mode)=hmode then space_factor:=1000
```

A box is not a character, so nothing about the letter before it should still
govern the space after it. `\showbox` on the reduced paragraph:

```
   ref    \hbox(…)x3.04167 [ ] · \glue 3.65 plus 1.825 minus 1.21666
   ours   \hbox(…)x3.04167 [ ] · \glue 4.86665 plus 5.47499 minus 0.40555
```

— a sentence space against a normal one, stretch ×3, shrink ÷3, plus
`\extraspace`. hyperref sets every citation label in an `\hbox`, so a
`\citet{…}` following a `?` or a `.` carried a space factor of 3000 straight
across the box and the space after it was 1.21665pt too wide.

Both are covered by `tests/showbox/space-factor-box.tex`.

### How it was found, which was not how it was looked for

The paragraph reduced to thirteen lines by copying the preamble and the
paragraph into a standalone file — no bisection needed, because
`tools/logdiff.sh`'s `text` mode named the sentence. Measuring the *pieces* of
the diverging line as `\hbox`es found nothing: every fragment was identical to
the scaled point. The width difference was not in any fragment but in the glue
*between* two of them, and only `\showbox` of the whole set paragraph showed
that.

Worth remembering: when every part measures the same and the whole does not,
the difference is in what joins them.

## 2608.17818v1 — the original one-character reading

`\tracingparagraphs` on both sides, with the line numbers normalised away,
leaves 43 differing hunks in 1680 lines. Two shapes:

**A break TeX calls a kern and we call glue.**

```
   ref    @\kern via @@5 b=72 p=0 d=6724      ours   @ via @@5 b=72 p=0 d=6724
   ref    @\kern via @@6 b=9  p=0 d=361       ours   @ via @@6 b=9  p=0 d=361
```

§856 prints `@\kern` when the break node is a kern and bare `@` when it is
glue. Every number matches and so does the node the break produces, so the
decision is identical — but TeX has a zero-width kern before that glue and we
do not. Harmless here; a node we are not emitting.

**A real measurement difference**, later in the same document:

```
   ref    @\discretionary via @@1 b=103 p=50 d=15269
   ours   @\discretionary via @@1 b=65  p=50 d=8125
```

`p=50` is `\hyphenpenalty`, so this is a hyphenated break, and *ours is the
better fit* — our line is less loose, which means the material up to that break
is a different width. Same shape as §915's missing hyphen kern in
`2608.17978v1`, and `tools/parreduce.sh` exists for exactly this.

**And a third thing, not yet explained.** The paragraph beginning "A
parameterized problem is a language" is numbered from line 10 by TeX and from
line 1 by us — `\prevgraf` is 9 there and 0 here. The mechanism itself is
right: `\prevgraf` across a plain `$$…$$` reads 0, 0, 6, 9, 0 in both engines.
So something in this document's structure ends the paragraph for us where TeX
carries it on. The source has `\paragraph{Parameterized Complexity.}` — a
*run-in* heading, whose `\@xsect` inserts the heading from `\everypar` and
deletes the indent box with `{\setbox\z@\lastbox}` — and a `\begin{align*}`
a few paragraphs above. Marking each paragraph with `\message{<\the\prevgraf>}`
is the way to find which one, and the note below says why `\everypar` is not.

## The rule for hand-running any of these

Clear **every** generated file — `.aux .toc .lof .lot .out .bbl .blg .log` — as
`tools/corpus_check.sh` does. Two iterations were once spent on a "-300 penalty
per line" pattern that was a stale `.toc` from a previous run, in a scratch
directory one over from the harness that already had this right.
