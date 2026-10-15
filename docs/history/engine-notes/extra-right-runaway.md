# §1193's `Extra \right`: the fix is right, the loop is a mode restore

**Status:** the fix is written and verified against real TeX; it is not
committed because trip.tex then runs away. The cause is now located precisely
and is *not* in §1193.

## The fix

§1192: a `\right` closes the group `\left` opened, and only that one.
§1193: anywhere else the delimiter is scanned and the `\right` is *ignored*,
with `Extra \right`. We closed the formula's own group with it, so
`$A\right)B$` ended the math at the `\right` and set `B` as text — 18.47227pt
against TeX's 15.58682pt. §1160's `@<Get the next non-blank non-relax non-call
token@>` belongs with it, so `scan_delimiter` skips `\relax` and the error
lands on the right token. Both probes match `tex -ini` exactly, and trip's
command streams go 740 -> **778**.

## What the state at trip line 292 actually is

```text
RIGHT line 256 depth=2 lr=[]   ms=[2]        -> ignored, correctly
RIGHT line 257 depth=3 lr=[3]  ms=[2]        -> closes its \left
RIGHT line 278 depth=6 lr=[6]  ms=[2]        -> closes its \left
RIGHT line 292 depth=7 lr=[6]  ms=[2, 3, 7]  -> ignored, and then loops
```

The `lr=[6]` at 292 is **legitimate**, which is the thing I had wrong for two
sessions. A `\left` group is open at depth 6 inside formula 3, and formula 7 is
open inside *it*. TeX in that state has `cur_group = math_shift_group`, so
§1192's `cur_group <> math_left_group` is true and §1193 ignores the `\right`
— exactly as the fix does. TeX then unwinds: `\endgroup` at 7 inserts a `$`
that closes formula 7, `\endgroup` at 6 inserts a `\right.` that closes the
`\left`, `\endgroup` at 3 inserts a `$`. Three steps, done.

## The actual fault

Ours does not unwind, because **closing formula 7 leaves the mode Horizontal**
when it should be Math — formula 7 is inside a `\left` group, which is inside
formula 3. The next inserted `\right.` is then dispatched in horizontal mode,
§1046 inserts a `$` for it, and that `$` *opens* formula 7 again. Two `$` and
two `\endgroup` per turn, 119,255 turns.

So the question is one line wide: when a `$` closes a formula that was opened
inside a `\left` group, what does `saved_mode` hold? It should be `Math`.

## The missing guard — §1065

Measuring every `$` around trip.tex line 292 gives the entry to the loop:

```text
MS line 285 depth=6 mode=Math       ms=[2, 3]    lr=[6]
MS line 288 depth=6 mode=Horizontal ms=[2, 3]    lr=[6]
MS line 290 depth=7 mode=Math       ms=[2, 3, 7] lr=[6]
MS line 292 depth=6 mode=Horizontal ms=[2, 3]    lr=[6]
```

Line 285's `$` is dispatched at depth 6 — *inside* the `\left` group — and we
end the formula there. §1065 does not:

```text
mmode+math_shift: if cur_group=math_shift_group then after_math
  else off_save;
```

`after_math` is reached only when the formula's own group is the innermost
one. A `$` inside a `\left` group is not the end of the formula; there is a
`\right` missing first, and §1064 inserts it. Ending the formula regardless
leaves the `\left` group standing with no formula around it — which is exactly
the `mode=Horizontal` at line 288, and every recovery after that is working
from a state TeX cannot be in.

## The guard, and what it does not yet fix

Adding it — `off_save` when `math_shift_at.last() != group_depth` — keeps all
nine gates green and **still runs away on trip**, though differently: 40
million lines instead of 175, and almost all of them `{restoring \fam=N}` with
no commands traced between. That is a group being unwound over and over rather
than a command loop, so the next thing to measure is the save stack's depth,
not the token stream.

The guard is right by §1065 and is worth having; it is not committed only
because it cannot be shown green on trip, which is the gate that matters here.
