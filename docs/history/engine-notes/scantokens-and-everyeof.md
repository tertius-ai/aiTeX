# `\scantokens`, `\everyeof`, and why LaTeX's hooks depend on both

Opened 2026-08-21, chasing `2608.18000v1`, whose every theorem came out as the
literal text of its own `\begin{thm}[…]`:

```
   pdftex   Theorem 1.1 (Theorem ??). Let g ≥ 4. …
   ours     '\def\kerneltmpDoNotUse#1{\Hy@deferred@thm@head{#1}}'thm[Theorem ??] Let g ≥ 4. …
```

Bisecting the preamble one package at a time put it on **hyperref**, in ten
lines. The chain underneath took six fixes, each of which had to land before
the next became visible.

## What the chain is

LaTeX's hook mechanism defers a definition by *rescanning* it, and expl3's
`\tl_set_rescan:Nnn` is what does the rescanning. hyperref installs a hook on
`\@thm`. So every theorem in every document that loads hyperref goes through
`\__tl_set_rescan:NNnn`, which opens:

```text
\group_begin: \if_false: {\fi:
\int_set_eq:NN \tex_tracingnesting:D \c_zero_int
\int_compare:nNnT \tex_endlinechar:D = {32} { \int_set:Nn \tex_endlinechar:D {-1} }
\int_set_eq:NN \tex_newlinechar:D \tex_endlinechar:D
…
```

and finishes by parking a terminator in `\everyeof`, wrapping the material in
`'::` markers, and reading it back through `\scantokens` inside a `\tl_set:No`.
Six different things have to be right for that to work, and none of them were.

## The first five, in the order they surfaced

1. **`\tracingnesting` did not exist**, nor `\tracingassigns`, `\tracinggroups`,
   `\tracingifs`, `\tracingscantokens`. expl3's `l3names` aliases every e-TeX
   primitive unconditionally, so the very first line of the rescan died on an
   undefined control sequence. Added as integer parameters that store their
   value; the traces they ask for are still not emitted, which is a transcript
   gap and not a typesetting one.

   Deliberately still undefined: `\lastlinefit`, `\predisplaydirection`,
   `\savinghyphcodes`, `\savingvdiscards`, `\TeXXeTstate`, `\interactionmode`.
   Those *do* something when set, and a package that guards on `\ifdefined`
   would take a branch we cannot honour. Undefined is the honest state.

2. **`\everyeof` was never inserted.** `etex.ch` puts it in §362, before the
   file is closed and once per file:

   ```text
   if (every_eof<>null) and not eof_seen[index] then
     begin limit:=first-1; eof_seen[index]:=true; {fake one empty line}
     begin_token_list(every_eof,every_eof_text); goto restart;
     end
   ```

   The *second* arrival at the end is the one that closes the file.

3. **`\scantokens` was not expandable.** e-TeX spells it `pseudo_start` and
   §367 handles it in `expand`, so the pseudo-file starts *during* expansion
   and its material becomes part of whatever is being built. Ours dispatched it
   from `main_control`, which works only at the top level:

   ```
   \edef\y{\scantokens{ab}}   pdftex: macro:->ab␣   ours: macro:->\scantokens {ab}
   ```

   `is_expandable` is an ordinal test on the primitive enum and `\scantokens`
   sits on the command side of it, so it is named explicitly in both
   `get_x_token` and `expand_group_body` — as `\the` and `\detokenize` already
   were.

4. **A pseudo-file's last line lost its `\endlinechar`.** §31 reads *lines* and
   §360 appends the character to each, the last included. With
   `\endlinechar=-1` the two engines already agreed, which is what named it.
   Applied in `do_scantokens` and not in `push_named_file`, because that
   function is also the host's way of feeding the engine a *fragment* — one
   that is deliberately not a whole line, and a hundred unit tests say so.

5. **`\noexpand` was depositing a brace instead of backing it up.** §369 reads
   the token and puts it back, attaching the `\notexpanded:` marker *only to a
   control sequence*:

   ```text
   get_token; … back_input;
   if t>=cs_token_flag then
     begin p:=get_avail; info(p):=cs_token_flag+frozen_dont_expand;
     link(p):=loc; start:=p; loc:=p; end;
   ```

   A brace goes back as itself and `scan_toks` reads it as a brace, so
   `\noexpand` cannot stop one from closing a definition. We pushed it into the
   body instead, so the `}` of `\edef\a{\scantokens{ab}}` was swallowed and the
   `\edef` ate the rest of the file looking for a brace it already had.

   This one only became reachable once (3) landed, and biblatex is where it
   showed: `\everyeof{\noexpand}` is exactly how biblatex reads a `.bbx` inside
   an `\edef`, and the package went from loading to hanging the run. The gate
   catching that is `tools/packages.sh`.

## The sixth: one step of expansion has to start the file too

The catcodes were a false lead — the rescan honours them, as
`tests/battery/scantokens-everyeof.tex` now checks. What the trace was actually
showing is visible only with `\tracingmacros=2`'s *argument* lines, which a
filter of mine was dropping:

```
 ref   #3<-'\def \foo ####1{BAR{####1}}
 ours  #3<-\tex_scantokens:D {'\def \foo ##1{BAR{##1}}'}\if_false: }\fi: \tl_show:N …
```

`#3` is a *delimited* argument, and delimited matching does not expand. expl3
therefore expands `\tex_scantokens:D` first, through an `\exp_after:wN` chain:

```text
\exp_after:wN \__tl_rescan:NNw
\exp_after:wN #4 \exp_after:wN #5
\tex_scantokens:D { #2 #3 #2 }
```

Our `expand_one` — the one-step expander `\expandafter` uses — did not know
`\scantokens`, so the literal `\tex_scantokens:D {…}` was swallowed into `#3`
along with the rest of the document.

That is the third and last site: `get_x_token`, `expand_group_body` and
`expand_one` all have to name the primitive, because `is_expandable` is an
ordinal test on the enum and `\scantokens` sits on the command side of it.

The obvious test does not catch it. `\edef\x{\expandafter Z\scantokens{AB}}`
gives `ZAB` either way, because the unexpanded `\scantokens` reaches
`expand_group_body` a moment later and is handled there. It shows only where
the expansion has to happen *before* something reads tokens without expanding
them — `\expandafter\grab\scantokens{abSTOP}` with `\grab` taking a delimited
argument, which is the form the battery uses.

**`2608.18000v1` is exact with that fixed** — 17597 characters over 7 pages —
and so is the ten-line hyperref reduction it came from.

## What is still wrong

Nothing in the rescan. What remains is a transcript difference in expl3's
message *wrapping*: `\c__iow_wrap_end_marker_tl` holds a tab and six control
characters, and pdflatex writes the tab as a raw byte where we write `^^I`.
That is `-translate-file=cp227.tcx` again — the same printable-set gap as the
`ff` ligature, from the other direction.

## Two measurements worth keeping

`tools/primitives.sh` reports **321 of 321** primitives and that number is
**TeX82 only**; its oracle is `tex -ini`. A direct sweep against
`pdftex -ini -etex` finds thirty e-TeX primitives with no name anywhere in the
engine:

```
tracingassigns tracinggroups tracingifs tracingscantokens tracingnesting
predisplaydirection lastlinefit savinghyphcodes savingvdiscards pagediscards
splitdiscards interactionmode showgroups showtokens showifs middle
gluestretch glueshrink gluestretchorder glueshrinkorder gluetomu mutoglue
parshapelength parshapeindent parshapedimen beginL beginR endL endR TeXXeTstate
```

(five of them are fixed above). The harness's figure is sound but its scope is
narrower than it reads, and extending it to the e-TeX and pdfTeX sets is the
obvious next thing to do to it.

And: `\csname foo\endcsname` **defines** `\foo` as `\relax` when it was
undefined, so a probe built on `\expandafter\meaning\csname …\endcsname`
reports `\relax` for every name that does not exist. Two false conclusions came
out of that in one hour. `\ifx\foo\undefined` with the answer sent to the *log*
via `\message` is the form that tells the truth — `\write17` does not reach
this engine's stdout, which is how the same probe read "nothing missing" twice.
