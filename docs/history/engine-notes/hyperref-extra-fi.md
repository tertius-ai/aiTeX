# hyperref's `Extra \fi` — found, and it was §500

Opened and closed 2026-08-20. It began as a single `! Extra \fi.` at the moment
`\usepackage{hyperref}` finished, and the answer was three layers down from
where it announced itself. hyperref now loads with no errors and
`tools/packages.sh` reads 49/50. The search is kept because the method is
reusable and took several attempts to make work at all.

## The cause

`branch_from`'s false path counted where §500 compares identities.

TeX skips a false branch in a *loop*: `pass_text` scans to the next
`\else`/`\or`/`\fi` at nesting level zero, and then §500 asks whether the
condition stack is back where this conditional started — `cond_ptr =
save_cond_ptr`, a pointer identity. If it is, the token is ours. If it is not,
a conditional opened while scanning *this* conditional's operands is still
open, and a `\fi` swallows one of those before the loop tries again.

We started the skip `pending = cond_depth - cond_base` levels deep instead.
That count is not the identity: `cond_depth` counts only conditionals whose
branch has been *entered*, so an operand scan leaving two nodes at `if_code`
makes `pending` name a level that does not correspond to those nodes — and the
skip then pops whatever is on top, which can be a conditional with nothing to
do with this one.

The fix is §500 as written: `pass_text` returns the token it stopped at and
touches no node, and the caller loops on `cond_limit.last() == our id`. §496's
`pop_the_condition` folds in this engine's split bookkeeping — a node still at
`if_code` never entered a branch, so `cond_depth` never counted it and must not
be decremented for it.

## The reproduction

```tex
\documentclass{article}
\makeatletter
\AddToHookWithArguments{label}{\def\x{#1}}
\iftrue \AddToHook{label}{\relax} \count255=\currentiflevel \message{[V=\the\count255]}\fi
\makeatother
\begin{document}x\end{document}
```

```
pdftex: [V=1]
ours:   [V=0]   ! Extra \fi.
```

All three ingredients are needed, and each was checked on its own:

* the first hook must be **`WithArguments`** *and* its body must contain `#1`
  (`\AddToHookWithArguments{label}{\relax}` does not do it),
* the second must add to the **same** hook (a fresh hook name does not do it),
* it must be inside a conditional — that is what there is to lose.

So a condition node is dropped while `\AddToHook` reworks a hook that already
holds an arguments-using chunk. Everything downstream follows: the node that
goes missing in hyperref is the one LaTeX opens *around the whole package file*
— `\@onefilewithoptions` reads the package inside
`\ifx\@currname\@empty …\else <the entire file> \expandafter\@firstofone\fi` —
so the complaint arrives at the closing `\fi`, thousands of lines from the
cause, in `\@secondoftwo`, with nothing around it to suggest hooks.

## How it was found, in the order that worked

1. **Trace diff.** `\tracingmacros=2 \tracingcommands=2` over the whole
   `\usepackage{hyperref}`, both engines, 79-column wrapping undone, first
   differing event. This needed five transcript fixes before it worked at all
   (below), and then drove the frontier from event 3,321 to **655,294 of
   655,294 — the whole load, identical**. Every divergence it reported on the
   way was a real missing primitive.
2. **`\currentiflevel`**, implemented for the purpose (with `\currentiftype`
   and `\currentifbranch`, all measured against pdfTeX). The traces being
   identical *proved* the bug was in state the trace does not show, and this is
   the primitive that shows it.
3. **Probe bisection.** Insert `\message{[CIF=\the\currentiflevel]}` after line
   N of the real `.sty` — *not* truncation, which produces its own dangling
   `\fi`s and gave a non-monotone signal that wasted two passes. Bisect on
   agreement between the two engines: hyperref.sty:115 → `nameref` →
   nameref.sty:110 → `\AddToHookWithArguments`.
4. **Delta debugging** the four surviving lines.

Probe points must be *statement boundaries*: a probe landing inside a `\def`
body or between a macro and its argument reports nothing, and reads as
agreement if you let it.

## What the search fixed on the way

| first differed at | cause |
|---|---|
| event 3,321 | four transcript gaps: `\escapechar` ignored in names *and macro bodies*, `\noexpand` untraced, `\detokenize` over-traced, `\write`'s list unannounced (§63, §367, §477, §323) |
| event 76,902 | `\quitvmode` |
| event 107,050 | the pdftexcmds string primitives |
| event 107,187 | `\pdffilemoddate` |
| event 107,336 | `\pdfprimitive`, `\ifpdfprimitive` |
| event 121,734 | §292 cuts a traced argument *between* tokens, not at exactly 1000 characters |
| event 135,529 | `\pdfmatch` — a POSIX regex engine |
| event 206,134 | e-TeX prints the conditional after `\unless` as one `{\unless\ifcsname}` line |
| event 273,423 | `\endinput` ended the file where it stood; §362 ends it when the *line* does |
| event 283,243 | with `\endlinechar` out of range the ending is dropped, but §331 still starts the next line in `new_line` state — ours stayed mid-line, so the next line's indentation became a space |
| event 443,169 | `\pdfminorversion` and twenty-odd other pdfTeX parameters |
| event 655,294 | (none — the traces agree for the whole load) |

## Instruments left behind

* `AITEX_DBG_SEL=1` logs every condition-stack operation with its file and
  line, and dumps the input stack at an `Extra \fi`. Note that `nodes > depth`
  is *normal* — a node exists from `\if` to `\fi`, and depth counts only those
  whose branch has been entered — so the `LOST NODE` flag is the signal, not
  the inequality.
* `\currentiflevel` / `\currentiftype` / `\currentifbranch`.

## One known gap the search turned up and did not close

**`\endlinechar` is sampled when a line's ending is *reached*, not when the line
is *read*.** TeX appends `\endlinechar` to the buffer in `input_ln`, so a line
already buffered keeps the value in force when it was read:

```tex
\endlinechar=-1
\endlinechar=13
\message{[x
y]}
```

is `xy` in pdfTeX — the `\message` line was buffered while `\endlinechar` was
still −1, because the number scan for `13` had to look ahead onto it — and
`x y` in ours. Fixing it means giving each file frame the value its current
line was read with; the tokenizer is rebuilt per call and has no per-line state
to hang it on.
