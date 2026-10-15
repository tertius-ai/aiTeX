# One breakpoint too many, before trip's second `% split200`

**Status:** located, not explained. Not blocked on anything — see the last
section for what to try, and note that the previous note in this directory was
filed as blocked and turned out not to be.

## The divergence

With §1010's arithmetic fixed (81d88ed), trip.tex's page trace agrees for 32
lines. Then:

```
tex                                                        ours
% t=-13.0 plus 293.0 plus 8.0filll minus 33.0 …  <glue>    (same)
                                                           % t=-3.0 plus 334.0 plus 8.0filll minus 33.0 …  <glue>
% split200 to 1832.4585,10000.0 p=-10000                   (same)
% t=1008.0 plus 375.0 plus 8.0filll minus 33.0 …           (same)
```

We weigh a glue breakpoint that TeX does not. `AITEX_TRACING_PAGES=1
AITEX_TRACING_NODES=1` names both as `<glue>`.

## What this is not

**Not a state divergence.** The line after the split is identical in both
engines — `t=1008.0 plus 375.0 plus 8.0filll minus 33.0`. Whatever we weigh, we
accumulate the same total, the same stretch and the same shrink as TeX. So this
is a breakpoint *considered* and not taken, not material landing in a different
place.

That also means it cannot be found by comparing boxes, pages or DVI. It is
visible only in `\tracingpages`, which is why it surfaced now and not earlier.

## What is established

The step from `t=-13.0` to `t=-3.0` adds 10pt of width and 41pt of stretch and
no shrink — one glue node, accumulated between the two weighings. So the node
sequence around here is, for us:

```
glue(weighed, t=-13) … glue(weighed, t=-3) … insertion(split200)
```

and for TeX:

```
glue(weighed, t=-13) … insertion(split200)
```

§1000 makes glue a breakpoint exactly when the node before it on the page is
non-discardable — `precedes_break(p)`, which is `type(p) < math_node`. Two
adjacent glues therefore give one breakpoint, not two, and our `is_breakpoint`
implements that against `page.last()`. So either

  * there is an extra glue node in our contribution stream that TeX does not
    have (a `\baselineskip` we insert and TeX does not, most likely), and the
    two glues are separated by something non-discardable; or
  * `page.last()` is not what we think it is at that moment — the head is taken
    out of `contributions` and replaced by a sentinel rather than removed, so
    the index arithmetic in `is_breakpoint` refers to the node *after* the
    current one, and it is worth checking that the sentinel cannot be observed
    as the previous node.

The totals agreeing on both sides of the extra weighing is the awkward part for
the first explanation: an extra glue node would have to contribute zero width,
zero stretch and zero shrink.

## What has been ruled out

Probes built on `tests/trace/insert-split.tex`, varying what precedes the
insertion, all agree with TeX line for line — so none of these is the trigger:

    \baselineskip = 0pt / 12pt        a box before the insertion
    a box then a \vskip               a \mark
    a \penalty                        two adjacent \vskips
    \unvbox255 handing the page back once, alone
    \unvbox255 + \holdinginserts + a split insertion (19 trace lines, identical)

The last is worth naming: two adjacent glues produce **one** breakpoint in both
engines, so the plain §1000 rule is right and the extra weighing is not a
missing "previous node is discardable" test in the simple case.

`\unvbox255` was the standing lead when this note was first written and it is
now ruled out, both alone and combined with a held, split insertion. The
re-contribution path is right.

## The context this actually happens in

trip.log puts the divergence inside the `\output` fired from trip.tex line 150.
The material reaching the page comes from lines 144-148:

```
  \kern-50pt
  A\hfill\vadjust{\newlinechar128\special{^^80\the\prevdepth}\penalty-5000}%
  \penalty-1000000000 % forces line break in paragraph
  A\par\insert200{\vskip10000pt\floatingpenalty3}% this insert will be held over
```

Two features none of the ruled-out probes had:

  * a **`\vadjust`** whose material — a `\special` and a `\penalty-5000` —
    migrates out of the line and into the vertical list (§1083). Where exactly
    it lands decides what sits between the glue we weigh and the insertion. A
    penalty node does *not* precede a break (§148: `type < math_node`), so a
    glue immediately after migrated `\vadjust` material is not a breakpoint —
    which is the right shape for an extra weighing that changes no totals.
  * a **`\kern-50pt`** ahead of the paragraph, already the subject of a §1000
    fix in `page.rs` (a kern is a breakpoint only once glue follows it, so the
    builder must stop and wait rather than guess).

That probe was built and is **also identical** — glue, a paragraph with
`\vadjust{\penalty-5000}` and a forced `\penalty-1000000000`, then a split
insertion: twelve trace lines, no diff. So the shape alone does not do it; what
matters is something about trip's accumulated state by that point (three
paragraphs from one `\everypar`, held-over insertions from the previous page,
a `\mark`).

## The boundary itself is right in isolation

Two further probes test the contribution boundary head-on rather than the
breakpoint, since block 3 says that is what differs:

  * boxes and glue with `\showlists` after each of a box, a `\penalty` and a
    `\vskip` — identical, including which glue stays in contributions;
  * the block 3 shape itself — a forced-break paragraph with a `\vadjust`,
    `\hsize=0pt`, `\showlists` after `\par` — identical, including
    `prevgraf 3 lines` and `total height 44.0`.

Eleven probes, none reproducing. The trigger is not the shape of the material;
it is accumulated state. The obvious remaining candidate is that trip's page at
this point is nearly full and carrying **held-over insertions from the previous
page** (`\holdinginserts`, a `split200` that has already run), which no probe
here reproduces because setting it up means replaying trip's previous page.

At that point guessing at shapes has clearly stopped paying. The next attempt
should bisect trip.tex itself rather than build a twelfth probe — cut the file
down until the extra line disappears — accepting that pass one's format has to
be rebuilt for each cut.

## Correction: what `\showlists` block 3 is

An earlier version of this note treated block 3 as a `\showlists` of its own.
It is not, or at least not straightforwardly. `tools/trip_lists.sh` numbers
blocks by `### vertical mode entered`, and §218 prints one such header **per
nest level**, so one `\showlists` can produce several. In trip.log the first
three all follow the same `\showlists` at line 149 — a copy of trip.tex with a
`\message` inserted just before it puts that marker at log line 377, and the
three headers at 380, 676 and 804.

Their sections differ:

    block 1 (380)  ### current page:                                  matches
    block 2 (676)  ### current page: (held over for next output)      matches
    block 3 (804)  ### recent contributions:                          differs

So the page contents agree — twice, including the held-over page — and only the
material *not yet moved* differs. That is a sharper statement than "the
boundary is in the wrong place", and it is consistent with the extra
breakpoint: same nodes, same totals, different amount still waiting.

### `\showboxbreadth` does not lift the truncation, in either engine

Recorded so it is not retried blind. Setting `\showboxbreadth9999
\showboxdepth9999` immediately before that `\showlists` does not change block
3 — nor blocks 1 and 2, which stay at 107 and 28 lines exactly as without it.
The assignment is in effect: `\message{[bb=\the\showboxbreadth]}` beside it
prints `[bb=9999,bd=9999]`, and `\tracingcommands` shows `{\showlists}` as the
very next command.

The important half is that **real TeX does the same thing**. Both engines print
`etc.` after the same number of items with breadth set to 9999. So this is not
our bug and not a divergence — it is TeX behaviour neither understood nor
needed to be, and the fact that we reproduce it exactly is a small piece of
evidence that our §182 `show_node_list` is faithful.

What it does mean is that `\showlists` cannot be made to yield the two full
contribution lists, so comparing them needs another route — dumping ours with
`AITEX_VLIST` and reading TeX's against whatever `\showbox` of a constructed
copy will show, or instrumenting the builder directly.

## It is the same fault as `\showlists` block 3

Worth recording, because they have been treated as two problems. Block 3 is the
`\showlists` at trip.tex line 149 — inside this same region — and its diff is:

```
tex:   \glue(\baselineskip) 9.0 plus 41.0     ours:  \glue(\baselineskip) 3.0 plus 41.0
       \hbox(0.0+0.0)x0.0                            \hbox(7.0+1.0)x0.0
       .\penalty -1000000000                         .\rip A
```

**`plus 41.0` is the same glue** whose accumulation the extra breakpoint sits
across (`t=-13.0 plus 293.0` to `t=-3.0 plus 334.0` is +41.0 of stretch).

The baselineskip widths are consistent with each other — 9.0 before a 0-height
box and 3.0 before a 7-height one, both `\baselineskip - prevdepth` of 10 minus
the box height — so neither engine is computing the glue wrongly. The difference
is *which box comes next*.

### Which way round it goes

An earlier version of this note said "TeX's recent contributions begin one item
later than ours", and concluded our builder stops one item *early*. That is
backwards, and the correction matters because it points the opposite way.

The paragraph at trip.tex 145-148 makes three lines:

    line 1   A\hfill                        + \rightskip
    line 2   (empty) \penalty -1000000000   + \rightskip
    line 3   A \penalty 10000 \parfillskip  + \rightskip

§816 appends that `\penalty 10000` at the *end of the paragraph*, so only the
last line carries one directly after its text. TeX's first waiting item is
line 2; ours is line 3.

Recent contributions are what has **not** been moved onto the page. So TeX has
lines 2 and 3 still waiting and we have only line 3: **we have moved one item
more than TeX, not one fewer.**

That is consistent with the extra breakpoint rather than merely alongside it.
We weighed a breakpoint at the glue preceding line 3 because we had already
processed line 2 and moved past it; TeX stopped before that glue. One
divergence, one direction: our page builder runs one contribution further than
TeX's before the `\showlists` at line 149 sees it.

The question to answer is therefore not "why do we stop early" but "what stops
TeX there". Its contribution list is non-empty and it has not fired, so
`build_page` returned with work left — which §1000 does for a kern with nothing
after it, and which the surrounding code does by simply not calling the builder
again yet.
