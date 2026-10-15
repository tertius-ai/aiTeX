# trip's paragraph 8: the closing `\par` line is badness 0 for TeX and 10000 here

## Where

trip.tex around line 203, the paragraph whose trace begins

    @firstpass
    [][][]|[]\rip AAAB|[]B-

Eight of trip's paragraph traces are byte-identical now; this is the ninth,
and the first that differs.

## What differs

TeX sets it in the **first** pass:

    @firstpass
    [][][]|[]\rip AAAB|[]B-
    @\discretionary via @@0 b=0 p=88 d=7745
    @@1: line 1.2- t=7745 -> @@0
    []
    @\par via @@1 b=0 p=-10000 d=100001
    @@2: line 2.2- t=107746 -> @@1

We agree exactly as far as `@@1`, then give up and run a second pass, where
the same line comes out unsettable:

    @secondpass
    [][][]|[]\rip AAAB|[]B-
    @\discretionary via @@0 b=0 p=88 d=7745
    @@1: line 1.2- t=7745 -> @@0
    @\par via @@1 b=10000 p=-10000 d=*
    @@2: line 2.0- t=7745 -> @@1

So the difference is not in the pass logic — it is the **badness of the last
line**, `b=0` against `b=10000`. Ours cannot stretch it; TeX's fits exactly.
Note also the `[]` TeX prints before its `@\par`: material between `@@1` and
the paragraph's end that we do not report, which is the same fact seen from
the other side. Our first pass never found the `\par` feasible, so §857 never
printed what lay between.

## Where to look

The paragraph carries a `\vadjust` (trip.tex line 203):

    \hyphenchar\rip`-\vadjust{\def\B{B}\ \pretolerance10000 B\B BBBB}

An `\vadjust` node is `Node::Adjust` in the horizontal list. §866's background
totals and §816's `act_width` count a node's width, and an adjust node has
none — it is spliced out when the line is packed (§655). If we are counting
the adjusted material's width in the line, the line is too long to stretch
and every badness after it is wrong. That is the first thing to measure:
build the same paragraph with and without the `\vadjust` and compare `b=`.

The second candidate is `\parfillskip`: `b=0` on a final line usually means
the fill absorbed everything, so a `\parfillskip` that is not reaching the
background totals would give exactly this shape.

## What is already ruled out

Not the pass structure, not `easy_line`, not the active list's order — the
traces agree token for token up to the point the badness is computed, and
all of `tools/trace_check.sh`'s 2,420 lines and the three `\looseness`
probes match TeX node for node.
