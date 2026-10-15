# Where TRIP first goes wrong

Opened 2026-08-20, after `packages.sh` reached 50/50 and the transcript
machinery became good enough to align. Superseding guesswork with a measured
first divergence.

## Standing numbers

```
pages:  ours 13, tex 16, Knuth trip.typ 16
trip.log vs this machine's tex: 4291 of 7283 lines (58%)
trip.log vs Knuth, ceiling:     6713 of 7305 (91%)   <- what this machine's tex itself reaches
```

The 91% ceiling matters: this machine's `tex` does not reproduce Knuth's
`trip.log` either, because the log records the *memory manager's* state. So the
oracle is **this machine's tex**, not Knuth's file, and `tools/trip.sh` reports
both.

## Two lines that can never match, and should be filtered when reading a diff

* The banner (`This is TeX…` against `This is aiTeX…`).
* `Memory usage before: 159&313; after: 102&278; still untouched: 4998613` —
  §639 under `\tracingstats>1`. These are `var_used`, `dyn_used` and
  `hi_mem_min-lo_mem_max-1`: positions in TeX's single fixed-size `mem` array.
  This engine has an arena and a `Vec`; the numbers have no counterpart, and
  inventing plausible ones would be a lie in a file whose entire purpose is to
  be compared. Sixteen lines.

```sh
norm() { grep -avE '^This is |^Memory usage before' "$1"; }
diff <(norm tex-trip.log) <(norm ours-trip.log)
```

## The first substantive divergence: transcript line ~781

Everything before it now matches, which is new — the `(./trip.tex` echo that a
restored format used to suppress was holding the whole file two lines out of
alignment.

**One extra page-builder line.** We write a `%` cost line that TeX does not:

```
% t=-3.0 plus 334.0 plus 8.0filll minus 33.0 g=924.23029 b=0 p=0 c=0#
```

**A different `### recent contributions`**, thirty lines later:

```
             tex                             ours
\glue(\baselineskip) 9.0 plus 41.0     \glue(\baselineskip) 3.0 plus 41.0
\hbox(0.0+0.0)x0.0                     \hbox(7.0+1.0)x0.0
.\penalty -1000000000                  .\rip A
.\glue(\rightskip) 0.0                 .\penalty 10000
```

This *looks* like a paragraph whose last line came out wrong, and was read that
way at first. It is not: the two columns are **different boxes**. TeX's
contribution list still holds the box we have already moved onto the page, so
its list begins one item earlier than ours. Everything after — the `\vbox`
heights (`924.23029+1.0` against `+0.0`), the glue sets (`3.19875` against
`2.77614`), the split decisions and ultimately three missing pages — follows
from that one contribution, not from a break.

## Narrowed: it is the page builder, not the line breaker

Three measurements, each ruling something out:

* **The paragraph's breaks are identical.** `\tracingparagraphs` is on in trip,
  and the `@`/`@@` lines for this paragraph match to the character in both
  engines — `@@1: line 1.3`, `@@2: line 2.2`, `@@3: line 3.3-`, and
  `prevgraf 3 lines` afterwards. The line breaker agrees.
* **`\vadjust` with a forced penalty on the same line is right.**
  `\vbox{\hsize=100pt \noindent A\hfill\vadjust{\hrule height 3pt}%
  \penalty-1000000000 B\par}` produces identical boxes, including the
  `\penalty -1000000000` that stays *inside* the line because the break was not
  taken there.
* **The first eight `\showlists` blocks are identical.** The ninth is the first
  that differs, and it is the one above.

What differs is one contribution. Filtering the transcript to `\tracingpages`
lines only, the first divergence is an **extra** `% t=` line:

```
   tex                                            ours
   % t=27.0  plus 210.0 …  b=0 p=0 c=0#           same
   % t=-13.0 plus 293.0 …  b=0 p=0 c=0#           same
                                                  % t=-3.0 plus 334.0 … c=0#   <- extra
   % split200 to 3955.99365,10000.0 p=-10000      % split200 to 3963.98926,…
```

`3963.98926 - 3955.99365 = 7.99561` — within rounding of the `\hbox(7.0+1.0)`
that our `### recent contributions` no longer holds and TeX's still does. So we
move one box from the contribution list to the page that TeX leaves there, and
the `\vsplit` that follows then splits against a goal 8pt further on.

**The question for the next pass** is why TeX's `build_page` stops one item
earlier here. The construct is trip.tex:140-151 — three one-line paragraphs,
each `A\insert200{…}\mark{n}`, with `\count200=500` so the inserts are rated
200pt and the third is split, and a `\penalty-1000000000` forcing a break
mid-paragraph. §1008/§1009's insert accounting against `\pagegoal` is the part
that decides when the page is full, and it is the only machinery in that list
this engine has not had a differential test written for.

## A smaller thing seen in passing

`show_box`'s truncation prints one `.etc.` level more than TeX's in this block:
TeX writes `.\penalty -1000000000 / .\glue(\rightskip) 0.0 / etc.` where we
write `.\rip A / .\penalty 10000 / .etc. / etc.`. §182's `show_box` counts
depth and breadth against `\showboxdepth`/`\showboxbreadth`; ours is one out
somewhere in that accounting.
