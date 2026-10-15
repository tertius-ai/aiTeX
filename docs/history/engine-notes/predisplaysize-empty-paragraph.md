# `\predisplaysize` when there is no paragraph to measure

`tex.web` §1145 does not always break a paragraph into lines before a display:

    @<Go into display math mode@>=
    begin if head=tail then {`\.{\\noindent\$\$}' or `\.{\$\${ }\$\$}'}
      begin pop_nest; w:=-max_dimen;
      end
    else  begin line_break(true);@/
      @<Calculate the natural width, |w|, by which the characters of the
        final line extend to the right of the reference point…@>;
      end;

With nothing in the paragraph there is no last line to measure, and `w` — which
becomes `\predisplaysize` — is `-max_dimen` outright.

Knuth's comment names two cases, `\noindent$$` and `$${ }$$`, and both look like
curiosities. The third is neither rare nor curious: **a display straight after
another display**. §1200's `resume_after_display` opens a fresh horizontal list,
so the next `$$` finds it empty. Three consecutive `\[…\]` is an ordinary thing
to write.

## Why it matters

§1202 chooses between the long and the short display skips on that number:

    if (d+s<=pre_display_size)or l then {not enough clearance}
      begin g1:=above_display_skip_code; g2:=below_display_skip_code; end
    else  begin g1:=above_display_short_skip_code;
      g2:=below_display_short_skip_code; end;

`-max_dimen` is smaller than any `d+s`, so an empty paragraph always takes the
**short** skips — which is the point. There is no preceding line for the display
to clear, so no extra room is wanted above it.

Our `w` came from `ts.last_line_extent`, and only the line-breaker ever assigns
that. An interrupted-but-empty paragraph therefore left the *previous*
paragraph's reach in place, and every display following a display took the long
skips.

Measured on 2608.18003v1, whose three consecutive `\[…\]` are the whole of it —
probe inserted into a copy for each engine:

    \[\message{<D pds=\the\predisplaysize\space di=\the\displayindent
                \space dw=\the\displaywidth>} … \]

    pdftex:  pds=196.28824pt   pds=-16383.99998pt   pds=-16383.99998pt
    ours:    pds=196.28824pt   pds=196.28824pt      pds=196.28824pt

The first display follows body text and is right in both. The other two carried
`\abovedisplayskip`/`\belowdisplayskip` instead of the short pair:

    -\glue(\abovedisplayshortskip) 0.0 plus 4.89998
    +\glue(\abovedisplayskip) 4.89998 plus 4.89998
    -\glue(\belowdisplayshortskip) 2.44998 plus 4.89998
    +\glue(\belowdisplayskip) 4.89998 plus 4.89998
    -total height 612.92259 plus 57.39966 minus 13.99979
    +total height 627.62254 plus 57.39966 minus 13.99979

2 × 4.89998 above and 2 × 2.44998 below is 14.69992, and the page totals differ
by 14.69995. That is the entire discrepancy, and it was enough to push a
paragraph off the bottom of the page.

**Corpus: 15/34 → 17/34.** 2608.18003v1 and 2608.17815v1 both matched pdfTeX's
page *and* character counts already and failed only on the sequence; both now
pass outright. 2608.17233v1 was in that same group and still fails, so it is
something else.

## Finding it

The chain was three tools, each narrowing by an order of magnitude:

1. `tools/logdiff.sh <file> text` — says *which words* moved, not just that
   bytes differ. All three near-miss projects showed the same sentence: a block
   pdfTeX fits at the foot of a page, we push to the next.
2. `TRACE=pages tools/logdiff.sh <file> pages` — the two `\tracingpages` traces
   agree exactly up to `t=532.69994`, then one contribution adds 4.89997pt more
   for us. The `plus` component matched, which said the glue *items* were the
   same and the excess was a natural width.
3. `\showboxbreadth=200 \showboxdepth=5 \showlists` inserted at that point in a
   copy for each engine — names the glue outright.

Then the `\the\predisplaysize` probe above turns a hypothesis into a
measurement. That last step was worth insisting on: three reductions had already
*failed* to reproduce it, and reasoning from them would have been wrong.

## Negative results

All three compared by `\showlists` against pdflatex and **byte-identical** apart
from the `, display` marker (a separate gap):

- `article` + two consecutive `$$…$$`
- `article` + three consecutive `\[…\]`
- `amsart` 12pt + `\newtheorem*` + `\begin{defn}` after a paragraph

The first two look like they must reproduce the bug and do not, because LaTeX's
own `\[` inserts `\nointerlineskip\makebox[.6\linewidth]{}` when it starts in
vertical mode — which makes the paragraph non-empty, so the line-breaker runs
and assigns `last_line_extent` correctly by accident. Under `amsmath`, `\[` is
`\begin{equation*}` and no such box appears. A reduction that avoids the very
package the failing document loads can look decisive and mean nothing.
