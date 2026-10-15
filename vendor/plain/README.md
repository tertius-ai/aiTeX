# plain.tex

Knuth's plain TeX format — the macros described in *The TeXbook*, and what
`tex` preloads as `plain.fmt`. It defines `\newcount`, `\newdimen`, the math
and alignment macros, `\bye`, and the `\output` routine.

Copied verbatim from TeX Live 2024
(`texmf-dist/tex/plain/base/plain.tex`, version 3.141592653). Its own header
grants this:

> Unlimited copying and redistribution of this file are permitted as long
> as this file is not modified.

So it is here unmodified, and must stay that way.

## Why it is here

It is the engine's honest yardstick. Unit tests say the pieces behave; running
a real format says whether they compose. Every stall so far has pointed at
something we would not have thought to test:

| Stalled at | Cause |
|---|---|
| line 20 | `\catcode` could not affect text already tokenized — the input stack was needed |
| line 254 | dimensions with units, and `\newdimen`'s allocation machinery |
| line 268 | `\newif`, built from `\csname` composed with `\expandafter` |

Measure with:

```sh
cargo build --release && tools/plain_progress.sh
```

## What "reaching line N" does and does not mean

It means the engine executed the first N lines without erroring. It does **not**
mean it did so correctly — nothing here checks that the resulting state matches
real TeX's. And line count is a poor proxy for effort: the remaining lines
include math, `\halign`, and the output routine, which are far more than
proportionally difficult.
