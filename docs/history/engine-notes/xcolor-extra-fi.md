# xcolor's `Extra \fi`, and the condition stack that is built upside down

Updated 2026-08-20. Supersedes the "csname frontier" reading, which was wrong;
the first half of that story was `\ifx` on delimited macros, fixed in `74c9c55`.

## The cause, established

`\or` and `\else` are treated alike in this engine — both skip to `\fi` and
close the conditional. TeX gates all three of `\or`, `\else`, `\fi` on §495's
**`if_limit`**, which is per-conditional state saying which of them may still
close the branch being read (§510):

```text
if cur_chr>if_limit then
  if if_limit=if_code then insert_relax
  else begin print_err("Extra "); print_cmd_chr(fi_or_else,cur_chr); … error; end
else begin while cur_chr<>fi_code do pass_text; @<Pop the condition stack@>; end
```

`if_code=1` while the condition is still being evaluated, then `else_code=3` in
a true branch (so `\or` is "Extra"), `fi_code=2` after `\else` has been taken
(so a second `\else` is "Extra"), `or_code=4` inside a selected `\ifcase` case.

A stray `\else` in a branch that has already taken one is therefore an *error
that changes nothing*. We instead skip to `\fi` and pop — eating a `\fi` that
belonged to an enclosing conditional, and leaving that conditional's real `\fi`
with nothing to close. **That is xcolor's `Extra \fi`: not a `\fi` too many, a
`\fi` eaten too early.**

Differential battery (`tex -ini` against ours), twelve conditional-skip forms:

```
\iftrue A\or B\fi          tex: AB + "Extra \or"     ours: A
\iffalse\or\else G\fi      tex: G  + "Extra \or"     ours: G, silent
\iffalse A\or B\else C\fi  tex: C  + "Extra \or"     ours: C, silent
```

## Why implementing §510 is not a small change

An `if_limit` stack was written and it works on those twelve forms — and it
breaks `tests/battery/nested-conditionals.tex`:

```
\if x\if Exe\else x\fi T\else F\fi     tex: T     with if_limit: TF
```

`AITEX_DBG_SEL=1` says why:

```
push(skip-else)   -> 1     the inner \if, opened during the outer's operand scan
push(true-branch) -> 2     the outer \if, pushed only now
pop(fi-command)   -> 1     the inner's \fi — pops the OUTER's entry
```

**§495 pushes the condition stack when `\if` is *scanned*.** We push only when
the condition has been *evaluated*, so a conditional opened while scanning
another's operands lands below it rather than above, and the two `\fi`s pop in
the wrong order. A plain depth counter never noticed, because order does not
matter to a count; `if_limit` is per-level state and exposes it immediately.

So the change has to be made in this order:

1. **Push at `\if`, not at the branch.** `cond_depth`/`cond_limit` gain an entry
   the moment a conditional is scanned, with `if_limit = if_code`. `branch_from`
   then *sets* the top's limit instead of pushing.
2. That subsumes `cond_eval`, which exists only to detect the
   condition-not-yet-evaluated state — exactly `if_limit = if_code`.
3. It probably also subsumes `skip_false_branch`'s `pending` parameter, which is
   a counter standing in for §500's `cond_ptr = save_cond_ptr` test. With a real
   stack, that test is what it is in TeX: compare the stack pointer.

## Landed 2026-08-20 (`2293030`)

`cond-stack-495` is merged. What finally unblocked it was two more instances of
the same shape — a conditional setting state belonging to a different one:

* **A stack index is not a handle.** §500 identifies a conditional by
  `save_cond_ptr`, which stays valid however the stack moves; a `Vec` index does
  not, and `branch_from` was setting somebody else's `if_limit`. Nodes carry an
  id now.
* **`\unless` never pushed a node.** It is a prefix, not a conditional of its
  own, and `eval_next_condition` reads the following `\if…` directly rather than
  through §498's push site — so `branch_from` found `cond_eval` empty and set
  the limit of the first conditional the job ever opened.

That was the `Extra \else` in `\@opcol`: the limit being consulted was not that
conditional's.

    latex_check   34/35 -> 35/35
    packages      40/53 -> 41/53, xcolor loads

trip is unmoved (58%, 4 of 9 `\showlists` blocks, 13 of 16 pages) — trip.tex
does not lean on these idioms the way LaTeX packages do.

## Still open

`hyperref biblatex listings tikz pgfplots pgf parskip microtype siunitx
algorithm2e algorithmic todonotes`. pgf and todonotes still report
`group_depth=1558969` and 39 undefined `\pgfeov`, which is a *group* leak rather
than a conditional one and wants its own investigation. microtype needs
`\pdfmatch` (P6). The rest are unexamined since the criterion tightened.
