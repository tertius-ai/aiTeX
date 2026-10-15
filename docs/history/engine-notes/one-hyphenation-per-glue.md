# One hyphenation attempt per glue node

`(semi)stable` came out `(semi)sta-ble`. pdfTeX leaves it whole.

## The rule, in two halves

§866 is the only caller, and it fires on a glue node in the second pass:

```text
When the following code is activated, the |line_break| procedure is
in its second pass, and |cur_p| points to a glue node.
```

§896 then walks forward looking for a word to try, **stepping over** any
character whose `\lccode` is zero:

```text
if lc_code(c)<>0 then
  if (lc_code(c)=c)or(uc_hyph>0) then goto done2
  else goto done1;
continue: prev_s:=s; s:=link(prev_s);
```

which is why `(knowledge)` hyphenates as `(knowl-edge)` — the `(` is not a
boundary, it is scenery on the way to the word.

§897 then collects letters and stops at the first zero-`\lccode` character:

```text
if lc_code(c)=0 then goto done3;
```

and **that is the end of the attempt**. There is no path back to §896 for
whatever follows. A second word behind the same glue is never tried.

## Where we had it wrong

Our `hyphenate_list` carried an `after_glue` flag and let a zero-`\lccode`
character preserve it. That is right ahead of the word and wrong behind it —
the same character has to keep the search alive while §896 is looking and end
it once §897 has collected something.

`(semi)stable` is the case that separates the halves: §896 finds `semi`, four
letters cannot be hyphenated at `\lefthyphenmin=2`/`\righthyphenmin=3`, and
`stable` — `sta-ble` on its own — is never looked at. We stepped over the `)`
as though still searching and hyphenated it anyway.

The fix is one line: a flush that hands §897 an actual word spends the glue's
attempt.

```text
ref    XX (semi)stable YY        XX (knowl-edge) YY
ours   XX (semi)sta-ble YY       XX (knowl-edge) YY     (before)
ours   XX (semi)stable YY        XX (knowl-edge) YY     (after)
```

## A trap in the tooling

`tools/logdiff.sh` in `par` mode filters the transcript to lines matching `^@`.
The hyphenated *text* lines that `\tracingparagraphs` prints between the `@`
lines are not among them, so a document whose only difference is **which words
got hyphenated** reports `hunks 0`. It did, twice, on a reproduction that was
diverging the whole time. The `@` lines are the breakpoints; the text lines are
the discretionaries, and only one of the two is being compared.

Reading the two logs directly is the check that works:

```python
[l for l in unwrapped if "semi)" in l]
```

## An accent ends the word too

§897 takes a kern into the word it is collecting *by subtype*:

```text
else if (type(s)=kern_node)and(subtype(s)=normal) then
  begin hb:=s;
  ...
  end
else goto done3;
```

§896's skip loop ahead of the word tests the same way. §1125's `\accent` puts a
kern of subtype `acc_kern` on each side of its character, so a word containing
an accent ends at the accent.

On its own that only *shortens* the word. What makes it visible is the rule
above — one attempt per glue node — because the remainder is then never offered
either:

```text
ref    XX Gr^^?unbaum YY        XX co^^?operation YY
ours   XX Gr^^?un-baum YY       XX co^^?-op-er-a-tion YY
```

TeX finds `Gr`, cannot hyphenate two letters, and never looks at `unbaum`.

This one had been filed as a dead theory earlier, on the strength of
`\showhyphens{Gr\"unbaum}` reading identical. Two things were wrong with that:
`\showhyphens` sets `\showboxdepth=0` so the discretionaries are hidden, and the
second half of the mechanism — one attempt per glue — was not implemented yet,
so the probe could not have shown the difference even with the box visible. A
theory ruled out by a probe is only as ruled out as the probe.
