# pgf: it was `\ifx` again, and the parameter errors were downstream

Opened 2026-08-20 as "a parameter character loses a doubling level". **That
reading was wrong** and is kept here because the way it was wrong is the useful
part.

## What it actually was

`\usepackage{pgf}` raised 70 errors where pdfTeX raises 0, and the eight
`Illegal parameter number in definition of \pgfkeyscurrentkeyRAW` looked like a
parameter-doubling fault — a `#` that should still be doubled arriving halved.
It was not. §507 compares `(cur_cmd, cur_chr)`, and a control sequence `\let` to
a character carries the same pair as the explicit token; `same_meaning` matched
on shape and answered false for anything mixed. So

```tex
\pgfkeys@check@slash ->\ifx \pgfkeys@possibleslash /\expandafter
  \pgfkeys@nevermind \else \expandafter \pgfkeys@addpath \fi
```

took the wrong branch for every key pgf defines, and `\pgfkeys@addpath` is what
does `\def\pgfkeyscurrentkeyRAW{#1}`. The parameter errors were **downstream of
a mis-taken branch**, and §479 was right to report them.

Fixed in `6fc453a`. packages 41/53 → 45/53; pgf, tikz, pgfplots and todonotes
all load.

## The lesson, which is about method and not about pgf

Three sessions produced eight hand-written reductions of "pgf's problem" —
delimited-argument matching, `\expandafter` chains, `\csname`/`\let`
round-trips, the key store, parameter texts arriving through arguments at one
and two nesting levels. **Every one matched `tex -ini` exactly**, and each one
narrowed nothing, because none of them was the failing path.

What found it, in one pass, was instrumenting **the package's own file**: copy
`pgfkeys.code.tex` into a scratch directory, wrap the failing line
(`826`) in `\tracingmacros=2 \tracingcommands=2 \tracingonline=0`, run both
engines, align on the first traced command and diff. Identical for 73 lines,
then one `{true}` against one `{false}`.

That is now three bugs found this way — `\ifx` on delimited macros, §510's
`if_limit`, and this one — against zero found by writing probes from the outside.
When a *package* fails and a hand-written version of the same idiom does not,
stop writing the idiom.

**Save pdfTeX's `.log` before running ours.** Both engines write
`\jobname.log`; ours overwriting the reference has cost four measurements so far
— `packages.sh`'s `.aux`, `trip.sh`, `trip_lists.sh`, and a trace diff that
reported 135,502 identical lines because it was comparing our output with
itself.

## Still open

`hyperref biblatex listings parskip microtype siunitx algorithm2e algorithmic`.
microtype needs `\pdfmatch` (P6); the rest are unexamined since the criterion
tightened to require zero errors and a balanced group.
