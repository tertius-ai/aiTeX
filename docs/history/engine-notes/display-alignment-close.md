# An alignment in a display — DONE, except its box warnings

**Status:** §774 and §812 are implemented; see
`tests/trace/display-alignment-close.tex`. What is left is noted at the end.


## What TeX does

`tex.web` §812, "Finish an alignment in a display":

```text
begin do_assignments;
if cur_cmd<>math_shift then @<Pontificate about improper alignment in display@>
else @<Check that another $ follows@>;
pop_nest; tail_append(new_penalty(pre_display_penalty));
tail_append(new_param_glue(above_display_skip_code));
link(tail):=p;
if p<>null then tail:=q;
tail_append(new_penalty(post_display_penalty));
tail_append(new_param_glue(below_display_skip_code));
prev_depth:=aux_save.sc; resume_after_display;
end
```

So `fin_align` in a display reads the closing `$$` **itself** — neither `$`
reaches `main_control` — and it pops the math nest before appending the
display's penalties and skips.

We append the penalties and skips (that part is right and tested), but we
never consume the `$$` and never pop the nest. Two consequences:

* the first `$` is dispatched as a command and traced, one `{math shift
  character $}` TeX does not write;
* the math list stays on the stack, so `Typeset::mlist` is one deeper than it
  should be for the rest of the run — and since §1153's mode name is derived
  from that depth, every later formula is named `math mode` where TeX says
  `display math mode`. `tests/trace/math-group-mode.tex` is the gate that
  would catch a regression there; it passes only because it has no display
  alignment in it.

## The three pieces

1. **`do_assignments` (§1270).** Before looking for the `$`, TeX absorbs any
   run of assignments. trip.tex needs it — line 251 is `\global\count6=
   \displayindent`, which belongs to the alignment's ending, not to what
   follows. There is no equivalent in this engine yet; §1206's `\eqno` path
   wants the same routine.

2. **§1197's check**, which is already implemented (`check_display_end`) and
   just needs calling from here.

3. **`pop_nest` and `resume_after_display`**, which the ordinary `$$` path
   already does — see `do_math_shift`.

## What else is in the same place

`\halign` inside a display is legal only when the enclosing group is the
`math_shift_group` (§1145):

```text
mmode+halign: if privileged then
    if cur_group=math_shift_group then init_align
    else off_save;
```

`off_save` (§1064/§1065) inserts whatever token would close the current group
— `\endgroup`, `$`, `\right.` or `}` — and reports `Missing … inserted`. That
is a general recovery mechanism with several callers, and it is missing
entirely. trip.tex line 250 hits it: `\begingroup\halign …` inside `$$`
gives `Missing \endgroup inserted`, and only after the inserted `\endgroup`
runs does the `\halign` reach §1130's check.

§1130 itself is done — see `tests/errors/improper-halign.tex`.

## Gates

`tools/trace_check.sh math-group-mode` for the nest depth, and a new case for
§812 once the `$$` is consumed. `tools/showbox_check.sh` already holds the
penalties and skips.

## And §774, at the other end of the same alignment

`init_align` pushes its nest level *before* `scan_spec` reads `to<dimen>` and
the `{`:

```text
push_nest;
if mode=mmode then
  begin mode:=-vmode; prev_depth:=nest[nest_ptr-2].aux_field.sc;
  end
else if mode>0 then negate(mode);
scan_spec(align_group,false);
```

So the `\the` in trip.tex's `\halign to\the\displaywidth{…}` is expanded in
*internal vertical mode* and §367 names it so. This engine has no nest level
for an alignment at all — `do_align` scans `to`/`spread` with `ts.mode` still
whatever it was, so a display alignment reports `{\the}` where TeX reports
`{internal vertical mode: \the}`.

The cell's own mode is already right (`{restricted horizontal mode: end of
alignment template}` matches), so what is missing is only the level *between*
rows. `Typeset::mode_test` would need to answer from the alignment stack when
one is open and no cell is; that is the same question §812 needs answered when
it pops the level, so the two belong in one change.

This is the divergence the command streams stand at now (command 721).


## What is left

Two things, both found by the same probe.

**§663's third warning form.** A `\halign to<dimen>` whose row does not fill
the width raises `Underfull \hbox (badness 10000) in alignment at lines N--M`,
and this engine raises nothing. `in alignment` is the third of §663's three
attributions — the other two, `detected at line N` and `in paragraph at lines
N--M`, are done and gated by `tools/warn_check.sh`. The case above sets
`\hbadness=10000` to keep it out of the way.

**A `{` inside a display alignment opens a sub-formula.** `begin_group` opens a
math group whenever `ts.mode` is `Math` and no box is pending, and inside a
display alignment the mode *is* still Math, because this engine gives an
alignment a mode name (`align_mode`) but not a real nest level. The lists so
opened hold nothing — the rows are placed as they are read — so §812 unwinds
to the depth the display was entered at rather than popping one level, which is
what `pop_nest` means anyway. Giving the alignment a real level would remove
the need for both `align_mode` and the loop.
