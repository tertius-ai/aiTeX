# `\end` fires the page builder 63 times where TeX fires it twice

Found 2026-08-20 while chasing a page-break divergence; it is not that
divergence, and the output is unaffected. Recorded because it has a minimal
reproduction and a bounded cause.

## The reproduction

```tex
\documentclass{amsart}
\begin{document}
\tracingpages=1 \tracingonline=0
\section{One}
Alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi
omicron pi rho sigma tau upsilon phi chi psi omega and more words here to
make this paragraph wrap onto several lines so the page builder sees them.

Second paragraph with enough words to wrap as well, alpha beta gamma delta
epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau.
\end{document}
```

pdfTeX writes three `%% goal height=` blocks and stops. We write the same three
— identically, to the character — and then **63 more**:

```
%% goal height=16383.99998, max depth=4.0
% t=10.0 g=16383.99998 b=10000 p=0 c=100000#
% t=10.0 plus 1.0fill g=16383.99998 b=0 p=-1073741824 c=-1073741824#
```

repeated. `16383.99998pt` is `\maxdimen`; `-1073741824` is `-'10000000000`,
§1054's forced penalty.

**The output is identical**: 1 page, 309 characters, same as pdfTeX. So this is
63 wasted output-routine invocations per document, not a wrong document. On
2608.17952v1 it is 64 of them, and they are why our page trace has 563 lines
against pdfTeX's 376.

## What is known, measured

`AITEX_DBG_SEL=1` now logs three events on this path — `END` (the three
`job_is_over` components plus `\deadcycles`), `SHIPOUT`, and `DEADCHECK`. On the
reproduction:

```
DEADCHECK dead=0 max=100
SHIPOUT (dead was 1)
DEADCHECK dead=0 max=100
END page=1 contrib=0 vlist=0 dead=1
DEADCHECK dead=1 max=100
END page=0 contrib=0 vlist=0 dead=2
DEADCHECK dead=2 max=100
END page=0 contrib=0 vlist=0 dead=3      … and so on
```

**From the second iteration everything is empty**: `page=0 contrib=0 vlist=0`, so
`job_is_over()` is *true*. What blocks the exit is `dead_cycles == 0`, and
`\deadcycles` never comes back down. Each turn of the loop fires `\output`,
which ships nothing, so §1025's `incr(dead_cycles)` runs and §640's reset in
`ship_out` does not.

So the bug is **one `\output` invocation that TeX does not make**: after the last
real page ships, `dead_cycles` is 0 in TeX and its second `\end` finishes
immediately. Ours fires the routine once more on an empty page, and from then on
the condition can never be met.

Two earlier readings in this note were wrong and are corrected:

* **`\deadcycles` is counting.** An earlier version said it was not, on the
  strength of no `Output loop` error appearing. It counts fine.
* **`\maxdeadcycles` is correct.** It reads 100 in *both* engines — LaTeX sets
  it; INITEX's 25 is what `tex -ini` shows. That is why no `Output loop` error
  fires at 25, and it is not a bug.

The remaining question is narrow: what fires `\output` after the final
`ship_out`, when the page, the contribution list and the vertical list are all
empty. §1005's `build_page` returns at once on an empty contribution list, so
whatever calls it is passing something.


## Where it was parked (2026-08-20)

`AITEX_TRACE_PAGE=1` gives the richest view — what each fire held and what was
left:

```
FIRE: 24 nodes, op=-10000,       left page=0 contrib=1     <- ships
FIRE:  3 nodes, op=-10001,       left page=0 contrib=1     <- does not ship
FIRE:  4 nodes, op=-1073741824,  left page=0 contrib=1     <- \end, x63
```

Only the first ships. From the second on, `\output` is invoked and returns
without shipping, so §640's reset never runs and `dead_cycles` climbs.

**§1013's neutralisation is implemented** (`page.rs:956`, `*p = INF_PENALTY`,
with a comment naming this exact hazard), so a *stale* forcing penalty is not
the cause. Each `\end` turn appends a **fresh** `\penalty-'10000000000` via
`append_end_sentinel`, so neutralising the previous one cannot help. The loop is
sustained purely by `dead_cycles` never returning to zero.

The unanswered question is therefore narrow and unchanged: **why does the
second `\output` invocation not ship?** pdfTeX reaches `p=-1073741824` once and
finishes; we reach it 64 times. Both produce one page and identical characters.

Parked in favour of P6 after roughly six iterations of investigation without a
landed fix. It is worth returning to, because the 63 spurious fires are the bulk
of the difference between our 563-line page trace and pdfTeX's 376 on
2608.17952v1 — they mask whatever genuinely moves that paper's page break, and
`p=0` counts differ by exactly the number of extra fires (79 against 16 on the
small reproduction, 63 apart).

Instrumentation left in place, all behind `AITEX_DBG_SEL=1`: `END`, `FIRE`,
`SHIPOUT`, `DEADCHECK`. `AITEX_TRACE_PAGE=1` (not `AITEX_PAGE_DEBUG`) is the
older, richer one.
