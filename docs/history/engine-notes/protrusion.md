# Character protrusion, measured against pdfTeX before implementing

Opened 2026-08-22. All four remaining corpus near-misses load **microtype**,
which is one blocker wearing four faces:

```
2608.15742v1   8p/17198c vs 17204c
2608.17686v1  18p/25767c vs 25775c
2608.17805v1   6p/14147c vs 14159c
2608.17397v1  14p/23237c vs 23255c
```

The primitives are done (`\leftmarginkern`, `\rightmarginkern` — they read a
margin kern back and we insert none, so they answer zero, which is truthful).
What is left is inserting the kerns and letting them into the line breaker.

## What pdfTeX emits, measured

`\pdfprotrudechars=2`, `\lpcode\tenrm`\A=200`, `\rpcode\tenrm`\,=300`,
`\rpcode\tenrm`\.=400`, `\hsize=100pt`, `\parindent=0pt`:

```
\hbox(6.94444+1.94444)x100.0, glue set 3.06664
.\kern-2.0 (left margin)
.\hbox(0.0+0.0)x0.0          ← the \parindent box
.\tenrm A
…
.\tenrm ,
.\kern-3.0 (right margin)
.\glue(\rightskip) 0.0
```

and the last line of the paragraph:

```
..\tenrm .
..\penalty 10000
..\kern-4.0 (right margin)
..\glue(\parfillskip) 0.0 plus 1.0fil
..\glue(\rightskip) 0.0
```

Four things to read off it.

**The base is the font's quad, not the character's width.** `A` is 7.50002pt
wide and `\lpcode` is 200; 200/1000 of the width would be 1.5pt and the kern is
2.0pt — which is 200/1000 of cmr10's `\fontdimen6`, 10pt. Confirmed at a second
size: `cmr10 at 20pt` has quad 20.00005pt and an `A` 15.00003pt wide, and the
kern is **−4.00002**, which is the quad's fifth and not the width's.

So `left_pw(f,c) = quad(f) × lpcode(f,c) / 1000`, and `right_pw` the same with
`rpcode`. Both are *negative* kerns in the list.

**Where they go.** The left kern is the first thing in the line — *before* the
`\parindent` box, not after it. The right kern goes after the last node of the
line, which on the final line means after the `\penalty 10000` that §816
appended and *before* `\parfillskip`. So it is "after whatever the break node
is", not "immediately before `\rightskip`".

**The line still measures `\hsize`.** The negative kern is what lets the
content be wider than the measure; nothing about the packing changes.

**`\pdfprotrudechars` has two live values and they are not the same job.**
`=1` inserts the kerns and leaves line breaking alone; `=2` also lets the
protrusion into the badness, so the *breaks themselves* move. microtype's
default is the second, and the second is what the corpus needs: on
`2608.15742v1` we hyphenate `Corol-lary` where pdfTeX does not, because its
protrusion lets marginally more fit on that line.

## Done, and what it cost

Implemented and byte-identical to pdfTeX at all three settings of
`\pdfprotrudechars`. The staging was the useful part: `=1` inserts the kerns
and cannot move a break, so the widths and positions were validated with the
breaker frozen, and when `=2` moved the breaks the geometry underneath was
already known good.

`2608.15742v1` is exact — 8p/18097c — and corpus parity went 7 of 34 to 8.

**One hypothesis was wrong and is recorded so it is not retried.** At `=2` our
trace carries a candidate pdfTeX does not (`@\par via @@4 b=6`), which reads
like a node kept alive one step past its deactivation. §854's test was
therefore tried *without* the protrusion while the badness kept it — and six
box hunks appeared where there had been none. pdfTeX narrows the line for both
uses. The extra trace line has some other cause; the output is the authority
and the output is exact.

## What has not been measured yet

* Why our trace records one candidate more than pdfTeX's at `=2`, given the
  boxes agree exactly. Not the deactivation test — that was tried.
* ~~Whether a discretionary's pre-break text protrudes.~~ **Measured.** It
  does: pdfTeX sets `\discretionary / \tenrm - / \kern-7.00002 (right margin)`,
  the hyphen protruding and not the letter before it, and ours matches to the
  scaled point. microtype gives the hyphen one of the largest codes it sets, so
  this is the common case in a real document. Now in
  `tests/showbox/protrusion.tex` rather than an assumption in live code.
* A line whose last node is a box or a rule. `first_char`/`last_char` here
  recurse into a box, matching pdfTeX's `find_first_char`, and stop at a rule;
  the recursion is exercised by the `\parindent` box and nothing else.

## Why this is written down before it is written

The line breaker is the most delicate code in the engine and the one with the
least forgiving oracle: a width error of a fraction of a point moves a
hyphen four pages later, and every paragraph in the corpus is a test of it.
Two of this session's fixes (§915's hyphen kern, §1076's space factor) were
exactly that shape and each took a reduction to find. Guessing at the width
accounting and finding out through `corpus_check` would be the slow way round.


## The amount is `round_xn_over_d`, and it is asymmetric

pdfTeX computes the kern with its own `round_xn_over_d`, which is TeX's
`xn_over_d` plus a rounding step — and which normalises `x` but **not** `n`:

```text
function round_xn_over_d(@!x:scaled; @!n,@!d:integer):scaled;
begin if x>=0 then positive:=true
else  begin negate(x); positive:=false;
  end;
t:=(x mod @'100000)*n;
u:=(x div @'100000)*n+(t div @'100000);
v:=(u mod d)*@'100000 + (t mod @'100000);
if u div d>=@'100000 then arith_error:=true
else u:=@'100000*(u div d) + (v div d);
v := v mod d;
if 2*v >= d then u := u + 1;
if positive then round_xn_over_d:=u
else round_xn_over_d:=-u;
end;
```

`x` is the quad and is always positive; `n` is the protrusion code and is
routinely negative. When it is, `t`, `u` and `v` are negative too, `2*v >= d`
is false for every negative `v`, and the rounding step never fires — while for
a positive code it fires about half the time. So the function is deliberately
asymmetric about zero. On `cmr10 at 10.95pt`, quad 717621sp:

| code | pdfTeX kern |
|-----:|------------:|
|  −40 |  0.43799pt  |
|  +40 | −0.438pt    |
|  −35 |  0.38324pt  |
|  +35 | −0.38326pt  |

Rounding the magnitude and re-applying the sign — the natural thing to write —
is wrong by one scaled point for every negative code. That was four kerns in
2608.17397v1, each `0.03358pt` against pdfTeX's `0.03357pt`.

Pinned by sweeping `\rpcode` over −40..40 against `pdftex -ini -etex` and
reproducing all 80 values exactly. Rust's `/` and `%` truncate toward zero and
take the dividend's sign, which is what Pascal's `div` and `mod` do, so the
transcription is direct.

## A plain `\hbox` does *not* protrude

Worth recording because it looks as though it should. With
`\pdfprotrudechars=1` and codes set, `\setbox0=\hbox{(1.1)}` is 20.55562pt —
exactly what it is with protrusion off. Margin kerns come from the line
breaker, not from `hpack`. `tests/battery/protrusion-in-hpack.tex` holds the
eight probes.

So when an equation tag *does* carry margin kerns in a microtype document —
and it does, which is what sent me looking — they are microtype's own doing,
not something `hpack` added. That path is still unimplemented here; see the
task list.


## Only a glue node stops the search for the protruding character

Two different questions get asked of the same line, and they have different
answers:

- **Which character hangs into the margin** in the finished line. Asked after
  the trailing glue has been discarded, at the point the kern is inserted.
- **Which character narrows the line** for the width `try_break` scores. Asked
  of the candidate line, trailing glue and all.

Measured against `pdftex -ini -etex` by sweeping `\rpcode` between 0 and 1000
with `\pdfprotrudechars=2` and watching whether the reported badness moves.
A `.` sits before the break in every case:

| before the break | badness moves? |
|---|---|
| `ccc.\penalty-10000` | yes — the character protrudes |
| `ccc. \penalty-10000` | **no** — a glue node, and nothing protrudes |
| `ccc.\kern0pt\penalty-10000` | yes |
| `ccc.\penalty100 \penalty-10000` | yes |
| `ccc.\hbox{}\penalty-10000` | yes |
| `ccc.\hbox to0pt{}\penalty-10000` | yes |

So kerns, penalties, marks and zero-width boxes are stepped over and **only
glue stops the search**. The badness values are identical across the four
"yes" rows, so it is the same character being found in each.

Our `last_char_in` skips glue too. That is right where it is used — the
insertion site works on a line whose trailing glue is already gone, and its
kerns match pdfTeX exactly — and wrong for the breaking width.
`2608.17686v1` ends a line `$\mathbb Z$. ` at a penalty, so the glue before
that penalty is the last node; taking the period's 2.28pt anyway made the line
too narrow, b=6 against pdfTeX's b=13.

The breaking path now uses `last_char_before_break`, which differs from
`last_char_in` in exactly one arm.

Not yet measured: whether the **left** side has the same rule. The first node
of a line is normally a character, because §879 discards glue *after* the
break, so the case may not arise — but that is an argument, not a measurement.

## A forced break deactivates every active node

Found while writing the test for the rule above, and nothing to do with
protrusion — but it is what the test tripped over.

§851 loses the active node on **both** ways out of its first branch:

```text
if b>inf_bad or pi=eject_penalty then
  begin if final_pass and (minimum_demerits=awful_bad) and
    (link(r)=last_active) and (prev_r=active) then
    artificial_demerits:=true
  else if b>threshold then goto deactivate;
  node_r_stays_active:=false;
  end
```

`deactivate` removes it; the fall-through sets `node_r_stays_active:=false`.
We deactivated only when the line was *overfull*, so a node whose line at a
forced break was merely **too loose** survived past it:

```tex
\hsize=62pt \parindent=0pt
\setbox0=\vbox{\noindent aaa bbb ccc.\penalty-10000 zzz\par}
```

```text
ref    \vbox(18.94444+0.0)x62.0    two lines
ours   \vbox(6.94444+0.0)x62.0     one line, with `\penalty -10000` inside it
```

At plain's default `\pretolerance` the single line's badness is exactly 100,
which the *first* pass accepts — so the paragraph never reached the second pass
where §854 would have forced the break. Raising `\tolerance` to 9999 made the
two engines agree, which is what pointed at a feasibility test rather than at
the forced break itself.

## Where the right margin kern goes

The kern sits immediately **before the penalty the line broke at**, and
otherwise before the trailing glue. Measured with `\rpcode\font`\.=1000`:

```text
ccc.\penalty-10000 zzz            . KERN \penalty -10000 \glue(\rightskip)
ccc.\penalty-9000 zzz             . KERN \penalty -9000 \glue(\rightskip)
ccc.\penalty100 \penalty-10000    . \penalty 100 KERN \penalty -10000 \glue(\rightskip)
ccc.\par                          . \penalty 10000 KERN \glue(\parfillskip) \glue(\rightskip)
```

The third case is the one that pins it: an *ordinary* penalty inside the line
keeps the kern behind it, and only the break penalty takes it in front. The
fourth breaks at `\par`, so §816's appended `\penalty 10000` is ordinary
material and the trailing `\parfillskip` is what the kern goes ahead of.

A line that broke at a penalty has nothing after that penalty, so "no trailing
glue was skipped" is the whole test — the assembly loop does not need to be told
which node the break was.

## The two questions have one answer

A line broken at a penalty keeps the glue that sits *before* that penalty, and
there pdfTeX hangs **nothing** into the margin — the same rule the breaking
width follows. So both sites ask `last_char_before_break`, and the character
that narrows the line is the character that protrudes, by construction.

That correction is worth stating plainly: the insertion path was *not* simply
correct because `before_trailing_glue` strips the glue. That holds for an
ordinary line and fails for one broken at a penalty.

## A math node is stepped over too

The list of things that do *not* stop the search for the protruding character
grows by one: a math-on/math-off node. Measured — a line ending `… $\pi(1)$` is
set by pdfTeX as

```text
\OT1/cmr/m/n/12 )
\kern-1.37474 (right margin)
\mathoff
\glue(\rightskip) 0.0
```

so the `)` protrudes and the kern goes **ahead of** the `\mathoff`, exactly as
it goes ahead of the penalty a line broke at.

Stopping at the math node found no character at all, and the line protruded by
nothing. That is 1.37pt, and it was enough: in `2608.17953v1` the paragraph
beginning "(1) In these cases, the K3 cover …" then came out 1.12pt overfull
where pdfTeX has it at `b=95`, the first pass failed with nothing recorded, and
the second pass hyphenated `Proposi-tion` — one character more than pdfTeX in
81645.

Two things were ruled out first, and both are worth not re-testing:

- **Natural width.** Every piece of that line measured with `\hbox` — which does
  not protrude — is identical in the two engines: `AB=416.53339pt`,
  `CITE=107.45451pt`, `MATH=55.81622pt`.
- **Shrink.** The line has 19 glue nodes summing to 18.19052pt of shrink in both,
  with the same `\fontdimen4`.

Which left protrusion, and the `\pdfprotrudechars=1` dump — where breaking is
protrusion-free, so both engines break identically and the kerns are directly
comparable — showed 8 kerns in pdfTeX against our 7.

## "No character in the box" is two different answers

The walk into a box can end two ways, and they decide opposite things:

- **Exhausted** — the box's list ran out. Its *width* then decides: a zero-width
  box holds no position and the search steps over it, a box with width means the
  line does not begin at the margin and nothing protrudes.
- **Blocked** — the walk hit a glue (or a rule, or anything that is not a
  character). That is the end of the search, full stop. It does **not** resume
  after the box.

Collapsing both onto `None` made the blocked case fall through to the width
test and step past the box, finding a character pdfTeX never reaches. On
`2608.17686v1` that was eleven excess left margin kerns, one of them in front of

```text
\hbox(0.0+0.0)x0.0
 \glue 0.0
 \glue 0.0
 \glue -5.0
```

whose first node is a glue — pdfTeX protrudes nothing there, and we protruded
the `A` that follows the box.

The comment beside the code already said "it stops rather than resuming after
the box", with LaTeX's section-number box as the example. The code could not act
on it, because the function it called reported blocked and exhausted the same
way. Saying the rule in a comment is not the same as being able to express it.

`first_scan` now reports which, and the two simplifications the old comment
warns about stay dead: an empty `\hbox(0.0+0.0)x12.0` still blocks on width, and
`\hbox to 0pt{Alpha\hss}` still protrudes the character inside it.

### Measured while narrowing this

Against `pdftex -ini -etex`, sweeping `\lpcode` 0 → 1000 at
`\pdfprotrudechars=2` and watching the badness move — which leading nodes block
left protrusion:

| first node | protrudes? |
|---|---|
| a character | yes |
| `\kern0pt` | yes, stepped over |
| `\penalty100` | yes, stepped over |
| `\hbox to0pt{}` | yes, stepped over |
| `\hbox to0pt{(}` | yes, and it is the **inner** `(` that protrudes |
| `\hbox to5pt{}` | **no** — width blocks |
| `\parindent=12pt\indent` | **no** — width blocks |

And a non-zero `\leftskip` does *not* block: measured at
`\pdfprotrudechars=1`, pdfTeX inserts the left margin kern with `\leftskip` at
`0pt`, `0pt plus 1fil` and `2pt` alike. Badness is useless for that one — a
`fil` `\leftskip` makes every line `b=0` — so the kern has to be read out of the
box dump directly.

Unexplained and noted in passing: with a non-zero `\leftskip`, pdfTeX drops the
*right* margin kern.
