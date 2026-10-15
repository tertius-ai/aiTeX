# TRIP completion map

Initially re-measured 2026-08-26 on `fast-engine` at `bf193ab`, and updated as
each convergence batch lands. This supersedes the standing numbers and the
proposed next step in `docs/history/engine-notes/trip-frontier.md`; that note remains useful as
history, but its 13-page/2-block baseline and claim that the first eight list
blocks agree are no longer current.

## Baseline

| Surface | Ours | Local TeX reference | Meaning |
|---|---:|---:|---|
| INITEX transcript (`tripin.log`) | 456/456 semantic lines exact (100%); 456/464 raw | 425/456 semantic lines against Knuth (93%) | The complete semantic transcript is exact; the eight omitted raw lines are TeX-array allocation inventories with no aiTeX counterpart |
| Typesetting transcript (`trip.log`) | 6495/7283 matching lines (89%); 7071 lines produced | 6713/7305 against Knuth (91%) | The complete source and terminal output loop are reached; remaining differences are transcript presentation and recovery details rather than list structure |
| `\showlists` blocks | 9/9 exact | 9/9 exact | Every complete page/contribution-list state exposed by TRIP now matches TeX verbatim |
| Shipped pages | 16 | 16 | Page-count parity is restored; semantic DVI comparison is now meaningful |
| Semantic DVI records | 155/220 matching (70%); 216 records produced | 220 records across 16 pages | Exact units, magnification, extents, page counters, font identity/scale, and positioned glyph/rule/special events; opcode, stack, font-number, and comment differences are normalized |
| Error battery | 2084/2084 | exact | Ordinary error recovery, including text-accent recovery in math, incompatible glue units, paragraph-scoped error limits, and frozen-magnification repair, remains a strong invariant |
| Box-display battery | 324/324 | exact | Ordinary shaping, page-glue normalization, conditional alignment preambles, inert alignment-template expansion, math-list penalty placement, `\nonscript`, text and math accents, math-mode indentation, and diagnostics under arbitrary `\escapechar` values are exact |
| Trace battery | 3750/3750 lines exact; part of `done_check` | exact | Transcript lifecycle, recovery order, and semantic dump regressions are completion-gate failures |

The actionable reference is the `tex` installed on the same machine. Knuth's
files remain the canonical ceiling, but banners, error-context widths, and the
fixed-array memory statistics make byte-for-byte equality with a modern TeX
Live build impossible without inventing values. Those environmental lines need
an explicit normalizer, not fake compatibility output.

## The dependency graph

```text
honest artifact gates
├── INITEX semantics ✓ ────┬── language/pattern state ──┐
│                         └── ligature/kern programs ───┼── paragraph parity
├── §1026 output re-entry ─── block 3 / earliest pass-2 divergence
├── display-entry timing ──── block 5 / showlists-math
└── alignment row recovery ✓ ─ force_eof transfer ✓ ─ reach line 442 ✓
                                                      └── tail tokenization ✓ ── page 16 ✓ / block 9 ✓
                                                        │
paragraph parity + complete source reach ── math-list nodes ── block 6 ✓
                                                        │
                                  block 8 page contents ── transcript polish
                                                               │
                                                           DVI + trip.fot
```

This ordering matters. Working backward from the missing pages would mix a
recoverable alignment scanner defect with page building; treating block 6 as a
single math bug would mix upstream paragraph counts with several independent
math-list node classes.

## Work lanes, in order

### 1. Make the measurement complete

`tools/trip.sh` now preserves and scores the INITEX transcript before pass 2
overwrites it and compares the semantic contents of both 16-page DVI files.
The remaining terminal blind spot should be closed before declaring TRIP
complete:

1. Capture aiTeX's and TeX's terminal streams separately and compare
   `trip.fot`. aiTeX currently writes a compact CLI summary to stderr, not the
   terminal-selector transcript required by `docs/history/FINISH.md`.
2. The semantic DVI comparison is now non-vacuous and part of `done_check`.
   It compares units and magnification, declared extents, page counters, fonts
   by TFM name and actual loaded size, and every visible glyph, rule, and
   special at exact DVI-unit coordinates. It deliberately ignores byte
   offsets, movement-register choices, stack encoding, numeric font ids,
   comments, and non-positive invisible rules. A self-test proves that
   equivalent encodings pass and a shifted rule/special fails. The current full
   measurement is 155/220 records (70%), with 216 produced. The DVI now carries
   TRIP's frozen `magnification=2000`, exact postamble extents, and the offsets
   active at each page's shipout, and horizontal rules use their lower edge as
   DVI's reference point. TeX character codes in `\special` payloads are also
   serialized as single bytes rather than UTF-8, and cumulative set-glue
   movement obeys TeX's exact billion-sp `vet_glue` bound. The first difference
   is the vertical coordinate of page 10's first glyph.
3. `tools/trace_check.sh` is now a non-vacuous completion gate. Closing §1145's
   display-entry timing made its last red case exact before the gate was added.
4. The INITEX equality gate now retains the format identity, font inventory,
   exception count, and closing status exactly while normalizing only TeX's
   eight string-pool, `mem[]`, hash, flattened-font, and packed-trie inventory
   lines. Apply the same documented discipline to any later environmental
   fields rather than widening this normalizer.

**Exit:** pass 1, pass 2, terminal output, list blocks, and DVI semantics are
all named artifacts with separate scores, and no empty or omitted artifact can
pass.

### 2. Converge the INITEX pass — complete

Pass 1 has no page-builder downstream noise and exposes language and
font-program behavior needed again in pass 2. Its 457-line log including the
banner is now exact against all 456 representation-independent local-TeX lines.
The raw local reference has eight additional allocator/string/font-word/trie
inventory lines, which the gate reports separately as 456/464 rather than
inventing values for data structures aiTeX does not have.

TRIP exercises ligature op-byte cases 1, 2, 3, 5, 6, 7, and 11. The main
character builder now executes all of them, including left/right boundary
programs and `\noboundary`; the raw node sequence through that section is
exact. `crates/aitex-engine/src/hyphenate.rs` now carries TeX's bounded §902
cursor through cut prefixes and synchronizes the pre-break, post-break, and
replacement branches. §903's zero-letter generated ligatures and post-break
left boundary are now covered directly with TRIP's own font. The remaining
TRIP reductions are the first pattern-driven `11` break in the complete input
state and the later `q` case whose minor branch ripples across several prefixes.

The 2026-08-26 language-whatsit batch implemented §1034/§1377, including
automatic and explicit nodes, nested-list state restoration, and per-node
language/minimum changes during hyphenation. It moved pass 1 from 150 to 155
matching lines while leaving pass 2 at 4397: the new nodes themselves agree,
but their downstream line-breaking benefit is masked by the immediately
following ligature-program divergence. Focused differential coverage is exact
for all seven pattern/language displays and both `\setlanguage` displays.

The 2026-08-26 TFM-program batch implemented §545's complete ligature operator,
implicit boundary programs, `\noboundary`, boundary-hit provenance, and the
same shaper for normal text and hyphenation. Its purpose-built PL font matches
TeX in all four displays, the full 267-display and 1504-error batteries remain
exact, pass 1 moved from 155 to 210 matching lines, and pass 2 moved from 4397
to 4461. This closes ordinary shaping; it does not close §902 reconstitution.

The following §902 batch replaced the local span heuristic with the cut-prefix
cursor and branch synchronization from §§902–918. All eight operator cases now
match under forced hyphenation (12 displays in the purpose-built font), the
ordinary reconstitution reduction remains exact, and the full box/error
batteries are 275/275 and 1504/1504. Pass 1 moved from 210 to 227 lines and pass
2 from 4461 to 4467. The smaller-than-expected TRIP gain isolates the next work
to `init_list`/left-boundary context rather than another operator rule.

The next §903 batch made generated ligatures with empty `lig_ptr` continue a
word without contributing letters, moved the implicit left boundary ahead of
the post-branch synchronization test, and enforced §918's 127-node replacement
limit. A differential using `vendor/trip/trip.pl` now matches the seven-node
`11` discretionary and boundary-marked `q` post-list exactly; the full display
battery grows to 279. Pass 1 moves from 227 to 246 matching lines and its noisy
pathological branch shrinks by 42 lines, while pass 2 stays at 4467.

The following diagnostic batch restored the input-stack side effect of
§416's font-identifier scan: TeX backs up and re-reads the operand before
returning a font token, so `\showthe\font` and `\showthe` of a font control
sequence retain a spent `<recently read>` level. The error/context battery now
pins both forms and the family-selector case that consumes the level while
scanning its index. Pass 1 moves from 246 to 248 matching lines and pass 2 from
4467 to 4469.

The next nest-display batch made §218 expose every semantic level already
implicit in an active alignment: a `\valign` entry paragraph, its vertical
span, the row with its initial `\tabskip`, the alignment level, and its
enclosing box. Paragraph headings now use the line, language, and hyphen minima
frozen when the paragraph began, while the trailing current-language field
tracks later language nodes. The dedicated 38-line `\valign` trace and the
expanded ordinary-paragraph trace are exact, and the existing 279-display and
1514-error batteries remain exact. Pass 1 moves from 248 to 257 matching lines,
with its first 150 local-reference lines now consecutive-exact; pass 2 is
unchanged at 4469 because this initialization-only display is skipped by the
loaded format.

The following pre-hyphenation batch corrected §896's punctuation search for
ligatures. TeX classifies a ligature from the first character in `lig_ptr`;
TRIP's boundary-built `7A` displays as A with a nonzero lccode but begins with
7, whose lccode is zero, so TeX steps over it, applies the following language
whatsit, and offers `11` to language 98. aiTeX already made that distinction
when collecting a word but accidentally tested the displayed glyph when
deciding whether to keep searching after the preceding glue. A third
TRIP-font differential now pins the composition, the complete display battery
grows to 280 exact displays, and pass 1 jumps from 257 to 365 matching lines
(55% to 78%) while its log grows from 409 to 461 lines against TeX's 464. The
first 167 reference lines are consecutive-exact; the next mismatch is badness
12 versus 10000 at an already-present discretionary. Pass 2 is unchanged at
4469 because the paragraph belongs only to format initialization.

The next break-width batch implemented the unusual second half of §840: when
a discretionary has nonempty post-break text, TeX does not prune discardable
nodes following its replacement text. The following glue comes after the word
reconstructed at the head of the new line, so its natural width and stretch
belong to that line. aiTeX dropped it from the search and assigned badness
10000 where TeX assigns 12. The trace harness now accepts repository PL font
fixtures, and its new 26-line TRIP-font active-node differential is exact. Pass
1 moves from 365 to 397 matching lines (78% to 85%), its produced log contracts
from 461 to 443 lines, and the consecutive-exact prefix reaches line 209. The
next mismatch is now the visible `q-q` post-break branch itself; pass 2 remains
4469.

The following §903 batch now preserves `ha`, the character or ligature node
immediately before a hyphenated word. Its displayed character becomes
`hu[0]`, while its original character list and left-boundary subtype seed the
first cut prefix; provenance outside `hu` is carried through retained-operand
ligatures in every major and pre-break branch. A five-display TRIP-font
reduction pins the resulting 10-node `q-q` replacement exactly. The same
paragraph exposed a separate §840 detail: only explicit kerns are discarded
after a break, so the normal -1pt font kern following the preceding glue must
remain in the next line's active width. A 27-line trace pins that arithmetic.
Pass 1 moves from 397 to 446 matching lines (85% to 96%), and all 446 semantic
reference lines are now consecutive-exact; only the 18-line format-dump and
closing summary remains. Pass 2 is unchanged at 4469.

The format-report batch closes §1335's open input echoes before dumping, emits
§1327's exact format identity and font inventory (including a control byte
equal to `\newlinechar`), and reports §935's semantic exception count. Its
exception scanner now accepts literal, `\chardef`, implicit `\let`, and
`\char` forms, folds them through `\lccode`, and ignores one-letter entries;
TRIP's two spellings consequently replace one dictionary entry as they do in
TeX. A page-less run emits the exact closing status without creating a
spurious zero-page DVI. Pass 1 reaches 456/456 after the eight narrowly named
representation lines are removed, and the corrected exception state moves
pass 2 from 4469 to 4488 matching lines.

**Exit achieved:** the semantic aiTeX INITEX transcript matches all 456 local-
TeX lines, the raw comparison remains visible at 456/464, and the dumped format
reloads for pass 2.

### 3. Remove the pass-2 structural blockers

Work in transcript order, with one reduction for each failure.

1. **Output re-entry (§1026, list block 3) — complete.** Held insertions live
   in the new page list until re-entry, but aiTeX counted them as an offset into
   the old contribution list. It consequently spliced output-routine material
   after the first old contribution instead of directly after the held nodes.
   Re-entry now prepends routine material to old contributions and then
   prepends the held page, giving the exact order “held insertions, routine
   material, old contributions.” A direct three-node invariant and TRIP block
   3 both pin the order. The block moves from failing to exact and pass 2 from
   4488 to 4501 matching lines.
2. **Alignment and scanner recovery (§336/§339/§342/§392/§476, source lines
   331–375) — complete.** TeX's
   `align_state=-1000000`
   makes `\cr` and `\crcr` preamble delimiters only at the outer brace level.
   aiTeX ignored that level and ended the outer preamble at the `\crcr` inside
   `\halign to 0pt{\t2\dp3\A\crcr}`. Applying §783's outer-level test and
   pinning it with a 24-line differential restores page 15 and prevents the
   enclosing alignment from being destroyed before its first row. The new
   recovery path is structurally later but shares fewer lines with the old
   transcript, so the simple LCS moves from 4514 to 4464; source reach, page
   count, and exact list blocks are the deciding measures here. The following
   batch closes the runtime sequence too: §342 consumes `\span` in `get_next`
   just like `&` and `\cr`, so a `\span` read by `\expandafter` inserts the
   current v-template before expansion. An 18-line differential pins TeX's
   old-`\A`, redefine-`\A`, template-end order; the spurious number, unit, and
   misplaced-`\omit` errors disappear and pass 2 moves from 4464 to 4479
   matching lines. The scanner-status batch then implements §336/§339's
   `aligning` and `skipping` states: an outer token discovered by any nested
   preamble reader produces the exact runaway diagnostic, inserts the frozen
   primitive `\cr` and right brace, resets `align_state`, and consumes the
   conditional's inserted `\fi` before returning to the preamble. The exact
   33-line reduction covers both runaway preambles in TRIP's incomplete
   `\ifcase` sequence.

   The same batch shares §476 parameter-text recovery between `\def` and
   `\edef`, including wrong-number insertion, the nine-parameter limit, `#{`,
   and alternate catcode-6 marker characters. `\protected`, `\long`, and
   `\outer` are now properties of a meaning: definitions clear them, grouping
   restores them, `\let`/`\futurelet` copy them, and `\meaning` prints them.
   Definition scanning now detects an outer token before `\edef` expands it and
   reports the full parameter-text prefix. The exact 58-line reduction pins
   these paths. Pass 2 moves from 4479 to 4588 matching lines and resumes the
   source through line 365.

   The following matching/math batch closes that dependency. Macro matching now
   propagates an explicit abort through leading literals, undelimited arguments,
   braced arguments, and delimited arguments; completed argument ranges are
   traced and then reclaimed, while the replacement text is never entered.
   §392's compulsory-token mismatch and §336's inserted-`\par` recovery are exact,
   including the second mismatch diagnostic when an outer name occurs where a
   literal was required. Paragraph rejection compares the literal `\par` token,
   not aliases of its meaning, as §396 requires. The same work exposed a latent
   `\meaning` bug: macro prefixes had a hard-coded backslash instead of the
   current `\escapechar`, so LaTeX's slash-delimited hook parser only happened
   to work while leading parameter literals were ignored. The 88-line
   differential covers all of these shapes, including the LaTeX convention,
   and the real-corpus gate remains at its 32/34 established baseline.

   Rule leaders are exempt from the ordinary `mmode+hrule` missing-dollar path
   because §1078 scans the rule inside the leader command. A bad `\mskip` or
   `\nonscript` follower rejects the leader and is reread in the same formula;
   conversely every `vskip` variant, including `\vfill`, closes malformed math.
   The 56-line differential pins both sides. A bounded pass now finishes in
   about 12,000 steps rather than looping at line 407, and pass 2 moves from
   4588 to 5177 matching lines.

   The following input-stream batch closes the line-413 fault. TeX scans
   `\openin100` with `scan_four_bit_int`, diagnoses it, changes the stream to
   zero, and therefore makes the following `\read0` consume `tripos.tex`.
   `\openin`, `\closein`, and expandable `\ifeof` now share that restricted
   scanner and its zero recovery; `\read` deliberately retains §1370's
   unrestricted `scan_int`, mapping values outside 0..15 to terminal input.
   A 14-line error differential pins all three restricted operations. Pass 2
   moves from 5177 to 5210 matching lines and the line-414 loop reads all of
   `tripos.tex` as intended.

   The following alignment-group batch closes that fault. §1094 now invokes
   `off_save` when a paragraph reaches a horizontal alignment entry with a
   negative brace count. §1127 makes a right brace at the entry group back up
   and insert the frozen primitive `\cr`; §1126 then inserts the balancing `{`
   needed to reread that boundary at the entry's outer level. The same frozen
   v-template marker carries TeX's internal `\outer endtemplate:` meaning
   through `\futurelet`, and undefined expandable commands receive their
   level-two command trace before being diagnosed. An 81-line differential
   pins the complete `}`, `\cr`, `{` recovery, the surrounding-group restore,
   and the distinct misplaced-tab versus misplaced-`\cr`/`\span` help paths.

   `\stopinput` consequently survives line 419 and performs its intended
   redefinition while `tripos.tex` is read at line 424. Pass 2 rises from 5210
   to 5278 matching lines and from 14 to 15 pages. The job still does not reach
   line 425: ending the paragraph read from `tripos.tex` makes aiTeX fire the
   old output routine immediately, and that routine terminates the job. TeX
   keeps the current page live and proceeds through line 442. Diagnose this as
   a page-state divergence, not another input or alignment fault. Because the
   transcript still has an earlier independent defect at line 186; repair it
   and remeasure the page totals before reducing the line-424 fire in isolation.
3. **Display-entry page timing (list block 5) — complete.** `line_break(true)`
   correctly avoided `build_page`, but aiTeX omitted the separate call at the
   end of §1145 `init_math`, after the display group, dimensions, and
   `\everydisplay` are installed. The interrupted paragraph's lines now reach
   the current page before formula tokens run. The independent 37-line trace
   and TRIP block 5 are exact, the trace suite becomes wholly green and joins
   `done_check`, and pass 2 moves from 4501 to 4514 matching lines.

The following maximum-dimension batch closes the line-151 diagnostic. The
scanner already saturated magnitudes above `max_dimen` correctly, but reported
the `Dimension too large` headline under `MissingNumber`, which selected the
wrong help and made a correct value look like a different recovery. A distinct
§460 error kind now carries TeX's 19-foot help. Its 19-line differential pins
positive, negative, physical-unit, and glue-component saturation, including an
exact-limit value that raises no error. Pass 2 moves from 5278 to 5282 matching
lines and the error battery grows from 1528 to 1547 exact extracted lines.

The following alignment-auxiliary batch closes the line-186 defect. A
`\valign`'s `\noalign` body now inherits and updates the alignment's
restricted-horizontal `\spacefactor`, completed vertical-alignment rows and
horizontal `\vrule`s reset it to 1000, and the alignment copies its final
value back to the interrupted paragraph. A 50-line differential pins all four
transitions and the resulting `\xspaceskip`/scaled ordinary glue pair. Pass 2
moves from 5282 to 5286 matching lines; the earliest remaining semantic
divergence in this section is now the shortened `BBBBBB` material at source
lines 201--202.

The following delayed-EOF batch closes the line-424 source-reach defect.
§§367 and 1030 make `\endinput` set a global `force_eof` request rather than
truncate the file beneath the command. If an `\input` opens a new file before
the buffered line ends, that new file inherits the request: its eagerly read
first line runs and §362 aborts its second line. A 33-line differential pins
the transfer, the skipped second line, and resumption of the outer file. Pass
2 moves from 5401 to 5630 matching lines, reaches line 442, and produces all
nine list blocks. The corrected path ships 14 pages rather than the old
premature-EOF path's spurious fifteenth page; repeated terminal `\end`
handling and TeX's dead-cycle output sequence are now the page-count frontier.

The following incompatible-unbox batch closes the first divergence after
source line 424. §1107 checks both the current mode and the box register's list
kind before changing the register; a failed voiding form reports the error and
leaves the box intact. An 81-line differential pins wrong-kind unboxing in
horizontal and vertical modes, rejection in math mode, and register
preservation. Pass 2 moves from 5630 to 5641 matching lines, and the line-425
repair reduces block 8's differing lines from 361 to 350 without changing the
14-page or 6/9-exact-list-block totals. The earliest remaining semantic
divergence is now the catcode and control-sequence tokenization sequence at
source lines 428--429.

The following tail-tokenization batch closes those two lines. §352 repeatedly
contracts double-superscript notation at the same input position, so with `q`
at catcode 7, `qq1qM` becomes `qqM` and then carriage return. §354 makes an
escape-category `\endlinechar` at the physical line limit yield the null
control sequence instead of consuming the first character of the next line;
§373 reports the inserted `\endcsname` when a control sequence interrupts a
constructed name; and §1049 rejects a parameter token in vertical mode. A
50-line differential pins the complete sequence and its `^^M` rendering. Pass
2 moves from 5641 to 5688 matching lines, reaches 16/16 pages, makes block 9
exact, and raises the list-block score from 6/9 to 7/9; block 8 has 346
differing lines. The remaining structural blocks are now the independent math
list in block 6 and downstream page contents in block 8.

**Exit achieved:** list blocks 3, 5, 7, and 9 are exact; pass 2 reaches source
line 442; all 16 pages and all nine list blocks are present. Exact content of
block 6 is intentionally deferred to the math-list lane.

### 4. Finish paragraph shaping

Use the pass-1 reductions instead of debugging the long pass-2 paragraph as a
single unit. The active-search trace and chosen boxes through source line 212
are now exact.

The following branch-synchronization batch closes the line-201/202 symptom.
When a post-break cut prefix overtook the major cursor, aiTeX advanced the
major branch once and stopped even when that move overtook the post-break
cursor in turn. §916 alternates both translations until their input positions
are equal. A three-display differential using TRIP's own `B+B -> A` ligature
program pins the missing final `B` in one-break, adjacent-break, and
normal-kern-tail forms. Pass 2 moves from 5286 to 5295 matching lines; the
earliest remaining paragraph defect is the line-195 `C-A` branch above.

The following ligature-word-boundary batch closes the line-195 symptom. §896
uses the first source character of a ligature while looking for a word, but
§897 requires every source character to be a letter once collection has
started and rejects the whole ligature before committing a partial prefix.
aiTeX admitted TRIP's final `C/` ligature to the pattern word, so the trailing
slash suppressed the fourth break. A differential using TRIP's font and exact
four-break pattern now pins all four discretionaries. Pass 2 moves from 5295
to 5329 matching lines; the complete lines-195--204 paragraph trace and chosen
boxes are exact, and the infinite-shrink paragraph at lines 207--210 is next.

The following finite-shrink batch closes the glue symptom. §§826 and 867 keep
the shrink amount but reset its infinity order before background totals,
paragraph tracing, or line packing can observe it, and emit only one error per
paragraph. aiTeX now does this for both background skips and for ordinary or
leader glue in the paragraph list. Two box displays and an error-sequence
reduction pin the normalized specs and diagnostic cardinality. Pass 2 moves
from 5329 to 5362 matching lines; all five line boxes at lines 206--210 are
exact, leaving only their inherited line numbers and active-node search.

The following inherited-line batch closes the numbering symptom. §816 starts
the active list at `prev_graf + 1`, and §848 uses those absolute numbers for
`\parshape` and hanging measures; §1149 likewise measures a display at
`prev_graf + 2`, not at the typesetter's lifetime line total. A 73-line
differential separates those two counters and pins the resumed trace at line
4. Pass 2 moves from 5362 to 5379 matching lines. That exposes the replacement
run above as the next mismatch: TeX steps over all `replace_count` nodes after
a discretionary, while aiTeX's generic breakpoint scan still visits them.

The following replacement-scan batch closes that mismatch. §867 adds a
discretionary's replacement nodes to `active_width` and advances `cur_p` past
all of them: they contribute to the unbroken width but are never independent
breakpoints, and the next node sees the discretionary as its logical
predecessor. A 15-line differential pins the explicit-kern and following-glue
cases. Pass 2 moves from 5379 to 5384 matching lines and exposes the omitted
inline-math width above as the first remaining active-search difference.

The following math-surround batch closes the active-search section. §§192 and
867 represent both inline-formula boundaries as math nodes whose widths enter
`active_width` like kerns. Packing already included those widths, but both
breakers omitted them from their prefix totals. A 15-line differential pins
the closing-boundary breakpoint asymmetry: the opening `\mathsurround` is in
the candidate width while the closing boundary itself is not. Pass 2 moves
from 5384 to 5401 matching lines, and every active node and selected line box
through source line 212 now agrees with TeX.

**Exit:** the paragraph/hyphenation transcript section (source lines 165–212,
currently 877/924 lines after environmental line-wrapping differences) is
semantically exact and `\prevgraf` entering the hairy display agrees with TeX.

### 5. Split the math-list work into node classes

Only after paragraph counts and source reach are stable, compare block 6 from
its first differing node. Known independent defects include:

- active math character `z` remains literal instead of expanding to the
  delimiter macro installed at source lines 241–242;
- `\lastskip` in math returns `0.0pt` instead of the last mu glue
  (`9.0mu minus 1.0fil`);
- `\mkern`, `\vadjust`, `\insert`, glue, rule, accent, and discretionary nodes
  acquire extra `\mathord` wrappers; and
- delimiter, accent, fraction, family, and formula-dimension construction
  differ farther downstream.

Add one small differential display per node class. The 259/259 box battery is
evidence that ordinary forms are sound, not that these TRIP compositions
should be repaired together.

The first fraction batch implements §1184's special `fin_mlist(p)` splice for
an incomplete fraction closed by `\right`: the left and right noads now
surround the completed fraction instead of remaining inside its numerator and
denominator. It also applies §720's post-packing trivial-box simplification to
an already-unwrapped sub-mlist, removing a visible italic-correction kern while
retaining its width. A two-display differential covers both the malformed
second generalized fraction in TRIP's `\left...\right` group and the 0.5pt
correction in TRIP's own font. Pass 2 moves from 5688 to 5720 matching lines,
the box battery grows to 290 exact displays, and block 6 contracts from 38 to
14 differing lines. Its first remaining difference is now the line break at a
raw 0.27777mu glue followed by `\medmuskip`, independent of the corrected
fraction subtree.

The following §761 batch makes automatic binary and relation penalties inspect
the node immediately following the atom, not merely the next atom class. An
explicit penalty now suppresses the automatic one, while intervening raw glue
makes a later relation non-adjacent and retains the binary penalty. A four-
display differential pins both directions. Removing TRIP's spurious second
`-3333` breakpoint makes block 6 exact, raises the list score from 7/9 to 8/9,
and moves pass 2 from 5720 to 5740 matching lines. Block 8 is now the sole
structural list failure.

The next §730 batch preserves a `\nonscript` marker while suppressing the one
glue or kern node immediately following it in script and scriptscript styles.
The adjacency is resolved before raw nodes are flattened, so a style node or
atom is a barrier and adjacent raw noads are not. Three new displays cover mu
glue, mu kern, and an explicit point kern, while the existing text-style case
continues to retain its glue. The full nine-display reduction is exact, pass 2
moves from 5975 to 5980 matching lines, and block 8 contracts from 70 to 65
differing lines. Its first remaining semantic node is an extra `|ip ^^c8`
character between the affected paragraph and the following empty formula.

The following §1123 batch rejects a missing text accent before scanning the
character beneath it. The absent glyph still emits `char_warning`, but it no
longer consumes the following token or contributes a zero-width character to
the paragraph. A differential trace pins the warning and token order, while a
box display pins the one surviving ordinary character. Pass 2 moves from 5980
to 5981 matching lines, its produced transcript grows from 6549 to 6555 as the
unconsumed input follows TeX's recovery path, and block 8 contracts from 65 to
64 differing lines. Its first remaining difference is now the recovered empty
formula, where TeX contributes two zero-width boxes and aiTeX contributes none.

The next §1092 batch distinguishes `indent_in_hmode` from `new_graf`. In an
existing horizontal paragraph `\indent` now appends its null box without
resetting paragraph bookkeeping; in math mode it becomes an ordinary noad
whose nucleus is the `\parindent`-wide null box. A four-block differential
shows the live noad and its packed formula context. Pass 2 moves from 5981 to
5982 matching lines, its produced transcript from 6555 to 6556, and block 8
contracts from 64 to 63 differing lines as the first of TeX's two recovered
zero-width formula boxes appears. The remaining box is the following
`\accent`-in-math recovery, which TeX converts to `\mathaccent`.

The following §1165 batch implements that conversion. A text `\accent` found
in math mode now emits TeX's dedicated diagnostic and is then scanned exactly
as `\mathaccent`, including the 15-bit accent field and the ordinary nucleus
recovery. A six-line error/context differential pins the diagnostic before the
number scan, and a four-block list/box differential separates the math accent
noad from text-mode accent kerns. Pass 2 moves from 5982 to 5987 matching lines
and from 6556 to 6558 produced lines; block 8 contracts from 63 to 62 differing
lines as its second zero-width recovered formula box appears. The new first
mismatch is the shrink order of the glue immediately after that paragraph.

The next §1004 batch completes the already-diagnosed page recovery. When
infinitely shrinkable glue reaches the current page, the builder now records
the original spec for `\lastskip`, reports each occurrence as before, retains
the shrink amount, and mutates the page node's shrink order to finite. A two-
block page-list differential pins the post-builder node while the existing
ten-line error sequence pins the diagnostic. Pass 2 moves from 5987 to 5988
matching lines with 6558 still produced, and block 8 contracts from 62 to 60
differing lines. Its first remaining mismatch is now the following alignment:
TeX's rows use the newly assigned tiny `\tabskip`, while aiTeX's use the stale
large value.

The following §774 batch moves the alignment save level ahead of §782's
preamble scan. Preamble `\tabskip` assignments were executed before aiTeX had
opened that level, so they belonged to the surrounding group: a nested
alignment restored its own value only when the outer cell ended, and the outer
alignment leaked its final large value into later alignments. A two-alignment
box differential keeps both tables in one surrounding group and pins the
required restoration boundary. Pass 2 moves from 5988 to 6007 matching lines
and from 6558 to 6559 produced lines; block 8 contracts from 60 to 26 differing
lines. Its repeated alignment glue now matches exactly, and the first remaining
mismatch is the same rows' packed width under the outer alignment's negative
`spread`.

The next §274/§453 batch implements `true` dimensions against the job's frozen
magnification. The first such scan now validates and freezes `\mag`; an
incompatible later assignment produces TeX's dedicated diagnostic, restores
the frozen value globally, and continues with the correct ratio. The scanner
preserves TeX's integer/remainder path so `1truemm` at `\mag=2000` rounds to
93233sp rather than drifting by a scaled point. A five-line error/recovery
differential pins both conversion and restoration, and the full error battery
grows to 1563 exact lines. Pass 2 moves from 6007 to 6009 matching lines with
6559 still produced, and block 8 contracts from 26 to 22 differing lines. Both
alignment row widths now match; the next remaining node difference is the
stretch amount of `\thinmuskip`.

The following §445/§716 batch distinguishes internal glue from muglue while
scanning and preserves infinite-order amounts when math parameters become
ordinary glue. Crossing levels now emits TeX's `Incompatible glue units`
diagnostic but retains the complete specification under its 1mu=1pt recovery;
automatic `\thinmuskip` conversion scales only finite components. Those two
newly visible TRIP errors also exposed a transcript-wide guard bug: TeX resets
its non-interactive 100-error counter at every completed paragraph, while
aiTeX capped the whole run. A 103-fault, three-paragraph differential pins that
boundary while the lifetime error total remains separate for the CLI. The
error and box batteries grow to 2084 exact lines and 322 exact displays. Pass
2 moves from 6009 to 6455 of 7283 matching lines and from 6559 to 7113 produced
lines; block 8 contracts from 22 to 20 differing lines. Its only remaining
difference is three extra empty rows from the late malformed alignments, which
also account exactly for its 30pt excess total height.

The following §342/§782 batch makes the scanner's global `align_state` active
while the preamble itself is being read and uses it, rather than a local brace
counter, to recognize `&` and `\cr` boundaries. Braces consumed indirectly by
the one-step expansion after `\span` now affect the decision exactly as they do
in TeX, including tokens consumed inside skipped conditional text and backed
up during the preamble scan. A
focused malformed-preamble display removes two manufactured empty rows while
all 323 displays remain exact. Pass 2 moves from 6455 to 6466 of 7283 matching
lines and from 7113 to 7071 produced lines; block 8 contracts from 20 to 8
differing lines. Its only remaining difference is one extra empty row from the
following frozen alignment-template token being rejected as an outer command
while a macro argument is scanned.

The next §367/§789 batch models TeX's second inaccessible
`\endtemplate` token. The outer marker stored after every v-template now turns
into a distinct inert `endv` token when it reaches `get_x_token` or one-step
expansion, so an expanded copy captured earlier by `\futurelet` can become an
ordinary macro argument without triggering outer-command recovery. A focused
TRIP-shaped display pins both forms and removes the final manufactured empty
row. All 324 displays and 2084 error lines remain exact; pass 2 moves from 6466
to 6469 of 7283 matching lines and from 7071 to 7045 produced lines. Block 8
becomes exact, raising the structural list score from 8/9 to 9/9.

**Exit:** list block 6 and the math/display transcript section (source lines
213–326) match the normalized local reference.

### 6. Close presentation and output artifacts

Once semantic nodes and recovery order are stable, finish diagnostic wording,
macro tracing, format-dump summaries, and terminal-selector routing. Then make
the DVI semantic comparison exact enough for the contract chosen in lane 1.
Do not spend semantic debugging time imitating allocator statistics that have
no counterpart in this implementation.

The first presentation batch routes box contents, node names, glue parameter
names, font identifiers, page insertion summaries, and math-noad names through
§63's current `\escapechar`, including its no-prefix out-of-range case. TeX's
top-level `> \boxN=` label deliberately remains backslashed. The showbox
harness now recognizes all three header forms, and a 12-display differential
pins them. Pass 2 moves from 5740 to 5975 matching lines and block 8 contracts
from 346 to 70 differing lines. Its first semantic difference is now an
ordinary script-style `\nonscript` that fails to discard its following glue.

The output-token-list batch restores §323's `begin_token_list` trace at the
point §1025 enters an output routine. All nine TRIP invocations now announce
the stored braced value as `\output->{...}`, including the final hostile case
where `\escapechar=256` suppresses every control-sequence prefix. A four-line
differential forces page breaks under both ordinary and out-of-range escape
characters. Pass 2 moves from 6469 to 6495 of 7283 matching lines and from
7045 to 7071 produced lines; all 16 pages and all nine list blocks remain
exact.

The first semantic-DVI batch defers `pre` until the first page ships, calls
§274's magnification preparation at every shipout, and passes the validated,
frozen value through the page sink. `DviWriter` now requires that value and
repeats it in `pre` and `post`; a direct writer test pins both copies, while an
engine test proves that a later incompatible assignment is diagnosed and the
original value is retained. TRIP's DVI comparison moves from 20/220 to 21/220
matching records with 216 still produced, and its first difference advances
from the hard-coded `mag=1000` to the declared page extents. Transcript parity,
all 16 pages, and all nine list blocks remain unchanged.

The next semantic-DVI batch replaces the driver's fabricated 469pt horizontal
ceiling and traversal-plus-72pt vertical estimate with §640's exact per-page
maxima: `height+depth+\voffset` and `width+\hoffset`. Shipout metadata freezes
both offsets with the counters and magnification for every page; streaming and
retained-page DVI now share those values for positioning, counters, and
postamble accounting. The semantic comparator also ignores `dvitype`'s
placement-only echoes of early body font definitions, allowing the two output
paths to prove 216/216 records exact against each other. Against TeX, TRIP
jumps from 21/220 to 87/220 records with exact header, extent, font, and page
metadata; the first mismatch advances to a horizontal-list rule emitted at its
baseline instead of at baseline plus rule depth. Transcript parity, all 16
pages, and all nine list blocks remain unchanged.

The horizontal-rule batch implements §624's DVI reference convention for both
ordinary rules and rule leaders. A rule in an hlist is emitted at baseline plus
its depth, using `height+depth` as its DVI height, and the writer then restores
the surrounding baseline. As in TeX, only rules with positive combined height
and width reach the DVI; invisible rules still advance the logical position. A
direct opcode invariant pins the 20pt descent, its matching return before a
following rule, and suppression of a zero-height rule. TRIP moves from 87/220
to 93/220 matching semantic records; every visible rule through page 3 is now
at TeX's exact coordinate, and the first mismatch advances to page 4's special
payload. Transcript parity, all 16 pages, and all nine list blocks remain
unchanged.

The special-byte batch fixes the representation boundary between aiTeX's Rust
strings and TeX's byte-oriented DVI string pool. Character codes 0--255 in an
`xxx` payload are now serialized as their single TeX byte, while larger Unicode
scalars retain a UTF-8 fallback. A direct raw-byte invariant proves that
`^^80x` becomes `80 78`, not UTF-8 `c2 80 78`. TRIP moves from 93/220 to 94/220
matching semantic records: page 4's special is exact, and the first mismatch
advances to page 7's first glyph, whose vertical position is too low. Transcript
parity, all 16 pages, and all nine list blocks remain unchanged.

The glue-vetting batch corrects the DVI writer's cumulative glue bound. TeX's
`vet_glue` clamps the floating-point product to exactly 1,000,000,000sp before
rounding, not to `2^30`; the distinction is visible when TRIP stretches a
max-height page by more than fifteen thousand points. A direct boundary
invariant pins the billion-sp result. TRIP moves from 94/220 to 155/220 matching
semantic records, making every event through page 9 exact. The first mismatch
advances to page 10's first glyph, whose vertical position is 1,028,915sp too
low. Transcript parity, all 16 pages, and all nine list blocks remain unchanged.

**Exit:** normalized `tripin.log`, `trip.log`, and `trip.fot` match the local
reference; the documented relation to Knuth's files reaches the local-TeX
ceiling; all 16 DVI pages pass the chosen semantic or opcode-exact comparison;
and `tools/done_check.sh` fails on any regression in those claims.

## Batch discipline

Every implementation batch should start at the earliest remaining transcript
divergence, commit a minimal differential reduction that fails before the fix,
make that reduction and the relevant component battery green, then re-run the
full TRIP report. Record the changed artifact score in the commit's paragraph,
run `tools/done_check.sh` before committing, and push the batch to origin. A
higher percentage without an earlier first divergence, restored source reach,
or an independently green artifact is useful evidence but not completion.
