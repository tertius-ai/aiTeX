# `\if` and `\ifcat` classify by meaning, not by spelling

`\pdfstringdef\z{A B}` produced `\376\377\000A\000B` where pdfTeX produces
`\376\377\000A\000\040\000B`. Every space vanished from every hyperref PDF
bookmark; the visible symptom was a bookmark reading `1.Introduction`.

## Where it went

hyperref keeps a control sequence that *is* a space token:

```tex
\def\HyPsd@LetUnexpandableSpace#1{%
  \expandafter\futurelet\expandafter#1\expandafter\@gobble\space\relax
}
\HyPsd@LetUnexpandableSpace\HyPsd@UnexpandableSpace
```

`\HyPsd@ProtectSpaces` rewrites each real space in the argument to that control
sequence, so it survives the `\xdef`s that follow. `\HyPsd@CheckCatcodes` then
walks the result one token at a time and sorts each one:

```tex
\gdef\HyPsd@CheckCatcodes#1#2\HyPsd@End{%
  \global\let\HyPsd@Rest\relax
  \ifcat\relax\noexpand#1\relax        % a control sequence?
    ... \HyPsd@CatcodeWarning{#1}%
  \else
    ... \ifcat#1 % SPACE
          ...\HyPsd@String\HyPsd@SPACEOPTI
```

We answered *yes, a control sequence* to the first test, so each space went to
`\HyPsd@CatcodeWarning` — "Token not allowed in a PDF string ... removing
`\HyPsd@UnexpandableSpace'" — and was dropped. `\tracingmacros=2` over
`\pdfstringdef\z{A B}`, diffed against pdfTeX, shows the two engines separating
on exactly that line.

## The rule

§507 does not look at how a token is *written*. It reads `cur_cmd` and
`cur_chr`, which for a control sequence are `eq_type` and `equiv` — its
*meaning*:

```text
@<Test if two characters match@>=
begin get_x_token_or_active_char;
if (cur_cmd>active_char)or(cur_chr>255) then {not a character}
  begin m:=relax; n:=256;
  end
else  begin m:=cur_cmd; n:=cur_chr;
  end;
```

So `\let\x=A` makes `\ifcat\x A` true and `\if\x A` true, and a control
sequence `\let` to a space is a spacer. Note also what §507 does *not* do:
`get_x_token_or_active_char` is a plain `get_x_token`, so unlike almost every
other scanning context, `\ifcat` does **not** skip over spaces.

`\noexpand` does not interfere. §369 replaces a token's meaning with
`relax`/`no_expand_flag` only when it is expandable:

```text
cur_cmd:=eq_type(cur_cs); cur_chr:=equiv(cur_cs);
if cur_cmd>max_command then
  begin cur_cmd:=relax; cur_chr:=no_expand_flag;
  end;
```

`\HyPsd@UnexpandableSpace` is a spacer, well under `max_command`, so
`\noexpand` leaves it alone and `\ifcat\relax\noexpand#1` sees a space. Our
`if_char_class` asked `Token::as_char()`, which is a question about the token's
spelling, and every control sequence answered "not a character".

The fix resolves `Meaning::LetChar` before falling back to the lexical test.
Everything else already lined up with §507 by accident: `\noexpand` on a macro
gives `(256, None)` here and `(relax, 256)` in TeX; `\noexpand` on an active
macro gives `(c, Active)` here and `(active_char, c)` there.

## Not covered

`\chardef\x=65` stays "not a character" — TeX gives it `char_given`, which is
above `active_char`. That is correct and deliberate.
