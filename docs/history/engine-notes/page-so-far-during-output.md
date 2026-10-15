# `\pagetotal` and friends read wrong inside `\output`

The first divergence between our trip.tex transcript and Knuth's is a
`\showthe\pageshrink` inside an output routine: TeX says `51.0pt`, we say
`0.0pt`. It is the tenth `> …` line of 67 in trip.log (we produce 36).

    trip.log:719  > 51.0pt.
                  <output> ...\insertpenalties \showthe \pageshrink

This note records what is and is not established, because the page builder is
the part of this engine where a guess is most expensive.

## What is established

The *pages themselves* are not in question here. A direct probe:

    \output={\message{[out: total=\the\pagetotal ht255=\the\ht255]}\shipout\box255}
    \vsize=50pt \maxdepth=2pt
    \hbox{}\vskip 10pt plus 3pt minus 4pt
    \hbox{}\vskip 20pt plus 1fil minus 7pt
    \hbox{}\vskip 100pt
    \hbox{}
    \end

    tex   [out: total=130.0pt ht255=50.0pt]   [out: total=0.0pt   ht255=50.0pt]
    ours  [out: total=130.0pt ht255=50.0pt]   [out: total=130.0pt ht255=50.0pt]

Both engines fire `\output` twice, ship two pages, and pack `\box255` to
50.0pt each time. Only the *reported* `\pagetotal` differs — in this probe on
the second firing alone; the third probe below differs on the first as well, so
do not read "second firing only" as general. Either way it is a reporting
fault, not a page-breaking one, which means it cannot be found by counting
pages or comparing DVI output, and `tools/trip_lists.sh` sees it only
indirectly.

The error runs in **both directions**, which is why it is one note and not two
fixes:

  * here ours reads 130.0pt where TeX reads 0.0pt — stale, the previous page's
    figure surviving into the next firing;
  * in trip.tex ours reads 0.0pt where TeX reads 51.0pt — collapsed to nothing.

A third probe settles what the difference *is*. Give the glue that triggers
the break some stretch and shrink of its own:

    \output={\showthe\pagetotal \showthe\pagestretch \showthe\pageshrink \shipout\box255}
    \vsize=50pt \maxdepth=2pt
    \hbox{}\vskip 10pt plus 3pt minus 4pt
    \hbox{}\vskip 20pt plus 1fil minus 7pt
    \hbox{}\vskip 100pt plus 55pt minus 66pt
    \end

    tex   130.0pt  58.0pt  77.0pt
    ours   30.0pt   3.0pt  11.0pt

TeX's figures are ours plus that glue entire — 100pt of width, 55pt of stretch,
66pt of shrink. An earlier probe where the triggering glue was a bare
`\vskip 100pt` looked like "only `\pagetotal` is wrong"; it was not, the other
two were merely zero on both sides because the glue had nothing to add.

So: **we stop accumulating `page_so_far` at the node we fire on, and TeX does
not.** In `page.rs` that is the fire path in `step`:

    if c == AWFUL_BAD || penalty <= EJECT_PENALTY {
        self.contributions[0] = node;
        return Some(Decision::FirePage);
    }
    self.contributions.remove(0);
    self.accumulate(&node);

— the node goes back on the contribution list and `accumulate` never runs for
it.

## The catch

The obvious repair is wrong, or at least unproven. §1005 calls `fire_up(p)`
*before* `@<Update the current page measurements…@>`, so TeX does not
accumulate its firing node either. Which means the extra 100pt does not come
from TeX accumulating the node we skip — it comes from TeX **firing somewhere
else**, one or more nodes later, having recorded this break and carried on.

That fits the arithmetic: at the `\vskip 100pt` the page holds 30pt against a
50pt goal with a `fil` of stretch, so its badness is 0 and its cost is not
`awful`. TeX records the champion and continues; we treat it as the firing
point. Both then break in the same place — `\box255` is 50.0pt in both, and
both ship two pages — because the *recorded* break is the same. Only the
accumulator diverges, and only where something reads it.

So the question to answer first is not "should `accumulate` run here" but
"why does our `c` reach `AWFUL_BAD` at a node where TeX's does not". That is a
cost computation, not a bookkeeping order.

## Resolved: the trace was being thrown away

The page builder was right all along. Written out by hand, §1054's own
material —

    \hbox to\hsize{}\vfill\penalty-1073741824
    \end

— makes our `\tracingpages` output byte-identical to TeX's, all eight lines and
both pages. So every breakpoint TeX weighs, we weigh, and we score each one the
same.

What was wrong was the printing. `main.rs` wrote `f.ts.page.log` and *then*
called `f.finish_page()`, which is where §1054's material is contributed and the
last page's breakpoints are weighed. Those lines were appended to a buffer that
had already been written out. On a document short enough to fit one page, that
is every breakpoint there is.

This is the same fault as the transcript flush earlier in this work, which used
to happen straight after `f.run` and truncated whatever the closing `\output`
said. The rule both cases want: anything the engine can still append to has to
be printed after the last thing that appends to it.

Fixed, and `tests/trace/tracingpages.tex` now holds the page builder under
`tools/trace_check.sh` alongside the seven `\tracing*` parameters. Note the
difference in kind — those seven were registered and inert; this one worked and
was discarded. Verified failing without the fix, five lines short.

## Resolved: the published `\page…` parameters

Two faults, and only together did they produce what was seen.

`fire_up` zeroes the live accumulator on its way out, and it runs inside
`build_page` — so by the time a firing is visible to the caller there is
nothing left to publish. The values now come from a snapshot taken in `fire_up`
beside the `fired_size` that was already there, and published by a separate
`sync_fired_page_so_far` rather than by `sync_page_so_far`, which is for the
live accumulator and must stay that way.

The publish also sat *past the early return* in `harvest_insertions` — the one
taken whenever the page carries no insertion, which is nearly every page. So on
an ordinary page nothing was published at all, and the parameters kept whatever
the last **idle** publish had left in them: the accumulator as of some earlier
breakpoint, one contribution short of the page that actually fired. That is
where the 30.0pt came from, and why it looked like a stale *previous page*
rather than an early *this page*. `\pagegoal` went the same way, which matters
because plain.tex's `\pagebody` is `\vbox to\pagegoal{…}`.

Both probes in this note now agree with TeX exactly — 130.0/58.0/77.0 then
0.0/0.0/0.0, and `total=130.0pt goal=50.0pt shrink=11.0pt` then
`total=0.0pt goal=50.0pt shrink=0.0pt`. Pinned by
`tests/trace/pagesofar.tex`, verified failing without the change.

## Both halves were reporting, not page breaking

Worth recording as one fact, because the symptom argued the opposite the whole
way: the page builder was correct before either fix. It weighed every
breakpoint TeX weighs and scored each the same. What was wrong was that its
trace went to a buffer already printed, and its results were published on a
path nearly every page skips. Neither could be seen by counting pages or
comparing DVI output, which is what every gate here did until
`tests/trace/tracingpages.tex` and `tests/trace/pagesofar.tex` existed.

