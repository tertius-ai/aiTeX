# The forced-break scan has to measure the same line the search does

`2608.17397v1` differed from pdfTeX by one character in 23237. After the
hyphenation fix it differed by a word, and the trace said:

```text
ref    @\discretionary via @@6 b=87 p=50 d=21909
ours   @\discretionary via @@6 b=*  p=50 d=*
```

`b=*` is `awful_bad` and `d=*` is §851's `artificial_demerits` — so we thought
the line was overfull *and* that the break was forced, and took it free of
charge where pdfTeX charged 21909.

## It was not the protrusion amount

Four measurements ruled out everything except the plumbing:

- `\pdfprotrudechars=0` and `=1` agree with pdfTeX exactly; only `=2`, where
  protrusion enters the badness, diverges.
- At `=1`, `\showbox` of the same paragraph gives 8 margin kerns per engine,
  all identical.
- Every component of the line measured with `\hbox` agrees: the math is
  72.9943pt, the whole line 371.13698pt, `\hsize` 360.0pt.
- Instrumenting the trace printed the numbers outright:

  ```text
  nat=24322833  natp=24202991  shr=637880  meas=23592960  prot=119842
  ```

  371.137pt natural, 1.8286pt of protrusion, 9.733pt of shrink, 360pt measure.
  `natp - shr = 23565111 < 23592960` — **the line is not overfull**, and
  `protrusion_of` had computed the hyphen's protrusion correctly all along.

## Two places used the un-protruded width

**The forced-break scan.** `line_break` pre-computes, for each starting point,
the break §854 would be forced to take when no feasible break exists. That scan
built its own `natural` and called `assess` without subtracting protrusion. So
it declared the line overfull, recorded a forced break, and the search then took
that break free of charge instead of scoring it — `d=0` against pdfTeX's 21909.

The scan and the search have to measure the *same* line. There was already a
comment saying exactly that about §840's post-break material; protrusion is the
same kind of term and was missing.

**The `b=*` display.** `b_shown` tested `natural - shrink > measure` where the
scoring uses `natural_p`. That one is cosmetic on its own, but it is why the
trace read `b=*` and sent me looking for a width error that was not there.

## Why the reduction was awkward

The failing paragraph *contains a display* — it runs from source line 287 to
301 with `\[…\]` at 288–292. Starting a reduction at line 293, which is where
the visible text begins, does not reproduce: the break never lands on
`semista-` at all. The whole paragraph is needed.

`\hbox` measurement is the right instrument for the natural-width half of this,
precisely because a plain `\hbox` does not protrude — see
[protrusion.md](protrusion.md).
